# Counterparty asset coverage review — 2026-09-09

Reference: https://xcp.io/asset/DJPEPE
Target: http://88.99.51.244:8083/counterparty/asset/DJPEPE
Node: Counterparty Core 11.3.0, mainnet, API v2; inspected its live `/v2/routes` definitions and DJPEPE responses.

## Findings and implemented coverage

- Existing Espo displayed holders, basic issuances/dividends, open dispensers and open orders. Activity combined only sends, dispenses and XCP pool/DEX matches. Its public XCP API did not expose most asset histories.
- Native histories now include issuances (including quantity locks, resets, ownership/description changes in `asset_events`), all send types, dispensers, dispenses, orders, DEX matches, dividends, destructions, subassets, fairminters, fairmints, credits and debits. Pool swaps, deposits, withdrawals and reserve price history accept a selectable quote asset.
- Orders and dispensers default to all statuses and can be filtered. The UI shows current status and preserves full verbose details, including counterparties, quantities, remaining amounts, prices, fees, memos, tags, expiration and transaction references. Native records are not represented as a fabricated status-change timeline: ledger credits/debits expose additional actions such as cancellation refunds.
- DJPEPE live node counts observed during review: 3 issuances, 77 dispensers, 21 dispenses, 323 orders, 333 sends, 13 dividends, 4 destructions and 39 subassets. These agree with the reference tabs. Node order matches numbered 63; XCP.io showed 101 trades. These are different datasets and should not be represented as equivalent.
- `xcp.get_asset_history` exposes all native histories with type/status/quote selection, explicit limit/offset, node total and has_more. Original verbose records retain their message/match identifiers.
- `xcp.get_asset_activity` exposes the combined recent feed. Its all/trades selections deliberately cover a newest-250-record window; use native histories for full pagination. Separate messages within a transaction are preserved. Failed sources produce an unavailable response instead of silently incomplete activity.
- `xcp.get_asset_market` retains its price fields and adds a coverage-aware summary: completed trade volume grouped by original quote currency, last trades by currency, best DEX bids/asks, open listings, sell-order/dispenser escrow, dispenser BTC floor, and active trading months.
- Market calculations exclude pending/expired DEX matches. Oracle dispenser floors use the node's resolved BTC price divided by lot quantity, rather than treating a fiat rate as BTC.
- Summary scans are bounded to 2,000 rows per source and cached for 60 seconds. Each source reports total/loaded/complete. UI and API explicitly distinguish incomplete samples from all-node-history totals. Prices/aggregates are display estimates, not execution quotes.
- Explorer API documentation includes every new method and the market summary contract.

## Deliberate exclusions and source limits

- No rating, reputation or holder-profile feature was added. The pre-existing basic holder list remains available.
- Historical USD valuation, off-chain sales, collection classifications, artist attribution, related-asset recommendations and XCP.io proprietary rankings are not native Counterparty API data. No such values are fabricated or scraped into the node integration.
- Market summaries include the XCP pool pair; other pool pairs are available in the dedicated histories using quote_asset. They do not claim network-wide multi-pair pool aggregation.
- Subasset and pool histories follow the node's native ordering where no sort parameter exists. Mutable order/dispenser records expose current state at the node-reported block, not an independent chronological row for each status transition.
- The current XCP integration queries the node's already-indexed endpoints. No new Espo ledger schema or destructive reindex is required.

## Validation

Release build passed. Full library suite: 217 passed, 1 ignored. Live smoke checks matched all 17 native histories against the node, tested pagination beyond the recent window, status filters, invalid parameters, recent activity boundaries, independent Decimal volume totals, eight explorer pages and both new API docs. Desktop rendering and a 390px mobile viewport were inspected; no horizontal document overflow was observed. The pre-existing malformed dispense test fixture was corrected (it sliced four bytes from a three-byte vector).
