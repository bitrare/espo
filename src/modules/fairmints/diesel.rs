use anyhow::{Result, anyhow};
use bitcoin::{BlockHash, Txid};
use bitcoincore_rpc::RpcApi;
use borsh::{BorshDeserialize, BorshSerialize};
use serde_json::json;
use std::collections::HashSet;
use std::str::FromStr;

use crate::config::get_bitcoind_rpc_client;
use crate::utils::fee_rates::fee_rate_entry_from_weight_and_btc_fee;

pub const DIESEL_BASE_REWARD: u64 = 312_500_000;
pub const DIESEL_SHARED_MINT_FORK_HEIGHT: u32 = 909_862;
pub const PRICE_SCALE: u128 = 10_000_000_000_000_000;

#[derive(Clone, Debug, Default, BorshSerialize, BorshDeserialize)]
pub struct DieselBlockStats {
    pub mint_count: u32,
    pub total_fee_sats: u64,
    pub min_fee_rate: f64,
    pub reward_recipients: u32,
    pub distributed: u64,
    pub mint_cost_sats: u64,
}

#[derive(Clone, Copy, Debug, Default, BorshSerialize, BorshDeserialize)]
pub struct MintCostCandle {
    pub open: u128,
    pub high: u128,
    pub low: u128,
    pub close: u128,
    pub volume: u128,
}

#[derive(serde::Deserialize)]
struct VerboseBlockForDieselStats {
    tx: Vec<VerboseBlockTxForDieselStats>,
}

#[derive(serde::Deserialize)]
struct VerboseBlockTxForDieselStats {
    txid: String,
    weight: u64,
    fee: Option<f64>,
}

pub fn compute_diesel_block_stats(
    blockhash: &BlockHash,
    height: u32,
    diesel_txids: &HashSet<Txid>,
) -> Result<DieselBlockStats> {
    if diesel_txids.is_empty() {
        return Ok(DieselBlockStats::default());
    }

    let rpc = get_bitcoind_rpc_client();
    let block: VerboseBlockForDieselStats = rpc
        .call("getblock", &[json!(blockhash.to_string()), json!(3)])
        .map_err(|e| anyhow!("bitcoind getblock({blockhash}, 3) for diesel stats failed: {e}"))?;

    let mut total_fee_sats: u64 = 0;
    let mut min_fee_sats: u64 = u64::MAX;
    let mut min_fee_rate: f64 = f64::MAX;
    let mut mint_count: u32 = 0;

    for tx in &block.tx {
        let txid = match Txid::from_str(&tx.txid) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if !diesel_txids.contains(&txid) {
            continue;
        }
        mint_count += 1;

        let fee_sat = tx.fee.map(|f| (f * 100_000_000.0) as u64).unwrap_or(0);
        total_fee_sats += fee_sat;
        if fee_sat < min_fee_sats {
            min_fee_sats = fee_sat;
        }
        if let Some(entry) = fee_rate_entry_from_weight_and_btc_fee(tx.weight, tx.fee) {
            if entry.rate < min_fee_rate {
                min_fee_rate = entry.rate;
            }
        }
    }

    if min_fee_rate == f64::MAX {
        min_fee_rate = 0.0;
    }
    if min_fee_sats == u64::MAX {
        min_fee_sats = 0;
    }

    let (reward_recipients, distributed) =
        if mint_count > 0 { (mint_count, DIESEL_BASE_REWARD) } else { (0, 0) };

    let mint_cost_sats = if distributed > 0 {
        if height < DIESEL_SHARED_MINT_FORK_HEIGHT {
            (total_fee_sats as u128 * 100_000_000 / distributed as u128) as u64
        } else {
            (min_fee_sats as u128 * mint_count as u128 * 100_000_000 / distributed as u128) as u64
        }
    } else {
        0
    };

    Ok(DieselBlockStats {
        mint_count,
        total_fee_sats,
        min_fee_rate,
        reward_recipients,
        distributed,
        mint_cost_sats,
    })
}

pub fn mint_cost_scaled(stats: &DieselBlockStats) -> u128 {
    (stats.mint_cost_sats as u128).saturating_mul(PRICE_SCALE).saturating_div(100_000_000)
}

pub fn mint_volume_scaled(stats: &DieselBlockStats) -> u128 {
    (stats.total_fee_sats as u128).saturating_mul(PRICE_SCALE).saturating_div(100_000_000)
}

pub fn compute_and_default_diesel_stats(
    blockhash: &BlockHash,
    height: u32,
    diesel_txids: &HashSet<Txid>,
) -> DieselBlockStats {
    compute_diesel_block_stats(blockhash, height, diesel_txids).unwrap_or_else(|e| {
        eprintln!("[fairmints] failed to compute diesel stats for block {height}: {e}");
        DieselBlockStats::default()
    })
}
