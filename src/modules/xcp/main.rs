use super::config::XcpConfig;
use super::rpc;
use crate::alkanes::trace::EspoBlock;
use crate::modules::defs::{EspoModule, RpcNsRegistrar};
use crate::modules::essentials::consts::essentials_genesis_block;
use crate::modules::essentials::storage::{EssentialsProvider, GetIndexHeightParams};
use crate::runtime::mdb::Mdb;
use crate::runtime::state_at::StateAt;
use anyhow::Result;
use bitcoin::Network;
use std::sync::{Arc, RwLock};

pub struct Xcp {
    essentials: Option<Arc<EssentialsProvider>>,
    index_height: Arc<RwLock<Option<u32>>>,
}

impl Xcp {
    pub fn new() -> Self {
        Self { essentials: None, index_height: Arc::new(RwLock::new(None)) }
    }

    fn adopt_essentials_tip(&self) -> Option<u32> {
        let tip = self.essentials.as_ref().and_then(|essentials| {
            essentials
                .get_index_height(GetIndexHeightParams { blockhash: StateAt::Latest })
                .ok()
                .and_then(|result| result.height)
        });
        if let Some(height) = tip {
            *self.index_height.write().unwrap() = Some(height);
        }
        tip
    }
}

impl Default for Xcp {
    fn default() -> Self {
        Self::new()
    }
}

impl EspoModule for Xcp {
    fn get_name(&self) -> &'static str {
        "xcp"
    }

    fn set_mdb(&mut self, _mdb: Arc<Mdb>) {
        let essentials =
            Arc::new(EssentialsProvider::new(Arc::new(crate::config::espo_mdb(b"essentials:"))));
        self.essentials = Some(essentials);
        match self.adopt_essentials_tip() {
            Some(tip) => {
                eprintln!("[xcp] no-op index; adopting essentials tip {tip}");
            }
            None => {
                eprintln!(
                    "[xcp] essentials tip unavailable; get_index_height will stay unset until a block is seen"
                );
            }
        }
    }

    fn get_genesis_block(&self, network: Network) -> u32 {
        essentials_genesis_block(network)
    }

    fn index_block(&self, block: EspoBlock) -> Result<()> {
        if let Some(prev) = *self.index_height.read().unwrap() {
            if block.height <= prev {
                return Ok(());
            }
        }
        *self.index_height.write().unwrap() = Some(block.height);
        Ok(())
    }

    fn get_index_height(&self) -> Option<u32> {
        if let Some(height) = *self.index_height.read().unwrap() {
            return Some(height);
        }
        if let Some(tip) = self.adopt_essentials_tip() {
            return Some(tip);
        }
        // Never return None: module_resume_start_height maps None to genesis
        // and would rewind the view-only explorer tip.
        Some(u32::MAX - 1)
    }

    fn handle_reorg(&self, next_height: u32) -> Result<()> {
        if next_height == 0 {
            *self.index_height.write().unwrap() = None;
        } else {
            *self.index_height.write().unwrap() = Some(next_height.saturating_sub(1));
        }
        let _ = self.adopt_essentials_tip();
        Ok(())
    }

    fn register_rpc(&self, reg: &RpcNsRegistrar) {
        rpc::register_rpc(reg.clone());
    }

    fn config_spec(&self) -> Option<&'static str> {
        Some(XcpConfig::spec())
    }

    fn set_config(&mut self, config: &serde_json::Value) -> Result<()> {
        let parsed = XcpConfig::from_value(config)?;
        eprintln!(
            "[xcp] Counterparty Core {} (timeout {}ms)",
            parsed.counterparty_api_url, parsed.timeout_ms
        );
        Ok(())
    }
}
