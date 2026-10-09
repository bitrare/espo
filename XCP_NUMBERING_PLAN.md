# Espo XCP history and unit numbering — implementation plan, revision 3

Prepared 2026-09-09 after read-only inspection of `88.99.51.244` and three design
passes. Proposed implementation target: `/var/www/espo-xcp`.
This revision supersedes the earlier local proposal. No implementation or backfill
has been started; the only planned write from this investigation is this document.

## Decision

Add a **separate XCP history service**, built from this Espo repository, and expose
it through the existing XCP plugin. It owns a new database, archives Core events,
and derives numbered units for every non-divisible asset. Existing Espo processes
continue from their existing state.

The new service does not initialize Alkanes, Metashrew, the Espo module registry,
`ESPO_HEIGHT`, `SAFE_TIP`, or the shared block-indexing loop. It never opens either
Espo database. Its checkpoint is never returned to `module_resume_start_height`.
No Alkanes rewind, reindex, or resynchronization is part of this project.

```text
Existing Core v2 :4100
       |
       v
New espo-xcp-history service :9093 (loopback only)
  complete XCP event archive -> deterministic numbering projection
  own checkpoints / undo / reads / resource limits
       |
       v
Existing espo-xcp :8082 -> xcp.* RPC -> explorer / Nakamotos Club / Bitrare

Existing Alkanes indexing and databases remain outside this data flow.
```

The service is an implementation component of the XCP plugin, not a second
Counterparty consensus implementation. Core decides valid ledger effects; the
numbering rules decide which fungible units each effect represents.

## What the server inspection established

- Espo checkout: `48487b10432ebf33c2e7c7b15773f22d0d4c7202`, dated 2026-09-09.
  Baseline untracked files: `config.json.bak-xcp`, `run-view-only.sh`.
- `espo-xcp.service` is active in `/var/www/espo-xcp`, RPC port 8082, with `./db`.
  A separate `espo.service` runs from `/var/www/espo`. Neither was restarted.
- The inspected running Espo XCP command line did **not** contain `--view-only`.
  A view-only launcher exists, but its presence does not establish the active
  service mode. The proposed worker is independent of either Espo mode.
- Core runs from `/root/counterparty-core`; its localhost v2 endpoint reports
  version **11.3.0**, commit `cd827e0fafe62b3166b2a186749e4191e1897170`, ready and
  following at block **966132** at inspection time. The source checkout matches.
- The current XCP module still performs no persistent indexing and adopts
  Essentials' tip. Espo's shared resume height is the minimum module height.
- `/blocks/428919/events?limit=3` reported 87 records and descending event order.
  Following the returned cursor produced the next records without a gap.
  `NEW_BLOCK` includes hash and parent hash; `BLOCK_PARSED` includes ledger,
  transaction-list and message hashes. System events can have a null tx hash.
- Actual non-divisible UTXO assets were verified: KEKMONTECRLO (supply 240) and
  A3344466055756786903 (supply 4618). Attach, detach, move, pool deposit, pool fill,
  and valid/invalid pool withdrawal payloads were sampled, without scanning history.
- Approximately 1.3 TiB disk and 45 GiB available memory were reported. Swap was
  almost full; short vmstat samples did not show sustained heavy swapping.
  This supports a bounded pilot, not an unbounded import or a throughput estimate.

## The three iterations

**Pass 1 — deployment isolation.** Challenge: the previous proposal put a worker
inside Espo and reused module storage. An older XCP checkpoint could interact
with the minimum-height resume logic; shared storage and memory also couple the
workloads. Revision: independent executable, lifecycle, database and loopback
read API; only a small bridge lives in the XCP plugin. A stop/rebuild of XCP history
must not stop, rewind, or migrate Alkanes.

**Pass 2 — protocol semantics.** Challenge: pairing credits/debits by transaction
hash and treating all movements as wallet transfers breaks multi-leg operations,
UTXO containers, order-funded pool fills and fairmint block actions. Revision:
archive every event; normalize complete block effects with operation-specific
references; use typed locations and explicit serial-order rules. Invalid records
remain historical evidence but do not cause effects.

**Pass 3 — replay and operations.** Challenge: individual block/header requests
are expensive for the initial archive; one checkpoint conflates fetched history
with verified numbering; shared RocksDB access creates process ownership problems.
Revision: batched global-event bootstrap, two coverage checkpoints, one database
owner, independent undo/rebuild, strict reader status and a resource-limited pilot.
The following sections are the consolidated revision-3 plan.

## 1. Service and storage boundary

Add `src/bin/espo_xcp_history.rs` as an explicit Cargo binary and whitelist it in
`.gitignore` (the repository otherwise ignores new `src/bin/*` files). Reuse the
existing Rust library infrastructure but put the worker's implementation under
`src/modules/xcp/history/`, with no calls to global Espo initialization.

Suggested components:

- `reader.rs`: strict Core transport, pagination, readiness and hash checks.
- `archive.rs`: complete raw block envelopes and resumable download coverage.
- `normalize.rs` and `operations/`: typed operation effects and source references.
- `engine.rs`, `ranges.rs`: pure deterministic numbering and ordered-range logic.
- `storage.rs`, `undo.rs`: atomic writes, query indexes, checkpoints and rollback.
- `service.rs`, `api.rs`, `client.rs`: lifecycle, loopback reads and Espo bridge.

Use a new database under `/var/lib/espo-xcp-history/`, opened read/write by the
history service **only**. Espo queries it over loopback HTTP; it does not open a
second RocksDB handle or tail database files. Explicitly separate raw archive
and derived projection keyspaces. No changes to existing Mdb versioning or the
Alkanes tree schema are required.

Modify `xcp/config.rs` and `xcp/rpc.rs` for the optional history-service endpoint,
status and query forwarding. Keep existing XCP read methods operational if the
worker is unavailable. New methods return a clear unavailable/stale status rather
than an empty holder list. Document every new method, query and response in
`src/explorer/pages/docs.rs`, as required by the repository instructions.

Leave `src/main.rs` shared resume logic, the existing module index height contract,
Alkanes genesis heights and both existing services' database paths alone. The
history worker has a different entry point and its own small configuration file.

## 2. Complete XCP archive, then deterministic replay

Maintain distinct concepts:

- Download coverage: which historical event intervals are safely archived.
- `archive_tip`: highest contiguous, validated complete XCP block available locally.
- `numbering_tip`: highest contiguous block fully applied by a specific ruleset.

A download that has reached a recent height does not imply older coverage or
correct current ownership. Status includes archive coverage, both tips, source
version, hashes, rules version, readiness and any blocking event.

**Bootstrap:** fix a Core source version and target block, initially six blocks
behind Core's processed tip. Obtain that block's final event boundary. Page the
unfiltered `/v2/events` feed backward using Core's exact `next_cursor`; do not
subtract one or combine cursor and offset. Store pages on disk with interval
coverage and payload digests. Bound all data to the fixed target. Once the archive
covers the required Core genesis boundary, walk events forward and assemble full
blocks using `NEW_BLOCK`/`BLOCK_PARSED`, checking parent links and event coverage.
This avoids issuing a separate header request for every historical block.

Confirm any genesis boundary exception against Core rather than inventing an
empty block. Check the pinned target hash and Core source identity during and
before publishing bootstrap coverage. Reorgs or a reparse invalidate affected
staging intervals; discarded pages never become trusted current state.

**Following:** fetch complete new blocks, all pages, in ascending event order,
including pre-transaction and post-transaction effects. Recheck Core readiness,
block completion and hashes. Temporarily missing data is a retry, not an empty
block. Bound concurrent requests and spill large blocks to disk if needed.

Retain raw event payloads, Core event index, containing block hash/height, tx hash,
`msg_index` where present, and operation references. Do not deduplicate by tx hash:
block actions may reuse an earlier fairminter hash or have no ordinary transaction.
Source event indices are tied to a Core parsing version, not eternal identities.

Normalize each effect exactly once from semantic records plus their ledger
records. Issuance and its CREDIT are one allocation, not two. Pool reserve updates
are corroboration of a pool effect, not an additional transfer. Preserve relative
order of interleaved DEX/pool fills within a transaction. Build complete-block
context, but apply normalized effects at their ledger positions rather than simply
sorting transaction summaries by completion time.

Initially stop the **numbering projection** at an unknown relevant balance-changing
operation; the raw archive may continue. This single contiguous verified prefix is
simpler and safer than advancing some dependent assets across an unresolved event.
Whitelisted metadata/non-effect events can pass. Unknown effects never disappear
behind a "fully indexed" flag.

## 3. Identity, location and serial rules

Use `(network, canonical asset ID, ruleset, serial)` for identity. Resolve names,
numeric asset names and subasset aliases to the canonical asset. Follow historical
divisibility and maintain metadata/supply state for all assets needed by an
operation, while assigning serials only to eligible non-divisible supply.

Use checked integer arithmetic in Rust and decimal strings in JSON. Persist the
next serial independently of balances. Additional issuance never reuses numbers;
metadata changes and issuer-right transfers allocate nothing. Destruction retires
numbers. Proposed reset policy: retire the old supply, record a new divisibility
epoch, and continue with fresh serials if the replacement supply is non-divisible.
A divisible epoch is not represented as numbered units. This reset policy is an
application extension and belongs in the versioned rules specification.

Typed locations include address, UTXO outpoint, DEX order, pending BTC order match,
dispenser, pool reserve, fairminter escrow, and retired/destroyed state. Keep the
associated/controller address separate. Never equate an issuer with a unit owner,
or automatically classify everything at Core's UNSPENDABLE address as destroyed:
Core uses it for temporary fairmint/premint/liquidity escrow too.

Store ordered ranges with direction, for example `[1..1000000 ascending]`, in
persistent location stacks. Splitting, reversing and appending ranges reproduces
the deck model without allocating one row per unit. Add a serial interval/location
index, address/location indexes, and range-to-event history index. Fragmentation
and history length still cost storage, so test them separately from supply size.

For legacy operations, target XCPToNFT behavior: ordinary sends remove the tail,
reverse the selected slice and append it. Preserve the legacy exceptions, including
non-reversing dispenser refill. A self-send must use one atomic stack, not stale
source/destination copies. Restore actual stack order after restart.

### UTXO wrapping: proposed extension

- `ATTACH_TO_UTXO`: address stack -> exact outpoint; tail/reverse/append, same serials.
- `UTXO_MOVE`: move the entire per-asset stack to Core's destination outpoint,
  preserving internal order. A Bitcoin move passes the container rather than
  redealing its cards. When sources merge, append in Core's recorded move order.
- `DETACH_FROM_UTXO`: exact outpoint -> Core-resolved address; tail/reverse/append
  for the affected quantity. Support both quantity-based detach and detach-all
  forms, with a distinct leg for each asset.
- Handle automatic detach when Core resolves a spent UTXO without a normal move
  destination; do not infer the destination from the Bitcoin transaction ourselves.

Example under these proposed rules: attaching two from `[1,2,3]` creates UTXO
stack `[3,2]`; moving it preserves `[3,2]`; detaching both appends `[2,3]` to the
recipient. Wrapping never creates new serials. Container-order preservation is an
explicit new convention, not something Counterparty itself guarantees for serials.

### Pools: proposed extension

Track one reserve stack per Core pool/asset, not a collection of serials owned
individually by LP holders. LP tokens are divisible in the inspected implementation;
that does not exclude non-divisible assets from pool reserves.

- Valid `NEW_POOL_DEPOSIT`: take the actual debited amounts from the depositor
  stacks and tail/reverse/append them to the reserve stacks. Record pool creation
  metadata once; do not also transfer on `OPEN_POOL`/`POOL_UPDATE`.
- `POOL_MATCH`: resolve `order_tx_hash`. Move `backward_quantity` from that order's
  reserved stack into the matching reserve and `forward_quantity` from the other
  reserve to the recipient, using tail/reverse/append on both economic legs.
  The wallet was already debited at order opening; do not debit it again.
- Include the full recorded pool input. In the deployed Core code the fee stays
  in reserves; do not subtract it again or create a fictitious fee-owner stack.
- Valid `NEW_POOL_WITHDRAWAL`: move the recorded redeemed quantities out of each
  reserve to the recipient. Do not number LP tokens or attempt to restore the
  depositor's original serials. Rounding and slippage outcomes come from Core.
- Fairminter-created liquidity moves units from the fairminter's escrow into the
  pool during block-level processing, not from an assumed wallet balance. Cover
  successful soft-cap release and failed-soft-cap destruction/refunds.

These rules must be frozen in fixtures before numbers are exposed publicly.
Invalid pool records retain their history/status and cause no unit movements.

The complete coverage set also includes classic/enhanced/MPMA sends, sweeps,
dividends paid in a numbered asset, fairmints/premints, destruction, DEX partial
fills, pending BTC settlement and expiry/refunds, and dispenser open/refill/dispense/
delayed-close effects. Preserve Core's operation order; do not recompute settlement,
fees, output routing, recipient ordering or pool pricing from today's rules.

## 4. Atomicity, reorgs and schema changes

One service owns all XCP writes. Archive writes and projection writes have separate
committed checkpoints; projection cannot pass verified archive coverage. Apply a
projection block as one atomic database batch containing state edits, histories,
undo preimages, counters and checkpoint. Use a database snapshot for multi-index
API reads. On crash, restart at the last committed boundary without duplicate history.

Persist block hash/parent, Core source version, raw payload digest and available
Core ledger/message hashes. Periodically check recent hashes even when idle.
A fork pauses projection, invalidates canonical archive coverage from the fork,
undoes affected projection blocks and replays the replacement branch. A deep fork
beyond the retained undo window rebuilds from a verified XCP checkpoint/archive.
No Espo global rollback is invoked.

A Core reparse/upgrade may change events without changing Bitcoin hashes. An
unrecognized source version pauses ingestion pending adapter verification; digest
and checkpoint checks detect sampled history changes. A rules/schema change creates
a fresh XCP projection from the raw archive, with an explicit generation switch.
It does not require supporting the old index schema indefinitely or rebuilding
Alkanes. Budget disk for simultaneous generations before starting that rebuild.

API cursors contain the rules generation and checkpoint. A request reads one
snapshot; a later page against a changed checkpoint returns a stale-cursor error
rather than silently mixing owners. Initial scope is current state plus history,
not arbitrary historical holder snapshots.

## 5. API and clients

Proposed new public RPC methods, forwarded by Espo's XCP plugin:

- `xcp.get_numbering_status`
- `xcp.get_asset_units` (paginated ranges or bounded unit pages)
- `xcp.get_unit` and `xcp.get_unit_history`
- `xcp.get_address_units` (available, escrowed and UTXO locations distinguished)
- `xcp.get_transfer_units` (all legs; nullable transaction for block actions)

Responses carry network, ruleset, Core version, projection height/hash and coverage.
Worker failure affects only these new methods; existing asset and market methods
continue to work. Control actions such as rebuild/reset are CLI/service operations,
not unauthenticated public RPCs.

After parity checks, switch Nakamotos Club to the generic asset parameter and
numbering endpoints. Keep its static PFP assignment outside this engine. ETH vault
ownership enrichment remains a separate concern; XCP UTXO wrapping is not an
Emblem Vault ownership lookup.

## 6. Implementation batches and release gates

**A — fixtures and rules.** Pin the deployed Core commit; capture bounded complete
blocks and payloads. Freeze legacy stack rules and the UTXO/pool extensions above.
Use the archived RAREPEPE and DJPEPE outputs as regression references, comparing
RAREPEPE at block 725934. Historical parser differences must be explained at the
first differing effect; they cannot be silently patched by changing established IDs.

**B — independent archive service.** Add binary, configuration, separate storage,
bootstrap/follow reader, coverage/status API and service definition. Start with a
bounded pilot. Verify the entry point never initializes Alkanes and that stopping
or restarting it does not touch either existing Espo service/database.

**C — numbering engine.** Build pure range operations, metadata epochs, complete
normalizer, query indexes and atomic projection/undo. Cover legacy operations,
UTXO wrapping, pools and fairmint liquidity before advertising general support.
Test large supplies and highly fragmented stacks without expanding all units.

**D — failure and equivalence tests.** Compare one-shot replay with many batch
boundaries and restarts. Inject failure before/after commit; simulate same-height
reorgs, lagging Core, repeated pages, missing pages, source-version changes and
unknown operations. Require no duplicate/lost serials, same ordered stacks after
recovery, and quantity conservation across wallet, UTXO, escrow, pool and retired
locations. Maintain an independent quantity-only mirror from Core ledger effects
for per-block checks. End-to-end balance checks use a fixture/snapshot at the
comparison height, or a temporary verification projection advanced to a live Core
tip sampled before and after the queries. Retry if that tip changes; never compare
the six-block-lagged public projection to latest balances or assume the API offers
arbitrary historical balances.
Successful numbering of RAREPEPE alone is not general protocol coverage.

**E — bridge and staged rollout.** Add/document Espo methods, build a separate
candidate binary, and test the bridge before a planned restart of `espo-xcp.service`.
Its normal startup must resume existing state; do not pass rollback/reindex flags.
Production `espo.service` and Core do not need restarts. Run the worker privately,
validate through the legacy checkpoint, then enable the new UI behind a flag.
Rollback disables the bridge or stops the new worker; it does not restore Alkanes DBs.

Suggested initial worker limits: two fetch tasks, two Core requests/second, bounded
page/block buffers, a 128 MiB RocksDB cache, one CPU quota, 2 GiB MemoryHigh and
3 GiB MemoryMax, low I/O weight. Start conservatively and tune after measuring
throughput and Core latency. Use `StateDirectory=espo-xcp-history`, a dedicated
service account, and write access only to its state/log directory. Enforce a disk
free-space floor and stop before exhausting it. The proposed loopback port is 9093;
verify it remains free at deployment. Benchmark a representative bounded range
before promising a completion time or raising the import rate.

## Evidence and review boundaries

Deployed Espo source references: `src/modules/xcp/main.rs`, `src/modules/xcp/rpc.rs`,
`src/modules/xcp/config.rs`, `src/modules/defs.rs`, `src/main.rs:288`,
`src/runtime/mdb.rs`, `src/modules/runes/storage.rs`, `Cargo.toml`, `.gitignore`,
`AGENT_RULES_MUST_READ.md`, and `CLAUDE.md`.

Core source at the deployed commit:

- [UTXO movement](https://github.com/CounterpartyXCP/counterparty-core/blob/cd827e0fafe62b3166b2a186749e4191e1897170/counterparty-core/counterpartycore/lib/messages/move.py)
- [UTXO attach/detach message](https://github.com/CounterpartyXCP/counterparty-core/blob/cd827e0fafe62b3166b2a186749e4191e1897170/counterparty-core/counterpartycore/lib/messages/utxo.py)
- [Pool fill execution](https://github.com/CounterpartyXCP/counterparty-core/blob/cd827e0fafe62b3166b2a186749e4191e1897170/counterparty-core/counterpartycore/lib/ledger/markets.py)
- [Fairminter block operations](https://github.com/CounterpartyXCP/counterparty-core/blob/cd827e0fafe62b3166b2a186749e4191e1897170/counterparty-core/counterpartycore/lib/messages/fairminter.py)
- [Event pagination](https://github.com/CounterpartyXCP/counterparty-core/blob/cd827e0fafe62b3166b2a186749e4191e1897170/counterparty-core/counterpartycore/lib/api/queries.py)

Live examples inspected: attach event 20533375; detach 20511612; move 20534688;
pool deposit 20539140; pool fill 20539728; invalid withdrawal 20533466 and valid
withdrawal 20524879. Pool samples verify payload shape, not non-divisible numbering
correctness. Full block fixtures and controlled edge cases are still implementation
work. No consensus replay, performance benchmark, software deployment or service
restart occurred during this planning task.
