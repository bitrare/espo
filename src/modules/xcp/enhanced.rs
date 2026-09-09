use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::core::{CoreFetch, asset_name_from_value, fetch_asset};

const FETCH_TIMEOUT_MS: u64 = 2500;
const CACHE_TTL: Duration = Duration::from_secs(30 * 60);
const XCP_CDN_ICON: &str = "https://cdn.xcp.io/img/icon";
const USER_AGENT: &str = "Mozilla/5.0 (compatible; EspoExplorer/1.0)";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnhancedAsset {
    pub asset: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub website: Option<String>,
    pub icon_url: Option<String>,
    pub image_url: Option<String>,
    pub video_url: Option<String>,
    pub json_url: Option<String>,
}

impl EnhancedAsset {
    pub fn icon(&self) -> Option<&str> {
        self.icon_url.as_deref().or(self.image_url.as_deref())
    }

    pub fn display_name<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.name.as_deref().filter(|name| !name.is_empty()).unwrap_or(fallback)
    }
}

#[derive(Clone)]
struct CacheEntry {
    value: Option<EnhancedAsset>,
    fetched_at: Instant,
}

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn json_info_url(description: &str) -> Option<String> {
    let trimmed = description.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.contains("://") {
        return None;
    } else {
        format!("http://{trimmed}")
    };
    let path = with_scheme.split(['?', '#']).next().unwrap_or(&with_scheme);
    if path.to_ascii_lowercase().ends_with(".json") { Some(with_scheme) } else { None }
}

pub fn for_description(description: Option<&str>) -> Option<EnhancedAsset> {
    let description = description.map(str::trim).filter(|s| !s.is_empty())?;
    if let Some(url) = json_info_url(description) {
        return fetch_json(&url);
    }
    None
}

/// Fetch enhanced info from a known description and remember it under `asset`.
pub fn warm(asset: &str, description: Option<&str>) -> Option<EnhancedAsset> {
    let fetched = with_cdn_fallback(asset, for_description(description));
    if description.is_some() {
        remember_asset(asset, fetched.clone());
    }
    fetched
}

/// Cache-only lookup. Does not hit the network or Counterparty Core.
pub fn cached(asset: &str) -> Option<EnhancedAsset> {
    let asset = asset.trim();
    if asset.is_empty() {
        return None;
    }
    cache_get(&format!("asset:{}", asset.to_ascii_uppercase())).flatten()
}

pub fn for_asset(asset: &str) -> Option<EnhancedAsset> {
    let asset = asset.trim();
    if asset.is_empty() {
        return None;
    }
    if let Some(hit) = cached(asset) {
        return Some(hit);
    }
    if cache_get(&format!("asset:{}", asset.to_ascii_uppercase())).is_some() {
        return None;
    }
    let fetched = match fetch_asset(asset) {
        CoreFetch::Ok(value) => {
            let desc = value
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let mut enhanced = for_description(desc);
            if let Some(info) = enhanced.as_mut() {
                if info.asset.is_none() {
                    info.asset = asset_name_from_value(&value).or_else(|| Some(asset.to_string()));
                }
            }
            with_cdn_fallback(asset, enhanced)
        }
        _ => with_cdn_fallback(asset, None),
    };
    remember_asset(asset, fetched.clone());
    fetched
}

pub fn cdn_icon_url(asset: &str) -> Option<String> {
    let asset = asset.trim().to_ascii_uppercase();
    if asset.is_empty() {
        return None;
    }
    Some(format!("{XCP_CDN_ICON}/{asset}"))
}

fn with_cdn_fallback(asset: &str, info: Option<EnhancedAsset>) -> Option<EnhancedAsset> {
    if info.as_ref().and_then(EnhancedAsset::icon).is_some() {
        return info;
    }
    let Some(url) = cdn_icon_url(asset) else {
        return info;
    };
    if !cdn_icon_is_real(asset) {
        return info;
    }
    match info {
        Some(mut existing) => {
            existing.icon_url = Some(url);
            Some(existing)
        }
        None => Some(EnhancedAsset {
            asset: Some(asset.trim().to_ascii_uppercase()),
            icon_url: Some(url),
            ..Default::default()
        }),
    }
}

fn cdn_icon_is_real(asset: &str) -> bool {
    let Some(url) = cdn_icon_url(asset) else {
        return false;
    };
    let Some((_, body)) = fetch_bytes(&url) else {
        return false;
    };
    !is_default_xcp_placeholder(&body) || asset.eq_ignore_ascii_case("XCP")
}

fn is_default_xcp_placeholder(body: &[u8]) -> bool {
    let Some(default) = default_xcp_cdn_icon() else {
        return false;
    };
    body == default
}

fn default_xcp_cdn_icon() -> Option<&'static [u8]> {
    static BYTES: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    BYTES
        .get_or_init(|| fetch_bytes(&format!("{XCP_CDN_ICON}/XCP")).map(|(_, body)| body))
        .as_deref()
}

fn fetch_json(url: &str) -> Option<EnhancedAsset> {
    let key = format!("json:{url}");
    if let Some(hit) = cache_get(&key) {
        return hit;
    }
    let fetched = match get_json(url) {
        Some(body) => parse_enhanced(&body, url),
        None => None,
    };
    cache_set(key, fetched.clone());
    if let Some(ref info) = fetched {
        if let Some(ref asset) = info.asset {
            remember_asset(asset, fetched.clone());
        }
    }
    fetched
}

fn remember_asset(asset: &str, value: Option<EnhancedAsset>) {
    let asset = asset.trim();
    if asset.is_empty() {
        return;
    }
    cache_set(format!("asset:{}", asset.to_ascii_uppercase()), value);
}

pub fn icon_bytes(asset: &str) -> Option<(String, Vec<u8>)> {
    let mut urls = Vec::new();
    if let Some(url) = for_asset(asset).and_then(|info| info.icon().map(|s| s.to_string())) {
        urls.push(url);
    }
    if let Some(cdn) = cdn_icon_url(asset) {
        if !urls.iter().any(|url| url == &cdn) {
            urls.push(cdn);
        }
    }
    for url in urls {
        let Some((content_type, body)) = fetch_bytes(&url) else {
            continue;
        };
        if is_default_xcp_placeholder(&body) && !asset.eq_ignore_ascii_case("XCP") {
            continue;
        }
        return Some((content_type, body));
    }
    None
}

fn fetch_bytes(url: &str) -> Option<(String, Vec<u8>)> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(FETCH_TIMEOUT_MS))
        .build();
    let response = match agent.get(url).set("User-Agent", USER_AGENT).call() {
        Ok(response) => response,
        Err(e) => {
            eprintln!("[xcp] icon fetch failed for {url}: {e}");
            return None;
        }
    };
    let content_type = response
        .header("Content-Type")
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or("application/octet-stream")
        .trim()
        .to_string();
    let mut body = Vec::new();
    if response.into_reader().read_to_end(&mut body).is_err() || body.is_empty() {
        return None;
    }
    Some((content_type, body))
}

fn get_json(url: &str) -> Option<Value> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(FETCH_TIMEOUT_MS))
        .build();
    let response = match agent.get(url).set("User-Agent", USER_AGENT).call() {
        Ok(response) => response,
        Err(e) => {
            eprintln!("[xcp] enhanced json fetch failed for {url}: {e}");
            return None;
        }
    };
    match response.into_json() {
        Ok(body) => Some(body),
        Err(e) => {
            eprintln!("[xcp] enhanced json parse failed for {url}: {e}");
            None
        }
    }
}

fn parse_enhanced(body: &Value, json_url: &str) -> Option<EnhancedAsset> {
    if !body.is_object() {
        return None;
    }
    let mut info = EnhancedAsset {
        asset: string_field(body, "asset"),
        name: string_field(body, "name"),
        description: string_field(body, "description"),
        website: http_url_field(body, "website"),
        icon_url: http_url_field(body, "image"),
        image_url: http_url_field(body, "image"),
        video_url: http_url_field(body, "video"),
        json_url: Some(json_url.to_string()),
    };

    if let Some(images) = body.get("images").and_then(|v| v.as_array()) {
        for image in images {
            let url = image
                .get("data")
                .and_then(|v| v.as_str())
                .or_else(|| image.get("url").and_then(|v| v.as_str()))
                .and_then(http_url);
            let Some(url) = url else { continue };
            let kind =
                image.get("type").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
            if kind.contains("video") || looks_like_video(&url) {
                if info.video_url.is_none() {
                    info.video_url = Some(url);
                }
            } else if kind.contains("icon") {
                info.icon_url = Some(url);
            } else if kind.contains("standard") || kind.contains("image") || kind.is_empty() {
                info.image_url = Some(url);
            }
        }
    }

    if info.icon_url.is_none() {
        info.icon_url = info.image_url.clone();
    }
    if info
        .icon_url
        .as_ref()
        .or(info.image_url.as_ref())
        .or(info.video_url.as_ref())
        .or(info.description.as_ref())
        .or(info.website.as_ref())
        .is_none()
    {
        return None;
    }
    Some(info)
}

fn cache_get(key: &str) -> Option<Option<EnhancedAsset>> {
    let Ok(guard) = cache().lock() else {
        return None;
    };
    guard.get(key).and_then(|entry| {
        if entry.fetched_at.elapsed() <= CACHE_TTL { Some(entry.value.clone()) } else { None }
    })
}

fn cache_set(key: String, value: Option<EnhancedAsset>) {
    if let Ok(mut guard) = cache().lock() {
        guard.insert(key, CacheEntry { value, fetched_at: Instant::now() });
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

fn http_url_field(value: &Value, key: &str) -> Option<String> {
    string_field(value, key).and_then(|s| http_url(&s))
}

fn http_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn looks_like_video(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    path.ends_with(".mp4")
        || path.ends_with(".webm")
        || path.ends_with(".ogg")
        || path.ends_with(".mov")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_json_info_urls() {
        assert_eq!(
            json_info_url("https://xcp.fun/MSGA.json"),
            Some("https://xcp.fun/MSGA.json".to_string())
        );
        assert_eq!(
            json_info_url("xcp.fun/MSGA.json"),
            Some("http://xcp.fun/MSGA.json".to_string())
        );
        assert!(json_info_url("The Counterparty protocol native currency").is_none());
        assert!(json_info_url("https://example.com/photo.png").is_none());
    }

    #[test]
    fn parses_msga_style_json() {
        let body = json!({
            "asset": "MSGA",
            "name": "MSGA",
            "description": "Make Spam Great Again",
            "website": "https://xcp.fun/MSGA",
            "image": "https://xcp.fun/full/MSGA",
            "images": [
                {"type": "icon", "size": "48x48", "data": "https://xcp.fun/icon/MSGA"},
                {"type": "standard", "data": "https://xcp.fun/full/MSGA"}
            ]
        });
        let parsed = parse_enhanced(&body, "https://xcp.fun/MSGA.json").expect("json");
        assert_eq!(parsed.icon_url.as_deref(), Some("https://xcp.fun/icon/MSGA"));
        assert_eq!(parsed.image_url.as_deref(), Some("https://xcp.fun/full/MSGA"));
        assert_eq!(parsed.description.as_deref(), Some("Make Spam Great Again"));
        assert_eq!(parsed.website.as_deref(), Some("https://xcp.fun/MSGA"));
    }

    #[test]
    fn cdn_icon_uses_xcp_io() {
        assert_eq!(cdn_icon_url("fairest").as_deref(), Some("https://cdn.xcp.io/img/icon/FAIREST"));
        assert_eq!(
            cdn_icon_url("FAIRLADY").as_deref(),
            Some("https://cdn.xcp.io/img/icon/FAIRLADY")
        );
        assert!(cdn_icon_url("").is_none());
    }
}
