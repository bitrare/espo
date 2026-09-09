use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use maud::{Markup, PreEscaped, html};
use serde::Deserialize;
use serde_json::Value;

use crate::explorer::components::layout::layout_with_meta;
use crate::explorer::components::svg_assets::{
    icon_pager_first, icon_pager_last, icon_pager_left, icon_pager_right,
};
use crate::explorer::components::table::holders_table;
use crate::explorer::pages::common::format_integer;
use crate::explorer::pages::state::ExplorerState;
use crate::explorer::paths::explorer_path;
use crate::modules::xcp::activity::{self, ActivityKind, ActivityPage};
use crate::modules::xcp::core::{
    CoreFetch, CoreList, asset_name_from_value, fetch_asset, fetch_asset_list,
};
use crate::modules::xcp::display::{asset_letter, short_hash};
use crate::modules::xcp::enhanced;
use crate::modules::xcp::market::{self, format_price};
use crate::modules::xcp::{history, market_summary};

#[derive(Deserialize)]
pub struct PageQuery {
    pub tab: Option<String>,
    pub activity: Option<String>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub status: Option<String>,
    pub quote_asset: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AssetTab {
    Holders,
    Issuances,
    Dispensers,
    Orders,
    Dividends,
    Activity,
    Market,
    Native(&'static str),
}

impl AssetTab {
    fn from_query(raw: Option<&str>) -> Self {
        match raw {
            Some("holders") => Self::Holders,
            Some("issuances") => Self::Issuances,
            Some("dispensers") => Self::Dispensers,
            Some("orders") => Self::Orders,
            Some("dividends") => Self::Dividends,
            Some("activity") => Self::Activity,
            Some("market") => Self::Market,
            Some(kind) if history::RESOURCES.contains(&kind) => {
                Self::Native(history::RESOURCES.iter().copied().find(|v| *v == kind).unwrap())
            }
            _ => Self::Market,
        }
    }

    fn as_query(self) -> &'static str {
        match self {
            Self::Holders => "holders",
            Self::Issuances => "issuances",
            Self::Dispensers => "dispensers",
            Self::Orders => "orders",
            Self::Dividends => "dividends",
            Self::Activity => "activity",
            Self::Market => "market",
            Self::Native(kind) => kind,
        }
    }
}

struct AssetRecord {
    name: String,
    longname: Option<String>,
    asset_id: Option<String>,
    issuer: Option<String>,
    owner: Option<String>,
    divisible: bool,
    locked: bool,
    supply_raw: u128,
    supply_label: String,
    description: Option<String>,
    mime_type: Option<String>,
    first_issuance_block: Option<u64>,
    first_issuance_time: Option<i64>,
}

impl AssetRecord {
    fn from_core(value: &Value) -> Option<Self> {
        let name = asset_name_from_value(value)?;
        let longname = string_field(value, "asset_longname");
        let asset_id = value.get("asset_id").and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.as_u64().map(|n| n.to_string()))
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        });
        Some(Self {
            name,
            longname,
            asset_id,
            issuer: string_field(value, "issuer"),
            owner: string_field(value, "owner"),
            divisible: value.get("divisible").and_then(|v| v.as_bool()).unwrap_or(false),
            locked: value.get("locked").and_then(|v| v.as_bool()).unwrap_or(false),
            supply_raw: json_u128(value.get("supply")),
            supply_label: fmt_normalized(
                string_field(value, "supply_normalized").as_deref().unwrap_or("0"),
            ),
            description: string_field(value, "description"),
            mime_type: string_field(value, "mime_type"),
            first_issuance_time: value.get("first_issuance_block_time").and_then(Value::as_i64),
            first_issuance_block: value.get("first_issuance_block_index").and_then(|v| {
                v.as_u64().or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
            }),
        })
    }

    fn title(&self) -> &str {
        self.longname.as_deref().unwrap_or(&self.name)
    }

    fn subtitle(&self) -> String {
        if let Some(id) = self.asset_id.as_deref() {
            if id.chars().all(|c| c.is_ascii_digit()) {
                if self.name.starts_with('A') {
                    return format!("A{id}");
                }
                return format!("{id} · {}", self.name);
            }
            return id.to_string();
        }
        if self.name == "XCP" {
            return "1".to_string();
        }
        self.name.clone()
    }

    fn parent_name(&self) -> Option<&str> {
        self.longname.as_deref()?.split_once('.').map(|(parent, _)| parent)
    }

    fn icon_url(&self) -> Option<&str> {
        let mime = self.mime_type.as_deref()?;
        if !mime.starts_with("image/") {
            return None;
        }
        self.description
            .as_deref()
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
    }
}

pub async fn xcp_icon(Path(asset): Path<String>) -> Response {
    let lookup = asset;
    let fetched = match tokio::task::spawn_blocking(move || enhanced::icon_bytes(&lookup)).await {
        Ok(result) => result,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let Some((content_type, body)) = fetched else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(content_type) = HeaderValue::from_str(&content_type) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, content_type)
        .header(CACHE_CONTROL, "public, max-age=3600")
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub async fn xcp_asset_page(
    State(_state): State<ExplorerState>,
    Path(asset): Path<String>,
    Query(q): Query<PageQuery>,
) -> Response {
    let query = asset.clone();
    let fetched = match tokio::task::spawn_blocking(move || fetch_asset(&query)).await {
        Ok(result) => result,
        Err(_) => {
            return (StatusCode::BAD_GATEWAY, "Counterparty Core unavailable").into_response();
        }
    };
    let value = match fetched {
        CoreFetch::Ok(value) => value,
        CoreFetch::NotFound => return (StatusCode::NOT_FOUND, "asset not found").into_response(),
        CoreFetch::Unreachable => {
            return (StatusCode::BAD_GATEWAY, "Counterparty Core unavailable").into_response();
        }
    };
    let Some(record) = AssetRecord::from_core(&value) else {
        return (StatusCode::NOT_FOUND, "asset not found").into_response();
    };
    if !asset_query_matches(&asset, &record) {
        let dest = explorer_path(&format!("/counterparty/asset/{}", record.name));
        return Redirect::permanent(&dest).into_response();
    }
    let desc = record.description.clone();
    let asset_name = record.name.clone();
    let enhanced =
        tokio::task::spawn_blocking(move || enhanced::warm(&asset_name, desc.as_deref()))
            .await
            .ok()
            .flatten();

    let page = q.page.unwrap_or(1).max(1);
    let limit = q.limit.unwrap_or(25).clamp(1, 200);
    let tab = AssetTab::from_query(q.tab.as_deref());
    let activity = ActivityKind::from_query(q.activity.as_deref());
    let offset = limit.saturating_mul(page.saturating_sub(1));
    let asset_name = record.name.clone();
    let status = q.status.as_deref().unwrap_or("all").to_string();
    if !history::valid_status(tab.as_query(), &status) {
        return (StatusCode::BAD_REQUEST, "Invalid status for this history").into_response();
    }
    let quote = q.quote_asset.as_deref().unwrap_or("XCP").to_string();
    let summary_name = asset_name.clone();
    let summary_handle = (tab == AssetTab::Market)
        .then(|| tokio::task::spawn_blocking(move || market_summary::fetch(&summary_name)));

    let market_name = asset_name.clone();
    let market_handle =
        tokio::task::spawn_blocking(move || market::fetch_asset_market(&market_name));
    let loaded = tokio::task::spawn_blocking(move || {
        let holders = if tab == AssetTab::Holders {
            fetch_asset_list(&asset_name, "balances", "sort=quantity:DESC", offset, limit)
        } else {
            fetch_asset_list(&asset_name, "balances", "sort=quantity:DESC", 0, 1)
        };
        let extra = match tab {
            AssetTab::Activity | AssetTab::Holders | AssetTab::Market => None,
            _ => Some(history::fetch(&asset_name, tab.as_query(), &quote, &status, offset, limit)),
        };
        let activity_page = if tab == AssetTab::Activity {
            Some(activity::fetch_asset_activity(&asset_name, activity, offset, limit))
        } else {
            None
        };
        (holders, extra, activity_page)
    })
    .await;

    let (holders_fetch, extra_fetch, activity_fetch) = match loaded {
        Ok(triple) => triple,
        Err(_) => {
            return (StatusCode::BAD_GATEWAY, "Counterparty Core unavailable").into_response();
        }
    };
    let market = market_handle.await.ok().and_then(|fetched| fetched.ok());
    let holders_list = match holders_fetch {
        CoreFetch::Ok(list) => list,
        CoreFetch::NotFound => CoreList { items: Vec::new(), total: 0 },
        CoreFetch::Unreachable => {
            return (StatusCode::BAD_GATEWAY, "Counterparty Core unavailable").into_response();
        }
    };
    let extra_list = match extra_fetch {
        Some(CoreFetch::Ok(list)) => Some(list),
        Some(CoreFetch::NotFound) => Some(CoreList { items: Vec::new(), total: 0 }),
        Some(CoreFetch::Unreachable) => {
            return (StatusCode::BAD_GATEWAY, "Counterparty Core unavailable").into_response();
        }
        None => None,
    };
    let activity_page = match activity_fetch {
        Some(CoreFetch::Ok(page)) => Some(page),
        Some(CoreFetch::NotFound) => Some(ActivityPage { items: Vec::new(), total: 0 }),
        Some(CoreFetch::Unreachable) => {
            return (StatusCode::BAD_GATEWAY, "Counterparty Core unavailable").into_response();
        }
        None => None,
    };

    let summary = match summary_handle {
        Some(handle) => handle.await.ok(),
        None => None,
    };
    let holders_total = holders_list.total;
    let canonical = format!("/counterparty/asset/{}", record.name);
    let title = record.title().to_string();

    layout_with_meta(&title, &canonical, Some("Counterparty asset"), html! {
        div class="xui" {
            (PreEscaped(include_str!("xcp_asset_ui.css")))
            div class="xui-breadcrumb" { a href=(explorer_path("/counterparty/assets")) { "Counterparty assets" } span { " / " } (title.clone()) }
            header class="xui-identity" {
                div class="xui-icon" aria-hidden="true" {
                    @if let Some(url)=enhanced.as_ref().and_then(|info|info.icon()).or_else(||record.icon_url()) {
                        img src=(url) alt="";
                    } @else { span { (asset_letter(&record.name)) } }
                }
                div class="xui-title" {
                    h1 { (title.clone()) }
                    div class="xui-tags" {
                        span class="xui-badge" { (if record.locked {"Locked supply"} else {"Unlocked supply"}) }
                        span class="xui-badge" { (if record.divisible {"Divisible"} else {"Indivisible"}) }
                        @if let Some(time)=record.first_issuance_time { span class="xui-muted" { "Issued " (date_label(Some(time))) } }
                    }
                    @if let Some(parent)=record.parent_name() { p class="xui-muted" { "Subasset of " a href=(asset_href(parent)) { (parent) } } }
                }
            }
            @if tab==AssetTab::Market {
                div class="xui-stats" {
                    (stat("Supply",html!{(record.supply_label.clone()) small{ " " (record.name.clone()) }},None))
                    @if let Some(market)=market.as_ref() {
                        @if let Some((price,quote))=market.price() {
                            (stat(if market.last_trade.is_some(){"Last sale"}else{"Pool spot price"},html!{(fmt_normalized(&format_price(price))) small{" " (quote)}},market.last_trade.as_ref().and_then(|t|t.block_time).map(|t|date_label(Some(t as i64)))))
                        }
                    }
                    @if let Some(summary)=summary.as_ref() {
                        @if let Some(floor)=summary.get("dispenser_floor_btc").filter(|v|!v.is_null()) {
                            (stat("Lowest dispenser price",html!{(number_label(floor)) small{" BTC / unit"}},None))
                        }
                        @if summary.get("listings_complete").and_then(Value::as_bool)==Some(true) {
                            (stat("In dispensers",html!{(number_label(&summary["dispenser_escrow"])) small{" " (record.name.clone())}},Some(format!("{} open",number_label(&summary["open_dispensers"])))))
                        }
                    }
                }
            }
            nav class="xui-nav" aria-label="Asset sections" {
                @for (group,label,dest) in [(0,"Overview",AssetTab::Market),(1,"Activity",AssetTab::Activity),(2,"Trading",AssetTab::Orders),(3,"Asset history",AssetTab::Issuances),(4,"Ledger",AssetTab::Native("credits"))] {
                    a class=(if tab_group(tab)==group {"active"} else {""}) aria-current=[(tab_group(tab)==group).then_some("page")] href=(tab_url(&record.name,dest,1,limit,ActivityKind::All)) { (label) }
                }
            }
            section id="asset-content" class="xui-content" {
                @if tab==AssetTab::Market {
                    div class="xui-columns" {
                        div {
                            @if let Some(summary)=summary.as_ref() { (overview_market(summary,&record.name)) }
                            @else { div class="xui-panel" { h2 { "Market history" } p class="xui-muted" { "Market data is temporarily unavailable." } } }
                            @if let Some(spot)=market.as_ref().and_then(|m|m.spot.as_ref()) {
                                section class="xui-panel xui-pool" { h2 { "Pool liquidity" } p { (fmt_normalized(&spot.reserve_base)) " " (record.name.clone()) " / " (fmt_normalized(&spot.reserve_quote)) " " (spot.quote_asset.clone()) } a href=(tab_url(&record.name,AssetTab::Native("pool_history"),1,limit,ActivityKind::All)) { "Explore pool history →" } }
                            }
                        }
                        (asset_information(&record,enhanced.as_ref(),holders_total))
                    }
                } @else if tab==AssetTab::Activity {
                    (activity_view(&record.name,activity,page,limit,activity_page.as_ref()))
                } @else if tab==AssetTab::Holders {
                    div class="xui-panel" { (history_controls(&record.name,tab,limit,q.status.as_deref().unwrap_or("all"),q.quote_asset.as_deref().unwrap_or("XCP"),holders_list.total)) (holders_panel(&record,page,limit,&holders_list)) }
                } @else {
                    (native_view(&record.name,tab,page,limit,extra_list.as_ref(),q.status.as_deref().unwrap_or("all"),q.quote_asset.as_deref().unwrap_or("XCP")))
                }
            }
        }
    }).into_response()
}
fn asset_query_matches(query: &str, record: &AssetRecord) -> bool {
    let q = query.trim();
    q.eq_ignore_ascii_case(&record.name)
        || record.longname.as_deref().is_some_and(|name| q.eq_ignore_ascii_case(name))
}

fn tab_url(
    asset: &str,
    tab: AssetTab,
    page: usize,
    limit: usize,
    activity: ActivityKind,
) -> String {
    match tab {
        AssetTab::Activity => explorer_path(&format!(
            "/counterparty/asset/{asset}?tab=activity&activity={}&page={page}&limit={limit}",
            activity.as_query()
        )),
        _ => explorer_path(&format!(
            "/counterparty/asset/{asset}?tab={}&page={page}&limit={limit}",
            tab.as_query()
        )),
    }
}

fn holders_panel(record: &AssetRecord, page: usize, limit: usize, list: &CoreList) -> Markup {
    let rows_len = list.items.len();
    let rows = list
        .items
        .iter()
        .filter(|row| json_u128(row.get("quantity")) > 0)
        .map(|row| {
            let address = string_field(row, "address").unwrap_or_else(|| "—".to_string());
            let qty = string_field(row, "quantity_normalized").unwrap_or_else(|| "0".to_string());
            let qty_raw = json_u128(row.get("quantity"));
            let pct_label = if record.supply_raw == 0 {
                "0%".to_string()
            } else {
                format!("{:.4}%", (qty_raw as f64 / record.supply_raw as f64) * 100.0)
            };
            vec![
                html! { a class="link mono" href=(explorer_path(&format!("/address/{address}"))) { (address) } },
                html! { span class="mono" { (fmt_normalized(&qty)) } },
                html! { span class="alk-holding-pct mono" { (pct_label) } },
            ]
        })
        .collect();
    html! {
        div class="alkane-panel alkane-holders-card" {
            @if list.total == 0 {
                p class="muted" { "No holders yet." }
            } @else {
                (holders_table(&["Holder", "Balance", "Holding %"], rows))
            }
        }
        (pager(list.total, rows_len, page, limit, |target| {
            tab_url(&record.name, AssetTab::Holders, target, limit, ActivityKind::All)
        }))
    }
}

fn pager<F>(total: usize, rows_len: usize, page: usize, limit: usize, url: F) -> Markup
where
    F: Fn(usize) -> String,
{
    let off = limit.saturating_mul(page.saturating_sub(1));
    let has_prev = page > 1;
    let has_next = off + rows_len < total;
    let display_start = if total > 0 && off < total { off + 1 } else { 0 };
    let display_end = (off + rows_len).min(total);
    let last_page = if total > 0 { (total + limit - 1) / limit } else { 1 };
    html! {
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
                @if total > 0 {
                    "-"
                    (format_integer(display_end as u128))
                }
                " / "
                (format_integer(total as u128))
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
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn json_u128(value: Option<&Value>) -> u128 {
    let Some(value) = value else { return 0 };
    value
        .as_u64()
        .map(u128::from)
        .or_else(|| value.as_i64().and_then(|n| u128::try_from(n).ok()))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
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

fn field_label(key: &str) -> String {
    let text = key.trim_end_matches("_normalized").replace('_', " ");
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

fn record_value(key: &str, value: &Value) -> Markup {
    if value.is_null() {
        return html! { span class="muted" { "—" } };
    }
    let text = value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string());
    let href =
        if (key.ends_with("tx_hash") || key == "tx0_hash" || key == "tx1_hash" || key == "event")
            && text.len() == 64
            && text.chars().all(|c| c.is_ascii_hexdigit())
        {
            Some(format!("/tx/{text}"))
        } else if [
            "asset",
            "give_asset",
            "get_asset",
            "forward_asset",
            "backward_asset",
            "dividend_asset",
        ]
        .contains(&key)
        {
            Some(format!("/counterparty/asset/{}", crate::modules::xcp::core::encode_path(&text)))
        } else if [
            "source",
            "destination",
            "address",
            "owner",
            "issuer",
            "tx0_address",
            "tx1_address",
            "oracle_address",
        ]
        .contains(&key)
        {
            Some(format!("/address/{}", crate::modules::xcp::core::encode_path(&text)))
        } else if key.ends_with("block_index") && value.as_u64().is_some() {
            Some(format!("/block/{text}"))
        } else {
            None
        };
    if let Some(href) = href {
        return html! { a class="link mono" href=(explorer_path(&href)) title=(text.clone()) { (if text.len()>26 {short_hash(&text)} else {text}) } };
    }
    if key.ends_with("block_time") || key == "block_time" {
        if let Some(date) =
            value.as_i64().and_then(|t| time::OffsetDateTime::from_unix_timestamp(t).ok())
        {
            return html! { span { (format!("{}-{:02}-{:02} {:02}:{:02} UTC",date.year(),date.month() as u8,date.day(),date.hour(),date.minute())) } };
        }
    }
    if let Some(obj) = value.as_object() {
        return html! { dl { @for (k,v) in obj { @if !v.is_null() { dt { (field_label(k)) } dd style="overflow-wrap:anywhere" { (record_value(k,v)) } } } } };
    }
    html! { span style="overflow-wrap:anywhere" { (text) } }
}

fn asset_href(asset: &str) -> String {
    explorer_path(&format!("/counterparty/asset/{}", crate::modules::xcp::core::encode_path(asset)))
}
fn number_label(v: &Value) -> String {
    if v.is_null() {
        return "—".into();
    }
    let raw = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
    if raw.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && raw.bytes().any(|b| b.is_ascii_digit())
    {
        fmt_normalized(&raw)
    } else {
        raw
    }
}
fn date_label(timestamp: Option<i64>) -> String {
    timestamp
        .and_then(|t| time::OffsetDateTime::from_unix_timestamp(t).ok())
        .filter(|t| t.unix_timestamp() > 0)
        .map(|t| {
            format!(
                "{} {} {}",
                t.day(),
                [
                    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
                    "Dec"
                ][t.month() as usize - 1],
                t.year()
            )
        })
        .unwrap_or_else(|| "Date unavailable".into())
}
fn stat(label: &str, value: Markup, note: Option<String>) -> Markup {
    html! { div { span class="xui-label"{(label)} strong{(value)} @if let Some(note)=note{small class="xui-stat-note"{(note)}} } }
}
fn tab_group(tab: AssetTab) -> u8 {
    match tab {
        AssetTab::Market => 0,
        AssetTab::Activity => 1,
        AssetTab::Orders | AssetTab::Dispensers => 2,
        AssetTab::Holders => 4,
        AssetTab::Native(kind) if ["credits", "debits"].contains(&kind) => 4,
        AssetTab::Native(kind)
            if kind.starts_with("pool_") || ["matches", "dispenses"].contains(&kind) =>
        {
            2
        }
        _ => 3,
    }
}
fn group_tabs(group: u8) -> Vec<AssetTab> {
    match group {
        2 => vec![
            AssetTab::Orders,
            AssetTab::Native("matches"),
            AssetTab::Dispensers,
            AssetTab::Native("dispenses"),
            AssetTab::Native("pool_matches"),
            AssetTab::Native("pool_deposits"),
            AssetTab::Native("pool_withdrawals"),
            AssetTab::Native("pool_history"),
        ],
        3 => vec![
            AssetTab::Issuances,
            AssetTab::Native("sends"),
            AssetTab::Dividends,
            AssetTab::Native("destructions"),
            AssetTab::Native("subassets"),
            AssetTab::Native("fairminters"),
            AssetTab::Native("fairmints"),
        ],
        _ => vec![AssetTab::Native("credits"), AssetTab::Native("debits"), AssetTab::Holders],
    }
}
fn friendly_kind(kind: &str) -> &str {
    match kind {
        "matches" => "DEX trades",
        "pool_matches" => "Pool swaps",
        "pool_history" => "Pool price history",
        "fairminters" => "Minting campaigns",
        _ => history::label(kind),
    }
}
fn history_controls(
    asset: &str,
    tab: AssetTab,
    limit: usize,
    status: &str,
    quote: &str,
    total: usize,
) -> Markup {
    let statuses: &[&str] = match tab {
        AssetTab::Orders => &["all", "open", "filled", "cancelled", "expired"],
        AssetTab::Dispensers => &["all", "open", "closing", "closed", "open_empty_address"],
        AssetTab::Native("matches") => &["all", "completed", "pending", "expired"],
        _ => &[],
    };
    html! {
        div class="xui-toolbar" {
            div { h2{(friendly_kind(tab.as_query()))} p class="xui-muted"{(format_integer(total as u128)) " records"} }
            div class="xui-filters" {
                form method="get" action=(asset_href(asset)) {
                    label { span class="xui-sr-only" { "History type" }
                        select name="tab" onchange="this.form.submit()" {
                            @for item in group_tabs(tab_group(tab)) { option value=(item.as_query()) selected[item==tab] { (friendly_kind(item.as_query())) } }
                        }
                    }
                    input type="hidden" name="limit" value=(limit);
                    noscript { button type="submit" { "View" } }
                }
                @if !statuses.is_empty() {
                    form method="get" action=(asset_href(asset)) {
                        input type="hidden" name="tab" value=(tab.as_query());
                        input type="hidden" name="limit" value=(limit);
                        label { span class="xui-sr-only" { "Status" }
                            select name="status" onchange="this.form.submit()" { @for item in statuses { option value=(item) selected[*item==status] { (if *item=="all" {"All statuses".into()} else {field_label(item)}) } } }
                        }
                        noscript { button type="submit" { "Filter" } }
                    }
                }
            }
        }
        @if tab.as_query().starts_with("pool_") {
            form method="get" action=(asset_href(asset)) class="xui-pair" {
                input type="hidden" name="tab" value=(tab.as_query()); input type="hidden" name="limit" value=(limit);
                label { "Pool pair " span { (asset) " / " } input name="quote_asset" aria-label="Quote asset" value=(quote); }
                button type="submit" { "View pair" }
            }
        }
    }
}
fn asset_information(
    record: &AssetRecord,
    info: Option<&enhanced::EnhancedAsset>,
    holders: usize,
) -> Markup {
    html! {
        aside id="asset-information" class="xui-panel xui-info" {
            h2 { "Asset information" }
            dl {
                @if let Some(owner)=record.owner.as_ref().or(record.issuer.as_ref()) {
                    dt { "Owner" } dd { (record_value("owner",&Value::String(owner.clone()))) }
                }
                @if record.issuer!=record.owner {
                    @if let Some(issuer)=record.issuer.as_ref() { dt { "Issuer" } dd { (record_value("issuer",&Value::String(issuer.clone()))) } }
                }
                dt { "First issued" } dd { (date_label(record.first_issuance_time)) @if let Some(block)=record.first_issuance_block { br; a href=(explorer_path(&format!("/block/{block}"))) { "Block " (format_integer(block as u128)) " ↗" } } }
                dt { "Supply" } dd { (record.supply_label.clone()) " · " (if record.locked {"Locked"} else {"Unlocked"}) }
                dt { "Precision" } dd { (if record.divisible {"8 decimals"} else {"Whole units"}) }
                dt { "Holders" } dd { a href=(tab_url(&record.name,AssetTab::Holders,1,25,ActivityKind::All)) { (format_integer(holders as u128)) } }
                @if let Some(desc)=info.and_then(|i|i.description.as_ref()).or(record.description.as_ref()).filter(|s|!s.trim().is_empty()) {
                    dt { "Description" } dd class="xui-description" { (record_value("description",&Value::String(desc.clone()))) }
                }
                @if let Some(site)=info.and_then(|i|i.website.as_ref()) { dt { "Website" } dd { a href=(site) rel="noopener noreferrer" target="_blank" { (short_hash(site)) " ↗" } } }
            }
            a class="xui-text-link" href=(tab_url(&record.name,AssetTab::Issuances,1,25,ActivityKind::All)) { "View issuance history →" }
            details class="xui-disclosure" { summary { "More asset details" }
                dl { dt { "Asset ID" } dd class="mono" { (record.subtitle()) }
                    @if let Some(url)=info.and_then(|i|i.json_url.as_ref()) {dt { "Metadata" } dd { a href=(url) rel="noopener noreferrer" target="_blank" { "View metadata ↗" } } }
                }
            }
            @if let Some(info)=info {
                @if info.image_url.as_deref().is_some_and(|image|info.icon_url.as_deref()!=Some(image)) || info.video_url.is_some() {
                    details class="xui-disclosure" { summary { "Artwork & media" }
                        @if let Some(image)=info.image_url.as_ref() { img class="xui-media" src=(image) alt=(record.title()); }
                        @if let Some(video)=info.video_url.as_ref() { video class="xui-media" src=(video) controls playsinline {} }
                    }
                }
            }
        }
    }
}
fn overview_market(data: &Value, asset: &str) -> Markup {
    let empty = Vec::new();
    let mut volumes = data.get("volumes").and_then(Value::as_array).cloned().unwrap_or_default();
    volumes.sort_by_key(|v| match v["quote_asset"].as_str() {
        Some("BTC") => 0,
        Some("XCP") => 1,
        _ => 2,
    });
    let trades = data.get("last_trades").and_then(Value::as_array).unwrap_or(&empty);
    let sources = data.get("sources").and_then(Value::as_array).unwrap_or(&empty);
    html! {
        section class="xui-panel" {
            div class="xui-section-heading" { h2 { "Market history" } span class="xui-muted" { "On-chain records" } }
            @if data.get("complete").and_then(Value::as_bool)!=Some(true) { p class="xui-notice" { "Some market history is unavailable or exceeds the summary limit. These figures cover loaded records only." } }
            @if volumes.is_empty() { div class="xui-empty" { h3 { "No recorded sales" } p { "There are no priced trades in the available history." } a href=(tab_url(asset,AssetTab::Orders,1,25,ActivityKind::All)) { "Browse orders →" } } }
            @else {
                div class="xui-table-wrap" { table class="xui-market-table" {
                    thead { tr { th scope="col" { "Currency" } th scope="col" { "Last price" } th scope="col" { "Volume" } th scope="col" { "Trades" } } }
                    tbody { @for v in &volumes {
                        @let trade=trades.iter().find(|t|t["quote_asset"]==v["quote_asset"]);
                        tr {
                            th scope="row" { (v["quote_asset"].as_str().unwrap_or("—")) }
                            td data-label="Last price" {
                                @if let Some(t)=trade { strong { (number_label(&t["price"])) } small class="xui-trade-meta" { (date_label(t["block_time"].as_i64()))
                                    @if let Some(hash)=t["tx_hash"].as_str() {a class="xui-trade-link" href=(explorer_path(&format!("/tx/{hash}"))) title="View latest trade transaction" { (if t["venue"]=="matches" {"DEX ↗"}else if t["venue"]=="dispenses" {"Dispenser ↗"}else{"Pool ↗"}) } } }
                                }
                            }
                            td data-label="Volume" { (number_label(&v["quote_quantity"])) small { (number_label(&v["asset_quantity"])) " " (asset) } }
                            td data-label="Trades" { (number_label(&v["trades"])) }
                        }
                    } }
                } }
                p class="xui-caption" { "Price per unit and traded volume in each row’s currency. " (if data["trade_history_complete"]==true {"All available history."}else{"Loaded history only."}) }
            }
            details class="xui-disclosure" { summary { "About these figures" }
                p { "Completed DEX trades, BTC dispenses, and the " (asset) "/XCP pool. No off-chain sales or historical USD conversion. Summaries refresh every minute and load up to 2,000 records per source." }
                p { (number_label(&data["active_months"])) " months with trades in loaded history." }
                ul { @for source in sources { li { (friendly_kind(source["type"].as_str().unwrap_or(""))) ": " (number_label(&source["loaded"])) " of " (number_label(&source["total"])) (if source["complete"]==true {" · complete"}else{" · partial"}) } } }
            }
        }
        section class="xui-panel xui-listings" {
            div class="xui-section-heading" { h2 { "Available listings" } a href=(format!("{}&status=open",tab_url(asset,AssetTab::Dispensers,1,25,ActivityKind::All))) { "Browse open dispensers →" } }
            div class="xui-listing-grid" {
                div { span class="xui-label" { "Open DEX orders" } strong { a href=(format!("{}&status=open",tab_url(asset,AssetTab::Orders,1,25,ActivityKind::All))) { (number_label(&data["open_orders"])) } } small { (number_label(&data["order_escrow"])) " " (asset) " in sell orders" } }
                div { span class="xui-label" { "Open dispensers" } strong { a href=(format!("{}&status=open",tab_url(asset,AssetTab::Dispensers,1,25,ActivityKind::All))) { (number_label(&data["open_dispensers"])) } } small { (number_label(&data["dispenser_escrow"])) " " (asset) " remaining" } }
            }
            @for (key,label) in [("best_asks","Best DEX ask"),("best_bids","Best DEX bid")] { @if let Some(prices)=data.get(key).and_then(Value::as_array) { @for price in prices { p { span class="xui-muted" { (label) " · " } (number_label(&price["price"])) " " (price["quote_asset"].as_str().unwrap_or("")) " / " (asset) } } } }
        }
    }
}
fn native_view(
    asset: &str,
    tab: AssetTab,
    page: usize,
    limit: usize,
    list: Option<&CoreList>,
    status: &str,
    quote: &str,
) -> Markup {
    let empty = CoreList { items: vec![], total: 0 };
    let list = list.unwrap_or(&empty);
    html! {
        div class="xui-panel" {
            (history_controls(asset,tab,limit,status,quote,list.total))
            @if list.items.is_empty() { div class="xui-empty" { h3 { "No " (friendly_kind(tab.as_query()).to_lowercase()) " in this selection" } p { "Try another history or status filter." } @if status!="all" { a href=(format!("{}#asset-content",tab_url(asset,tab,1,limit,ActivityKind::All))) { "Clear status filter →" } } @else if page>1 { a href=(format!("{}&quote_asset={}#asset-content",tab_url(asset,tab,1,limit,ActivityKind::All),crate::modules::xcp::core::encode_path(quote))) { "Back to first page →" } } } }
            @else {
                div class="xui-row-labels" aria-hidden="true" { span { "Record / amount" } span { "Participants" } span { "Status" } span {} }
                div class="xui-records" { @for row in &list.items { (history_record(asset,tab.as_query(),row)) } }
            }
            (pager(list.total,list.items.len(),page,limit,|target|format!("{}&status={}&quote_asset={}#asset-content",tab_url(asset,tab,target,limit,ActivityKind::All),status,crate::modules::xcp::core::encode_path(quote))))
        }
    }
}
fn activity_view(
    asset: &str,
    kind: ActivityKind,
    page: usize,
    limit: usize,
    list: Option<&ActivityPage>,
) -> Markup {
    let empty = ActivityPage { items: vec![], total: 0 };
    let list = list.unwrap_or(&empty);
    html! {
        div class="xui-panel" {
            div class="xui-toolbar" {
                div { h2 { "Activity" } p class="xui-muted" { (if matches!(kind,ActivityKind::All|ActivityKind::Trades) {"Recent records across histories"}else{kind.label()}) } }
                form method="get" action=(asset_href(asset)) {
                    input type="hidden" name="tab" value="activity"; input type="hidden" name="limit" value=(limit);
                    label { span class="xui-sr-only" { "Activity type" } select name="activity" onchange="this.form.submit()" {
                        @for item in ActivityKind::all() {option value=(item.as_query()) selected[item==kind] {(if item==ActivityKind::All {"All activity"}else{item.label()})}}
                    } } noscript {button type="submit" {"Filter"}}
                }
            }
            @if list.items.is_empty() {div class="xui-empty" {h3 {"No activity in this selection"} p {"Choose another activity type to explore its history."}}}
            @else {div class="xui-records" { @for item in &list.items {
                @let resource=match item.action {activity::ActivityAction::Send=>"sends",activity::ActivityAction::Dispense=>"dispenses",activity::ActivityAction::PoolTrade=>"pool_matches",activity::ActivityAction::DexTrade=>"matches",activity::ActivityAction::Native(kind)=>kind};
                (history_record(asset,resource,&item.raw))
            } } }
            @if matches!(kind,ActivityKind::All|ActivityKind::Trades) {details class="xui-disclosure" {summary {"About this activity feed"} p {"The latest 250 records from the combined histories. Choose a specific history for older records. Orders and dispensers show current state, not individual status changes."}}}
            (pager(list.total,list.items.len(),page,limit,|target|format!("{}#asset-content",tab_url(asset,AssetTab::Activity,target,limit,kind))))
        }
    }
}
fn value_text(row: &Value, key: &str) -> String {
    row.get(key)
        .filter(|v| !v.is_null())
        .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))
        .unwrap_or_default()
}
fn quantity_text(row: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| row.get(*key).filter(|v| !v.is_null()))
        .map(number_label)
        .unwrap_or_else(|| "—".into())
}
fn record_title(asset: &str, kind: &str, row: &Value) -> String {
    let qty = quantity_text(
        row,
        &[
            "quantity_normalized",
            "dispense_quantity_normalized",
            "supply_normalized",
            "quantity_per_unit_normalized",
            "quantity",
        ],
    );
    match kind {
        "orders" => format!(
            "{} {} → {} {}",
            quantity_text(row, &["give_quantity_normalized"]),
            value_text(row, "give_asset"),
            quantity_text(row, &["get_quantity_normalized"]),
            value_text(row, "get_asset")
        ),
        "matches" | "pool_matches" => format!(
            "{} {} ↔ {} {}",
            quantity_text(row, &["forward_quantity_normalized"]),
            value_text(row, "forward_asset"),
            quantity_text(row, &["backward_quantity_normalized"]),
            value_text(row, "backward_asset")
        ),
        "dispensers" => {
            format!("{} {asset} per lot", quantity_text(row, &["give_quantity_normalized"]))
        }
        "dispenses" => {
            format!("{qty} {asset} · {} BTC", quantity_text(row, &["btc_amount_normalized"]))
        }
        "subassets" => {
            let name = value_text(row, "asset_longname");
            if name.is_empty() { value_text(row, "asset") } else { name }
        }
        "dividends" => format!(
            "{qty} {} per unit",
            row.get("dividend_asset_info")
                .and_then(|v| v.get("asset_longname"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| value_text(row, "dividend_asset"))
        ),
        "issuances" => {
            let event = value_text(row, "asset_events");
            if qty == "0" && !event.is_empty() {
                match event.as_str() {
                    "lock_quantity" => "Supply locked".into(),
                    "transfer" => "Ownership transferred".into(),
                    _ => field_label(&event),
                }
            } else {
                format!("{qty} {asset}")
            }
        }
        "pool_history" => "Pool reserve update".into(),
        "pool_deposits" => "Liquidity added".into(),
        "pool_withdrawals" => "Liquidity withdrawn".into(),
        "fairminters" => "Minting campaign".into(),
        _ => format!("{qty} {asset}"),
    }
}
fn event_label<'a>(asset: &str, kind: &'a str, row: &Value) -> &'a str {
    match kind {
        "orders" if row["give_asset"].as_str() == Some(asset) => "Sell order",
        "orders" => "Buy order",
        "sends" => "Send",
        "matches" => "DEX trade",
        "dispensers" => "Dispenser",
        "dispenses" => "Dispense",
        "issuances" => "Issuance",
        "destructions" => "Destruction",
        "dividends" => "Dividend",
        "credits" => "Credit",
        "debits" => "Debit",
        _ => friendly_kind(kind),
    }
}
fn history_record(asset: &str, kind: &str, row: &Value) -> Markup {
    let title = record_title(asset, kind, row);
    let timestamp = row
        .get("block_time")
        .or_else(|| row.get("first_issuance_block_time"))
        .and_then(Value::as_i64);
    let block = row.get("block_index").or_else(|| row.get("first_issuance_block_index"));
    let source = ["source", "address", "tx0_address", "owner"]
        .iter()
        .find_map(|k| row.get(*k).and_then(Value::as_str))
        .unwrap_or("");
    let destination = row
        .get("destination")
        .or_else(|| row.get("tx1_address"))
        .and_then(Value::as_str);
    let raw_status = value_text(row, "status");
    let status = if kind == "dispensers" {
        match raw_status.as_str() {
            "0" => "Open".into(),
            "1" => "Open".into(),
            "10" => "Closed".into(),
            "11" => "Closing".into(),
            _ => raw_status.clone(),
        }
    } else {
        field_label(&raw_status)
    };
    let status_class = if ["Open", "Completed", "Filled", "Valid"].contains(&status.as_str()) {
        "xui-status positive"
    } else if status.to_lowercase().starts_with("invalid") {
        "xui-status invalid"
    } else {
        "xui-status"
    };
    let tx = ["tx_hash", "tx1_hash", "event"]
        .iter()
        .find_map(|k| row.get(*k).and_then(Value::as_str))
        .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()));
    html! {
        details class="xui-record" {
            summary {
                span class="xui-record-main" {
                    span class="xui-event" { (event_label(asset,kind,row)) }
                    strong { (title) }
                    span class="xui-muted" { (date_label(timestamp)) @if let Some(block)=block {" · Block " (number_label(block))} }
                    @if kind=="dispensers" {span class="xui-muted" { (quantity_text(row,&["give_remaining_normalized"])) " remaining · " (quantity_text(row,&["satoshi_price_normalized"])) " BTC per lot" }}
                }
                span class="xui-participants" { @if !source.is_empty() {span title=(source) {(short_hash(source))}} @if let Some(dest)=destination {small title=(dest) {"→ " (short_hash(dest))}} }
                span { @if !status.is_empty() {span class=(status_class) title=(status.clone()) {(if status.len()>35 {"Invalid"}else{&status})}} }
                span class="xui-expand" {"Details"}
            }
            div class="xui-record-body" {
                @if let Some(tx)=tx {p class="xui-transaction" {span class="xui-label" {"Transaction"} a href=(explorer_path(&format!("/tx/{tx}"))) title=(tx) {(tx) " ↗"}}}
                @if kind=="subassets" {p {a href=(asset_href(&value_text(row,"asset"))) {"Open subasset →"}}}
                dl class="xui-fields" {
                    @if let Some(fields)=row.as_object() { @for (key,value) in fields {
                        @if !value.is_null() && !fields.contains_key(&format!("{key}_normalized")) && key!="tx_hash" && key!="tx1_hash" {
                            div {dt {(field_label(key))} dd {
                                @if value.is_object() {details class="xui-nested" {summary {"View asset information"} (record_value(key,value))}}
                                @else if key.ends_with("_normalized") {(number_label(value))}
                                @else {(record_value(key,value))}
                            }}
                        }
                    } }
                }
            }
        }
    }
}
