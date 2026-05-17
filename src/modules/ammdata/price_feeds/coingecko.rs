use super::defs::PriceFeed;
use crate::config::get_bitcoind_rpc_client;
use crate::modules::ammdata::config::{AmmDataConfig, CoinGeckoConfig};
use crate::modules::ammdata::consts::PRICE_SCALE_DECIMALS;
use anyhow::{Context, Result, anyhow};
use bitcoincore_rpc::RpcApi;
use reqwest::blocking::Client;
use serde::Deserialize;
use std::time::Duration;
use time::{OffsetDateTime, UtcOffset};

#[derive(Clone)]
pub struct CoinGeckoPriceFeed {
    config: CoinGeckoConfig,
    client: Client,
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
    pub fn new(config: CoinGeckoConfig) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .expect("build reqwest client");
        Self { config, client }
    }

    pub fn from_global_config() -> Result<Option<Self>> {
        let cfg = AmmDataConfig::load_from_global_config()?;
        Ok(cfg.coingecko.map(Self::new))
    }

    fn get_block_timestamp(&self, height: u64) -> Result<i64> {
        let rpc = get_bitcoind_rpc_client();
        let hash = rpc
            .get_block_hash(height)
            .with_context(|| format!("failed to get block hash for height {height}"))?;
        let header = rpc
            .get_block_header(&hash)
            .with_context(|| format!("failed to get block header for hash {hash}"))?;
        Ok(header.time as i64)
    }

    fn get_historical_price(&self, timestamp: i64) -> Result<f64> {
        let dt = OffsetDateTime::from_unix_timestamp(timestamp)
            .map_err(|_| anyhow!("invalid timestamp {timestamp}"))?
            .to_offset(UtcOffset::UTC);
        let now = OffsetDateTime::now_utc();
        
        // If date is today or in the future, get current price
        if dt.date() >= now.date() {
            return self.get_current_price();
        }
        
        // CoinGecko expects DD-MM-YYYY format
        let formatted_date = format!(
            "{:02}-{:02}-{}",
            dt.day(),
            dt.month() as u8,
            dt.year()
        );
        
        let url = format!(
            "{}/api/v3/coins/bitcoin/history",
            self.config.api_url.trim_end_matches('/')
        );
        
        let resp = self
            .client
            .get(&url)
            .query(&[("date", &formatted_date), ("localization", &"false".to_string())])
            .header("x-cg-pro-api-key", &self.config.api_key)
            .send()
            .with_context(|| format!("CoinGecko historical price request failed for {formatted_date}"))?;
        
        if resp.status() == 429 {
            // Rate limited - wait and retry once
            std::thread::sleep(Duration::from_millis(200));
            let retry_resp = self
                .client
                .get(&url)
                .query(&[("date", &formatted_date), ("localization", &"false".to_string())])
                .header("x-cg-pro-api-key", &self.config.api_key)
                .send()
                .with_context(|| "CoinGecko retry request failed")?;
            
            let data: HistoricalPriceResponse = retry_resp
                .json()
                .with_context(|| "failed to parse CoinGecko retry response")?;
            
            return data
                .market_data
                .and_then(|m| m.current_price)
                .and_then(|c| c.usd)
                .ok_or_else(|| anyhow!("no price data in CoinGecko retry response"));
        }
        
        let resp = resp.error_for_status().with_context(|| "CoinGecko API error")?;
        
        let data: HistoricalPriceResponse = resp
            .json()
            .with_context(|| "failed to parse CoinGecko response")?;
        
        data.market_data
            .and_then(|m| m.current_price)
            .and_then(|c| c.usd)
            .ok_or_else(|| anyhow!("no price data in CoinGecko response for {formatted_date}"))
    }

    fn get_current_price(&self) -> Result<f64> {
        let url = format!(
            "{}/api/v3/simple/price",
            self.config.api_url.trim_end_matches('/')
        );
        
        let resp = self
            .client
            .get(&url)
            .query(&[("ids", "bitcoin"), ("vs_currencies", "usd")])
            .header("x-cg-pro-api-key", &self.config.api_key)
            .send()
            .with_context(|| "CoinGecko current price request failed")?;
        
        if resp.status() == 429 {
            std::thread::sleep(Duration::from_millis(200));
            let retry_resp = self
                .client
                .get(&url)
                .query(&[("ids", "bitcoin"), ("vs_currencies", "usd")])
                .header("x-cg-pro-api-key", &self.config.api_key)
                .send()
                .with_context(|| "CoinGecko retry request failed")?;
            
            let data: SimplePriceResponse = retry_resp
                .json()
                .with_context(|| "failed to parse CoinGecko retry response")?;
            
            return data
                .bitcoin
                .and_then(|b| b.usd)
                .ok_or_else(|| anyhow!("no price in CoinGecko retry response"));
        }
        
        let resp = resp.error_for_status().with_context(|| "CoinGecko API error")?;
        
        let data: SimplePriceResponse = resp
            .json()
            .with_context(|| "failed to parse CoinGecko response")?;
        
        data.bitcoin
            .and_then(|b| b.usd)
            .ok_or_else(|| anyhow!("no price in CoinGecko response"))
    }

    fn price_to_scaled(&self, price: f64) -> u128 {
        let scale = 10u128.pow(PRICE_SCALE_DECIMALS);
        (price * scale as f64) as u128
    }
}

impl PriceFeed for CoinGeckoPriceFeed {
    fn get_bitcoin_price_usd_at_block_height(&self, height: u64) -> Result<u128> {
        let timestamp = self.get_block_timestamp(height)?;
        let price = self.get_historical_price(timestamp)?;
        Ok(self.price_to_scaled(price))
    }
}
