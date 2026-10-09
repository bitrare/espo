# Counterparty unit numbering

The `espo-xcp-history` binary is an independent component of Espo's XCP plugin.
It reads the existing Counterparty Core API, owns a separate RocksDB database,
and serves a loopback-only read API. It never initializes Espo's module registry,
opens Espo's existing database, or contributes a height to Espo's resume logic.
The XCP plugin calls it over HTTP. Existing Alkanes indexing is outside this path.

The explorer guide is `/counterparty/numbering`. API explanations and example
requests/responses are in `/docs#xcp-numbering`.

## Deployment

This installation uses:

- Source checkout: `/var/www/espo-xcp`.
- Core API: `http://127.0.0.1:4100/v2`.
- Worker: `espo-xcp-history.service`, API `http://127.0.0.1:9093`.
- Worker state: `/var/lib/espo-xcp-history` (systemd manages its private directory).
- Espo JSON-RPC: port 8082; explorer: port 8083.
- Source adapter: Core 11.3.0, commit
  `cd827e0fafe62b3166b2a186749e4191e1897170`, mainnet.
- Ruleset: `nakamoto-stack-v1`; database marker: schema 1.

The only required existing configuration addition is
`modules.xcp.history_url = "http://127.0.0.1:9093"`. It is optional: an unconfigured
or unavailable worker returns an explicit error for numbering methods, while
existing XCP methods continue to use Core.

The service unit is in `deploy/espo-xcp-history.service`. It uses DynamicUser,
StateDirectory, a read-only system filesystem, one CPU core, MemoryHigh=2G,
MemoryMax=3G, and low I/O priority. The RocksDB cache is 128 MiB and writes use
small bounded buffers. The archive is rate limited to two Core requests/second
and pauses if fewer than 64 GiB remain free. Jemalloc background purging is enabled.

Build and checks:

```sh
CARGO_BUILD_JOBS=2 cargo build --release --features binary --bin espo --bin espo-xcp-history
CARGO_BUILD_JOBS=2 cargo test --release --features binary --bin espo-xcp-history
CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2 cargo test --release --features binary --lib
```

Read status without opening a database:

```sh
curl -sS http://127.0.0.1:9093/status
systemctl status espo-xcp-history.service
journalctl -u espo-xcp-history.service -n 30
```

The initial explorer binary/config backup is outside the repository at
`/var/backups/espo-xcp-numbering-20260909`. Restoring the explorer does not require
changing history storage. The separate production `/var/www/espo` service must
not be modified for this feature.

## Archive and replay

Bootstrap pins Core's processed tip minus six blocks, proves genesis is event 0
at height 278270, and follows the global descending event cursor exactly. Pages
must contain contiguous event indexes and the expected next cursor. Raw events,
a page digest and its resume cursor commit atomically. Restarts resume the saved
cursor. The target block, message hash and source identity are checked regularly.
The source event count is informational; it is not used to infer coverage.

Once download reaches genesis, the archive validates full blocks in ascending
order, including NEW_BLOCK/BLOCK_PARSED boundaries and parent hashes. A separate
projection thread follows this contiguous validated prefix. Following mode fetches
complete confirmed blocks and rechecks their boundary before committing them.

Every projected block atomically commits holdings, directed ranges, ownership
indexes, movement history, serial counters, its checkpoint, and undo preimages.
An error discards the entire projected block. Core address/UTXO debit-credit
quantity changes are reconciled with the numbered changes; final pool reserve
quantities are also checked. Unknown relevant semantic events or ledger actions
pause projection instead of skipping units. Archive work remains independent.

The worker retains 288 blocks of projection undo. Shallow forks reconcile to a
common archived boundary under the same write lock used for projection, undo the
numbering changes, discard the invalid raw suffix, and fetch the replacement.
Deep forks, source adapter changes, or bootstrap target changes pause explicitly.
Rebuilding after these cases uses a fresh XCP history StateDirectory and the
existing Core API; it never requires an Alkanes database reset. No destructive
rebuild operation is exposed through the public API.

## Numbering rules

Serials begin at 1 for valid non-divisible issuance and advance through later
issuances. They are never reused. Each location holds an ordered stack compressed
as inclusive directional ranges. A normal transfer takes the sender's tail,
reverses it, and appends it to the recipient. The original dispenser refill
exception appends without reversing. Sorting by serial changes the meaning.

Addresses, UTXOs, order escrow, pending BTC matches, dispensers, pool reserves,
fairmint escrow, destroyed units and retired units are distinct locations.
Controller addresses are associations, not issuance rights. UTXO moves preserve
internal order; attach/detach reverse. Pending BTC reservation and return to a
live order preserve the reserved order slice; final delivery reverses it once.

Pool deposits use actual Core quantities. Pool matches take input from reserved
orders, include the fee in the reserve, and deliver output from the other reserve.
Withdrawals use the current reserve stack, without promising original deposit
serials. LP assets remain divisible. Fairmint releases follow Core's recorded
allocation order; an escrow address is not automatically a destruction event.
Supply resets retire the old supply and increment the epoch; newly issued
indivisible units continue from the persistent serial counter.

## Read API and interpretation

Public methods are `xcp.get_numbering_status`, `xcp.get_asset_units`,
`xcp.get_unit`, `xcp.get_unit_history`, `xcp.get_address_units`, and
`xcp.get_transfer_units`. Private `/query` accepts the same method without `xcp.`
and a `params` object. No write endpoint exists.

All data responses include a checkpoint context. `complete` requires the
projection to match the validated archive and target, a recent source heartbeat,
and no pause errors. Partial historical reads require `allow_partial:true`.
`verified:false` remains explicit until the separate full-chain compatibility
and live audit gates have been completed. Download completion alone is not
numbering completion.

Ranges preserve direction and include their stack position. Serials, canonical
asset IDs and range quantities use decimal strings. A cursor encodes the exact
checkpoint and query; it fails with `stale_cursor` after a change. A filtered
history page can contain no matches but still have `has_more:true` because each
scan is bounded. Follow its cursor. `tx_hash` may be null for block effects;
`event_reference` preserves the original Core reference, including old transaction
hashes and match IDs. Transaction lookups include later effects referencing that
transaction, and never deduplicate whole transactions into one movement.

## Verification scope

The offline convention fixtures were derived from the original XCPToNFT archive
using `scripts/prepare_legacy_numbering_fixture.py`. They carry SHA-256 hashes of
the original inputs and expected saved outputs. Tests compare every saved serial
history and every ordered nonempty holding: 300 RAREPEPE units (1,685 normalized
operations) and 169 DJPEPE units (568 operations).

Complete Core blocks captured from the pinned running node are retained under
`tests/fixtures/xcp_numbering/core-11.3.0`. Focused adapter tests replay the
relevant full transactions with synthetic pre-block serials. They cover actual
RAREPEPE issuance, KEKMONTECRLO attachment, numeric-asset UTXO movement/detachment,
SIDEEYE pool deposit and order-funded swap, and CAKE pool withdrawal. These prove
adapter semantics for the captured operations; synthetic starting serials do not
establish pre-block historical identities.

Other tests cover large compressed supplies, randomized stack/vector agreement,
MPMA credit-before-debit ordering, self-send, dispenser refills/delayed close,
BTC settlement/expiry, fairmint success/failure and pool funding, multi-input
automatic detach, unknown-event rejection, persistence across restart, undo of
all indexes/counters, explicit partial reads and stale cursors.

Full current-Core replay against both historical checkpoints and a same-height
live Core balance audit remain distinct checks after backfill. Do not label all
current asset ownership verified solely because the offline fixtures pass.

## Initial replay corrections (2026-09-09)

The worker suite now has 20 passing tests. Captured block 295004 verifies that
book-fill refunds use the immediately following filled order update to identify
the refunded escrow, because Core can put the opposite order hash in the credit.
The update is checked against the credited asset, address and remaining quantity.
Captured block 329675 verifies that a divisible SJCX send to a null address never
enters the indivisible-unit planner. Transfer planning runs after metadata
resolution and excludes divisible assets. Failed blocks remain atomic, so these
fixes resume from the saved numbered checkpoint without resetting the archive.

The local Naka site has a separate Espo checkpoint panel for each RAREPEPE card.
Its collection and holder search retain the recovered archive until the current
replay and verification gates are satisfied. The integration does not imply
that all current RAREPEPE ownership is verified.

## RAREPEPE audit completed (2026-09-09)

The BITCORN match at block 713886 traded the same asset on both sides. Match-credit
routing now follows Core's forward then backward ledger order and validates asset,
quantity and destination, including identical-credit self-matches. The captured
block and self-match regression tests pass; the worker suite has 23 passing tests.

Replay reached the confirmed chain and follows new blocks with the existing
six-block buffer. No Alkanes resynchronization occurred. The separate production
Espo and the public XCP Espo processes were not restarted for these worker fixes.

RAREPEPE passed an independent audit at checkpoint 966161 against Core at 966167:
all 300 original serial histories through legacy block 725934, every old and
current ordered holding, serial uniqueness, current Core wallet and escrow
quantities, and surviving supply. Core held 295 units at 208 addresses and three
in dispenser escrow. Serials 143 and 228 were destroyed, leaving 298 surviving
units; the destroyed serials remain queryable. The intervening six-block window
contained no RAREPEPE effect, and the audit checked the block/message anchors.

Public get_numbering_status accepts params {"asset":"RAREPEPE"}. Its verified flag
is scoped to that asset, as shown by verification_scope. Asset-specific data reads
include the same scoped result. Unscoped status retains verified:false because
all other assets have not received this independent audit.

Audit certificates are local operator files at
/var/lib/espo-xcp-history/audits/<hex-encoded-canonical-asset>.json. RPC cannot create
or edit them. A certificate is valid only for its source, ruleset, serial count,
archived block/message anchor, and last asset movement, with complete fresh replay.
It survives subsequent blocks with no asset movement; another asset movement or
changed anchor invalidates it. Never manually set passed checks to bypass audits.

To repeat the read-only RAREPEPE audit, run from this checkout:

    python3 scripts/verify_rarepepe_current.py /var/backups/rarepepe-audit-NEW

Use a new output directory for the audit. The script writes certificate.json only
after all eight gates pass. Publish that file atomically under the audits directory
as 5241524550455045.json with read permission for the worker. No restart is required
to load an updated certificate. Any failed read, changed checkpoint or mismatched
quantity/history requires investigation, not a forced pass.

Evidence is under /var/backups/espo-xcp-numbering-20260909/rarepepe-current-audit.
The all-300 snapshot RAREPEPE-966161.json includes all locations, movement histories,
and certificate. Deployment records are same-asset-match-deployment.json and
asset-audit-deployment.json in the parent backup directory.
