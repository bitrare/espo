use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
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
use crate::modules::xcp::core::{
    CoreFetch, CoreList, asset_name_from_value, fetch_asset, fetch_asset_list,
};
use crate::modules::xcp::display::{asset_letter, short_hash};

#[derive(Deserialize)]
pub struct PageQuery {
    pub tab: Option<String>,
    pub activity: Option<String>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AssetTab {
    Holders,
    Issuances,
    Dispensers,
    Orders,
    Dividends,
    Activity,
}

impl AssetTab {
    fn from_query(raw: Option<&str>) -> Self {
        match raw {
            Some("issuances") => Self::Issuances,
            Some("dispensers") => Self::Dispensers,
            Some("orders") => Self::Orders,
            Some("dividends") => Self::Dividends,
            Some("activity") => Self::Activity,
            _ => Self::Holders,
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
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Holders => "Holders",
            Self::Issuances => "Issuances",
            Self::Dispensers => "Dispensers",
            Self::Orders => "Orders",
            Self::Dividends => "Dividends",
            Self::Activity => "Activity",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivityKind {
    Sends,
    Dispenses,
}

impl ActivityKind {
    fn from_query(raw: Option<&str>) -> Self {
        match raw {
            Some("dispenses") | Some("dispense") => Self::Dispenses,
            _ => Self::Sends,
        }
    }

    fn as_query(self) -> &'static str {
        match self {
            Self::Sends => "sends",
            Self::Dispenses => "dispenses",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Sends => "Sends",
            Self::Dispenses => "Dispenses",
        }
    }

    fn suffix(self) -> &'static str {
        self.as_query()
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
                string_field(value, "supply_normalized")
                    .as_deref()
                    .unwrap_or("0"),
            ),
            description: string_field(value, "description"),
            mime_type: string_field(value, "mime_type"),
            first_issuance_block: value
                .get("first_issuance_block_index")
                .and_then(|v| v.as_u64().or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))),
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
        self.description.as_deref().filter(|url| {
            url.starts_with("http://") || url.starts_with("https://")
        })
    }
}

pub async fn xcp_asset_page(
    State(_state): State<ExplorerState>,
    Path(asset): Path<String>,
    Query(q): Query<PageQuery>,
) -> Response {
    let query = asset.clone();
    let fetched = match tokio::task::spawn_blocking(move || fetch_asset(&query)).await {
        Ok(result) => result,
        Err(_) => return (StatusCode::BAD_GATEWAY, "Counterparty Core unavailable").into_response(),
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

    let page = q.page.unwrap_or(1).max(1);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let tab = AssetTab::from_query(q.tab.as_deref());
    let activity = ActivityKind::from_query(q.activity.as_deref());
    let offset = limit.saturating_mul(page.saturating_sub(1));
    let asset_name = record.name.clone();

    let loaded = tokio::task::spawn_blocking(move || {
        let holders = if tab == AssetTab::Holders {
            fetch_asset_list(&asset_name, "balances", "sort=quantity:DESC", offset, limit)
        } else {
            fetch_asset_list(&asset_name, "balances", "sort=quantity:DESC", 0, 1)
        };
        let extra = match tab {
            AssetTab::Issuances => Some(fetch_asset_list(&asset_name, "issuances", "", offset, limit)),
            AssetTab::Dispensers => {
                Some(fetch_asset_list(&asset_name, "dispensers", "status=open", offset, limit))
            }
            AssetTab::Orders => {
                Some(fetch_asset_list(&asset_name, "orders", "status=open", offset, limit))
            }
            AssetTab::Dividends => Some(fetch_asset_list(&asset_name, "dividends", "", offset, limit)),
            AssetTab::Activity => {
                Some(fetch_asset_list(&asset_name, activity.suffix(), "", offset, limit))
            }
            AssetTab::Holders => None,
        };
        (holders, extra)
    })
    .await;

    let (holders_fetch, extra_fetch) = match loaded {
        Ok(pair) => pair,
        Err(_) => return (StatusCode::BAD_GATEWAY, "Counterparty Core unavailable").into_response(),
    };
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

    let holders_total = holders_list.total;
    let canonical = format!("/counterparty/asset/{}", record.name);
    let title = record.title().to_string();

    layout_with_meta(
        &title,
        &canonical,
        Some("Counterparty asset"),
        html! {
            div class="alkane-page" {
                div class="alkane-hero-card" {
                    div class="alk-icon-wrap alk-icon-lg" aria-hidden="true" {
                        @if let Some(url) = record.icon_url() {
                            img class="alk-icon-img" src=(url) alt="";
                        }
                        span class="alk-icon-letter" { (asset_letter(&record.name)) }
                    }
                    div class="alkane-hero-text" {
                        a class="alkane-tag" href=(explorer_path("/counterparty/assets")) { "COUNTERPARTY" }
                        h1 class="alkane-hero-title" { (title.clone()) }
                        span class="alkane-hero-id mono" { (record.subtitle()) }
                        @if let Some(parent) = record.parent_name() {
                            span class="alkane-stat-sub" {
                                "Subasset of "
                                a class="link mono" href=(explorer_path(&format!("/counterparty/asset/{parent}"))) {
                                    (parent)
                                }
                            }
                        }
                    }
                }

                section class="alkane-section" data-alkane-overview="" {
                    div class="alkane-overview-grid" data-chart-hidden="1" {
                        div class="alkane-overview-pane" {
                            h2 class="section-title" { "Overview" }
                            div class="alkane-overview-card" {
                                div class="alkane-stat" {
                                    span class="alkane-stat-label" { "Supply" }
                                    div class="alkane-stat-line" {
                                        span class="alkane-stat-value" { (record.supply_label.clone()) }
                                        span class="alkane-stat-sub" {
                                            (if record.divisible { "(divisible, 8 decimals)" } else { "(indivisible)" })
                                        }
                                    }
                                }
                                div class="alkane-stat" {
                                    span class="alkane-stat-label" { "Locked" }
                                    div class="alkane-stat-line" {
                                        span class="alkane-stat-value" { (if record.locked { "Yes" } else { "No" }) }
                                    }
                                }
                                div class="alkane-stat" {
                                    span class="alkane-stat-label" { "Holders" }
                                    div class="alkane-stat-line" {
                                        span class="alkane-stat-value" { (format_integer(holders_total as u128)) }
                                    }
                                }
                                @if let Some(issuer) = record.issuer.as_ref() {
                                    div class="alkane-stat" {
                                        span class="alkane-stat-label" { "Issuer" }
                                        div class="alkane-stat-line" {
                                            a class="alkane-stat-value link mono" href=(explorer_path(&format!("/address/{issuer}"))) {
                                                (short_hash(issuer))
                                            }
                                        }
                                    }
                                }
                                @if let Some(owner) = record.owner.as_ref() {
                                    @if record.issuer.as_deref() != Some(owner.as_str()) {
                                        div class="alkane-stat" {
                                            span class="alkane-stat-label" { "Owner" }
                                            div class="alkane-stat-line" {
                                                a class="alkane-stat-value link mono" href=(explorer_path(&format!("/address/{owner}"))) {
                                                    (short_hash(owner))
                                                }
                                            }
                                        }
                                    }
                                }
                                @if let Some(block) = record.first_issuance_block {
                                    div class="alkane-stat" {
                                        span class="alkane-stat-label" { "First issuance" }
                                        div class="alkane-stat-line" {
                                            a class="alkane-stat-value link mono" href=(explorer_path(&format!("/block/{block}"))) {
                                                (format_integer(block as u128))
                                            }
                                        }
                                    }
                                }
                                @if let Some(description) = record.description.as_ref() {
                                    div class="alkane-stat" {
                                        span class="alkane-stat-label" { "Description" }
                                        div class="alkane-stat-line" {
                                            (description_value(description))
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                section class="alkane-section" {
                    div class="alkane-tabs" {
                        div class="alkane-tab-list" {
                            @for item in [AssetTab::Holders, AssetTab::Issuances, AssetTab::Dispensers, AssetTab::Orders, AssetTab::Dividends, AssetTab::Activity] {
                                (tab_link(&record.name, item, tab, limit, activity))
                            }
                        }
                        div class="alkane-tab-panel" {
                            (tab_body(
                                &record,
                                tab,
                                activity,
                                page,
                                limit,
                                &holders_list,
                                extra_list.as_ref(),
                            ))
                        }
                    }
                }
            }
            @if page > 1 {
                (tab_autoscroll_script())
            }
        },
    )
    .into_response()
}

fn asset_query_matches(query: &str, record: &AssetRecord) -> bool {
    let q = query.trim();
    q.eq_ignore_ascii_case(&record.name)
        || record.longname.as_deref().is_some_and(|name| q.eq_ignore_ascii_case(name))
}

fn tab_link(
    asset: &str,
    tab: AssetTab,
    active: AssetTab,
    limit: usize,
    activity: ActivityKind,
) -> Markup {
    let class = if tab == active { "alkane-tab active" } else { "alkane-tab" };
    html! {
        a class=(class) href=(tab_url(asset, tab, 1, limit, activity)) { (tab.label()) }
    }
}

fn tab_url(asset: &str, tab: AssetTab, page: usize, limit: usize, activity: ActivityKind) -> String {
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

fn tab_body(
    record: &AssetRecord,
    tab: AssetTab,
    activity: ActivityKind,
    page: usize,
    limit: usize,
    holders: &CoreList,
    extra: Option<&CoreList>,
) -> Markup {
    match tab {
        AssetTab::Holders => holders_panel(record, page, limit, holders),
        AssetTab::Issuances => {
            let list = extra.cloned().unwrap_or(CoreList { items: Vec::new(), total: 0 });
            issuances_panel(&record.name, page, limit, &list)
        }
        AssetTab::Dispensers => {
            let list = extra.cloned().unwrap_or(CoreList { items: Vec::new(), total: 0 });
            dispensers_panel(&record.name, page, limit, &list)
        }
        AssetTab::Orders => {
            let list = extra.cloned().unwrap_or(CoreList { items: Vec::new(), total: 0 });
            orders_panel(&record.name, page, limit, &list)
        }
        AssetTab::Dividends => {
            let list = extra.cloned().unwrap_or(CoreList { items: Vec::new(), total: 0 });
            dividends_panel(&record.name, page, limit, &list)
        }
        AssetTab::Activity => {
            let list = extra.cloned().unwrap_or(CoreList { items: Vec::new(), total: 0 });
            activity_panel(&record.name, activity, page, limit, &list)
        }
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
            tab_url(&record.name, AssetTab::Holders, target, limit, ActivityKind::Sends)
        }))
    }
}

fn issuances_panel(asset: &str, page: usize, limit: usize, list: &CoreList) -> Markup {
    let rows_len = list.items.len();
    let rows = list
        .items
        .iter()
        .map(|row| {
            let tx = string_field(row, "tx_hash").unwrap_or_default();
            let qty = string_field(row, "quantity_normalized").unwrap_or_else(|| "0".to_string());
            let source = string_field(row, "source").unwrap_or_else(|| "—".to_string());
            let locked = row.get("locked").and_then(|v| v.as_bool()).unwrap_or(false);
            let transfer = row.get("transfer").and_then(|v| v.as_bool()).unwrap_or(false);
            let kind = if transfer {
                "Transfer"
            } else if locked {
                "Lock"
            } else {
                "Issue"
            };
            vec![
                html! {
                    a class="link mono" href=(explorer_path(&format!("/tx/{tx}"))) { (short_hash(&tx)) }
                },
                html! { span class="mono" { (fmt_normalized(&qty)) } },
                html! { span { (kind) } },
                html! {
                    a class="link mono" href=(explorer_path(&format!("/address/{source}"))) { (short_hash(&source)) }
                },
            ]
        })
        .collect();
    html! {
        div class="alkane-panel alkane-holders-card" {
            @if list.total == 0 {
                p class="muted" { "No issuances." }
            } @else {
                (holders_table(&["Tx", "Quantity", "Type", "Source"], rows))
            }
        }
        (pager(list.total, rows_len, page, limit, |target| {
            tab_url(asset, AssetTab::Issuances, target, limit, ActivityKind::Sends)
        }))
    }
}

fn dispensers_panel(asset: &str, page: usize, limit: usize, list: &CoreList) -> Markup {
    let rows_len = list.items.len();
    let rows = list
        .items
        .iter()
        .map(|row| {
            let tx = string_field(row, "tx_hash").unwrap_or_default();
            let source = string_field(row, "source").unwrap_or_else(|| "—".to_string());
            let give = string_field(row, "give_remaining_normalized")
                .or_else(|| string_field(row, "give_quantity_normalized"))
                .unwrap_or_else(|| "0".to_string());
            let rate = string_field(row, "satoshirate_normalized").unwrap_or_else(|| "0".to_string());
            vec![
                html! {
                    a class="link mono" href=(explorer_path(&format!("/address/{source}"))) { (short_hash(&source)) }
                },
                html! { span class="mono" { (fmt_normalized(&give)) } },
                html! { span class="mono" { (format!("{} BTC", fmt_normalized(&rate))) } },
                html! {
                    a class="link mono" href=(explorer_path(&format!("/tx/{tx}"))) { (short_hash(&tx)) }
                },
            ]
        })
        .collect();
    html! {
        div class="alkane-panel alkane-holders-card" {
            @if list.total == 0 {
                p class="muted" { "No open dispensers." }
            } @else {
                (holders_table(&["Seller", "Remaining", "Rate", "Machine"], rows))
            }
        }
        (pager(list.total, rows_len, page, limit, |target| {
            tab_url(asset, AssetTab::Dispensers, target, limit, ActivityKind::Sends)
        }))
    }
}

fn orders_panel(asset: &str, page: usize, limit: usize, list: &CoreList) -> Markup {
    let rows_len = list.items.len();
    let rows = list
        .items
        .iter()
        .map(|row| {
            let tx = string_field(row, "tx_hash").unwrap_or_default();
            let source = string_field(row, "source").unwrap_or_else(|| "—".to_string());
            let give_asset = string_field(row, "give_asset").unwrap_or_else(|| "—".to_string());
            let get_asset = string_field(row, "get_asset").unwrap_or_else(|| "—".to_string());
            let give = string_field(row, "give_remaining_normalized")
                .or_else(|| string_field(row, "give_quantity_normalized"))
                .unwrap_or_else(|| "0".to_string());
            let get = string_field(row, "get_remaining_normalized")
                .or_else(|| string_field(row, "get_quantity_normalized"))
                .unwrap_or_else(|| "0".to_string());
            vec![
                html! {
                    a class="link mono" href=(explorer_path(&format!("/address/{source}"))) { (short_hash(&source)) }
                },
                html! {
                    span {
                        a class="link mono" href=(explorer_path(&format!("/counterparty/asset/{give_asset}"))) { (give_asset.clone()) }
                        " "
                        span class="mono" { (fmt_normalized(&give)) }
                    }
                },
                html! {
                    span {
                        a class="link mono" href=(explorer_path(&format!("/counterparty/asset/{get_asset}"))) { (get_asset.clone()) }
                        " "
                        span class="mono" { (fmt_normalized(&get)) }
                    }
                },
                html! {
                    a class="link mono" href=(explorer_path(&format!("/tx/{tx}"))) { (short_hash(&tx)) }
                },
            ]
        })
        .collect();
    html! {
        div class="alkane-panel alkane-holders-card" {
            @if list.total == 0 {
                p class="muted" { "No open DEX orders." }
            } @else {
                (holders_table(&["Maker", "Giving", "Getting", "Tx"], rows))
            }
        }
        (pager(list.total, rows_len, page, limit, |target| {
            tab_url(asset, AssetTab::Orders, target, limit, ActivityKind::Sends)
        }))
    }
}

fn dividends_panel(asset: &str, page: usize, limit: usize, list: &CoreList) -> Markup {
    let rows_len = list.items.len();
    let rows = list
        .items
        .iter()
        .map(|row| {
            let tx = string_field(row, "tx_hash").unwrap_or_default();
            let source = string_field(row, "source").unwrap_or_else(|| "—".to_string());
            let dividend_asset = string_field(row, "dividend_asset").unwrap_or_else(|| "XCP".to_string());
            let qty = string_field(row, "quantity_per_unit_normalized")
                .or_else(|| string_field(row, "quantity_per_unit"))
                .unwrap_or_else(|| "0".to_string());
            let block = row
                .get("block_index")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            vec![
                html! {
                    a class="link mono" href=(explorer_path(&format!("/tx/{tx}"))) { (short_hash(&tx)) }
                },
                html! {
                    a class="link mono" href=(explorer_path(&format!("/address/{source}"))) { (short_hash(&source)) }
                },
                html! {
                    a class="link mono" href=(explorer_path(&format!("/counterparty/asset/{dividend_asset}"))) { (dividend_asset) }
                },
                html! { span class="mono" { (fmt_normalized(&qty)) } },
                html! {
                    a class="link mono" href=(explorer_path(&format!("/block/{block}"))) { (format_integer(block as u128)) }
                },
            ]
        })
        .collect();
    html! {
        div class="alkane-panel alkane-holders-card" {
            @if list.total == 0 {
                p class="muted" { "No dividends." }
            } @else {
                (holders_table(&["Tx", "Source", "Paid in", "Per unit", "Block"], rows))
            }
        }
        (pager(list.total, rows_len, page, limit, |target| {
            tab_url(asset, AssetTab::Dividends, target, limit, ActivityKind::Sends)
        }))
    }
}

fn activity_panel(
    asset: &str,
    activity: ActivityKind,
    page: usize,
    limit: usize,
    list: &CoreList,
) -> Markup {
    let rows_len = list.items.len();
    let rows = list
        .items
        .iter()
        .map(|row| match activity {
            ActivityKind::Sends => send_row(row),
            ActivityKind::Dispenses => dispense_row(row),
        })
        .collect();
    let headers = match activity {
        ActivityKind::Sends => ["From", "To", "Amount", "Tx"],
        ActivityKind::Dispenses => ["Buyer", "Paid", "Amount", "Tx"],
    };
    html! {
        div class="alkane-volume-toolbar" {
            div class="alkane-tab-list" {
                @for kind in [ActivityKind::Sends, ActivityKind::Dispenses] {
                    @let class = if kind == activity { "alkane-tab active" } else { "alkane-tab" };
                    a class=(class) href=(tab_url(asset, AssetTab::Activity, 1, limit, kind)) { (kind.label()) }
                }
            }
        }
        div class="alkane-panel alkane-holders-card alkane-activity-card" {
            @if list.total == 0 {
                p class="muted" { "No activity yet." }
            } @else {
                (holders_table(&headers, rows))
            }
        }
        (pager(list.total, rows_len, page, limit, |target| {
            tab_url(asset, AssetTab::Activity, target, limit, activity)
        }))
    }
}

fn send_row(row: &Value) -> Vec<Markup> {
    let tx = string_field(row, "tx_hash").unwrap_or_default();
    let source = string_field(row, "source").unwrap_or_else(|| "—".to_string());
    let dest = string_field(row, "destination").unwrap_or_else(|| "—".to_string());
    let qty = string_field(row, "quantity_normalized").unwrap_or_else(|| "0".to_string());
    vec![
        html! { a class="link mono" href=(explorer_path(&format!("/address/{source}"))) { (short_hash(&source)) } },
        html! { a class="link mono" href=(explorer_path(&format!("/address/{dest}"))) { (short_hash(&dest)) } },
        html! { span class="mono" { (fmt_normalized(&qty)) } },
        html! { a class="link mono" href=(explorer_path(&format!("/tx/{tx}"))) { (short_hash(&tx)) } },
    ]
}

fn dispense_row(row: &Value) -> Vec<Markup> {
    let tx = string_field(row, "tx_hash").unwrap_or_default();
    let dest = string_field(row, "destination").unwrap_or_else(|| "—".to_string());
    let qty = string_field(row, "dispense_quantity_normalized")
        .or_else(|| string_field(row, "quantity_normalized"))
        .unwrap_or_else(|| "0".to_string());
    let paid = string_field(row, "btc_amount_normalized").unwrap_or_else(|| "0".to_string());
    vec![
        html! { a class="link mono" href=(explorer_path(&format!("/address/{dest}"))) { (short_hash(&dest)) } },
        html! { span class="mono" { (format!("{} BTC", fmt_normalized(&paid))) } },
        html! { span class="mono" { (fmt_normalized(&qty)) } },
        html! { a class="link mono" href=(explorer_path(&format!("/tx/{tx}"))) { (short_hash(&tx)) } },
    ]
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

fn tab_autoscroll_script() -> Markup {
    PreEscaped(
        r#"<script>
(() => {
  const scrollToTabs = () => {
    const target = document.querySelector('.alkane-tab-list');
    if (!target) return;
    const top = Math.max(0, target.getBoundingClientRect().top + window.scrollY);
    window.scrollTo({ top, left: 0, behavior: 'auto' });
  };
  scrollToTabs();
  requestAnimationFrame(scrollToTabs);
})();
</script>"#
            .to_string(),
    )
}

fn description_value(description: &str) -> Markup {
    if description.starts_with("http://") || description.starts_with("https://") {
        html! { a class="alkane-stat-value link" href=(description) rel="noopener noreferrer" target="_blank" { (short_hash(description)) } }
    } else {
        html! { span class="alkane-stat-value" { (description) } }
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
