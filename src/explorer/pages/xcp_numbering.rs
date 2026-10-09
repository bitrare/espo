use crate::explorer::{components::layout::layout_with_meta, paths::explorer_path};
use crate::modules::xcp::numbering_client;
use axum::{extract::Query, response::Html};
use maud::{PreEscaped, html};
use serde::Deserialize;
use serde_json::json;
#[derive(Deserialize)]
pub struct NumberingQuery {
    pub asset: Option<String>,
    pub serial: Option<String>,
}
pub async fn xcp_numbering_page(Query(q): Query<NumberingQuery>) -> Html<String> {
    let status = numbering_client::query("get_numbering_status", json!({})).await;
    let asset = q.asset.as_deref().unwrap_or("RAREPEPE");
    let result = if q.asset.is_some() {
        Some(
            numbering_client::query(
                if q.serial.as_deref().is_some_and(|s| !s.is_empty()) {
                    "get_unit"
                } else {
                    "get_asset_units"
                },
                json!({"asset":asset,"serial":q.serial,"limit":20,"allow_partial":true}),
            )
            .await,
        )
    } else {
        None
    };
    layout_with_meta(
        "Counterparty unit numbering",
        "/counterparty/numbering",
        Some(
            "How Espo follows the history of indivisible Counterparty units, including UTXOs, DEX orders, dispensers and pools.",
        ),
        html! {
            (PreEscaped(include_str!("xcp_numbering.css")))
            div class="numbering" {
                p class="numbering-kicker" { a href=(explorer_path("/counterparty/assets")) { "Counterparty" } " / Unit provenance" }
                h1 { "Every unit has a history." }
                p class="numbering-lead" { "A deterministic numbering convention for indivisible Counterparty assets. Built on the Nakamotos Club method, extended to today's Counterparty ledger." }
                aside class="numbering-status" aria-label="Current indexing status" {
                    strong { "Live history status: " (status["phase"].as_str().unwrap_or("unavailable")) }
                    p { "Numbering checkpoint: " @if let Some(h)=status["numbering_tip"]["height"].as_u64(){(h)}@else{"not available yet"}
                        " · Validated archive checkpoint: " @if let Some(h)=status["archive_tip"]["height"].as_u64(){(h)}@else{"not available yet"}
                    }
                    @if let Some(n)=status["download"]["downloaded_events"].as_u64(){p{(n) " / " (status["download"]["target"]["last"].as_u64().unwrap_or(0)+1) " Core events downloaded."}
                        progress value=(n) max=(status["download"]["target"]["last"].as_u64().unwrap_or(0)+1) aria-label="Core event download progress" {}}
                    p { "Only the reported checkpoint is represented. Downloading, replaying, paused or stale history must not be treated as current ownership. Full legacy compatibility and live balance verification are separate release gates." }
                    @if let Some(e)=status.get("error").and_then(|v|v.as_str()){p{(e)}}
                    @for key in ["archive_error","projection_error"] {
                        @if let Some(e)=status[key]["message"].as_str(){details{summary{"Indexer diagnostic"}pre{(e)}}}
                    }
                }
                div class="numbering-layout" {
                    nav aria-label="Numbering guide" {
                        @for (id,label) in [("meaning","What a number means"),("birth","Issuance and identity"),("stack","The stack rule"),("markets","Orders and dispensers"),("utxos","UTXO wrapping"),("pools","Liquidity pools"),("fairmint","Fairmints and resets"),("verification","Verification and forks"),("lookup","Inspect a unit"),("sources","Sources and API")] {
                            a href=(format!("#{id}")){(label)}
                        }
                    }
                    div class="numbering-body" {
                        section id="meaning" {
                            h2{"What a number means"}
                            p{"Counterparty records quantities of fungible tokens. It does not assign an NFT identity to each unit. Espo applies a published ordering convention to that quantity history, producing a stable serial and a trail of locations for each indivisible unit."}
                            p{"This adds provenance; it does not change Counterparty consensus, make a fungible token non-fungible, confer legal title, or let a normal Counterparty send explicitly select a serial. The actual units selected depend on this convention and the sender's complete ordered history."}
                            p{"The same engine applies to any non-divisible asset, including named assets, numeric assets and subassets. RAREPEPE is a compatibility reference, not a special case in the engine. Divisible assets—including ordinary LP tokens—are excluded from serial numbering."}
                        }
                        section id="birth" {
                            h2{"1. A serial is created at issuance"}
                            p{"A valid issuance of three indivisible units creates #1, #2 and #3 in that order. A later valid issuance of two units creates #4 and #5. The counter is persisted; a transfer, sale or destruction never makes an old number available again. Zero-quantity metadata changes and transfers of issuance rights create no units."}
                            p{"The full identity is network + canonical Counterparty asset ID + ruleset + serial. Names and subasset longnames are aliases for the canonical asset. The ruleset is nakamoto-stack-v1. Serials, quantities and asset IDs are returned as decimal strings where required to avoid JavaScript integer rounding."}
                            p{"A unit's location is separate from the asset's issuer. An address may hold units without having issuance rights, and an issuer may own none of the circulating units."}
                        }
                        section id="stack" {
                            h2{"2. Ownership is an ordered stack"}
                            p{"Think of each balance as a deck laid left to right. The rightmost card is the top. A normal transfer takes the requested number of cards from the top, reverses their order, then appends them to the recipient's stack. Existing cards at the recipient stay in their original order."}
                            div class="numbering-example" {
                                p{strong{"Before"} " · Alice [1, 2, 3, 4, 5] · Bob [8, 9]"}
                                p{strong{"Alice sends 2"} " · take [4, 5] → reverse [5, 4]"}
                                p{strong{"After"} " · Alice [1, 2, 3] · Bob [8, 9, 5, 4]"}
                                p{"Bob's next one-unit send moves #4, even though #9 is the larger serial."}
                            }
                            p{"A balance must never be sorted by serial before sending. Its stack order comes from its history. A self-send is one atomic operation on the same stack and can reorder its top cards. Zero-quantity and invalid records do not move cards."}
                            p{"Classic and enhanced sends use the same rule. Multi-recipient sends, sweeps and dividends paid in an indivisible asset use Core's recorded leg order. Core may record MPMA credits before the corresponding debit, so Espo interprets the complete block rather than assuming every debit precedes its credits."}
                            p{"Storage uses directed ranges: [5 → 3] represents 5, 4, 3. Splitting or reversing a range changes its order without allocating a database row for every unit. Fragmentation is retained whenever history requires it."}
                        }
                        section id="markets" {
                            h2{"3. Orders and dispensers are locations"}
                            p{"Opening a DEX sell order moves the selected units into that order's escrow. A fill takes units from the order stack, reverses them and appends them to the buyer's stack. A cancellation, expiration or fill refund moves only the quantity Core actually credits back. It is not a second debit of the seller's wallet."}
                            p{"A BTC order match awaiting payment has a separate pending-match location. Moving an allocated slice into this temporary location preserves its order; settlement reverses it once on delivery. If the match expires, Core decides whether the units return to a live order or are refunded to an address. Returning to a live order preserves the reserved slice's order."}
                            p{"Opening a dispenser reserves its inventory with the ordinary reversal. Dispensing and returning unused inventory also use the ordinary reversal. A close request is not itself a refund: delayed closes take effect when Core records the returned quantity."}
                            div class="numbering-note" {strong{"The historical exception: dispenser refills."} p{"The original XCPToNFT method appends a refill's selected tail without reversing it. Espo preserves this exception because reversing a refill would change later card numbers. Opening a dispenser on an empty address is treated as one inventory reservation; Core's temporary address credit/debit is not counted as an extra card transfer."}}
                        }
                        section id="utxos" {
                            h2{"4. UTXO wrapping keeps the serial"}
                            p{"An attached asset lives at an exact Bitcoin outpoint: transaction ID plus output index. Espo keeps that outpoint as the location and records its controlling address separately. An attachment does not mint a new serial."}
                            ul {
                                li{strong{"Attach: "} "take the address stack's tail, reverse it and append to the outpoint."}
                                li{strong{"Move: "} "move the entire per-asset outpoint stack while preserving its internal order. When inputs merge, append them in Core's recorded order."}
                                li{strong{"Detach: "} "take the requested quantity from the outpoint, reverse it and append to the address resolved by Core. This includes partial detachment and automatic detachment when spending a wrapped output."}
                            }
                            div class="numbering-example" {p{"Address [1, 2, 3] → attach 2 → UTXO [3, 2]"}p{"Move that UTXO → new UTXO [3, 2]"}p{"Detach 2 → recipient receives [2, 3]"}}
                            p{"Espo follows Core's resolved destination; it does not guess from Bitcoin output order. Historical UTXO message formats are normalized to the same location rules."}
                        }
                        section id="pools" {
                            h2{"5. A pool owns a reserve stack"}
                            p{"Each pool pair has a separate stack for each numbered reserve asset. A liquidity deposit moves the actual quantities accepted by Core from the depositor into those reserves using the ordinary reversal. Requested amounts, reserve updates and summary events are not additional transfers."}
                            p{"A pool trade can fill a DEX order. In that case its input comes from the order's existing escrow, not from a fresh wallet debit. The full input—including the pool fee—enters the reserve. The output comes from the other reserve and is appended to the recipient after reversal. There is no invented fee owner or second fee transfer."}
                            p{"A withdrawal takes Core's recorded output quantity from each reserve's current tail, reverses it and appends it to the recipient. A depositor is not promised the original serials they deposited. LP shares express a claim on pooled value; they do not individually own specific reserve serials."}
                            p{"Prices, fees, rounding, validity and withdrawal amounts come from Counterparty Core. Espo does not run another AMM calculation to decide which quantities moved."}
                        }
                        section id="fairmint" {
                            h2{"6. Fairmints, destruction and resets"}
                            p{"Fairmint issuances allocate new serials only for the quantities actually issued. Premints, minter allocations, commissions and pool liquidity are distinct legs. Tokens at Counterparty's unspendable address can be temporary fairmint escrow, so that address alone is not proof of destruction."}
                            p{"When a fairmint's conditions resolve, Core may distribute the escrow, fund a pool, refund payments or destroy issued units after a failed soft cap. These can be block actions with no current transaction hash, or references to an older transaction. Espo retains the event index and all legs instead of deduplicating by transaction hash."}
                            p{"Explicit destruction moves serials to a terminal destroyed location. A valid supply reset retires the previous numbered supply and starts a new issuance epoch. New indivisible units continue from the persisted serial counter. A divisible epoch has no active numbered supply; a later indivisible epoch creates fresh serials. Retired and destroyed serials remain inspectable."}
                            p{"Artwork is separate from provenance. The original Nakamotos Club PFP-to-card mapping is not randomized again when history is replayed or wrapped units move."}
                        }
                        section id="verification" {
                            h2{"7. Coverage, verification and chain reorganizations"}
                            p{"The archive imports every Core event from Counterparty genesis, not only asset-page activity. It pins the Core version, source commit and a confirmed block boundary; validates pagination without gaps; then checks complete block boundaries and parent hashes. Download progress, validated archive height and numbering height are separate checkpoints."}
                            p{"Each projected block is atomic. Stack changes, serial counters, history, indexes and undo data commit together. The net address and UTXO quantities produced by the numbering engine must match Core's credits and debits for that block. An unknown relevant balance effect or a mismatch pauses numbering with a diagnostic; the archive can keep progressing."}
                            p{"The service retains 288 blocks of undo for shallow reorganizations. A deeper fork or a source-version change pauses processing for an XCP-only rebuild or adapter review. Pagination cursors include the projection checkpoint; after that checkpoint changes, the client must restart pagination."}
                            p{"Historical compatibility checks compare all RAREPEPE unit histories and ordered holdings against the saved XCPToNFT result, with DJPEPE as a second reference. New UTXO, pool and fairmint cases need their own fixtures. Live balance verification must compare the same Core and numbering height. A discrepancy must be explained and versioned rather than hidden by patching a card number."}
                            p{"The history process has its own database, checkpoint, memory limits and loopback API. It never opens the Alkanes database or contributes a height to Espo's shared indexing loop. Replaying XCP history does not resynchronize Alkanes."}
                        }
                        section id="lookup" {
                            h2{"Inspect a unit at the available checkpoint"}
                            p{"This lookup explicitly permits partial historical data. Read the returned context before interpreting a result as current ownership."}
                            form method="get" action=(explorer_path("/counterparty/numbering")) {
                                label {"Asset" input name="asset" value=(asset) maxlength="255" required;}
                                label {"Serial (optional)" input name="serial" value=(q.serial.as_deref().unwrap_or("")) inputmode="numeric" placeholder="1";}
                                button class="pill" type="submit" {"Inspect"}
                            }
                            @if let Some(result)=result{pre class="numbering-result"{(serde_json::to_string_pretty(&result).unwrap_or_default())}}
                        }
                        section id="sources" {
                            h2{"Sources and API"}
                            p{"The ordering convention originates in the original XCPToNFT asset-history parser and the Nakamotos Club archive. UTXO and pool rules are explicit extensions in nakamoto-stack-v1. Counterparty Core 11.3.0, commit cd827e0fafe62b3166b2a186749e4191e1897170, is the initial source adapter."}
                            p{a href="https://thenakamotos.club/" {"The Nakamotos Club"} " · " a href="https://github.com/ffmad/XCPToNFT/blob/200aa5cd19b2e108413db949e1d3061a112ebdbc/modules/assethistory.mjs" {"Original XCPToNFT parser"} " · " a href="https://github.com/CounterpartyXCP/counterparty-core/tree/cd827e0fafe62b3166b2a186749e4191e1897170/counterparty-core/counterpartycore/lib/messages" {"Pinned Counterparty parsers"} " · " a href=(explorer_path("/docs#xcp-numbering")){"Numbering API documentation"}}
                            p{"The read-only methods are xcp.get_numbering_status, xcp.get_asset_units, xcp.get_unit, xcp.get_unit_history, xcp.get_address_units and xcp.get_transfer_units. Every data response includes its checkpoint context. Unavailable and incomplete data produce explicit errors; they are never reported as an empty successful ownership list."}
                        }
                    }
                }
            }
        },
    )
}
