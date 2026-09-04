use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use maud::html;
use serde::Deserialize;

use crate::explorer::components::layout::layout_with_meta;
use crate::explorer::components::svg_assets::{
    icon_pager_first, icon_pager_last, icon_pager_left, icon_pager_right,
};
use crate::explorer::components::table::holders_table;
use crate::explorer::pages::common::format_integer;
use crate::explorer::paths::explorer_path;
use crate::modules::xcp::core::{CoreFetch, fetch_path_list};
use crate::modules::xcp::display::{asset_letter, short_hash};

#[derive(Deserialize)]
pub struct PageQuery {
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub named: Option<String>,
}

pub async fn xcp_assets_page(Query(q): Query<PageQuery>) -> Response {
    let page = q.page.unwrap_or(1).max(1);
    let limit = q.limit.unwrap_or(50).clamp(1, 100);
    let named = q.named.as_deref() != Some("0");
    let offset = limit.saturating_mul(page.saturating_sub(1));
    let extra = if named { "named=true" } else { "" };
    let fetched = match tokio::task::spawn_blocking(move || {
        fetch_path_list("/assets", extra, offset, limit)
    })
    .await
    {
        Ok(result) => result,
        Err(_) => return (StatusCode::BAD_GATEWAY, "Counterparty Core unavailable").into_response(),
    };
    let list = match fetched {
        CoreFetch::Ok(list) => list,
        CoreFetch::NotFound => {
            return (StatusCode::NOT_FOUND, "assets not found").into_response();
        }
        CoreFetch::Unreachable => {
            return (StatusCode::BAD_GATEWAY, "Counterparty Core unavailable").into_response();
        }
    };

    let rows_len = list.items.len();
    let rows = list
        .items
        .iter()
        .map(|row| {
            let name = row
                .get("asset")
                .and_then(|v| v.as_str())
                .unwrap_or("—")
                .to_string();
            let longname = row
                .get("asset_longname")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let title = longname.unwrap_or(&name).to_string();
            let supply = row
                .get("supply_normalized")
                .and_then(|v| v.as_str())
                .unwrap_or("0");
            let issuer = row.get("issuer").and_then(|v| v.as_str()).unwrap_or("");
            let locked = row.get("locked").and_then(|v| v.as_bool()).unwrap_or(false);
            vec![
                html! {
                    a class="link" href=(explorer_path(&format!("/counterparty/asset/{name}"))) {
                        span class="alk-icon-wrap" aria-hidden="true" {
                            span class="alk-icon-letter" { (asset_letter(&name)) }
                        }
                        span { (title) }
                    }
                },
                html! { span class="mono" { (fmt_normalized(supply)) } },
                html! {
                    @if issuer.is_empty() {
                        span class="muted" { "—" }
                    } @else {
                        a class="link mono" href=(explorer_path(&format!("/address/{issuer}"))) {
                            (short_hash(issuer))
                        }
                    }
                },
                html! { span { (if locked { "Yes" } else { "No" }) } },
            ]
        })
        .collect();

    let named_query = if named { "" } else { "&named=0" };
    let url = |target: usize| {
        explorer_path(&format!("/counterparty/assets?page={target}&limit={limit}{named_query}"))
    };
    let off = offset;
    let has_prev = page > 1;
    let has_next = off + rows_len < list.total;
    let display_start = if list.total > 0 && off < list.total { off + 1 } else { 0 };
    let display_end = (off + rows_len).min(list.total);
    let last_page = if list.total > 0 { (list.total + limit - 1) / limit } else { 1 };

    layout_with_meta(
        "Counterparty assets",
        "/counterparty/assets",
        Some("Named Counterparty assets from Core"),
        html! {
            div class="row" {
                h1 class="h1" { "Counterparty assets" }
                div class="order-control" {
                    @if named {
                        a class="pill" href=(explorer_path(&format!("/counterparty/assets?page=1&limit={limit}&named=0"))) { "All assets" }
                    } @else {
                        a class="pill" href=(explorer_path(&format!("/counterparty/assets?page=1&limit={limit}"))) { "Named only" }
                    }
                }
            }
            p class="muted" {
                "Live from Counterparty Core. Newest issuances first; Core does not rank by holders or volume."
            }
            div class="alkane-panel alkane-holders-card" {
                @if list.total == 0 {
                    p class="muted" { "No assets." }
                } @else {
                    (holders_table(&["Asset", "Supply", "Issuer", "Locked"], rows))
                }
            }
            div class="pager" {
                @if has_prev {
                    a class="pill iconbtn" href=(url(1)) aria-label="First page" { (icon_pager_first()) }
                } @else {
                    span class="pill disabled iconbtn" aria-hidden="true" { (icon_pager_first()) }
                }
                @if has_prev {
                    a class="pill iconbtn" href=(url(page - 1)) aria-label="Previous page" { (icon_pager_left()) }
                } @else {
                    span class="pill disabled iconbtn" aria-hidden="true" { (icon_pager_left()) }
                }
                span class="pager-meta muted" { "Showing "
                    (format_integer(display_start as u128))
                    @if list.total > 0 {
                        "-"
                        (format_integer(display_end as u128))
                    }
                    " / "
                    (format_integer(list.total as u128))
                }
                @if has_next {
                    a class="pill iconbtn" href=(url(page + 1)) aria-label="Next page" { (icon_pager_right()) }
                } @else {
                    span class="pill disabled iconbtn" aria-hidden="true" { (icon_pager_right()) }
                }
                @if has_next {
                    a class="pill iconbtn" href=(url(last_page)) aria-label="Last page" { (icon_pager_last()) }
                } @else {
                    span class="pill disabled iconbtn" aria-hidden="true" { (icon_pager_last()) }
                }
            }
        },
    )
    .into_response()
}

fn fmt_normalized(raw: &str) -> String {
    let (whole, frac) = raw.split_once('.').unwrap_or((raw, ""));
    let whole_n = whole.parse::<u128>().unwrap_or(0);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        format_integer(whole_n)
    } else {
        format!("{}.{}", format_integer(whole_n), frac)
    }
}