# Counterparty asset UX review

## Design exploration

1. **Compact overview:** replaced the stacked fact list with a small identity header, a four-stat strip, and five navigation groups. A browser review showed that a full-width market card left too much empty space and disconnected asset details from the market.
2. **Split overview:** introduced a secondary asset-information column and a main market column. Browser review found that prices appeared repeatedly in the hero, stat cards, and separate market tables.
3. **Consolidated market:** combined currency, last price/date/venue, volume, and trade count into one table; moved source details behind a disclosure. Tested a Trading prototype with contextual history/status filters and readable order rows. Selected this design for implementation.

## Implementation

- Five sections: Overview, Activity, Trading, Asset history, Ledger.
- Contextual selectors keep all native histories available without a wall of tabs.
- Record summaries prioritize amounts and participants; expandable details retain the complete node fields and explorer links.
- Number grouping, trailing-zero removal, human-readable dates, status badges, and consistent transaction placement.
- Asset description, ownership, issuance, metadata and media are organized in a sidebar.
- URL-driven filters and pagination remain usable without a client framework. Disclosure controls use native HTML keyboard behavior.

## Post-implementation refinements

1. **Desktop/data refinement:** Live screenshots showed overly tall market rows and repetitive event labels. Combined the date and venue on one line, restored base-asset quantities beneath quote volume, labeled buy/sell orders, and linked listing counts directly to open listings. Removed the unresolved dispenser-rate fallback so a fiat oracle rate is never labeled BTC. Rebuilt, deployed, and rechecked the overview and order list.
2. **Mobile/navigation refinement:** At 390px the last navigation item was clipped. Kept all five sections visible with a responsive grid, enlarged touch controls to at least 44px, allowed listing headings and pagination to wrap, and offset section anchors for the sticky site header. Empty filtered lists now offer a clear-filter action. Native disclosure controls were checked with Enter and a visible focus outline; expanded data fit the mobile width.

## Validation

- Release build and Rust library tests: 217 passed, 0 failed, 1 existing ignored test.
- Browser checks: desktop overview, real order summaries and expansion, cancelled-order filtering, page-two filter preservation, 390px overview/trading/detail layouts, keyboard disclosure interaction.
- Final live checks passed: all 17 native UI routes, selected navigation and filters, filtered pagination, empty-filter recovery, market quantities, active-listing links, activity and existing holders route. All 17 API histories matched the Counterparty node; independent Decimal calculations confirmed market volumes.
- Final browser checks confirmed all five mobile tabs fit, keyboard expansion shows focus, and page-two headings start at 123px below the 74px sticky header.
- Dedicated `espo-xcp.service` deployed; source and previous executable backed up under `/var/tmp/espo-xcp-before-ui-20260909.*`.
