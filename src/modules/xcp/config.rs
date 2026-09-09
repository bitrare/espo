use crate::config::get_module_config;
use anyhow::{Result, anyhow};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct XcpConfig {
    pub counterparty_api_url: String,
    pub timeout_ms: u64,
}

impl XcpConfig {
    pub fn spec() -> &'static str {
        r#"{ "counterparty_api_url": "http://127.0.0.1:4100/v2", "timeout_ms": 4000 }"#
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let obj = value
            .as_object()
            .ok_or_else(|| anyhow!("xcp config must be an object; expected: {}", Self::spec()))?;

        let counterparty_api_url = obj
            .get("counterparty_api_url")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!("xcp.counterparty_api_url missing; expected: {}", Self::spec())
            })?;

        let timeout_ms = obj.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(4000).max(200);

        Ok(Self { counterparty_api_url, timeout_ms })
    }

    pub fn from_global() -> Option<Self> {
        get_module_config("xcp").and_then(|value| Self::from_value(value).ok())
    }

    pub fn enabled() -> bool {
        Self::from_global().is_some()
    }
}
