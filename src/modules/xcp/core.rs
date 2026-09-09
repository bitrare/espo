use super::config::XcpConfig;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;

/// Named Counterparty assets encode as base-26 (A=0). IDs above 26^12 are
/// numeric and displayed as `A{id}`.
const MAX_NAMED_ASSET_ID: u128 = 26u128.pow(12);
const ADDRESS_BALANCE_PAGE: usize = 200;
const ADDRESS_BALANCE_MAX: usize = 1000;
const ADDRESS_TX_PAGE: usize = 200;
const ADDRESS_TX_MAX: usize = 500;

#[derive(Clone, Debug)]
pub enum CoreFetch<T> {
    Ok(T),
    NotFound,
    Unreachable,
}

impl<T> CoreFetch<T> {
    pub fn ok(self) -> Option<T> {
        match self {
            Self::Ok(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CoreList {
    pub items: Vec<Value>,
    pub total: usize,
}

#[derive(Clone, Debug)]
pub struct AddressAssetBalance {
    pub asset: String,
    pub longname: Option<String>,
    pub quantity: u128,
    pub quantity_normalized: String,
    pub divisible: bool,
    pub description: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AddressTransaction {
    pub txid: String,
    pub block_index: u32,
    pub transaction_type: String,
    pub raw: Value,
}

impl AddressAssetBalance {
    pub fn to_api(&self) -> Value {
        let icon = super::enhanced::warm(&self.asset, self.description.as_deref())
            .and_then(|info| info.icon().map(|s| s.to_string()));
        json!({
            "asset": self.asset,
            "asset_longname": self.longname,
            "quantity": self.quantity.to_string(),
            "quantity_normalized": self.quantity_normalized,
            "divisible": self.divisible,
            "description": self.description,
            "icon": icon,
        })
    }
}

/// Fetch one Counterparty Core transaction. Fail-open: any network, timeout,
/// or envelope problem returns `None` so the explorer still renders.
pub fn fetch_transaction(txid: &str) -> Option<Value> {
    let cfg = XcpConfig::from_global()?;
    fetch_transaction_with(&cfg, txid)
}

pub fn fetch_transaction_with(cfg: &XcpConfig, txid: &str) -> Option<Value> {
    let txid = txid.trim();
    if txid.is_empty() || !txid.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match get_result(cfg, &format!("/transactions/{txid}?verbose=true")) {
        CoreFetch::Ok(value) => Some(value),
        _ => None,
    }
}

pub fn fetch_asset(query: &str) -> CoreFetch<Value> {
    let Some(cfg) = XcpConfig::from_global() else {
        return CoreFetch::Unreachable;
    };
    fetch_asset_with(&cfg, query)
}

pub fn fetch_asset_with(cfg: &XcpConfig, query: &str) -> CoreFetch<Value> {
    let mut last = CoreFetch::NotFound;
    for candidate in asset_lookup_candidates(query) {
        match get_result(cfg, &format!("/assets/{}?verbose=true", encode_path(&candidate))) {
            CoreFetch::Ok(value) if value.is_object() => return CoreFetch::Ok(value),
            CoreFetch::Unreachable => return CoreFetch::Unreachable,
            other => last = other,
        }
    }
    last
}

pub fn fetch_asset_list(
    asset: &str,
    suffix: &str,
    extra: &str,
    offset: usize,
    limit: usize,
) -> CoreFetch<CoreList> {
    let Some(cfg) = XcpConfig::from_global() else {
        return CoreFetch::Unreachable;
    };
    fetch_asset_list_with(&cfg, asset, suffix, extra, offset, limit)
}

pub fn fetch_address_balances(address: &str) -> CoreFetch<Vec<AddressAssetBalance>> {
    fetch_address_balances_filtered(address, None)
}

pub fn fetch_address_balances_filtered(
    address: &str,
    asset: Option<&str>,
) -> CoreFetch<Vec<AddressAssetBalance>> {
    let address = address.trim();
    if address.is_empty() {
        return CoreFetch::NotFound;
    }

    let (path, extra) = match asset.map(str::trim).filter(|s| !s.is_empty()) {
        Some(asset) => (
            format!("/addresses/{}/balances/{}", encode_path(address), encode_path(asset)),
            String::new(),
        ),
        None => (
            format!("/addresses/{}/balances", encode_path(address)),
            "sort=quantity:DESC".to_string(),
        ),
    };

    let mut offset = 0;
    let mut rows = Vec::new();
    let mut total = usize::MAX;
    while rows.len() < ADDRESS_BALANCE_MAX && offset < total {
        match fetch_path_list(&path, &extra, offset, ADDRESS_BALANCE_PAGE) {
            CoreFetch::Ok(list) => {
                total = list.total;
                if list.items.is_empty() {
                    break;
                }
                let batch = list.items.len();
                rows.extend(list.items);
                offset += batch;
                if rows.len() >= total {
                    break;
                }
            }
            CoreFetch::NotFound if rows.is_empty() => return CoreFetch::NotFound,
            CoreFetch::Unreachable if rows.is_empty() => return CoreFetch::Unreachable,
            CoreFetch::NotFound | CoreFetch::Unreachable => break,
        }
    }

    CoreFetch::Ok(aggregate_address_balances(&rows))
}

pub fn fetch_address_transactions(address: &str) -> CoreFetch<Vec<AddressTransaction>> {
    let address = address.trim();
    if address.is_empty() {
        return CoreFetch::NotFound;
    }
    let path = format!("/addresses/{}/transactions", encode_path(address));
    let mut offset = 0;
    let mut rows = Vec::new();
    let mut total = usize::MAX;
    while rows.len() < ADDRESS_TX_MAX && offset < total {
        match fetch_path_list(&path, "", offset, ADDRESS_TX_PAGE) {
            CoreFetch::Ok(list) => {
                total = list.total;
                if list.items.is_empty() {
                    break;
                }
                let batch = list.items.len();
                rows.extend(list.items);
                offset += batch;
                if rows.len() >= total {
                    break;
                }
            }
            CoreFetch::NotFound if rows.is_empty() => return CoreFetch::NotFound,
            CoreFetch::Unreachable if rows.is_empty() => return CoreFetch::Unreachable,
            CoreFetch::NotFound | CoreFetch::Unreachable => break,
        }
    }
    CoreFetch::Ok(rows.iter().filter_map(parse_address_transaction).collect())
}

pub fn fetch_pool(asset_a: &str, asset_b: &str) -> CoreFetch<Value> {
    let Some(cfg) = XcpConfig::from_global() else {
        return CoreFetch::Unreachable;
    };
    get_result(
        &cfg,
        &format!("/pools/{}/{}?verbose=true", encode_path(asset_a), encode_path(asset_b)),
    )
}

pub fn fetch_path_list(
    path: &str,
    extra: &str,
    offset: usize,
    limit: usize,
) -> CoreFetch<CoreList> {
    let Some(cfg) = XcpConfig::from_global() else {
        return CoreFetch::Unreachable;
    };
    let mut url = format!("{path}?verbose=true&limit={limit}&offset={offset}");
    if !extra.is_empty() {
        url.push('&');
        url.push_str(extra);
    }
    match get_body(&cfg, &url) {
        CoreFetch::Ok(body)
            if body.get("result").is_some_and(Value::is_array)
                && body.get("result_count").and_then(Value::as_u64).is_some() =>
        {
            CoreFetch::Ok(list_from_body(&body))
        }
        CoreFetch::Ok(_) => CoreFetch::Unreachable,
        CoreFetch::NotFound => CoreFetch::NotFound,
        CoreFetch::Unreachable => CoreFetch::Unreachable,
    }
}

pub fn fetch_asset_list_with(
    cfg: &XcpConfig,
    asset: &str,
    suffix: &str,
    extra: &str,
    offset: usize,
    limit: usize,
) -> CoreFetch<CoreList> {
    let mut path = format!(
        "/assets/{}/{suffix}?verbose=true&limit={limit}&offset={offset}",
        encode_path(asset)
    );
    if !extra.is_empty() {
        path.push('&');
        path.push_str(extra);
    }
    match get_body(cfg, &path) {
        CoreFetch::Ok(body)
            if body.get("result").is_some_and(Value::is_array)
                && body.get("result_count").and_then(Value::as_u64).is_some() =>
        {
            CoreFetch::Ok(list_from_body(&body))
        }
        CoreFetch::Ok(_) => CoreFetch::Unreachable,
        CoreFetch::NotFound => CoreFetch::NotFound,
        CoreFetch::Unreachable => CoreFetch::Unreachable,
    }
}

fn parse_address_transaction(row: &Value) -> Option<AddressTransaction> {
    let txid = row
        .get("tx_hash")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    Some(AddressTransaction {
        txid,
        block_index: json_u128(row.get("block_index")) as u32,
        transaction_type: row
            .get("transaction_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        raw: row.clone(),
    })
}

fn aggregate_address_balances(rows: &[Value]) -> Vec<AddressAssetBalance> {
    let mut by_asset: HashMap<String, AddressAssetBalance> = HashMap::new();
    for row in rows {
        let Some(asset) = row
            .get("asset")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if asset.eq_ignore_ascii_case("BTC") {
            continue;
        }
        let quantity = json_u128(row.get("quantity"));
        if quantity == 0 {
            continue;
        }
        let divisible = row
            .get("asset_info")
            .and_then(|info| info.get("divisible"))
            .and_then(|v| v.as_bool())
            .unwrap_or(asset.eq_ignore_ascii_case("XCP"));
        let longname = longname_from_row(row);
        let description = description_from_row(row);
        let entry = by_asset.entry(asset.to_string()).or_insert_with(|| AddressAssetBalance {
            asset: asset.to_string(),
            longname: longname.clone(),
            quantity: 0,
            quantity_normalized: String::new(),
            divisible,
            description: description.clone(),
        });
        entry.quantity = entry.quantity.saturating_add(quantity);
        entry.divisible = entry.divisible || divisible;
        if entry.longname.is_none() {
            entry.longname = longname;
        }
        if entry.description.is_none() {
            entry.description = description;
        }
    }

    let mut items: Vec<AddressAssetBalance> = by_asset
        .into_values()
        .map(|mut item| {
            item.quantity_normalized = format_normalized(item.quantity, item.divisible);
            item
        })
        .collect();
    items.sort_by(|a, b| {
        let a_xcp = a.asset.eq_ignore_ascii_case("XCP");
        let b_xcp = b.asset.eq_ignore_ascii_case("XCP");
        b_xcp
            .cmp(&a_xcp)
            .then_with(|| b.quantity.cmp(&a.quantity))
            .then_with(|| a.asset.cmp(&b.asset))
    });
    items
}

fn description_from_row(row: &Value) -> Option<String> {
    row.get("description")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            row.get("asset_info")
                .and_then(|info| info.get("description"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
}

fn longname_from_row(row: &Value) -> Option<String> {
    row.get("asset_longname")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            row.get("asset_info")
                .and_then(|info| info.get("asset_longname"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
}

fn format_normalized(quantity: u128, divisible: bool) -> String {
    if !divisible {
        return quantity.to_string();
    }
    let whole = quantity / 100_000_000;
    let frac = quantity % 100_000_000;
    format!("{whole}.{frac:08}")
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

fn list_from_body(body: &Value) -> CoreList {
    let items = body.get("result").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let total =
        body.get("result_count").and_then(|v| v.as_u64()).unwrap_or(items.len() as u64) as usize;
    CoreList { items, total }
}

pub fn looks_like_asset_query(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    let upper = trimmed.to_ascii_uppercase();
    if upper == "XCP" || upper == "BTC" {
        return true;
    }
    if upper.contains('.') {
        return upper.split('.').all(|part| is_named_asset(part) || is_numeric_asset(part));
    }
    is_named_asset(&upper) || is_numeric_asset(&upper)
}

pub fn asset_name_from_value(value: &Value) -> Option<String> {
    value
        .get("asset")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub fn asset_id_to_name(id: u128) -> String {
    if id == 0 {
        return "BTC".to_string();
    }
    if id == 1 {
        return "XCP".to_string();
    }
    if id > MAX_NAMED_ASSET_ID {
        return format!("A{id}");
    }
    let mut n = id;
    let mut chars = Vec::new();
    while n > 0 {
        let rem = (n % 26) as u8;
        chars.push(char::from(b'A' + rem));
        n /= 26;
    }
    chars.reverse();
    if chars.is_empty() { "A".to_string() } else { chars.into_iter().collect() }
}

pub fn asset_name_to_id(name: &str) -> Option<u128> {
    let name = name.trim().to_ascii_uppercase();
    if name == "BTC" {
        return Some(0);
    }
    if name == "XCP" {
        return Some(1);
    }
    if let Some(digits) = name.strip_prefix('A') {
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            return digits.parse().ok();
        }
    }
    if !is_named_asset(&name) {
        return None;
    }
    let mut id = 0u128;
    for c in name.chars() {
        id = id.saturating_mul(26).saturating_add(u128::from(c as u8 - b'A'));
    }
    Some(id)
}

fn asset_lookup_candidates(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    push_unique(&mut out, raw.to_string());
    let upper = raw.to_ascii_uppercase();
    push_unique(&mut out, upper.clone());
    if raw.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(id) = raw.parse::<u128>() {
            push_unique(&mut out, asset_id_to_name(id));
            push_unique(&mut out, format!("A{id}"));
        }
    }
    if let Some(digits) = upper.strip_prefix('A') {
        if digits.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(id) = digits.parse::<u128>() {
                let decoded = asset_id_to_name(id);
                if !decoded.starts_with('A') {
                    push_unique(&mut out, decoded);
                }
            }
        }
    }
    out
}

fn is_named_asset(name: &str) -> bool {
    let len = name.chars().count();
    (4..=12).contains(&len) && name.chars().all(|c| c.is_ascii_uppercase())
}

fn is_numeric_asset(name: &str) -> bool {
    name.starts_with('A') && name.len() > 1 && name[1..].chars().all(|c| c.is_ascii_digit())
}

fn push_unique(out: &mut Vec<String>, value: String) {
    if !value.is_empty() && !out.iter().any(|existing| existing == &value) {
        out.push(value);
    }
}

pub(crate) fn encode_path(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn get_result(cfg: &XcpConfig, path: &str) -> CoreFetch<Value> {
    match get_body(cfg, path) {
        CoreFetch::Ok(body) => match body.get("result") {
            Some(result) if !result.is_null() => CoreFetch::Ok(result.clone()),
            _ => CoreFetch::NotFound,
        },
        other => other,
    }
}

fn get_body(cfg: &XcpConfig, path: &str) -> CoreFetch<Value> {
    let url = format!("{}{path}", cfg.counterparty_api_url);
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_millis(cfg.timeout_ms)).build();
    let response = match agent.get(&url).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(code, _)) if code == 404 => return CoreFetch::NotFound,
        Err(e) => {
            eprintln!("[xcp] core fetch failed for {path}: {e}");
            return CoreFetch::Unreachable;
        }
    };
    match response.into_json() {
        Ok(body) => CoreFetch::Ok(body),
        Err(e) => {
            eprintln!("[xcp] core json failed for {path}: {e}");
            CoreFetch::Unreachable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_asset_id_roundtrip() {
        assert_eq!(asset_name_to_id("PEPECASH"), Some(121892899915));
        assert_eq!(asset_id_to_name(121892899915), "PEPECASH");
        assert_eq!(asset_id_to_name(1), "XCP");
        assert_eq!(asset_id_to_name(95428956661682177), "A95428956661682177");
    }

    #[test]
    fn lookup_candidates_decode_numeric_named_id() {
        let candidates = asset_lookup_candidates("121892899915");
        assert!(candidates.iter().any(|c| c == "PEPECASH"));
        assert!(candidates.iter().any(|c| c == "A121892899915"));
    }

    #[test]
    fn looks_like_named_and_numeric_assets() {
        assert!(looks_like_asset_query("XCP"));
        assert!(looks_like_asset_query("pepecash"));
        assert!(looks_like_asset_query("A95428956661682177"));
        assert!(!looks_like_asset_query("964678"));
        assert!(!looks_like_asset_query("abc"));
    }

    #[test]
    fn address_balances_sum_utxos_and_put_xcp_first() {
        let rows = vec![
            json!({
                "asset": "PEPECASH",
                "quantity": 50,
                "asset_info": { "divisible": false }
            }),
            json!({
                "asset": "XCP",
                "quantity": 11200000000u64,
                "asset_info": { "divisible": true }
            }),
            json!({
                "asset": "XCP",
                "quantity": 120000000000u64,
                "asset_info": { "divisible": true }
            }),
            json!({
                "asset": "BTC",
                "quantity": 1000
            }),
            json!({
                "asset": "DUST",
                "quantity": 0
            }),
        ];
        let items = aggregate_address_balances(&rows);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].asset, "XCP");
        assert_eq!(items[0].quantity, 131200000000);
        assert_eq!(items[0].quantity_normalized, "1312.00000000");
        assert_eq!(items[1].asset, "PEPECASH");
        assert_eq!(items[1].quantity_normalized, "50");
    }

    #[test]
    fn address_transaction_reads_hash_and_height() {
        let row = json!({
            "tx_hash": "b44ef1626f37e7427f2f19d85d733267a26950e5d24c836a02a87454c60ef8a2",
            "block_index": 965405,
            "transaction_type": "order"
        });
        let parsed = parse_address_transaction(&row).expect("tx");
        assert_eq!(parsed.txid, "b44ef1626f37e7427f2f19d85d733267a26950e5d24c836a02a87454c60ef8a2");
        assert_eq!(parsed.block_index, 965405);
        assert_eq!(parsed.transaction_type, "order");
    }
}
