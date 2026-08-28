use super::storage::FairmintsProvider;
use crate::modules::ammdata::storage::AmmDataProvider;
use crate::modules::defs::RpcNsRegistrar;
use crate::modules::essentials::storage::EssentialsProvider;
use serde_json::json;
use std::sync::Arc;

pub fn register_rpc(
    reg: RpcNsRegistrar,
    provider: Arc<FairmintsProvider>,
    essentials: Arc<EssentialsProvider>,
    amm: Arc<AmmDataProvider>,
) {
    eprintln!("[RPC::FAIRMINTS] registering RPC handlers…");

    {
        let reg = reg.clone();
        let provider = Arc::clone(&provider);
        tokio::spawn(async move {
            let p = Arc::clone(&provider);
            reg.register("get_known_marketplaces", move |_cx, _payload| {
                let p = Arc::clone(&p);
                async move { p.rpc_get_known_marketplaces() }
            })
            .await;
        });
    }

    {
        let reg = reg.clone();
        let provider = Arc::clone(&provider);
        tokio::spawn(async move {
            let p = Arc::clone(&provider);
            reg.register("get_tx_types", move |_cx, _payload| {
                let p = Arc::clone(&p);
                async move { p.rpc_get_tx_types() }
            })
            .await;
        });
    }

    {
        let reg = reg.clone();
        let provider = Arc::clone(&provider);
        let essentials = Arc::clone(&essentials);
        tokio::spawn(async move {
            let p = Arc::clone(&provider);
            let e = Arc::clone(&essentials);
            reg.register("get_block_summary", move |_cx, payload| {
                let p = Arc::clone(&p);
                let e = Arc::clone(&e);
                async move {
                    p.rpc_get_block_summary(e.as_ref(), payload.get("height").and_then(|v| v.as_u64()))
                }
            })
            .await;
        });
    }

    {
        let reg = reg.clone();
        let provider = Arc::clone(&provider);
        let essentials = Arc::clone(&essentials);
        tokio::spawn(async move {
            let p = Arc::clone(&provider);
            let e = Arc::clone(&essentials);
            reg.register("get_alkane_block_txs", move |_cx, payload| {
                let p = Arc::clone(&p);
                let e = Arc::clone(&e);
                async move {
                    p.rpc_get_alkane_block_txs(
                        e.as_ref(),
                        payload.get("height").and_then(|v| v.as_u64()),
                        payload.get("page").and_then(|v| v.as_u64()),
                        payload.get("limit").and_then(|v| v.as_u64()),
                        payload.get("hide_diesel_mints").and_then(|v| v.as_bool()),
                        payload.get("tx_type").and_then(|v| v.as_str()),
                    )
                    .unwrap_or_else(|err| json!({"ok": false, "error": err.to_string()}))
                }
            })
            .await;
        });
    }

    {
        let reg = reg.clone();
        let provider = Arc::clone(&provider);
        tokio::spawn(async move {
            let p = Arc::clone(&provider);
            reg.register("get_diesel_block_stats", move |_cx, payload| {
                let p = Arc::clone(&p);
                async move {
                    p.rpc_get_diesel_block_stats(payload.get("height").and_then(|v| v.as_u64()))
                }
            })
            .await;
        });
    }

    {
        let reg = reg.clone();
        let provider = Arc::clone(&provider);
        tokio::spawn(async move {
            let p = Arc::clone(&provider);
            reg.register("get_diesel_mint_cost_candles", move |_cx, payload| {
                let p = Arc::clone(&p);
                async move {
                    p.rpc_get_diesel_mint_cost_candles(
                        payload.get("timeframe").and_then(|v| v.as_str()),
                        payload.get("limit").and_then(|v| v.as_u64()),
                        payload.get("page").and_then(|v| v.as_u64()),
                        payload.get("now").and_then(|v| v.as_u64()),
                    )
                    .unwrap_or_else(|e| json!({"ok": false, "error": e.to_string()}))
                }
            })
            .await;
        });
    }

    {
        let reg = reg.clone();
        let provider = Arc::clone(&provider);
        let amm = Arc::clone(&amm);
        tokio::spawn(async move {
            let p = Arc::clone(&provider);
            let a = Arc::clone(&amm);
            reg.register("get_btc_usd_price", move |_cx, payload| {
                let p = Arc::clone(&p);
                let a = Arc::clone(&a);
                async move {
                    p.rpc_get_btc_usd_price(a.as_ref(), payload.get("height").and_then(|v| v.as_u64()))
                }
            })
            .await;
        });
    }

    {
        let reg = reg.clone();
        let provider = Arc::clone(&provider);
        let essentials = Arc::clone(&essentials);
        tokio::spawn(async move {
            let p = Arc::clone(&provider);
            let e = Arc::clone(&essentials);
            reg.register("check_outpoints_spent", move |_cx, payload| {
                let p = Arc::clone(&p);
                let e = Arc::clone(&e);
                async move {
                    let outpoints = payload.get("outpoints").and_then(|v| v.as_array()).map(|arr| {
                        arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()
                    });
                    p.rpc_check_outpoints_spent(e.as_ref(), outpoints)
                }
            })
            .await;
        });
    }

    {
        let reg = reg.clone();
        let provider = Arc::clone(&provider);
        let essentials = Arc::clone(&essentials);
        tokio::spawn(async move {
            let p = Arc::clone(&provider);
            let e = Arc::clone(&essentials);
            reg.register("get_alkane_tx_summary", move |_cx, payload| {
                let p = Arc::clone(&p);
                let e = Arc::clone(&e);
                async move {
                    p.rpc_get_alkane_tx_summary(e.as_ref(), payload.get("txid").and_then(|v| v.as_str()))
                }
            })
            .await;
        });
    }

    {
        let reg = reg.clone();
        let provider = Arc::clone(&provider);
        let essentials = Arc::clone(&essentials);
        tokio::spawn(async move {
            let p = Arc::clone(&provider);
            let e = Arc::clone(&essentials);
            reg.register("get_alkane_block_txs_full", move |_cx, payload| {
                let p = Arc::clone(&p);
                let e = Arc::clone(&e);
                async move {
                    p.rpc_get_alkane_block_txs_full(
                        e.as_ref(),
                        payload.get("height").and_then(|v| v.as_u64()),
                        payload.get("page").and_then(|v| v.as_u64()),
                        payload.get("limit").and_then(|v| v.as_u64()),
                        payload.get("hide_diesel_mints").and_then(|v| v.as_bool()),
                        payload.get("tx_type").and_then(|v| v.as_str()),
                    )
                    .unwrap_or_else(|err| json!({"ok": false, "error": err.to_string()}))
                }
            })
            .await;
        });
    }

    {
        let provider = Arc::clone(&provider);
        let essentials = Arc::clone(&essentials);
        tokio::spawn(async move {
            let p = Arc::clone(&provider);
            let e = Arc::clone(&essentials);
            reg.register("get_mempool_alkane_txs_full", move |_cx, payload| {
                let p = Arc::clone(&p);
                let e = Arc::clone(&e);
                async move {
                    p.rpc_get_mempool_alkane_txs_full(
                        e.as_ref(),
                        payload.get("page").and_then(|v| v.as_u64()),
                        payload.get("limit").and_then(|v| v.as_u64()),
                        payload.get("hide_diesel_mints").and_then(|v| v.as_bool()),
                        payload.get("next_block_only").and_then(|v| v.as_bool()),
                        payload.get("diesel_only").and_then(|v| v.as_bool()),
                    )
                    .unwrap_or_else(|err| json!({"ok": false, "error": err.to_string()}))
                }
            })
            .await;
        });
    }
}
