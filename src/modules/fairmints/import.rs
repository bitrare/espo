use super::diesel::DieselBlockStats;
use super::storage::FairmintsProvider;
use crate::modules::ammdata::schemas::SchemaCandleV1;
use crate::modules::essentials::storage::BlockSummaryPool;
use anyhow::Result;
use borsh::BorshDeserialize;
use std::collections::HashMap;

const KEY_IMPORT_MARKER: &[u8] = b"/diesel/imported_from_rc3_v2";
const AMM_CANDLE_PREFIX: &[u8] = b"dmc1:10m:";
const ESSENTIALS_SUMMARY_PREFIX: &[u8] = b"/block_summary/";

/// rc3 essentials BlockSummary V5: stock fields plus DIESEL stats.
#[derive(Clone, Debug, BorshDeserialize)]
struct Rc3BlockSummaryV5 {
    pub height: u32,
    pub _blockhash: [u8; 32],
    pub _trace_count: u32,
    pub _interaction_count: u32,
    pub _tx_count: u32,
    pub _header: Vec<u8>,
    pub _fee_avg: f64,
    pub _fee_median: f64,
    pub _fee_range: Vec<f64>,
    pub _pool: Option<BlockSummaryPool>,
    pub diesel_mint_count: u32,
    pub diesel_total_fee_sats: u64,
    pub diesel_min_fee_rate: f64,
    pub diesel_reward_recipients: u32,
    pub diesel_distributed: u64,
    pub diesel_mint_cost_sats: u64,
}

impl FairmintsProvider {
    pub fn import_legacy_diesel_if_needed(&self) -> Result<(u64, u64)> {
        if self.mdb().get(KEY_IMPORT_MARKER)?.is_some() {
            eprintln!("[FAIRMINTS] diesel import already done; skipping");
            return Ok((0, 0));
        }

        let t0 = std::time::Instant::now();
        eprintln!("[FAIRMINTS] importing DIESEL stats/candles from rc3 essentials/ammdata…");

        let candle_puts = import_ammdata_candles(self)?;
        let stats_puts = import_essentials_diesel_stats(self)?;

        const CHUNK: usize = 4_000;
        let mut i = 0;
        while i < candle_puts.len() {
            let end = (i + CHUNK).min(candle_puts.len());
            let slice = &candle_puts[i..end];
            self.mdb().bulk_write(|wb| {
                for (k, v) in slice {
                    wb.put(k, v);
                }
            })?;
            i = end;
        }

        i = 0;
        while i < stats_puts.len() {
            let end = (i + CHUNK).min(stats_puts.len());
            let slice = &stats_puts[i..end];
            self.mdb().bulk_write(|wb| {
                for (k, v) in slice {
                    wb.put(k, v);
                }
            })?;
            i = end;
        }

        self.mdb().put(KEY_IMPORT_MARKER, &[1])?;
        eprintln!(
            "[FAIRMINTS] diesel import done: stats={} candles={} in {:?}",
            stats_puts.len(),
            candle_puts.len(),
            t0.elapsed()
        );
        Ok((stats_puts.len() as u64, candle_puts.len() as u64))
    }
}

fn import_ammdata_candles(provider: &FairmintsProvider) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let amm = crate::config::espo_mdb(b"ammdata:");
    let entries = amm.scan_prefix_entries(AMM_CANDLE_PREFIX)?;
    let mut puts = Vec::new();
    for (key, value) in entries {
        let Some(ts) = parse_trailing_decimal_u64(&key) else {
            continue;
        };
        if provider.get_candle_m10(ts)?.is_some() {
            continue;
        }
        if SchemaCandleV1::try_from_slice(&value).is_err() {
            continue;
        }
        puts.push((FairmintsProvider::candle_m10_key(ts), value));
    }
    eprintln!("[FAIRMINTS] diesel import: {} ammdata candles to copy", puts.len());
    Ok(puts)
}

fn import_essentials_diesel_stats(provider: &FairmintsProvider) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let ess_blob = crate::config::espo_mdb(b"essentials_blob:");
    let entries = ess_blob.scan_prefix_entries(ESSENTIALS_SUMMARY_PREFIX)?;
    let scanned = entries.len();
    let mut decoded = 0u64;
    let mut by_height: HashMap<u32, DieselBlockStats> = HashMap::new();
    for (_key, value) in entries {
        let Ok(summary) = Rc3BlockSummaryV5::try_from_slice(&value) else {
            continue;
        };
        decoded += 1;
        if summary.diesel_mint_count == 0 {
            continue;
        }
        by_height.entry(summary.height).or_insert(DieselBlockStats {
            mint_count: summary.diesel_mint_count,
            total_fee_sats: summary.diesel_total_fee_sats,
            min_fee_rate: summary.diesel_min_fee_rate,
            reward_recipients: summary.diesel_reward_recipients,
            distributed: summary.diesel_distributed,
            mint_cost_sats: summary.diesel_mint_cost_sats,
        });
    }

    let mut puts = Vec::new();
    for (height, stats) in by_height {
        if provider.get_diesel_stats(height)?.is_some() {
            continue;
        }
        puts.push((FairmintsProvider::diesel_stats_key(height), borsh::to_vec(&stats)?));
    }
    eprintln!(
        "[FAIRMINTS] diesel import: scanned {} blob summaries, decoded {}, {} stats to copy",
        scanned,
        decoded,
        puts.len()
    );
    Ok(puts)
}

fn parse_trailing_decimal_u64(key: &[u8]) -> Option<u64> {
    let start = key.iter().rposition(|&b| !b.is_ascii_digit()).map(|i| i + 1).unwrap_or(0);
    let digits = std::str::from_utf8(&key[start..]).ok()?;
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}
