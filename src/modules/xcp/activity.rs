use super::core::{self, CoreFetch, CoreList};
use serde_json::Value;

const MERGE_SOURCE_CAP: usize = 250;
const NEWEST_FIRST: &str = "sort=block_index:DESC";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    All,
    Trades,
    Sends,
    Dispenses,
    Native(&'static str),
}

impl ActivityKind {
    pub fn from_query(raw: Option<&str>) -> Self {
        match raw {
            Some("dispenses") | Some("dispense") => Self::Dispenses,
            Some("sends") | Some("send") => Self::Sends,
            Some("trades") | Some("trade") => Self::Trades,
            Some(kind) if super::history::RESOURCES.contains(&kind) => Self::Native(
                super::history::RESOURCES.iter().copied().find(|v| *v == kind).unwrap(),
            ),
            _ => Self::All,
        }
    }

    pub fn as_query(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Trades => "trades",
            Self::Sends => "sends",
            Self::Dispenses => "dispenses",
            Self::Native(kind) => kind,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Trades => "Trades",
            Self::Sends => "Sends",
            Self::Dispenses => "Dispenses",
            Self::Native(kind) => super::history::label(kind),
        }
    }

    pub fn all() -> Vec<Self> {
        let mut kinds = vec![Self::All, Self::Trades, Self::Sends, Self::Dispenses];
        kinds.extend(
            [
                "issuances",
                "orders",
                "dispensers",
                "dividends",
                "destructions",
                "fairminters",
                "fairmints",
                "credits",
                "debits",
                "pool_deposits",
                "pool_withdrawals",
            ]
            .into_iter()
            .map(Self::Native),
        );
        kinds
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityAction {
    Send,
    Dispense,
    PoolTrade,
    DexTrade,
    Native(&'static str),
}

impl ActivityAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Send => "Send",
            Self::Dispense => "Dispense",
            Self::PoolTrade => "Pool",
            Self::DexTrade => "DEX match",
            Self::Native(kind) => super::history::label(kind),
        }
    }

    fn trade_group(self) -> &'static str {
        match self {
            Self::PoolTrade | Self::DexTrade => "trade",
            Self::Send => "send",
            Self::Dispense => "dispense",
            Self::Native(kind) => kind,
        }
    }
}

fn action_rank(action: ActivityAction) -> u8 {
    match action {
        ActivityAction::PoolTrade => 0,
        ActivityAction::DexTrade => 1,
        ActivityAction::Dispense => 2,
        ActivityAction::Send => 3,
        ActivityAction::Native(_) => 4,
    }
}

#[derive(Clone, Debug)]
pub struct ActivityItem {
    pub action: ActivityAction,
    pub block_index: u32,
    pub block_time: u64,
    pub tx_index: u128,
    pub tx_hash: String,
    pub raw: Value,
}

#[derive(Clone)]
pub struct ActivityPage {
    pub items: Vec<ActivityItem>,
    pub total: usize,
}

pub fn fetch_asset_activity(
    asset: &str,
    kind: ActivityKind,
    offset: usize,
    limit: usize,
) -> CoreFetch<ActivityPage> {
    let asset = asset.trim();
    if asset.is_empty() {
        return CoreFetch::NotFound;
    }
    match kind {
        ActivityKind::Native(resource) => {
            match super::history::fetch(asset, resource, "XCP", "all", offset, limit) {
                CoreFetch::Ok(list) => CoreFetch::Ok(ActivityPage {
                    total: list.total,
                    items: list
                        .items
                        .into_iter()
                        .filter_map(|row| item_from_row(ActivityAction::Native(resource), row))
                        .collect(),
                }),
                CoreFetch::NotFound => CoreFetch::Ok(ActivityPage { total: 0, items: vec![] }),
                CoreFetch::Unreachable => CoreFetch::Unreachable,
            }
        }
        ActivityKind::Sends => paged_source(asset, "sends", ActivityAction::Send, offset, limit),
        ActivityKind::Dispenses => {
            paged_source(asset, "dispenses", ActivityAction::Dispense, offset, limit)
        }
        ActivityKind::Trades | ActivityKind::All => merged_activity(asset, kind, offset, limit),
    }
}

fn paged_source(
    asset: &str,
    suffix: &str,
    action: ActivityAction,
    offset: usize,
    limit: usize,
) -> CoreFetch<ActivityPage> {
    match core::fetch_asset_list(asset, suffix, NEWEST_FIRST, offset, limit) {
        CoreFetch::Ok(list) => CoreFetch::Ok(ActivityPage {
            total: list.total,
            items: list.items.into_iter().filter_map(|row| item_from_row(action, row)).collect(),
        }),
        CoreFetch::NotFound => CoreFetch::Ok(ActivityPage { items: Vec::new(), total: 0 }),
        CoreFetch::Unreachable => CoreFetch::Unreachable,
    }
}

fn merged_activity(
    asset: &str,
    kind: ActivityKind,
    offset: usize,
    limit: usize,
) -> CoreFetch<ActivityPage> {
    let need = offset.saturating_add(limit).clamp(1, MERGE_SOURCE_CAP);
    let mut unreachable = true;
    let mut failed = false;
    let mut items = Vec::new();
    let mut total = 0usize;

    let mut push_list = |list: CoreFetch<CoreList>, action: ActivityAction| match list {
        CoreFetch::Ok(list) => {
            unreachable = false;
            total = total.saturating_add(list.total);
            items.extend(list.items.into_iter().filter_map(|row| item_from_row(action, row)));
        }
        CoreFetch::NotFound => {
            unreachable = false;
        }
        CoreFetch::Unreachable => {
            failed = true;
        }
    };

    if kind == ActivityKind::All {
        for resource in [
            "issuances",
            "orders",
            "dispensers",
            "dividends",
            "destructions",
            "fairminters",
            "fairmints",
            "pool_deposits",
            "pool_withdrawals",
        ] {
            push_list(
                super::history::fetch(asset, resource, "XCP", "all", 0, need),
                ActivityAction::Native(resource),
            );
        }
        push_list(
            core::fetch_asset_list(asset, "sends", NEWEST_FIRST, 0, need),
            ActivityAction::Send,
        );
    }
    push_list(
        core::fetch_asset_list(asset, "dispenses", NEWEST_FIRST, 0, need),
        ActivityAction::Dispense,
    );
    push_list(
        core::fetch_path_list(
            &format!("/pools/{}/XCP/matches", core::encode_path(asset)),
            "",
            0,
            need,
        ),
        ActivityAction::PoolTrade,
    );
    push_list(
        core::fetch_asset_list(
            asset,
            "matches",
            if kind == ActivityKind::Trades {
                "status=completed&sort=block_index:DESC"
            } else {
                NEWEST_FIRST
            },
            0,
            need,
        ),
        ActivityAction::DexTrade,
    );

    if failed || (unreachable && items.is_empty()) {
        return CoreFetch::Unreachable;
    }

    items.sort_by(|a, b| {
        b.block_index
            .cmp(&a.block_index)
            .then_with(|| b.block_time.cmp(&a.block_time))
            .then_with(|| a.tx_hash.cmp(&b.tx_hash))
            .then_with(|| a.action.trade_group().cmp(&b.action.trade_group()))
            .then_with(|| action_rank(a.action).cmp(&action_rank(b.action)))
            .then_with(|| b.tx_index.cmp(&a.tx_index))
    });
    // Do not deduplicate by tx_hash: MPMA sends and multiple order matches
    // are distinct records in the same transaction. The merged view is an
    // explicitly bounded recent window; native histories expose every page.
    let total = total.min(MERGE_SOURCE_CAP);

    let items = items.into_iter().take(MERGE_SOURCE_CAP).skip(offset).take(limit).collect();
    CoreFetch::Ok(ActivityPage { items, total })
}

fn item_from_row(action: ActivityAction, row: Value) -> Option<ActivityItem> {
    let tx_hash = match action {
        ActivityAction::DexTrade => first_string(&row, &["tx1_hash", "tx_hash", "tx0_hash"]),
        _ => first_string(&row, &["tx_hash", "tx1_hash", "event"]),
    }
    .filter(|s| !s.is_empty())?;
    Some(ActivityItem {
        action,
        block_index: json_u32(row.get("block_index")),
        block_time: json_u64(row.get("block_time")),
        tx_index: json_u128(row.get("tx_index"))
            .max(json_u128(row.get("tx1_index")))
            .max(json_u128(row.get("tx0_index"))),
        tx_hash,
        raw: row,
    })
}

fn first_string(row: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        row.get(*key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    })
}

fn json_u32(value: Option<&Value>) -> u32 {
    json_u64(value) as u32
}

fn json_u64(value: Option<&Value>) -> u64 {
    let Some(value) = value else { return 0 };
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
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

pub async fn rpc(payload: Value) -> Value {
    let Some(asset) = payload.get("asset").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
    else {
        return serde_json::json!({"ok":false,"error":"asset required"});
    };
    let kind_name = payload.get("type").and_then(Value::as_str).unwrap_or("all");
    let Some(kind) = ActivityKind::all().into_iter().find(|kind| kind.as_query() == kind_name)
    else {
        return serde_json::json!({"ok":false,"error":"invalid activity type; use get_asset_history for native histories"});
    };
    let offset = payload.get("offset").map_or(Some(0), Value::as_u64);
    let limit = payload.get("limit").map_or(Some(50), Value::as_u64);
    let (Some(offset), Some(limit)) = (offset, limit) else {
        return serde_json::json!({"ok":false,"error":"invalid pagination"});
    };
    let bounded = matches!(kind, ActivityKind::All | ActivityKind::Trades);
    if limit == 0 || limit > 200 || offset > 10_000_000 || (bounded && offset >= 250) {
        return serde_json::json!({"ok":false,"error":"limit must be 1..200; merged activity offset must be below 250; native offset maximum is 10000000"});
    }
    let asset = asset.to_string();
    let lookup = asset.clone();
    let take = if bounded { limit.min(250 - offset) } else { limit };
    match tokio::task::spawn_blocking(move || {
        fetch_asset_activity(&lookup, kind, offset as usize, take as usize)
    })
    .await
    {
        Ok(CoreFetch::Ok(page)) => {
            serde_json::json!({"ok":true,"asset":asset,"type":kind.as_query(),"offset":offset,"limit":take,"total":page.total,"recent_window":if bounded {Some(250)} else {None},"has_more":(offset+page.items.len() as u64)<(page.total as u64),"result":page.items.iter().map(|item|serde_json::json!({"action":item.action.label(),"block_index":item.block_index,"block_time":item.block_time,"tx_hash":item.tx_hash,"record":item.raw})).collect::<Vec<_>>()})
        }
        _ => serde_json::json!({"ok":false,"error":"upstream_unavailable"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_message_identity_in_raw_rows() {
        let a = item_from_row(
            ActivityAction::Send,
            serde_json::json!({"tx_hash":"abc","msg_index":1,"quantity_normalized":"2"}),
        )
        .unwrap();
        let b = item_from_row(
            ActivityAction::Send,
            serde_json::json!({"tx_hash":"abc","msg_index":2,"quantity_normalized":"3"}),
        )
        .unwrap();
        assert_eq!(a.tx_hash, b.tx_hash);
        assert_ne!(a.raw, b.raw);
    }
    #[test]
    fn ledger_records_use_event_transaction() {
        let item = item_from_row(
            ActivityAction::Native("credits"),
            serde_json::json!({"event":"abc","block_index":123}),
        )
        .unwrap();
        assert_eq!(item.tx_hash, "abc");
        assert_eq!(item.block_index, 123);
    }
}
