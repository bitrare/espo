use super::config::CoinGeckoConfig;
use crate::modules::ammdata::consts::PRICE_SCALE_DECIMALS;
use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::time::Duration;
use time::{OffsetDateTime, UtcOffset};

#[derive(Clone)]
pub struct CoinGeckoPriceFeed {
    config: CoinGeckoConfig,
    client: reqwest::blocking::Client,
}

#[derive(Deserialize)]
struct HistoricalPriceResponse {
    market_data: Option<MarketData>,
}

#[derive(Deserialize)]
struct MarketData {
    current_price: Option<CurrentPrice>,
}

#[derive(Deserialize)]
struct CurrentPrice {
    usd: Option<f64>,
}

#[derive(Deserialize)]
struct SimplePriceResponse {
    bitcoin: Option<BitcoinPrice>,
}

#[derive(Deserialize)]
struct BitcoinPrice {
    usd: Option<f64>,
}

impl CoinGeckoPriceFeed {
    pub fn new(config: CoinGeckoConfig) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .context("build CoinGecko reqwest client")?;
        Ok(Self { config, client })
    }

    pub fn price_at_timestamp(&self, timestamp: i64) -> Result<f64> {
        let dt = OffsetDateTime::from_unix_timestamp(timestamp)
            .map_err(|_| anyhow!("invalid timestamp {timestamp}"))?
            .to_offset(UtcOffset::UTC);
        let now = OffsetDateTime::now_utc();
        if dt.date() >= now.date() {
            return self.current_price();
        }
        let formatted_date = format!("{:02}-{:02}-{:04}", dt.day(), dt.month() as u8, dt.year());
        let url = format!(
            "{}/api/v3/coins/bitcoin/history?date={formatted_date}&localization=false",
            self.config.api_url.trim_end_matches('/')
        );
        let resp = self
            .client
            .get(&url)
            .header("x-cg-pro-api-key", &self.config.api_key)
            .send()
            .with_context(|| {
                format!("CoinGecko historical price request failed for {formatted_date}")
            })?;
        let resp = resp.error_for_status().context("CoinGecko API error")?;
        let parsed: HistoricalPriceResponse =
            resp.json().context("failed to parse CoinGecko response")?;
        parsed
            .market_data
            .and_then(|m| m.current_price)
            .and_then(|p| p.usd)
            .ok_or_else(|| anyhow!("no price data in CoinGecko response for {formatted_date}"))
    }

    pub fn current_price(&self) -> Result<f64> {
        let url = format!(
            "{}/api/v3/simple/price?ids=bitcoin&vs_currencies=usd",
            self.config.api_url.trim_end_matches('/')
        );
        let resp = self
            .client
            .get(&url)
            .header("x-cg-pro-api-key", &self.config.api_key)
            .send()
            .context("CoinGecko current price request failed")?;
        let resp = resp.error_for_status().context("CoinGecko API error")?;
        let parsed: SimplePriceResponse =
            resp.json().context("failed to parse CoinGecko response")?;
        parsed
            .bitcoin
            .and_then(|b| b.usd)
            .ok_or_else(|| anyhow!("no price in CoinGecko response"))
    }

    /// Same 1e16 scale as ammdata CoinGecko / backend `getBtcPrice`.
    pub fn price_scaled_at_timestamp(&self, timestamp: i64) -> Result<u128> {
        let usd = self.price_at_timestamp(timestamp)?;
        let scale = 10u128.pow(PRICE_SCALE_DECIMALS);
        Ok((usd * scale as f64).round() as u128)
    }
}
