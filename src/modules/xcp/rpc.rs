use crate::modules::defs::RpcNsRegistrar;
use serde_json::json;

pub fn register_rpc(reg: RpcNsRegistrar) {
    eprintln!("[RPC::XCP] registering RPC handlers…");
    tokio::spawn(async move {
        reg.register("get_tx", move |_cx, payload| async move {
            let Some(txid) = payload
                .get("txid")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return json!({ "ok": false, "error": "txid required" });
            };
            let txid = txid.to_string();
            let lookup = txid.clone();
            match tokio::task::spawn_blocking(move || super::core::fetch_transaction(&lookup)).await
            {
                Ok(Some(transaction)) => {
                    let view = super::display::view_from_core(&transaction)
                        .map(|decoded| decoded.to_api())
                        .unwrap_or(json!(null));
                    json!({
                        "ok": true,
                        "txid": transaction.get("tx_hash").cloned().unwrap_or(json!(txid)),
                        "transaction": transaction,
                        "view": view,
                    })
                }
                Ok(None) => json!({ "ok": false, "txid": txid, "error": "not_found" }),
                Err(e) => json!({ "ok": false, "txid": txid, "error": e.to_string() }),
            }
        })
        .await;
        reg.register("get_asset", move |_cx, payload| async move {
            let Some(asset) = payload
                .get("asset")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return json!({ "ok": false, "error": "asset required" });
            };
            let asset = asset.to_string();
            let lookup = asset.clone();
            match tokio::task::spawn_blocking(move || super::core::fetch_asset(&lookup)).await {
                Ok(super::core::CoreFetch::Ok(value)) => {
                    let name = super::core::asset_name_from_value(&value)
                        .unwrap_or_else(|| asset.clone());
                    let holders = {
                        let asset_name = name.clone();
                        tokio::task::spawn_blocking(move || {
                            super::core::fetch_asset_list(&asset_name, "holders", "", 0, 1)
                        })
                        .await
                        .ok()
                        .and_then(|list| list.ok())
                        .map(|list| list.total)
                    };
                    json!({
                        "ok": true,
                        "asset": name,
                        "holders": holders,
                        "result": value,
                    })
                }
                Ok(super::core::CoreFetch::NotFound) => {
                    json!({ "ok": false, "asset": asset, "error": "not_found" })
                }
                Ok(super::core::CoreFetch::Unreachable) => {
                    json!({ "ok": false, "asset": asset, "error": "unreachable" })
                }
                Err(e) => json!({ "ok": false, "asset": asset, "error": e.to_string() }),
            }
        })
        .await;
        reg.register("get_address_balances", move |_cx, payload| async move {
            let Some(address) = payload
                .get("address")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return json!({ "ok": false, "error": "address required" });
            };
            let address = address.to_string();
            let asset = payload
                .get("asset")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            let lookup = address.clone();
            let filter = asset.clone();
            match tokio::task::spawn_blocking(move || {
                super::core::fetch_address_balances_filtered(&lookup, filter.as_deref())
            })
            .await
            {
                Ok(super::core::CoreFetch::Ok(items)) => {
                    let balances = items
                        .iter()
                        .map(|item| (item.asset.clone(), json!(item.quantity_normalized)))
                        .collect::<serde_json::Map<String, serde_json::Value>>();
                    json!({
                        "ok": true,
                        "address": address,
                        "asset": asset,
                        "balances": balances,
                        "items": items.iter().map(super::core::AddressAssetBalance::to_api).collect::<Vec<_>>(),
                    })
                }
                Ok(super::core::CoreFetch::NotFound) => {
                    json!({ "ok": true, "address": address, "asset": asset, "balances": {}, "items": [] })
                }
                Ok(super::core::CoreFetch::Unreachable) => {
                    json!({ "ok": false, "address": address, "error": "unreachable" })
                }
                Err(e) => json!({ "ok": false, "address": address, "error": e.to_string() }),
            }
        })
        .await;
    });
}
