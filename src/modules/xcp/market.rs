use super::core::{self, CoreFetch, CoreList};
use super::enhanced;
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const QUOTE_ASSETS: [&str; 2] = ["XCP", "BTC"];

#[derive(Clone, Debug)]
pub struct MarketSpot {
    pub quote_asset: String,
    pub price: f64,
    pub reserve_base: String,
    pub reserve_quote: String,
    pub block_index: u32,
    pub tx_hash: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MarketTrade {
    pub venue: &'static str,
    pub side: &'static str,
    pub quote_asset: String,
    pub price: f64,
    pub base_amount: String,
    pub quote_amount: String,
    pub block_index: u32,
    pub block_time: Option<u64>,
    pub tx_hash: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AssetMarket {
    pub asset: String,
    pub quote_asset: String,
    pub spot: Option<MarketSpot>,
    pub last_trade: Option<MarketTrade>,
    pub pool_matches: usize,
    pub dex_matches: usize,
    pub dispenses: usize,
}

impl AssetMarket {
    pub fn price(&self) -> Option<(f64, &str)> {
        if let Some(trade) = &self.last_trade {
            return Some((trade.price, trade.quote_asset.as_str()));
        }
        self.spot.as_ref().map(|spot| (spot.price, spot.quote_asset.as_str()))
    }

    pub fn to_api(&self) -> Value {
        let (price, quote) = match self.price() {
            Some((price, quote)) => (json!(format_price(price)), json!(quote)),
            None => (Value::Null, json!(self.quote_asset)),
        };
        let xcp_btc = xcp_btc_price();
        let price_btc = self.price().and_then(|(price, quote)| quote_to_btc(price, quote, xcp_btc));
        let icon =
            enhanced::for_asset(&self.asset).and_then(|info| info.icon().map(|s| s.to_string()));
        json!({
            "ok": true,
            "asset": self.asset,
            "quote_asset": quote,
            "price": price,
            "price_btc": price_btc.map(format_price),
            "price_sats": price_btc.map(|btc| format_price(btc * 100_000_000.0)),
            "xcp_btc": xcp_btc.map(format_price),
            "icon": icon,
            "spot": self.spot.as_ref().map(|spot| json!({
                "quote_asset": spot.quote_asset,
                "price": format_price(spot.price),
                "reserve_base": spot.reserve_base,
                "reserve_quote": spot.reserve_quote,
                "block_index": spot.block_index,
                "tx_hash": spot.tx_hash,
            })),
            "last_trade": self.last_trade.as_ref().map(|trade| json!({
                "venue": trade.venue,
                "side": trade.side,
                "quote_asset": trade.quote_asset,
                "price": format_price(trade.price),
                "base_amount": trade.base_amount,
                "quote_amount": trade.quote_amount,
                "block_index": trade.block_index,
                "block_time": trade.block_time,
                "tx_hash": trade.tx_hash,
            })),
            "venues": {
                "pool_matches": self.pool_matches,
                "dex_matches": self.dex_matches,
                "dispenses": self.dispenses,
            }
        })
    }
}

fn quote_to_btc(price: f64, quote: &str, xcp_btc: Option<f64>) -> Option<f64> {
    if !price.is_finite() || price <= 0.0 {
        return None;
    }
    if quote.eq_ignore_ascii_case("BTC") {
        return Some(price);
    }
    if quote.eq_ignore_ascii_case("XCP") {
        return xcp_btc.filter(|rate| rate.is_finite() && *rate > 0.0).map(|rate| price * rate);
    }
    None
}

fn xcp_btc_price() -> Option<f64> {
    const TTL: Duration = Duration::from_secs(5 * 60);
    struct Cache {
        value: Option<f64>,
        fetched_at: Instant,
    }
    fn cache() -> &'static Mutex<Option<Cache>> {
        static CACHE: OnceLock<Mutex<Option<Cache>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(None))
    }
    if let Ok(guard) = cache().lock() {
        if let Some(entry) = guard.as_ref() {
            if entry.fetched_at.elapsed() <= TTL {
                return entry.value;
            }
        }
    }
    let value = match core::fetch_path_list("/assets/XCP/dispenses", "sort=block_index:DESC", 0, 1)
    {
        CoreFetch::Ok(list) => list.items.first().and_then(xcp_btc_from_dispense),
        _ => None,
    };
    if let Ok(mut guard) = cache().lock() {
        *guard = Some(Cache { value, fetched_at: Instant::now() });
    }
    value
}

fn xcp_btc_from_dispense(row: &Value) -> Option<f64> {
    let btc = json_f64(row.get("btc_amount_normalized"))?;
    let xcp = json_f64(row.get("dispense_quantity_normalized"))?;
    if xcp <= 0.0 {
        return None;
    }
    Some(btc / xcp)
}

/// Last-activity price for a Counterparty asset.
///
/// Prefers the XCP AMM pool (spot + last pool fill), then a recent DEX match
/// quoted in XCP/BTC, then the last BTC dispense. Unrelated asset-to-asset
/// fills are ignored so a PEPECASH↔meme swap does not become the bag price.
pub fn fetch_asset_market(asset: &str) -> CoreFetch<AssetMarket> {
    let asset = asset.trim().to_ascii_uppercase();
    if asset.is_empty() {
        return CoreFetch::NotFound;
    }

    let pool = fetch_xcp_pool(&asset);
    let pool_list = if pool.is_some() {
        core::fetch_path_list(&format!("/pools/{asset}/XCP/matches"), "", 0, 1)
    } else {
        CoreFetch::NotFound
    };
    let dex_list =
        core::fetch_asset_list(&asset, "matches", "status=completed&sort=block_index:DESC", 0, 200);
    let dispense_list = core::fetch_asset_list(&asset, "dispenses", "sort=block_index:DESC", 0, 1);

    if matches!(
        (&pool_list, &dex_list, &dispense_list),
        (CoreFetch::Unreachable, _, _)
            | (_, CoreFetch::Unreachable, _)
            | (_, _, CoreFetch::Unreachable)
    ) && pool.is_none()
    {
        return CoreFetch::Unreachable;
    }

    let (pool_match, pool_matches) = first_item(pool_list);
    let (dex_rows, dex_matches) = items(dex_list);
    let (dispense, dispenses) = first_item(dispense_list);

    let spot = pool.as_ref().and_then(|row| spot_from_pool(&asset, row));
    let mut last_trade = pool_match.as_ref().and_then(|row| trade_from_swap(&asset, row, "pool"));
    last_trade =
        newer(last_trade, dex_rows.iter().find_map(|row| trade_from_swap(&asset, row, "dex")));
    last_trade =
        newer(last_trade, dispense.as_ref().and_then(|row| trade_from_dispense(&asset, row)));

    let quote_asset = last_trade
        .as_ref()
        .map(|trade| trade.quote_asset.clone())
        .or_else(|| spot.as_ref().map(|s| s.quote_asset.clone()))
        .unwrap_or_else(|| "XCP".to_string());

    CoreFetch::Ok(AssetMarket {
        asset,
        quote_asset,
        spot,
        last_trade,
        pool_matches,
        dex_matches,
        dispenses,
    })
}

fn fetch_xcp_pool(asset: &str) -> Option<Value> {
    match core::fetch_pool(asset, "XCP") {
        CoreFetch::Ok(value) => Some(value),
        CoreFetch::NotFound => match core::fetch_pool("XCP", asset) {
            CoreFetch::Ok(value) => Some(value),
            _ => None,
        },
        CoreFetch::Unreachable => None,
    }
}

fn spot_from_pool(asset: &str, row: &Value) -> Option<MarketSpot> {
    let asset_a = json_str(row.get("asset_a"))?;
    let asset_b = json_str(row.get("asset_b"))?;
    let reserve_a = json_f64(row.get("reserve_a_normalized"))
        .or_else(|| raw_to_normalized(row.get("reserve_a"), row.get("asset_a_info")))?;
    let reserve_b = json_f64(row.get("reserve_b_normalized"))
        .or_else(|| raw_to_normalized(row.get("reserve_b"), row.get("asset_b_info")))?;
    if reserve_a <= 0.0 || reserve_b <= 0.0 {
        return None;
    }

    let (reserve_base, reserve_quote, quote_asset, price) = if asset_a.eq_ignore_ascii_case(asset) {
        (reserve_a, reserve_b, asset_b, reserve_b / reserve_a)
    } else if asset_b.eq_ignore_ascii_case(asset) {
        (reserve_b, reserve_a, asset_a, reserve_a / reserve_b)
    } else {
        return None;
    };

    Some(MarketSpot {
        quote_asset,
        price,
        reserve_base: format_amount(reserve_base),
        reserve_quote: format_amount(reserve_quote),
        block_index: json_u32(row.get("block_index")),
        tx_hash: json_str(row.get("tx_hash")),
    })
}

fn trade_from_swap(asset: &str, row: &Value, venue: &'static str) -> Option<MarketTrade> {
    if venue == "dex"
        && row
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status != "completed")
    {
        return None;
    }
    let forward_asset = json_str(row.get("forward_asset"))?;
    let backward_asset = json_str(row.get("backward_asset"))?;
    let forward_qty = json_f64(row.get("forward_quantity_normalized"))?;
    let backward_qty = json_f64(row.get("backward_quantity_normalized"))?;
    if forward_qty <= 0.0 || backward_qty <= 0.0 {
        return None;
    }

    // Pool forward leaves the pool (user receives). DEX forward is the book give.
    let pool = venue == "pool";
    let (side, base_amount, quote_amount, quote_asset, price) =
        if forward_asset.eq_ignore_ascii_case(asset) && is_quote(&backward_asset) {
            (
                if pool { "buy" } else { "sell" },
                forward_qty,
                backward_qty,
                backward_asset,
                backward_qty / forward_qty,
            )
        } else if backward_asset.eq_ignore_ascii_case(asset) && is_quote(&forward_asset) {
            (
                if pool { "sell" } else { "buy" },
                backward_qty,
                forward_qty,
                forward_asset,
                forward_qty / backward_qty,
            )
        } else {
            return None;
        };

    Some(MarketTrade {
        venue,
        side,
        quote_asset,
        price,
        base_amount: format_amount(base_amount),
        quote_amount: format_amount(quote_amount),
        block_index: json_u32(row.get("block_index")),
        block_time: json_u64(row.get("block_time")),
        tx_hash: json_str(row.get("tx_hash")).or_else(|| json_str(row.get("tx1_hash"))),
    })
}

fn trade_from_dispense(asset: &str, row: &Value) -> Option<MarketTrade> {
    let dispensed_asset = json_str(row.get("asset")).unwrap_or_else(|| asset.to_string());
    if !dispensed_asset.eq_ignore_ascii_case(asset) {
        return None;
    }
    let base_amount = json_f64(row.get("dispense_quantity_normalized"))?;
    let quote_amount = json_f64(row.get("btc_amount_normalized"))?;
    if base_amount <= 0.0 || quote_amount <= 0.0 {
        return None;
    }
    Some(MarketTrade {
        venue: "dispense",
        side: "buy",
        quote_asset: "BTC".to_string(),
        price: quote_amount / base_amount,
        base_amount: format_amount(base_amount),
        quote_amount: format_amount(quote_amount),
        block_index: json_u32(row.get("block_index")),
        block_time: json_u64(row.get("block_time")),
        tx_hash: json_str(row.get("tx_hash")),
    })
}

fn newer(current: Option<MarketTrade>, candidate: Option<MarketTrade>) -> Option<MarketTrade> {
    match (current, candidate) {
        (None, other) | (other, None) => other,
        (Some(a), Some(b)) => {
            if b.block_index > a.block_index
                || (b.block_index == a.block_index
                    && b.block_time.unwrap_or(0) > a.block_time.unwrap_or(0))
            {
                Some(b)
            } else {
                Some(a)
            }
        }
    }
}

fn first_item(list: CoreFetch<CoreList>) -> (Option<Value>, usize) {
    match list {
        CoreFetch::Ok(list) => (list.items.into_iter().next(), list.total),
        _ => (None, 0),
    }
}

fn items(list: CoreFetch<CoreList>) -> (Vec<Value>, usize) {
    match list {
        CoreFetch::Ok(list) => (list.items, list.total),
        _ => (Vec::new(), 0),
    }
}

fn is_quote(asset: &str) -> bool {
    QUOTE_ASSETS.iter().any(|quote| asset.eq_ignore_ascii_case(quote))
}

fn json_str(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn json_f64(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_u64().map(|n| n as f64))
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
}

fn json_u32(value: Option<&Value>) -> u32 {
    json_u64(value).unwrap_or(0) as u32
}

fn json_u64(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn raw_to_normalized(raw: Option<&Value>, info: Option<&Value>) -> Option<f64> {
    let raw = json_f64(raw)?;
    let divisible = info
        .and_then(|info| info.get("divisible"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    Some(if divisible { raw / 100_000_000.0 } else { raw })
}

pub fn format_price(value: f64) -> String {
    if value >= 1.0 {
        format_amount(value)
    } else {
        let formatted = format!("{value:.12}");
        trim_float(&formatted)
    }
}

fn format_amount(value: f64) -> String {
    let formatted = format!("{value:.8}");
    trim_float(&formatted)
}

fn trim_float(raw: &str) -> String {
    let trimmed = raw.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msga_spot_matches_xcp_fun() {
        let row = json!({
            "asset_a": "MSGA",
            "asset_b": "XCP",
            "reserve_a_normalized": "32895186.37312087",
            "reserve_b_normalized": "672.44885897",
            "block_index": 965410,
            "tx_hash": "dff2594547d7ef8c181c9c8352e114bd31b74bd7b3cf8022186f51125303df6c"
        });
        let spot = spot_from_pool("MSGA", &row).expect("spot");
        assert_eq!(spot.quote_asset, "XCP");
        assert!((spot.price - 0.00002044216596).abs() < 1e-12);
    }

    #[test]
    fn pool_buy_price_is_quote_per_base() {
        let row = json!({
            "forward_asset": "MSGA",
            "backward_asset": "XCP",
            "forward_quantity_normalized": "245192.64660789",
            "backward_quantity_normalized": "5.00000000",
            "block_index": 965410,
            "tx_hash": "4f6e62a01102e0f7d04e6fd034dd0a17eea336276d32daa5374656fb09036ef7"
        });
        let trade = trade_from_swap("MSGA", &row, "pool").expect("trade");
        assert_eq!(trade.side, "buy");
        assert_eq!(trade.venue, "pool");
        assert!((trade.price - 5.0 / 245192.64660789).abs() < 1e-12);
    }

    #[test]
    fn xcp_quote_converts_to_sats() {
        let btc = quote_to_btc(0.001431614549, "XCP", Some(0.00011)).expect("btc");
        assert!((btc - 0.0000001574776).abs() < 1e-12);
        assert!(((btc * 100_000_000.0) - 15.74776).abs() < 1e-5);
    }

    #[test]
    fn ignores_non_quote_dex_match() {
        let row = json!({
            "forward_asset": "A8843753281994996216",
            "backward_asset": "PEPECASH",
            "forward_quantity_normalized": "1",
            "backward_quantity_normalized": "6900.00000000",
            "block_index": 964909
        });
        assert!(trade_from_swap("PEPECASH", &row, "dex").is_none());
    }

    #[test]
    fn newer_trade_wins() {
        let older = MarketTrade {
            venue: "dex",
            side: "sell",
            quote_asset: "XCP".into(),
            price: 0.01,
            base_amount: "1".into(),
            quote_amount: "0.01".into(),
            block_index: 100,
            block_time: Some(1),
            tx_hash: None,
        };
        let newer_trade = MarketTrade {
            venue: "pool",
            side: "buy",
            quote_asset: "XCP".into(),
            price: 0.02,
            base_amount: "1".into(),
            quote_amount: "0.02".into(),
            block_index: 200,
            block_time: Some(2),
            tx_hash: None,
        };
        let picked = newer(Some(older), Some(newer_trade)).expect("pick");
        assert_eq!(picked.block_index, 200);
        assert_eq!(picked.venue, "pool");
    }
}
