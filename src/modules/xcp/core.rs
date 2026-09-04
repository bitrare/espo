use super::config::XcpConfig;
use serde_json::Value;
use std::time::Duration;

/// Named Counterparty assets encode as base-26 (A=0). IDs above 26^12 are
/// numeric and displayed as `A{id}`.
const MAX_NAMED_ASSET_ID: u128 = 26u128.pow(12);

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

pub fn fetch_asset_list(asset: &str, suffix: &str, extra: &str, offset: usize, limit: usize) -> CoreFetch<CoreList> {
    let Some(cfg) = XcpConfig::from_global() else {
        return CoreFetch::Unreachable;
    };
    fetch_asset_list_with(&cfg, asset, suffix, extra, offset, limit)
}

pub fn fetch_path_list(path: &str, extra: &str, offset: usize, limit: usize) -> CoreFetch<CoreList> {
    let Some(cfg) = XcpConfig::from_global() else {
        return CoreFetch::Unreachable;
    };
    let mut url = format!("{path}?verbose=true&limit={limit}&offset={offset}");
    if !extra.is_empty() {
        url.push('&');
        url.push_str(extra);
    }
    match get_body(&cfg, &url) {
        CoreFetch::Ok(body) => CoreFetch::Ok(list_from_body(&body)),
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
        CoreFetch::Ok(body) => CoreFetch::Ok(list_from_body(&body)),
        CoreFetch::NotFound => CoreFetch::NotFound,
        CoreFetch::Unreachable => CoreFetch::Unreachable,
    }
}

fn list_from_body(body: &Value) -> CoreList {
    let items = body
        .get("result")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let total = body
        .get("result_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(items.len() as u64) as usize;
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

fn encode_path(raw: &str) -> String {
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
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(cfg.timeout_ms))
        .build();
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
}
