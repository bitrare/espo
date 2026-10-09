//! Read-only bridge. Never opens history storage or contributes an index height.
use serde_json::{Value, json};
pub const METHODS: [&str; 6] = [
    "get_numbering_status",
    "get_asset_units",
    "get_unit",
    "get_unit_history",
    "get_address_units",
    "get_transfer_units",
];
pub async fn query(method: &'static str, params: Value) -> Value {
    let Some(config) = super::config::XcpConfig::from_global() else {
        return json!({"ok":false,"error":"xcp_disabled"});
    };
    let Some(url) = config.history_url else {
        return json!({"ok":false,"error":"numbering_not_configured"});
    };
    match tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let response: Value = ureq::post(&format!("{url}/query"))
            .timeout(std::time::Duration::from_millis(config.timeout_ms))
            .send_json(json!({"method":method,"params":params}))?
            .into_json()?;
        anyhow::ensure!(
            response.get("ok").and_then(Value::as_bool).is_some(),
            "invalid history response"
        );
        Ok(response)
    })
    .await
    {
        Ok(Ok(value)) => value,
        _ => json!({"ok":false,"error":"numbering_service_unavailable"}),
    }
}
