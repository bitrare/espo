use super::classify::is_diesel_mint_tx_sandshrew;
use super::config::FairmintsConfig;
use super::diesel::compute_and_default_diesel_stats;
use super::price::CoinGeckoPriceFeed;
use super::rpc;
use super::storage::{FairmintsProvider, classify_block_transaction};
use crate::alkanes::trace::{EspoBlock, EspoSandshrewLikeTrace};
use crate::config::get_network;
use crate::modules::ammdata::storage::AmmDataProvider;
use crate::modules::defs::{EspoModule, RpcNsRegistrar};
use crate::modules::essentials::consts::essentials_genesis_block;
use crate::modules::essentials::storage::{EssentialsProvider, GetIndexHeightParams};
use crate::runtime::mdb::Mdb;
use crate::runtime::state_at::StateAt;
use anyhow::Result;
use bitcoin::Network;
use std::collections::HashSet;
use std::sync::{Arc, RwLock};

pub struct Fairmints {
    provider: Option<Arc<FairmintsProvider>>,
    essentials_provider: Option<Arc<EssentialsProvider>>,
    amm_provider: Option<Arc<AmmDataProvider>>,
    index_height: Arc<RwLock<Option<u32>>>,
    config: FairmintsConfig,
    price_feed: Option<CoinGeckoPriceFeed>,
}

impl Fairmints {
    pub fn new() -> Self {
        Self {
            provider: None,
            essentials_provider: None,
            amm_provider: None,
            index_height: Arc::new(RwLock::new(None)),
            config: FairmintsConfig::default(),
            price_feed: None,
        }
    }

    fn provider(&self) -> &FairmintsProvider {
        self.provider.as_ref().expect("ModuleRegistry must call set_mdb()").as_ref()
    }
}

impl Default for Fairmints {
    fn default() -> Self {
        Self::new()
    }
}

impl EspoModule for Fairmints {
    fn get_name(&self) -> &'static str {
        "fairmints"
    }

    fn config_spec(&self) -> Option<&'static str> {
        Some(FairmintsConfig::spec())
    }

    fn set_config(&mut self, config: &serde_json::Value) -> Result<()> {
        self.config = FairmintsConfig::from_value(config)?;
        self.price_feed = match self.config.coingecko.clone() {
            Some(cg) => match CoinGeckoPriceFeed::new(cg) {
                Ok(feed) => {
                    eprintln!("[fairmints] CoinGecko price feed enabled");
                    Some(feed)
                }
                Err(e) => {
                    eprintln!("[fairmints] CoinGecko feed disabled: {e:#}");
                    None
                }
            },
            None => None,
        };
        Ok(())
    }

    fn set_mdb(&mut self, mdb: Arc<Mdb>) {
        let essentials_provider =
            Arc::new(EssentialsProvider::new(Arc::new(crate::config::espo_mdb(b"essentials:"))));
        let amm_provider = Arc::new(AmmDataProvider::new(
            Arc::new(crate::config::espo_mdb(b"ammdata:")),
            Arc::clone(&essentials_provider),
        ));
        self.essentials_provider = Some(Arc::clone(&essentials_provider));
        self.amm_provider = Some(amm_provider);
        let provider = FairmintsProvider::new(mdb);
        let local_height = provider.get_index_height();
        let height = match local_height {
            Ok(Some(h)) => {
                eprintln!("[FAIRMINTS] loaded index height: Some({h})");
                Some(h)
            }
            Ok(None) => {
                // New prefix on an already-indexed DB: join at essentials tip.
                // Otherwise module_resume_start_height() takes min() and replays genesis.
                match essentials_provider
                    .get_index_height(GetIndexHeightParams { blockhash: StateAt::Latest })
                {
                    Ok(res) => match res.height {
                        Some(tip) => match provider.set_index_height(tip) {
                            Ok(()) => {
                                eprintln!(
                                    "[FAIRMINTS] no local index; adopting essentials tip {tip} (skip genesis replay)"
                                );
                                Some(tip)
                            }
                            Err(e) => {
                                eprintln!("[FAIRMINTS] failed to persist adopted tip {tip}: {e:?}");
                                Some(tip)
                            }
                        },
                        None => {
                            eprintln!("[FAIRMINTS] loaded index height: None");
                            None
                        }
                    },
                    Err(e) => {
                        eprintln!("[FAIRMINTS] essentials tip lookup failed: {e:?}");
                        None
                    }
                }
            }
            Err(e) => {
                eprintln!("[FAIRMINTS] failed to load /index_height: {e:?}");
                None
            }
        };
        *self.index_height.write().unwrap() = height;
        if let Err(e) = provider.import_legacy_diesel_if_needed() {
            eprintln!("[FAIRMINTS] diesel import failed: {e:?}");
        }
        self.provider = Some(Arc::new(provider));
    }

    fn get_genesis_block(&self, network: Network) -> u32 {
        essentials_genesis_block(network)
    }

    fn index_block(&self, block: EspoBlock) -> Result<()> {
        let t0 = std::time::Instant::now();
        let height = block.height;
        if let Some(prev) = *self.index_height.read().unwrap() {
            if height <= prev {
                eprintln!("[FAIRMINTS] skipping already indexed block #{height} (last={prev})");
                return Ok(());
            }
        }

        let network = get_network();
        let block_hash = block.block_header.block_hash();
        let block_ts = block.block_header.time as u64;
        let mut classes = Vec::new();
        let mut diesel_pairs: Vec<(bitcoin::Txid, Vec<EspoSandshrewLikeTrace>)> = Vec::new();

        for tx in &block.transactions {
            let traces: Vec<EspoSandshrewLikeTrace> = tx
                .traces
                .as_ref()
                .map(|traces| traces.iter().map(|t| t.sandshrew_trace.clone()).collect())
                .unwrap_or_default();
            if traces.is_empty() && tx.traces.is_none() {
                continue;
            }
            let txid = tx.transaction.compute_txid();
            if is_diesel_mint_tx_sandshrew(&traces) {
                diesel_pairs.push((txid, traces.clone()));
            }
            let class = classify_block_transaction(&traces, &tx.transaction, network);
            if traces.is_empty() && class.tx_type == super::classify::TxType::Unknown {
                continue;
            }
            classes.push((txid, class));
        }

        let diesel_txids: HashSet<_> = diesel_pairs.into_iter().map(|(txid, _)| txid).collect();
        let diesel_stats = compute_and_default_diesel_stats(&block_hash, height, &diesel_txids);

        self.provider().commit_block(height, &classes, &diesel_stats, Some(block_ts))?;

        if let Some(feed) = &self.price_feed {
            match feed.price_scaled_at_timestamp(block.block_header.time as i64) {
                Ok(price) => {
                    if let Err(e) = self.provider().put_btc_usd(height, price) {
                        eprintln!("[fairmints] failed to store btc/usd at {height}: {e}");
                    }
                }
                Err(e) => {
                    eprintln!("[fairmints] CoinGecko price at height {height} failed: {e}");
                }
            }
        }

        *self.index_height.write().unwrap() = Some(height);
        eprintln!(
            "[indexer] module=fairmints height={} classified={} diesel_mints={} index_block done in {:?}",
            height,
            classes.len(),
            diesel_stats.mint_count,
            t0.elapsed()
        );
        Ok(())
    }

    fn get_index_height(&self) -> Option<u32> {
        *self.index_height.read().unwrap()
    }

    fn handle_reorg(&self, next_height: u32) -> Result<()> {
        let height = self.provider().get_index_height().ok().flatten();
        *self.index_height.write().unwrap() = height;
        eprintln!(
            "[fairmints] reorg rollback complete; next_height={next_height}, index height: {height:?}"
        );
        Ok(())
    }

    fn register_rpc(&self, reg: &RpcNsRegistrar) {
        let provider = self.provider.as_ref().expect("ModuleRegistry must call set_mdb()");
        let essentials =
            self.essentials_provider.as_ref().expect("ModuleRegistry must call set_mdb()");
        let amm = self.amm_provider.as_ref().expect("ModuleRegistry must call set_mdb()");
        rpc::register_rpc(
            reg.clone(),
            Arc::clone(provider),
            Arc::clone(essentials),
            Arc::clone(amm),
        );
    }
}
