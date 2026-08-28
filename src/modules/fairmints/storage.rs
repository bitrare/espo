use super::classify::{
    KNOWN_MARKETPLACES, StoredTxClass, TxClassification, TxType, classify_transaction,
    is_diesel_mint_trace_sandshrew,
};
use super::diesel::{
    DieselBlockStats, MintCostCandle, PRICE_SCALE, mint_cost_scaled, mint_volume_scaled,
};
use crate::alkanes::trace::{EspoSandshrewLikeTrace, prettyify_protobuf_trace_json};
use crate::config::{get_electrum_like, get_network};
use crate::modules::ammdata::schemas::Timeframe;
use crate::modules::ammdata::storage::{AmmDataProvider, GetLatestBtcUsdPriceParams};
use crate::modules::essentials::storage::{
    AddressIndexListKind, AlkaneTxSummary, BalanceEntry, EssentialsProvider, GetBlockSummaryParams,
    RpcGetBlockSummaryParams, address_index_list_id_alkane_block_txs, get_address_index_list_len,
    get_address_index_list_range, load_tx_pointer_blob_v3_by_id, load_tx_summary_v2,
    resolve_outpoint_id_v2, resolve_outpoint_spent_by_id_v2,
};
use crate::modules::essentials::utils::balances::{
    OutpointLookup, get_outpoint_balances_with_spent_batch,
};
use crate::runtime::mdb::{Mdb, MdbBatch};
use crate::runtime::mempool::{
    MempoolBlockTx, MempoolTxFilter, get_mempool_block_detail, get_mempool_block_ordered_transactions,
    get_mempool_index_transactions_ordered_by_block_and_fee,
};
use crate::runtime::state_at::StateAt;
use anyhow::{Result, anyhow};
use bitcoin::consensus::encode::deserialize;
use bitcoin::hashes::Hash;
use bitcoin::{Address, Network, Transaction, Txid};
use borsh::BorshDeserialize;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const KEY_INDEX_HEIGHT: &[u8] = b"/index_height";
const KEY_TX_PREFIX: &[u8] = b"/tx/";
const KEY_DIESEL_STATS_PREFIX: &[u8] = b"/diesel/stats/";
const KEY_CANDLE_M10_PREFIX: &[u8] = b"/diesel/candle/m10/";
const KEY_BTC_USD_PREFIX: &[u8] = b"/btc_usd/";

#[derive(Clone)]
pub struct FairmintsProvider {
    mdb: Arc<Mdb>,
}

impl FairmintsProvider {
    pub fn new(mdb: Arc<Mdb>) -> Self {
        Self { mdb }
    }

    pub fn mdb(&self) -> &Mdb {
        self.mdb.as_ref()
    }

    fn tx_key(txid: &Txid) -> Vec<u8> {
        let mut k = KEY_TX_PREFIX.to_vec();
        k.extend_from_slice(txid.to_string().as_bytes());
        k
    }

    fn diesel_stats_key(height: u32) -> Vec<u8> {
        let mut k = KEY_DIESEL_STATS_PREFIX.to_vec();
        k.extend_from_slice(height.to_string().as_bytes());
        k
    }

    fn candle_m10_key(ts: u64) -> Vec<u8> {
        let mut k = KEY_CANDLE_M10_PREFIX.to_vec();
        k.extend_from_slice(format!("{ts:020}").as_bytes());
        k
    }

    pub fn btc_usd_key(height: u32) -> Vec<u8> {
        let mut k = KEY_BTC_USD_PREFIX.to_vec();
        k.extend_from_slice(height.to_string().as_bytes());
        k
    }

    pub fn get_index_height(&self) -> Result<Option<u32>> {
        let Some(bytes) = self.mdb.get(KEY_INDEX_HEIGHT)? else {
            return Ok(None);
        };
        if bytes.len() != 4 {
            return Err(anyhow!("invalid /index_height length {}", bytes.len()));
        }
        let mut arr = [0u8; 4];
        arr.copy_from_slice(&bytes);
        Ok(Some(u32::from_le_bytes(arr)))
    }

    pub fn set_index_height(&self, height: u32) -> Result<()> {
        self.mdb.put(KEY_INDEX_HEIGHT, &height.to_le_bytes())?;
        Ok(())
    }

    pub fn get_tx_class(&self, txid: &Txid) -> Result<Option<StoredTxClass>> {
        let Some(bytes) = self.mdb.get(&Self::tx_key(txid))? else {
            return Ok(None);
        };
        Ok(Some(StoredTxClass::try_from_slice(&bytes)?))
    }

    pub fn get_diesel_stats(&self, height: u32) -> Result<Option<DieselBlockStats>> {
        let Some(bytes) = self.mdb.get(&Self::diesel_stats_key(height))? else {
            return Ok(None);
        };
        Ok(Some(DieselBlockStats::try_from_slice(&bytes)?))
    }

    pub fn get_candle_m10(&self, ts: u64) -> Result<Option<MintCostCandle>> {
        let Some(bytes) = self.mdb.get(&Self::candle_m10_key(ts))? else {
            return Ok(None);
        };
        Ok(Some(MintCostCandle::try_from_slice(&bytes)?))
    }

    pub fn get_btc_usd(&self, height: u32) -> Result<Option<u128>> {
        let Some(bytes) = self.mdb.get(&Self::btc_usd_key(height))? else {
            return Ok(None);
        };
        if bytes.len() != 16 {
            return Ok(None);
        }
        let mut arr = [0u8; 16];
        arr.copy_from_slice(&bytes);
        Ok(Some(u128::from_le_bytes(arr)))
    }

    pub fn put_btc_usd(&self, height: u32, price: u128) -> Result<()> {
        self.mdb.put(&Self::btc_usd_key(height), &price.to_le_bytes())?;
        Ok(())
    }

    fn parse_btc_usd_key(key: &[u8]) -> Option<u32> {
        let rest = key.strip_prefix(KEY_BTC_USD_PREFIX)?;
        std::str::from_utf8(rest).ok()?.parse().ok()
    }

    fn decode_btc_usd_value(bytes: &[u8]) -> Option<u128> {
        if bytes.len() != 16 {
            return None;
        }
        let mut arr = [0u8; 16];
        arr.copy_from_slice(bytes);
        let price = u128::from_le_bytes(arr);
        (price > 0).then_some(price)
    }

    fn get_btc_usd_at_or_before(&self, height: u32) -> Result<Option<(u32, u128)>> {
        if let Some(price) = self.get_btc_usd(height)? {
            if price > 0 {
                return Ok(Some((height, price)));
            }
        }
        let mut best: Option<(u32, u128)> = None;
        for (key, value) in self.mdb.scan_prefix_entries(KEY_BTC_USD_PREFIX)? {
            let Some(h) = Self::parse_btc_usd_key(&key) else {
                continue;
            };
            if h > height {
                continue;
            }
            let Some(price) = Self::decode_btc_usd_value(&value) else {
                continue;
            };
            if best.map(|(bh, _)| h > bh).unwrap_or(true) {
                best = Some((h, price));
            }
        }
        Ok(best)
    }

    pub fn commit_block(
        &self,
        height: u32,
        classes: &[(Txid, TxClassification)],
        diesel_stats: &DieselBlockStats,
        candle_ts: Option<u64>,
    ) -> Result<()> {
        let mut candle_write: Option<(Vec<u8>, Vec<u8>)> = None;
        if diesel_stats.mint_count > 0 {
            if let Some(ts) = candle_ts {
                let dur = Timeframe::M10.duration_secs();
                let bucket = (ts / dur) * dur;
                let cost = mint_cost_scaled(diesel_stats);
                let volume = mint_volume_scaled(diesel_stats);
                let candle = if let Some(mut existing) = self.get_candle_m10(bucket)? {
                    existing.high = existing.high.max(cost);
                    existing.low = existing.low.min(cost);
                    existing.close = cost;
                    existing.volume = existing.volume.saturating_add(volume);
                    existing
                } else {
                    MintCostCandle {
                        open: cost,
                        high: cost,
                        low: cost,
                        close: cost,
                        volume,
                    }
                };
                candle_write = Some((Self::candle_m10_key(bucket), borsh::to_vec(&candle)?));
            }
        }

        let class_rows: Vec<(Vec<u8>, Vec<u8>)> = classes
            .iter()
            .map(|(txid, class)| {
                let stored = StoredTxClass {
                    tx_type: class.tx_type,
                    marketplace_info: class.marketplace_info.clone(),
                };
                Ok((Self::tx_key(txid), borsh::to_vec(&stored)?))
            })
            .collect::<Result<Vec<_>>>()?;

        let stats_bytes = borsh::to_vec(diesel_stats)?;
        let stats_key = Self::diesel_stats_key(height);
        let height_bytes = height.to_le_bytes();

        self.mdb.bulk_write(|wb: &mut MdbBatch<'_>| {
            for (k, v) in &class_rows {
                wb.put(k, v);
            }
            wb.put(&stats_key, &stats_bytes);
            if let Some((k, v)) = &candle_write {
                wb.put(k, v);
            }
            wb.put(KEY_INDEX_HEIGHT, &height_bytes);
        })?;
        Ok(())
    }

    pub fn rpc_get_known_marketplaces(&self) -> Value {
        let marketplaces: Vec<Value> = KNOWN_MARKETPLACES
            .iter()
            .map(|mp| {
                json!({
                    "id": mp.id,
                    "name": mp.name,
                    "fee_addresses": mp.fee_addresses,
                })
            })
            .collect();
        json!({ "ok": true, "marketplaces": marketplaces })
    }

    pub fn rpc_get_tx_types(&self) -> Value {
        json!({
            "ok": true,
            "tx_types": [
                { "id": "diesel_mint", "description": "Diesel mint (contract 2:0, opcode 77)" },
                { "id": "mint", "description": "Mint on other contracts (opcode 77)" },
                { "id": "transfer", "description": "Simple transfer (balance changes)" },
                { "id": "marketplace", "description": "Marketplace transaction (known fee address)" },
                { "id": "swap", "description": "AMM swap interaction" },
                { "id": "deploy", "description": "New contract deployment" },
                { "id": "other_contract_call", "description": "Other contract invocation" },
                { "id": "unknown", "description": "Unknown classification" }
            ]
        })
    }

    pub fn rpc_get_block_summary(
        &self,
        essentials: &EssentialsProvider,
        height: Option<u64>,
    ) -> Value {
        let base = match essentials.rpc_get_block_summary(RpcGetBlockSummaryParams { height }) {
            Ok(resp) => resp.value,
            Err(e) => return json!({"ok": false, "error": e.to_string()}),
        };
        let Some(h) = height else {
            return base;
        };
        let stats = self.get_diesel_stats(h as u32).ok().flatten();
        let mut obj = match base {
            Value::Object(map) => map,
            other => return other,
        };
        if let Some(stats) = stats {
            obj.insert("diesel_mint_count".into(), json!(stats.mint_count));
            obj.insert("diesel_total_fee_sats".into(), json!(stats.total_fee_sats));
            obj.insert("diesel_min_fee_rate".into(), json!(stats.min_fee_rate));
            obj.insert("diesel_reward_recipients".into(), json!(stats.reward_recipients));
            obj.insert("diesel_distributed".into(), json!(stats.distributed));
            obj.insert("diesel_mint_cost_sats".into(), json!(stats.mint_cost_sats));
        } else {
            obj.insert("diesel_mint_count".into(), json!(0));
            obj.insert("diesel_total_fee_sats".into(), json!(0));
            obj.insert("diesel_min_fee_rate".into(), json!(0.0));
            obj.insert("diesel_reward_recipients".into(), json!(0));
            obj.insert("diesel_distributed".into(), json!(0));
            obj.insert("diesel_mint_cost_sats".into(), json!(0));
        }
        Value::Object(obj)
    }

    pub fn rpc_get_alkane_block_txs(
        &self,
        essentials: &EssentialsProvider,
        height: Option<u64>,
        page: Option<u64>,
        limit: Option<u64>,
        hide_diesel_mints: Option<bool>,
        tx_type: Option<&str>,
    ) -> Result<Value> {
        let Some(height) = height else {
            return Ok(json!({"ok": false, "error": "missing_or_invalid_height"}));
        };
        let page = page.unwrap_or(1).max(1) as usize;
        let limit = limit.unwrap_or(50).max(1) as usize;
        let off = limit.saturating_mul(page.saturating_sub(1));
        let hide_diesel = hide_diesel_mints.unwrap_or(false);
        let filter_tx_type = tx_type.and_then(TxType::from_str);
        let list_id = address_index_list_id_alkane_block_txs(height);
        let raw_total = get_address_index_list_len(
            essentials,
            StateAt::Latest,
            AddressIndexListKind::AlkaneBlockTxs,
            &list_id,
        )
        .unwrap_or(0) as usize;
        if raw_total == 0 {
            return Ok(json!({
                "ok": true,
                "height": height,
                "page": page,
                "limit": limit,
                "total": 0,
                "txids": []
            }));
        }

        let all_ids = get_address_index_list_range(
            essentials,
            StateAt::Latest,
            AddressIndexListKind::AlkaneBlockTxs,
            &list_id,
            0,
            raw_total as u64,
        )
        .unwrap_or_default();

        let mut filtered_txids: Vec<String> = Vec::new();
        for id in all_ids {
            let Some(blob) = load_tx_pointer_blob_v3_by_id(essentials, id) else {
                continue;
            };
            let txid = Txid::from_byte_array(blob.txid);
            let class = self.get_tx_class(&txid).ok().flatten().unwrap_or_else(|| {
                StoredTxClass {
                    tx_type: classify_transaction(&blob.traces, &[]).tx_type,
                    marketplace_info: None,
                }
            });
            if let Some(wanted) = filter_tx_type {
                if class.tx_type != wanted {
                    continue;
                }
            }
            if hide_diesel && class.tx_type == TxType::DieselMint {
                continue;
            }
            filtered_txids.push(txid.to_string());
        }
        let total = filtered_txids.len();
        let end = (off + limit).min(total);
        let txids = if end > off { filtered_txids[off..end].to_vec() } else { Vec::new() };
        Ok(json!({
            "ok": true,
            "height": height,
            "page": page,
            "limit": limit,
            "total": total,
            "txids": txids
        }))
    }

    pub fn rpc_get_diesel_block_stats(&self, height: Option<u64>) -> Value {
        let Some(height) = height else {
            return json!({"ok": false, "error": "missing_or_invalid_height"});
        };
        match self.get_diesel_stats(height as u32) {
            Ok(Some(stats)) => json!({
                "ok": true,
                "height": height,
                "diesel_mint_count": stats.mint_count,
                "diesel_total_fee_sats": stats.total_fee_sats,
                "diesel_min_fee_rate": stats.min_fee_rate,
                "diesel_reward_recipients": stats.reward_recipients,
                "diesel_distributed": stats.distributed,
                "diesel_mint_cost_sats": stats.mint_cost_sats,
            }),
            Ok(None) => json!({"ok": true, "height": height, "diesel_mint_count": 0}),
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        }
    }

    pub fn rpc_get_diesel_mint_cost_candles(
        &self,
        timeframe: Option<&str>,
        limit: Option<u64>,
        page: Option<u64>,
        now: Option<u64>,
    ) -> Result<Value> {
        let tf = parse_timeframe(timeframe.unwrap_or("1d"));
        let limit = limit.map(|n| n as usize).unwrap_or(120).max(1);
        let page = page.map(|n| n as usize).unwrap_or(1).max(1);
        let now = now.unwrap_or_else(now_ts);
        let dur = tf.duration_secs();

        let entries = self.mdb.scan_prefix_entries(KEY_CANDLE_M10_PREFIX)?;
        let mut m10: std::collections::BTreeMap<u64, MintCostCandle> =
            std::collections::BTreeMap::new();
        for (k, v) in entries {
            if let Some(ts_bytes) = k.strip_prefix(KEY_CANDLE_M10_PREFIX) {
                if let Ok(ts_str) = std::str::from_utf8(ts_bytes) {
                    if let Ok(ts) = ts_str.parse::<u64>() {
                        if let Ok(c) = MintCostCandle::try_from_slice(&v) {
                            m10.entry(ts).or_insert(c);
                        }
                    }
                }
            }
        }

        let mut per_bucket: std::collections::BTreeMap<u64, MintCostCandle> =
            std::collections::BTreeMap::new();
        for (ts, c) in m10.iter() {
            let bucket = (ts / dur) * dur;
            per_bucket
                .entry(bucket)
                .and_modify(|agg| {
                    agg.high = agg.high.max(c.high);
                    agg.low = agg.low.min(c.low);
                    agg.close = c.close;
                    agg.volume = agg.volume.saturating_add(c.volume);
                })
                .or_insert(*c);
        }

        if per_bucket.is_empty() {
            return Ok(json!({
                "ok": true,
                "timeframe": tf.code(),
                "candles": [],
                "page": page,
                "limit": limit,
                "has_more": false,
                "total": 0
            }));
        }

        let start_bucket = *per_bucket.keys().next().unwrap();
        let newest_bucket_with_data = *per_bucket.keys().last().unwrap();
        let newest_bucket_now = (now / dur) * dur;
        let mut last_close: u128 = 0;
        let mut have_prev = false;
        let mut forward: std::collections::BTreeMap<u64, MintCostCandle> =
            std::collections::BTreeMap::new();
        let mut bts = start_bucket;
        while bts <= newest_bucket_with_data {
            if let Some(c) = per_bucket.get(&bts) {
                let mut candle = *c;
                if have_prev {
                    candle.open = last_close;
                    if candle.open > candle.high {
                        candle.high = candle.open;
                    }
                    if candle.open < candle.low {
                        candle.low = candle.open;
                    }
                }
                last_close = candle.close;
                have_prev = true;
                forward.insert(bts, candle);
            } else if have_prev {
                forward.insert(
                    bts,
                    MintCostCandle {
                        open: last_close,
                        high: last_close,
                        low: last_close,
                        close: last_close,
                        volume: 0,
                    },
                );
            }
            bts = match bts.checked_add(dur) {
                Some(n) => n,
                None => break,
            };
        }
        if newest_bucket_now > newest_bucket_with_data && have_prev {
            let mut t = newest_bucket_with_data.saturating_add(dur);
            while t <= newest_bucket_now {
                forward.insert(
                    t,
                    MintCostCandle {
                        open: last_close,
                        high: last_close,
                        low: last_close,
                        close: last_close,
                        volume: 0,
                    },
                );
                t = match t.checked_add(dur) {
                    Some(n) => n,
                    None => break,
                };
            }
        }

        let total = forward.len();
        let offset = (page - 1) * limit;
        let candles_newest_first: Vec<_> = forward.into_iter().rev().collect();
        let page_candles: Vec<Value> = candles_newest_first
            .iter()
            .skip(offset)
            .take(limit)
            .map(|(ts, c)| {
                let price_sats = c.close.saturating_mul(100_000_000) / PRICE_SCALE;
                json!({
                    "timestamp": *ts,
                    "open": c.open.to_string(),
                    "high": c.high.to_string(),
                    "low": c.low.to_string(),
                    "close": c.close.to_string(),
                    "volume": c.volume.to_string(),
                    "close_sats": price_sats.to_string()
                })
            })
            .collect();
        let has_more = offset + page_candles.len() < total;
        Ok(json!({
            "ok": true,
            "timeframe": tf.code(),
            "candles": page_candles,
            "page": page,
            "limit": limit,
            "has_more": has_more,
            "total": total
        }))
    }

    pub fn rpc_get_btc_usd_price(&self, amm: &AmmDataProvider, height: Option<u64>) -> Value {
        fn ok(height: u64, price: u128, source: &str, requested: Option<u64>) -> Value {
            let mut out = json!({
                "ok": true,
                "height": height,
                "price": price.to_string(),
                "source": source
            });
            if let Some(requested) = requested.filter(|h| *h != height) {
                out["requested_height"] = json!(requested);
            }
            out
        }

        let pick = |plugin: Option<(u32, u128)>, amm: Option<(u64, u128)>| -> Option<(u64, u128, &'static str)> {
            match (plugin, amm) {
                (Some((ph, pp)), Some((ah, ap))) => {
                    if u64::from(ph) >= ah {
                        Some((u64::from(ph), pp, "fairmints_coingecko"))
                    } else {
                        Some((ah, ap, "ammdata_index"))
                    }
                }
                (Some((ph, pp)), None) => Some((u64::from(ph), pp, "fairmints_coingecko")),
                (None, Some((ah, ap))) => Some((ah, ap, "ammdata_index")),
                (None, None) => None,
            }
        };

        if let Some(height) = height {
            if let Ok(Some(price)) = self.get_btc_usd(height as u32) {
                if price > 0 {
                    return ok(height, price, "fairmints_coingecko", None);
                }
            }
            if let Ok(Some((h, price))) =
                amm.get_btc_usd_price_entry_at_or_before_height(height)
            {
                if h == height && price > 0 {
                    return ok(h, price, "ammdata_index", None);
                }
                let plugin = self.get_btc_usd_at_or_before(height as u32).ok().flatten();
                return match pick(plugin, Some((h, price))) {
                    Some((ph, pp, source)) => ok(
                        ph,
                        pp,
                        if source == "fairmints_coingecko" {
                            "fairmints_coingecko_last_active"
                        } else {
                            "ammdata_index_last_active"
                        },
                        Some(height),
                    ),
                    None => json!({"ok": false, "error": "price_unavailable", "height": height}),
                };
            }
            return match self.get_btc_usd_at_or_before(height as u32) {
                Ok(Some((h, price))) => {
                    ok(u64::from(h), price, "fairmints_coingecko_last_active", Some(height))
                }
                Ok(None) => json!({"ok": false, "error": "price_unavailable", "height": height}),
                Err(e) => json!({"ok": false, "error": e.to_string()}),
            };
        }

        let plugin = self.get_btc_usd_at_or_before(u32::MAX).ok().flatten();
        let amm_latest = amm
            .get_latest_btc_usd_price_entry(GetLatestBtcUsdPriceParams {
                blockhash: StateAt::Latest,
            })
            .ok()
            .flatten();
        match pick(plugin, amm_latest) {
            Some((h, price, source)) => ok(h, price, source, None),
            None => json!({"ok": false, "error": "price_unavailable"}),
        }
    }

    pub fn rpc_check_outpoints_spent(
        &self,
        essentials: &EssentialsProvider,
        outpoints: Option<Vec<String>>,
    ) -> Value {
        let Some(outpoints) = outpoints else {
            return json!({"ok": false, "error": "missing_outpoints"});
        };
        if outpoints.is_empty() {
            return json!({"ok": true, "results": {}});
        }

        let mut results: Map<String, Value> = Map::new();
        for outpoint_str in outpoints {
            let parts: Vec<&str> = outpoint_str.split(':').collect();
            if parts.len() != 2 {
                results.insert(
                    outpoint_str.clone(),
                    json!({"error": "invalid_format", "spent": null}),
                );
                continue;
            }
            let vout: u32 = match parts[1].parse() {
                Ok(v) => v,
                Err(_) => {
                    results.insert(
                        outpoint_str.clone(),
                        json!({"error": "invalid_vout", "spent": null}),
                    );
                    continue;
                }
            };
            let txid_bytes: [u8; 32] = match hex::decode(parts[0]) {
                Ok(bytes) if bytes.len() == 32 => {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&bytes);
                    arr.reverse();
                    arr
                }
                _ => {
                    results.insert(
                        outpoint_str.clone(),
                        json!({"error": "invalid_txid", "spent": null}),
                    );
                    continue;
                }
            };

            let outpoint_id = match resolve_outpoint_id_v2(essentials, StateAt::Latest, &txid_bytes, vout)
            {
                Ok(Some(id)) => id,
                Ok(None) => {
                    results.insert(
                        outpoint_str.clone(),
                        json!({"error": "outpoint_not_found", "spent": null}),
                    );
                    continue;
                }
                Err(_) => {
                    results.insert(
                        outpoint_str.clone(),
                        json!({"error": "lookup_error", "spent": null}),
                    );
                    continue;
                }
            };

            match resolve_outpoint_spent_by_id_v2(essentials, StateAt::Latest, outpoint_id) {
                Ok(Some(spent_txid_bytes)) => {
                    let mut display_txid = spent_txid_bytes;
                    display_txid.reverse();
                    results.insert(
                        outpoint_str.clone(),
                        json!({
                            "spent": true,
                            "spent_by": hex::encode(display_txid)
                        }),
                    );
                }
                Ok(None) => {
                    results.insert(outpoint_str.clone(), json!({"spent": false, "spent_by": null}));
                }
                Err(_) => {
                    results.insert(
                        outpoint_str.clone(),
                        json!({"error": "lookup_error", "spent": null}),
                    );
                }
            }
        }

        json!({"ok": true, "results": results})
    }

    pub fn rpc_get_alkane_tx_summary(
        &self,
        essentials: &EssentialsProvider,
        txid_hex: Option<&str>,
    ) -> Value {
        let Some(txid_hex) = txid_hex.map(str::trim).filter(|s| !s.is_empty()) else {
            return json!({"ok": false, "error": "missing_or_invalid_txid"});
        };
        let txid = match Txid::from_str(txid_hex) {
            Ok(t) => t,
            Err(_) => return json!({"ok": false, "error": "invalid_txid_format"}),
        };
        let Some(summary) = load_tx_summary_v2(essentials, &txid) else {
            return json!({"ok": false, "error": "not_found"});
        };
        self.summary_to_json(&txid, &summary)
    }

    fn summary_to_json(&self, txid: &Txid, summary: &AlkaneTxSummary) -> Value {
        let traces_json = serde_json::to_value(&summary.traces).unwrap_or(Value::Null);
        let mut outflows_json: Vec<Value> = Vec::new();
        for entry in &summary.outflows {
            let mut outflow_map = Map::new();
            for (alk, delta) in &entry.outflow {
                outflow_map
                    .insert(format!("{}:{}", alk.block, alk.tx), Value::String(delta.to_string()));
            }
            outflows_json.push(json!({
                "txid": Txid::from_byte_array(entry.txid).to_string(),
                "height": entry.height,
                "outflow": outflow_map,
            }));
        }

        let class = self.get_tx_class(txid).ok().flatten().unwrap_or_else(|| {
            let sandshrew: Vec<EspoSandshrewLikeTrace> = summary.traces.clone();
            let classified = classify_transaction(&sandshrew, &[]);
            StoredTxClass {
                tx_type: classified.tx_type,
                marketplace_info: classified.marketplace_info,
            }
        });
        let marketplace_json = class.marketplace_info.as_ref().map(|info| {
            json!({
                "marketplace_id": info.marketplace_id,
                "marketplace_name": info.marketplace_name,
                "fee_address": info.fee_address,
                "fee_sats": info.fee_sats,
            })
        });

        json!({
            "ok": true,
            "txid": txid.to_string(),
            "height": summary.height,
            "tx_type": class.tx_type.as_str(),
            "marketplace_info": marketplace_json,
            "traces": traces_json,
            "outflows": outflows_json,
        })
    }

    pub fn rpc_get_alkane_block_txs_full(
        &self,
        essentials: &EssentialsProvider,
        height: Option<u64>,
        page: Option<u64>,
        limit: Option<u64>,
        hide_diesel_mints: Option<bool>,
        tx_type: Option<&str>,
    ) -> Result<Value> {
        let Some(height) = height else {
            return Ok(json!({"ok": false, "error": "missing_or_invalid_height"}));
        };
        let page = page.unwrap_or(1).max(1) as usize;
        let limit = limit.unwrap_or(50).max(1).min(100) as usize;
        let off = limit.saturating_mul(page.saturating_sub(1));
        let hide_diesel = hide_diesel_mints.unwrap_or(false);
        let filter_tx_type = tx_type.and_then(TxType::from_str);
        let needs_filtering = hide_diesel || filter_tx_type.is_some();
        let list_id = address_index_list_id_alkane_block_txs(height);
        let raw_total = get_address_index_list_len(
            essentials,
            StateAt::Latest,
            AddressIndexListKind::AlkaneBlockTxs,
            &list_id,
        )
        .unwrap_or(0) as usize;

        let block_time = block_time_from_essentials(essentials, height as u32);

        if raw_total == 0 {
            return Ok(json!({
                "ok": true,
                "height": height,
                "block_time": block_time,
                "page": page,
                "limit": limit,
                "total": 0,
                "transactions": []
            }));
        }

        let (page_txids, total_count): (Vec<Txid>, usize) = if !needs_filtering {
            let end = (off + limit).min(raw_total);
            let ids = if end > off {
                get_address_index_list_range(
                    essentials,
                    StateAt::Latest,
                    AddressIndexListKind::AlkaneBlockTxs,
                    &list_id,
                    off as u64,
                    end as u64,
                )
                .unwrap_or_default()
            } else {
                Vec::new()
            };
            let txids: Vec<Txid> = ids
                .into_iter()
                .filter_map(|id| {
                    load_tx_pointer_blob_v3_by_id(essentials, id)
                        .map(|blob| Txid::from_byte_array(blob.txid))
                })
                .collect();
            (txids, raw_total)
        } else {
            let all_ids = get_address_index_list_range(
                essentials,
                StateAt::Latest,
                AddressIndexListKind::AlkaneBlockTxs,
                &list_id,
                0,
                raw_total as u64,
            )
            .unwrap_or_default();
            let mut filtered_txids: Vec<Txid> = Vec::new();
            for id in all_ids {
                let Some(blob) = load_tx_pointer_blob_v3_by_id(essentials, id) else {
                    continue;
                };
                let txid = Txid::from_byte_array(blob.txid);
                let class = self.get_tx_class(&txid).ok().flatten().unwrap_or_else(|| {
                    StoredTxClass {
                        tx_type: classify_transaction(&blob.traces, &[]).tx_type,
                        marketplace_info: None,
                    }
                });
                if let Some(wanted) = filter_tx_type {
                    if class.tx_type != wanted {
                        continue;
                    }
                }
                if hide_diesel && class.tx_type == TxType::DieselMint {
                    continue;
                }
                filtered_txids.push(txid);
            }
            let filtered_total = filtered_txids.len();
            let end = (off + limit).min(filtered_total);
            let page_slice = if end > off { filtered_txids[off..end].to_vec() } else { Vec::new() };
            (page_slice, filtered_total)
        };

        if page_txids.is_empty() {
            return Ok(json!({
                "ok": true,
                "height": height,
                "block_time": block_time,
                "page": page,
                "limit": limit,
                "total": total_count,
                "transactions": []
            }));
        }

        let electrum_like = get_electrum_like();
        let raw_txs = electrum_like.batch_transaction_get_raw(&page_txids).unwrap_or_default();
        let mut decoded_txs: HashMap<Txid, Transaction> = HashMap::new();
        let mut all_outpoints: Vec<(Txid, u32)> = Vec::new();
        for (idx, txid) in page_txids.iter().enumerate() {
            let raw = raw_txs.get(idx).cloned().unwrap_or_default();
            if raw.is_empty() {
                continue;
            }
            let tx: Transaction = match deserialize(&raw) {
                Ok(t) => t,
                Err(_) => continue,
            };
            for vin in &tx.input {
                if !vin.previous_output.is_null() {
                    all_outpoints.push((vin.previous_output.txid, vin.previous_output.vout));
                }
            }
            for vout in 0..tx.output.len() {
                all_outpoints.push((*txid, vout as u32));
            }
            decoded_txs.insert(*txid, tx);
        }
        all_outpoints.sort();
        all_outpoints.dedup();

        let outpoint_balances = get_outpoint_balances_with_spent_batch(
            StateAt::Latest,
            essentials,
            &all_outpoints,
        )
        .unwrap_or_default();

        let mut needs_raw_tx_data: HashSet<Txid> = HashSet::new();
        let mut prev_txids_to_fetch: HashSet<Txid> = HashSet::new();
        for txid in &page_txids {
            if let Some(summary) = load_tx_summary_v2(essentials, txid) {
                if summary.traces.is_empty() {
                    needs_raw_tx_data.insert(*txid);
                    if let Some(tx) = decoded_txs.get(txid) {
                        for vin in &tx.input {
                            if !vin.previous_output.is_null() {
                                prev_txids_to_fetch.insert(vin.previous_output.txid);
                            }
                        }
                    }
                }
            }
        }

        let prev_txs: HashMap<Txid, Transaction> = if !prev_txids_to_fetch.is_empty() {
            let prev_txid_vec: Vec<Txid> = prev_txids_to_fetch.into_iter().collect();
            let prev_raw_txs =
                electrum_like.batch_transaction_get_raw(&prev_txid_vec).unwrap_or_default();
            prev_txid_vec
                .iter()
                .enumerate()
                .filter_map(|(idx, txid)| {
                    prev_raw_txs
                        .get(idx)
                        .and_then(|raw| deserialize::<Transaction>(raw).ok())
                        .map(|tx| (*txid, tx))
                })
                .collect()
        } else {
            HashMap::new()
        };

        let network = get_network();
        let mut transactions: Vec<Value> = Vec::new();
        for txid in &page_txids {
            let Some(summary) = load_tx_summary_v2(essentials, txid) else {
                continue;
            };
            let mut row = self.summary_to_json(txid, &summary);
            if let Some(obj) = row.as_object_mut() {
                obj.insert(
                    "balance_changes".into(),
                    balance_changes_json(*txid, decoded_txs.get(txid), &outpoint_balances),
                );
                if needs_raw_tx_data.contains(txid) {
                    obj.insert(
                        "raw_tx_data".into(),
                        raw_tx_data_json(
                            decoded_txs.get(txid),
                            &prev_txs,
                            network,
                            block_time,
                        ),
                    );
                } else {
                    obj.insert("raw_tx_data".into(), Value::Null);
                }
            }
            transactions.push(row);
        }

        Ok(json!({
            "ok": true,
            "height": height,
            "block_time": block_time,
            "page": page,
            "limit": limit,
            "total": total_count,
            "transactions": transactions
        }))
    }

    pub fn rpc_get_mempool_alkane_txs_full(
        &self,
        essentials: &EssentialsProvider,
        page: Option<u64>,
        limit: Option<u64>,
        hide_diesel_mints: Option<bool>,
        next_block_only: Option<bool>,
        diesel_only: Option<bool>,
    ) -> Result<Value> {
        let page = page.unwrap_or(1).max(1) as usize;
        let limit = limit.unwrap_or(50).max(1).min(100) as usize;
        let hide_diesel = hide_diesel_mints.unwrap_or(false);
        let next_block_only = next_block_only.unwrap_or(false);
        let diesel_only = diesel_only.unwrap_or(false);
        let off = limit.saturating_mul(page.saturating_sub(1));

        let is_diesel = |tx: &MempoolBlockTx| -> bool {
            tx.traces.as_ref().map_or(false, |traces| {
                traces.len() == 1
                    && traces
                        .first()
                        .map_or(false, |t| is_diesel_mint_trace_sandshrew(&t.sandshrew_trace))
            })
        };

        if diesel_only {
            let diesel_entries: Vec<MempoolBlockTx> =
                get_mempool_index_transactions_ordered_by_block_and_fee()
                    .into_iter()
                    .filter(|tx| tx.traces.as_ref().map_or(false, |t| !t.is_empty()) && is_diesel(tx))
                    .collect();
            let diesel_total = diesel_entries.len();
            let diesel_page: Vec<MempoolBlockTx> =
                diesel_entries.into_iter().skip(off).take(limit).collect();
            if diesel_page.is_empty() {
                return Ok(json!({
                    "ok": true,
                    "diesel_mints": { "total": diesel_total, "has_more": diesel_total > off + limit, "transactions": [] }
                }));
            }
            let diesel_txs = self.mempool_txs_to_json(essentials, &diesel_page)?;
            return Ok(json!({
                "ok": true,
                "diesel_mints": {
                    "total": diesel_total,
                    "has_more": diesel_total > off + limit,
                    "transactions": diesel_txs
                }
            }));
        }

        if next_block_only {
            let template = get_mempool_block_detail(0, 1, 1, MempoolTxFilter::All, false)
                .map(|d| d.template);
            let block_txs = get_mempool_block_ordered_transactions(0).unwrap_or_default();
            let mut diesel_entries = Vec::new();
            let mut other_entries = Vec::new();
            for block_tx in block_txs {
                if block_tx.traces.as_ref().map_or(true, |t| t.is_empty()) {
                    continue;
                }
                if is_diesel(&block_tx) {
                    diesel_entries.push(block_tx);
                } else {
                    other_entries.push(block_tx);
                }
            }
            let diesel_total = diesel_entries.len();
            let other_total = other_entries.len();
            let diesel_page: Vec<_> = diesel_entries.into_iter().skip(off).take(limit).collect();
            let other_page: Vec<_> = other_entries.into_iter().skip(off).take(limit).collect();
            let block_template_json = template.as_ref().map(|template| {
                json!({
                    "index": template.index,
                    "tx_count": template.tx_count,
                    "trace_count": template.trace_count,
                    "min_fee_rate": template.min_fee_rate,
                    "median_fee_rate": template.median_fee_rate,
                    "max_fee_rate": template.max_fee_rate,
                    "fee_range": template.fee_range,
                })
            });
            let mut combined = diesel_page.clone();
            combined.extend(other_page.iter().cloned());
            let all_json = if combined.is_empty() {
                HashMap::new()
            } else {
                let rows = self.mempool_txs_to_json(essentials, &combined)?;
                combined
                    .iter()
                    .zip(rows.into_iter())
                    .map(|(tx, row)| (tx.txid, row))
                    .collect()
            };
            let diesel_txs: Vec<Value> =
                diesel_page.iter().filter_map(|tx| all_json.get(&tx.txid).cloned()).collect();
            let other_txs: Vec<Value> =
                other_page.iter().filter_map(|tx| all_json.get(&tx.txid).cloned()).collect();
            return Ok(json!({
                "ok": true,
                "block_template": block_template_json,
                "diesel_mints": {
                    "total": diesel_total,
                    "has_more": diesel_total > off + limit,
                    "transactions": diesel_txs
                },
                "other": {
                    "total": other_total,
                    "has_more": other_total > off + limit,
                    "transactions": other_txs
                }
            }));
        }

        let mut alkane_entries: Vec<MempoolBlockTx> = Vec::new();
        for entry in get_mempool_index_transactions_ordered_by_block_and_fee() {
            if entry.traces.as_ref().map_or(true, |t| t.is_empty()) {
                continue;
            }
            if hide_diesel && is_diesel(&entry) {
                continue;
            }
            alkane_entries.push(entry);
        }
        let total = alkane_entries.len();
        let has_more = total > off + limit;
        let page_entries: Vec<MempoolBlockTx> =
            alkane_entries.into_iter().skip(off).take(limit).collect();
        if page_entries.is_empty() {
            return Ok(json!({
                "ok": true,
                "page": page,
                "limit": limit,
                "total": total,
                "has_more": has_more,
                "transactions": []
            }));
        }
        let transactions = self.mempool_txs_to_json(essentials, &page_entries)?;
        Ok(json!({
            "ok": true,
            "page": page,
            "limit": limit,
            "total": total,
            "has_more": has_more,
            "transactions": transactions
        }))
    }

    fn mempool_txs_to_json(
        &self,
        essentials: &EssentialsProvider,
        entries: &[MempoolBlockTx],
    ) -> Result<Vec<Value>> {
        let mut all_outpoints: Vec<(Txid, u32)> = Vec::new();
        for entry in entries {
            for vin in &entry.tx.input {
                if !vin.previous_output.is_null() {
                    all_outpoints.push((vin.previous_output.txid, vin.previous_output.vout));
                }
            }
            for vout in 0..entry.tx.output.len() {
                all_outpoints.push((entry.txid, vout as u32));
            }
        }
        all_outpoints.sort();
        all_outpoints.dedup();
        let outpoint_balances = get_outpoint_balances_with_spent_batch(
            StateAt::Latest,
            essentials,
            &all_outpoints,
        )
        .unwrap_or_default();

        let electrum_like = get_electrum_like();
        let mut prev_txids: HashSet<Txid> = HashSet::new();
        for entry in entries {
            for vin in &entry.tx.input {
                if !vin.previous_output.is_null() {
                    prev_txids.insert(vin.previous_output.txid);
                }
            }
        }
        let prev_txs: HashMap<Txid, Transaction> = if !prev_txids.is_empty() {
            let prev_vec: Vec<Txid> = prev_txids.into_iter().collect();
            let raw = electrum_like.batch_transaction_get_raw(&prev_vec).unwrap_or_default();
            prev_vec
                .iter()
                .enumerate()
                .filter_map(|(idx, txid)| {
                    raw.get(idx).and_then(|b| deserialize::<Transaction>(b).ok()).map(|tx| (*txid, tx))
                })
                .collect()
        } else {
            HashMap::new()
        };

        let network = get_network();
        Ok(entries
            .iter()
            .map(|entry| mempool_tx_to_json(entry, &outpoint_balances, &prev_txs, network))
            .collect())
    }
}

fn mempool_tx_to_json(
    entry: &MempoolBlockTx,
    outpoint_balances: &HashMap<(Txid, u32), OutpointLookup>,
    prev_txs: &HashMap<Txid, Transaction>,
    network: Network,
) -> Value {
    let sandshrew: Vec<EspoSandshrewLikeTrace> = entry
        .traces
        .as_ref()
        .map(|traces| traces.iter().map(|t| t.sandshrew_trace.clone()).collect())
        .unwrap_or_default();
    let outputs: Vec<(String, u64)> = entry
        .tx
        .output
        .iter()
        .filter_map(|out| {
            Address::from_script(out.script_pubkey.as_script(), network)
                .ok()
                .map(|addr| (addr.to_string(), out.value.to_sat()))
        })
        .collect();
    let class = classify_transaction(&sandshrew, &outputs);
    let tx_type = if sandshrew.len() == 1
        && sandshrew.first().map_or(false, is_diesel_mint_trace_sandshrew)
    {
        "diesel_mint"
    } else if class.tx_type != TxType::Unknown {
        class.tx_type.as_str()
    } else if !sandshrew.is_empty() {
        "contract_call"
    } else {
        "transfer"
    };

    let traces_json: Vec<Value> = entry.traces.as_ref().map_or(Vec::new(), |traces| {
        traces
            .iter()
            .map(|t| {
                let events_val = prettyify_protobuf_trace_json(&t.protobuf_trace)
                    .ok()
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .unwrap_or(Value::Null);
                json!({
                    "outpoint": format!("{}:{}", entry.txid, t.outpoint.vout),
                    "events": events_val,
                })
            })
            .collect()
    });

    let position_json =
        entry.position.as_ref().map(|p| json!({ "block": p.block, "vsize": p.vsize }));

    json!({
        "txid": entry.txid.to_string(),
        "first_seen": entry.first_seen,
        "fee_rate": entry.fee_rate,
        "fee_sat": entry.fee_sat,
        "vsize": entry.vsize,
        "position": position_json,
        "tx_type": tx_type,
        "traces": traces_json,
        "balance_changes": balance_changes_json(entry.txid, Some(&entry.tx), outpoint_balances),
        "raw_tx_data": raw_tx_data_json(Some(&entry.tx), prev_txs, network, None),
    })
}

fn balances_to_json(balances: &[BalanceEntry]) -> Vec<Value> {
    balances
        .iter()
        .map(|b| {
            json!({
                "alkane": format!("{}:{}", b.alkane.block, b.alkane.tx),
                "amount": b.amount.to_string(),
            })
        })
        .collect()
}

fn balance_changes_json(
    txid: Txid,
    tx: Option<&Transaction>,
    outpoint_balances: &HashMap<(Txid, u32), OutpointLookup>,
) -> Value {
    let Some(tx) = tx else {
        return Value::Null;
    };
    let mut inputs_json: Vec<Value> = Vec::new();
    for (vin_idx, vin) in tx.input.iter().enumerate() {
        if vin.previous_output.is_null() {
            continue;
        }
        let key = (vin.previous_output.txid, vin.previous_output.vout);
        if let Some(lookup) = outpoint_balances.get(&key) {
            if !lookup.balances.is_empty() {
                inputs_json.push(json!({
                    "vin": vin_idx,
                    "outpoint": format!("{}:{}", vin.previous_output.txid, vin.previous_output.vout),
                    "address": lookup.address,
                    "balances": balances_to_json(&lookup.balances),
                }));
            }
        }
    }
    let mut outputs_json: Vec<Value> = Vec::new();
    for vout in 0..tx.output.len() {
        let key = (txid, vout as u32);
        if let Some(lookup) = outpoint_balances.get(&key) {
            if !lookup.balances.is_empty() {
                outputs_json.push(json!({
                    "vout": vout,
                    "outpoint": format!("{}:{}", txid, vout),
                    "address": lookup.address,
                    "balances": balances_to_json(&lookup.balances),
                }));
            }
        }
    }
    if inputs_json.is_empty() && outputs_json.is_empty() {
        Value::Null
    } else {
        json!({ "inputs": inputs_json, "outputs": outputs_json })
    }
}

fn raw_tx_data_json(
    tx: Option<&Transaction>,
    prev_txs: &HashMap<Txid, Transaction>,
    network: Network,
    block_time: Option<u32>,
) -> Value {
    let Some(tx) = tx else {
        return Value::Null;
    };
    let vin_json: Vec<Value> = tx
        .input
        .iter()
        .map(|vin| {
            let mut obj = json!({
                "txid": vin.previous_output.txid.to_string(),
                "vout": vin.previous_output.vout,
            });
            if !vin.witness.is_empty() {
                obj["witness"] = json!(vin.witness.iter().map(hex::encode).collect::<Vec<_>>());
            }
            if let Some(prev_tx) = prev_txs.get(&vin.previous_output.txid) {
                if let Some(prev_out) = prev_tx.output.get(vin.previous_output.vout as usize) {
                    obj["prevout_value"] = json!(prev_out.value.to_sat());
                    if let Ok(addr) =
                        Address::from_script(prev_out.script_pubkey.as_script(), network)
                    {
                        obj["prevout_address"] = json!(addr.to_string());
                    }
                }
            }
            obj
        })
        .collect();
    let vout_json: Vec<Value> = tx
        .output
        .iter()
        .map(|out| {
            let mut obj = json!({ "value": out.value.to_sat() });
            if let Ok(addr) = Address::from_script(out.script_pubkey.as_script(), network) {
                obj["address"] = json!(addr.to_string());
            }
            obj
        })
        .collect();
    let mut obj = json!({ "vin": vin_json, "vout": vout_json });
    if let Some(ts) = block_time {
        obj["block_time"] = json!(ts);
    }
    obj
}

fn block_time_from_essentials(essentials: &EssentialsProvider, height: u32) -> Option<u32> {
    essentials
        .get_block_summary(GetBlockSummaryParams { blockhash: StateAt::Latest, height })
        .ok()
        .and_then(|r| r.summary)
        .and_then(|s| {
            if s.header.len() >= 72 {
                Some(u32::from_le_bytes([s.header[68], s.header[69], s.header[70], s.header[71]]))
            } else {
                None
            }
        })
}

fn parse_timeframe(s: &str) -> Timeframe {
    match s {
        "10m" | "m10" => Timeframe::M10,
        "1h" | "h1" => Timeframe::H1,
        "4h" | "h4" => Timeframe::H4,
        "1w" | "w1" => Timeframe::W1,
        "1M" | "m1" => Timeframe::M1,
        _ => Timeframe::D1,
    }
}

fn now_ts() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn classify_block_transaction(
    traces: &[EspoSandshrewLikeTrace],
    tx: &Transaction,
    network: Network,
) -> TxClassification {
    let outputs: Vec<(String, u64)> = tx
        .output
        .iter()
        .filter_map(|out| {
            Address::from_script(out.script_pubkey.as_script(), network)
                .ok()
                .map(|addr| (addr.to_string(), out.value.to_sat()))
        })
        .collect();
    classify_transaction(traces, &outputs)
}

