use anyhow::{Result, anyhow};
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct FairmintsConfig {
    pub coingecko: Option<CoinGeckoConfig>,
}

#[derive(Clone, Debug)]
pub struct CoinGeckoConfig {
    pub api_url: String,
    pub api_key: String,
    pub timeout_secs: u64,
}

impl FairmintsConfig {
    pub fn spec() -> &'static str {
        r#"{ "coingecko": { "api_url": "https://pro-api.coingecko.com", "api_key": "YOUR_KEY", "timeout_secs": 10 } }"#
    }

    pub fn from_value(value: &Value) -> Result<Self> {
        let obj = value
            .as_object()
            .ok_or_else(|| anyhow!("fairmints config must be an object; expected: {}", Self::spec()))?;

        let coingecko = match obj.get("coingecko") {
            None | Some(Value::Null) => None,
            Some(raw) => {
                let cg = raw.as_object().ok_or_else(|| {
                    anyhow!("fairmints.coingecko must be an object; expected: {}", Self::spec())
                })?;
                let api_url = cg
                    .get("api_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("https://pro-api.coingecko.com")
                    .to_string();
                let api_key = cg
                    .get("api_key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("fairmints.coingecko.api_key is required"))?
                    .to_string();
                let timeout_secs = cg.get("timeout_secs").and_then(|v| v.as_u64()).unwrap_or(10);
                Some(CoinGeckoConfig { api_url, api_key, timeout_secs })
            }
        };

        Ok(Self { coingecko })
    }
}
