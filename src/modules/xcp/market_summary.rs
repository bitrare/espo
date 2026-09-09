use super::{core::CoreFetch, history, market::format_price};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_ROWS: usize = 2000;
const PAGE_SIZE: usize = 200;
type Cache = HashMap<String, (Instant, Value)>;

pub fn fetch(asset: &str) -> Value {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(c) = cache.lock() {
        if let Some((at, value)) = c.get(asset) {
            if at.elapsed() < Duration::from_secs(60) {
                return value.clone();
            }
        }
    }
    let result = summarize(asset);
    if let Ok(mut c) = cache.lock() {
        c.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(60));
        if c.len() >= 64 {
            c.clear();
        }
        c.insert(asset.to_string(), (Instant::now(), result.clone()));
    }
    result
}

struct Sample {
    rows: Vec<Value>,
    total: Option<usize>,
    complete: bool,
}
fn collect(asset: &str, resource: &str, status: &str) -> Sample {
    let mut sample = Sample { rows: vec![], total: None, complete: false };
    while sample.rows.len() < MAX_ROWS {
        match history::fetch(asset, resource, "XCP", status, sample.rows.len(), PAGE_SIZE) {
            CoreFetch::Ok(list) => {
                sample.total = Some(list.total);
                let empty = list.items.is_empty();
                sample.rows.extend(list.items);
                if sample.rows.len() >= list.total {
                    sample.complete = true;
                    break;
                }
                if empty {
                    break;
                }
            }
            // Absent pools are distinct from failed node requests.
            CoreFetch::NotFound if resource.starts_with("pool_") => {
                sample.total = Some(0);
                sample.complete = true;
                break;
            }
            _ => break,
        }
    }
    sample
}
fn number(v: &Value, key: &str) -> Option<f64> {
    v.get(key)
        .and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
        .filter(|n| n.is_finite() && *n >= 0.0)
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn summarize(asset: &str) -> Value {
    let specs = [
        ("matches", "completed"),
        ("dispenses", "all"),
        ("pool_matches", "all"),
        ("orders", "open"),
        ("dispensers", "open"),
    ];
    let samples = std::thread::scope(|scope| {
        let handles: Vec<_> = specs
            .iter()
            .map(|(kind, status)| scope.spawn(move || collect(asset, kind, status)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or(Sample { rows: vec![], total: None, complete: false }))
            .collect::<Vec<_>>()
    });
    assemble(asset, &samples)
}
fn assemble(asset: &str, samples: &[Sample]) -> Value {
    let specs = [
        ("matches", "completed"),
        ("dispenses", "all"),
        ("pool_matches", "all"),
        ("orders", "open"),
        ("dispensers", "open"),
    ];
    let mut volumes: BTreeMap<String, (f64, f64, usize)> = BTreeMap::new();
    let mut latest: BTreeMap<String, Value> = BTreeMap::new();
    let mut months = BTreeSet::new();
    let mut last_time = 0;
    for (index, sample) in samples.iter().take(3).enumerate() {
        for row in &sample.rows {
            if index == 0 && text(row, "status") != "completed" {
                continue;
            }
            let Some((quote, base, paid)) = trade_amounts(asset, row, index == 1) else {
                continue;
            };
            let entry = volumes.entry(quote.clone()).or_default();
            entry.0 += base;
            entry.1 += paid;
            entry.2 += 1;
            let time = row.get("block_time").and_then(Value::as_i64).unwrap_or(0);
            last_time = last_time.max(time);
            if let Some(date) =
                time::OffsetDateTime::from_unix_timestamp(time).ok().filter(|_| time > 0)
            {
                months.insert(format!("{}-{:02}", date.year(), date.month() as u8));
            }
            let height = row.get("block_index").and_then(Value::as_u64).unwrap_or(0);
            let old_height =
                latest.get(&quote).and_then(|v| v.get("block_index")).and_then(Value::as_u64);
            if old_height.is_none_or(|old| height >= old) {
                latest.insert(quote.clone(),json!({"quote_asset":quote,"price":format_price(paid/base),"block_index":height,"block_time":time,"venue":specs[index].0,"tx_hash":row.get("tx_hash").or_else(||row.get("tx1_hash"))}));
            }
        }
    }
    let mut asks: BTreeMap<String, f64> = BTreeMap::new();
    let mut bids: BTreeMap<String, f64> = BTreeMap::new();
    let mut order_escrow = 0.0;
    for row in &samples[3].rows {
        let (Some(give), Some(get)) =
            (number(row, "give_remaining_normalized"), number(row, "get_remaining_normalized"))
        else {
            continue;
        };
        if give <= 0.0 || get <= 0.0 {
            continue;
        }
        if text(row, "give_asset") == asset {
            order_escrow += give;
            let quote = text(row, "get_asset").to_string();
            asks.entry(quote).and_modify(|p| *p = p.min(get / give)).or_insert(get / give);
        } else if text(row, "get_asset") == asset {
            let quote = text(row, "give_asset").to_string();
            bids.entry(quote).and_modify(|p| *p = p.max(give / get)).or_insert(give / get);
        }
    }
    let mut dispenser_escrow = 0.0;
    let mut dispenser_floor: Option<f64> = None;
    for row in &samples[4].rows {
        dispenser_escrow += number(row, "give_remaining_normalized").unwrap_or(0.0);
        // Oracle dispensers must use the node's resolved BTC price, never the fiat satoshirate.
        if let (Some(rate), Some(lot)) =
            (number(row, "satoshi_price_normalized"), number(row, "give_quantity_normalized"))
        {
            if lot > 0.0 && number(row, "give_remaining_normalized").unwrap_or(0.0) >= lot {
                let price = rate / lot;
                dispenser_floor = Some(dispenser_floor.map_or(price, |p| p.min(price)));
            }
        }
    }
    let trade_complete = samples[..3].iter().all(|s| s.complete);
    let listing_complete = samples[3..].iter().all(|s| s.complete);
    json!({
        "source":"Counterparty Core", "quote_asset":"XCP", "cached_seconds":60,
        "complete":samples.iter().all(|s|s.complete), "trade_history_complete":trade_complete,"listings_complete":listing_complete,
        "history_scope":if trade_complete {"all node records"} else {"bounded node sample"},
        "max_rows_per_source":MAX_ROWS,
        "sources":specs.iter().zip(samples).map(|((kind,status),s)|json!({"type":kind,"status":status,"total":s.total,"loaded":s.rows.len(),"complete":s.complete})).collect::<Vec<_>>(),
        "volumes":volumes.iter().map(|(quote,(base,paid,count))|json!({"quote_asset":quote,"asset_quantity":format_price(*base),"quote_quantity":format_price(*paid),"trades":count})).collect::<Vec<_>>(),
        "last_trades":latest.values().collect::<Vec<_>>(),
        "active_months":months.len(),"last_trade_time":if last_time>0 {Some(last_time)} else {None},
        "open_orders":samples[3].total,"open_dispensers":samples[4].total,
        "order_escrow":format_price(order_escrow),"dispenser_escrow":format_price(dispenser_escrow),
        "best_asks":asks.iter().map(|(q,p)|json!({"quote_asset":q,"price":format_price(*p)})).collect::<Vec<_>>(),
        "best_bids":bids.iter().map(|(q,p)|json!({"quote_asset":q,"price":format_price(*p)})).collect::<Vec<_>>(),
        "dispenser_floor_btc":dispenser_floor.map(format_price),
        "notes":"Volumes are grouped by original quote currency. Dispenses use BTC paid; DEX includes only completed matches. Pools cover the asset/XCP pair. No historical USD conversion or off-chain sales. Incomplete samples are not lifetime totals."
    })
}

fn trade_amounts(asset: &str, row: &Value, dispense: bool) -> Option<(String, f64, f64)> {
    let (quote, base, paid) = if dispense {
        (
            "BTC".to_string(),
            number(row, "dispense_quantity_normalized")?,
            number(row, "btc_amount_normalized")?,
        )
    } else if text(row, "forward_asset") == asset {
        (
            text(row, "backward_asset").to_string(),
            number(row, "forward_quantity_normalized")?,
            number(row, "backward_quantity_normalized")?,
        )
    } else if text(row, "backward_asset") == asset {
        (
            text(row, "forward_asset").to_string(),
            number(row, "backward_quantity_normalized")?,
            number(row, "forward_quantity_normalized")?,
        )
    } else {
        return None;
    };
    (base > 0.0 && paid > 0.0 && !quote.is_empty()).then_some((quote, base, paid))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_non_btc_quote_and_orientation() {
        let row = json!({"forward_asset":"PEPECASH","backward_asset":"DJPEPE","forward_quantity_normalized":"6900","backward_quantity_normalized":"2"});
        assert_eq!(trade_amounts("DJPEPE", &row, false), Some(("PEPECASH".into(), 2.0, 6900.0)));
        assert!(trade_amounts("OTHER", &row, false).is_none());
    }
    #[test]
    fn rejects_zero_and_missing_amounts() {
        assert!(
            trade_amounts(
                "DJPEPE",
                &json!({"dispense_quantity_normalized":"0","btc_amount_normalized":"1"}),
                true
            )
            .is_none()
        );
        assert!(trade_amounts("DJPEPE", &json!({}), true).is_none());
    }
    #[test]
    fn completed_sales_are_separate_from_expired_matches_and_listings() {
        let mut samples: Vec<_> = (0..5)
            .map(|_| Sample { rows: vec![], total: Some(0), complete: true })
            .collect();
        let trade = json!({"status":"completed","forward_asset":"DJPEPE","backward_asset":"XCP","forward_quantity_normalized":"2","backward_quantity_normalized":"10","block_index":100});
        let mut expired = trade.clone();
        expired["status"] = json!("expired");
        expired["backward_quantity_normalized"] = json!("9999");
        samples[0].rows = vec![trade, expired];
        samples[1].rows =
            vec![json!({"dispense_quantity_normalized":"1","btc_amount_normalized":"0.1"})];
        samples[3].rows = vec![
            json!({"give_asset":"DJPEPE","get_asset":"XCP","give_remaining_normalized":"3","get_remaining_normalized":"60"}),
            json!({"give_asset":"XCP","get_asset":"DJPEPE","give_remaining_normalized":"10","get_remaining_normalized":"2"}),
        ];
        samples[4].rows = vec![
            json!({"give_remaining_normalized":"6","give_quantity_normalized":"2","satoshi_price_normalized":"0.1","satoshirate_normalized":"1000"}),
        ];
        let result = assemble("DJPEPE", &samples);
        assert_eq!(result["volumes"][0]["quote_asset"], "BTC");
        assert_eq!(result["volumes"][1]["quote_quantity"], "10");
        assert_eq!(result["volumes"][1]["trades"], 1);
        assert_eq!(result["order_escrow"], "3");
        assert_eq!(result["best_asks"][0]["price"], "20");
        assert_eq!(result["best_bids"][0]["price"], "5");
        assert_eq!(result["dispenser_floor_btc"], "0.05");
        samples[0].complete = false;
        let partial = assemble("DJPEPE", &samples);
        assert_eq!(partial["trade_history_complete"], false);
        assert_eq!(partial["history_scope"], "bounded node sample");
    }
}
