use super::decode::decode_tx;
use bitcoin::Transaction;
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XcpTransfer {
    pub address: String,
    pub asset: String,
    pub amount: String,
    pub incoming: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XcpAction {
    pub method: String,
    pub headline: String,
    pub asset: String,
    pub amount: Option<String>,
    pub btc_total: Option<String>,
    pub rate_btc: Option<String>,
    pub success: bool,
    pub status: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub buyer: Option<String>,
    pub seller: Option<String>,
    pub dispenser_tx: Option<String>,
    pub machine_closed: bool,
}

#[derive(Clone, Debug)]
pub struct XcpView {
    pub raw: Value,
    pub events: Value,
    pub actions: Vec<XcpAction>,
    pub transfers: Vec<XcpTransfer>,
}

impl XcpView {
    pub fn to_api(&self) -> Value {
        json!({
            "headline": self.actions.first().map(|a| a.headline.clone()),
            "actions": self.actions.iter().map(XcpAction::to_api).collect::<Vec<_>>(),
            "transfers": self.transfers.iter().map(|t| json!({
                "address": t.address,
                "asset": t.asset,
                "amount": t.amount,
                "incoming": t.incoming,
            })).collect::<Vec<_>>(),
            "events": self.events,
        })
    }
}

impl XcpAction {
    pub fn to_api(&self) -> Value {
        json!({
            "method": self.method,
            "headline": self.headline,
            "asset": self.asset,
            "amount": self.amount,
            "btc_total": self.btc_total,
            "rate_btc": self.rate_btc,
            "status": self.status,
            "success": self.success,
            "from": self.from,
            "to": self.to,
            "buyer": self.buyer,
            "seller": self.seller,
            "dispenser_tx": self.dispenser_tx,
            "machine_closed": self.machine_closed,
        })
    }
}

pub fn for_tx(tx: &Transaction, core: Option<&Value>) -> Option<Value> {
    view(tx, core).map(|decoded| decoded.raw)
}

pub fn view(tx: &Transaction, core: Option<&Value>) -> Option<XcpView> {
    let decoded = decode_tx(tx);
    if decoded.is_none() && !core_is_xcp(core) {
        return None;
    }
    Some(assemble(core, decoded.as_ref().map(|d| d.message_name)))
}

pub fn view_from_core(core: &Value) -> Option<XcpView> {
    if !core_is_xcp(Some(core)) {
        return None;
    }
    Some(assemble(Some(core), None))
}

fn assemble(core: Option<&Value>, decoded_name: Option<&str>) -> XcpView {
    let message_name = core
        .and_then(|c| c.get("transaction_type"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or(decoded_name)
        .unwrap_or("unknown");
    let valid = core.and_then(|c| c.get("valid")).and_then(|v| v.as_bool());
    let (mut actions, transfers) = movements_from_core(core, valid);
    if actions.is_empty() {
        actions.push(generic_action(message_name, core, valid));
    }
    let summary = actions
        .first()
        .map(|a| a.headline.clone())
        .unwrap_or_else(|| title_case(message_name));
    let events = core.and_then(|c| c.get("events")).cloned().unwrap_or(json!([]));
    let raw = json!({
        "protocol": "counterparty",
        "message_name": message_name,
        "valid": core.and_then(|c| c.get("valid")).cloned(),
        "source": core.and_then(|c| c.get("source")).cloned(),
        "destination": core.and_then(|c| c.get("destination")).cloned(),
        "btc_amount": core.and_then(|c| c.get("btc_amount")).cloned(),
        "btc_amount_normalized": core.and_then(|c| c.get("btc_amount_normalized")).cloned(),
        "tx_index": core.and_then(|c| c.get("tx_index")).cloned(),
        "block_index": core.and_then(|c| c.get("block_index")).cloned(),
        "summary": summary,
        "events": events,
    });
    XcpView { raw, events, actions, transfers }
}

fn generic_action(message_name: &str, core: Option<&Value>, valid: Option<bool>) -> XcpAction {
    XcpAction {
        method: message_name.to_string(),
        headline: title_case(message_name),
        asset: first_asset_from_core(core),
        amount: None,
        btc_total: None,
        rate_btc: None,
        success: valid.unwrap_or(true),
        status: fallback_status(message_name, valid),
        from: core.and_then(|c| string_field(c, "source")),
        to: core.and_then(|c| string_field(c, "destination")),
        buyer: None,
        seller: None,
        dispenser_tx: None,
        machine_closed: false,
    }
}

pub fn summary_label(xcp: &Value) -> String {
    xcp.get("summary")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| xcp.get("message_name").and_then(|v| v.as_str()).map(|name| title_case(name)))
        .unwrap_or_else(|| "Counterparty".to_string())
}

pub fn transfers_for_action<'a>(
    action: &XcpAction,
    transfers: &'a [XcpTransfer],
) -> Vec<&'a XcpTransfer> {
    let swap = action.method == "pool_swap";
    let mut rows: Vec<&XcpTransfer> = transfers
        .iter()
        .filter(|transfer| {
            if !swap && !action.asset.is_empty() && transfer.asset != action.asset {
                return false;
            }
            action.from.as_deref() == Some(transfer.address.as_str())
                || action.to.as_deref() == Some(transfer.address.as_str())
                || action.buyer.as_deref() == Some(transfer.address.as_str())
                || action.seller.as_deref() == Some(transfer.address.as_str())
                || (action.from.is_none() && action.to.is_none())
        })
        .collect();
    rows.sort_by_key(|transfer| !transfer.incoming);
    rows
}

fn movements_from_core(
    core: Option<&Value>,
    valid: Option<bool>,
) -> (Vec<XcpAction>, Vec<XcpTransfer>) {
    let Some(events) = core.and_then(|c| c.get("events")).and_then(|v| v.as_array()) else {
        return (Vec::new(), Vec::new());
    };
    let success = valid.unwrap_or(true);
    let has_pool_match = events
        .iter()
        .any(|event| event.get("event").and_then(|v| v.as_str()) == Some("POOL_MATCH"));
    let mut closed_machines: HashMap<String, bool> = HashMap::new();
    for event in events {
        if event.get("event").and_then(|v| v.as_str()) != Some("DISPENSER_UPDATE") {
            continue;
        }
        let params = event.get("params").unwrap_or(&Value::Null);
        let Some(tx_hash) = string_field(params, "tx_hash") else { continue };
        let remaining = params.get("give_remaining").and_then(|v| v.as_u64()).unwrap_or(1);
        let status = params.get("status").and_then(|v| v.as_u64()).unwrap_or(0);
        closed_machines.insert(tx_hash, remaining == 0 || status == 10);
    }

    let mut actions = Vec::new();
    let mut transfers = Vec::new();
    for event in events {
        let kind = event.get("event").and_then(|v| v.as_str()).unwrap_or("");
        let params = event.get("params").unwrap_or(&Value::Null);
        let asset = asset_name_from_params(params).unwrap_or_default();
        let qty = normalized_qty(params);
        let source = string_field(params, "source");
        let destination =
            string_field(params, "destination").or_else(|| string_field(params, "address"));
        match kind {
            "DISPENSE" => {
                let btc_sats = int_field(params, "btc_amount")
                    .or_else(|| core.and_then(|c| int_field(c, "btc_amount")));
                let btc_total = string_field(params, "btc_amount_normalized")
                    .map(|s| trim_qty(&s))
                    .or_else(|| btc_sats.map(format_btc_sats));
                let base_units = int_field(params, "dispense_quantity");
                let rate_btc = match (btc_sats, base_units) {
                    (Some(sats), Some(units)) => rate_btc_from_sats(sats, units),
                    _ => None,
                };
                let dispenser_tx = string_field(params, "dispenser_tx_hash");
                let machine_closed = dispenser_tx
                    .as_ref()
                    .and_then(|txid| closed_machines.get(txid).copied())
                    .unwrap_or(false);
                let headline = match (qty.as_deref(), asset.as_str(), btc_total.as_deref()) {
                    (Some(amount), asset, Some(btc)) if !asset.is_empty() => {
                        format!("{amount} {asset} bought from a dispenser for {btc} BTC")
                    }
                    (Some(amount), asset, _) if !asset.is_empty() => {
                        format!("{amount} {asset} bought from a dispenser")
                    }
                    _ => "Dispense".to_string(),
                };
                actions.push(XcpAction {
                    method: "dispense".to_string(),
                    headline,
                    asset: asset.clone(),
                    amount: qty.clone(),
                    btc_total,
                    rate_btc,
                    success,
                    status: if success { "Settled".to_string() } else { "Invalid".to_string() },
                    from: source.clone(),
                    to: destination.clone(),
                    buyer: destination.clone(),
                    seller: source.clone(),
                    dispenser_tx,
                    machine_closed,
                });
                push_pair(&mut transfers, source, destination, asset, qty);
            }
            "ENHANCED_SEND" | "SEND" | "MPMA_SEND" => {
                let method = if kind == "MPMA_SEND" { "mpma_send" } else { "send" };
                let headline = match (qty.as_deref(), asset.as_str()) {
                    (Some(amount), asset) if !asset.is_empty() => format!("{amount} {asset} sent"),
                    _ => title_case(method),
                };
                actions.push(XcpAction {
                    method: method.to_string(),
                    headline,
                    asset: asset.clone(),
                    amount: qty.clone(),
                    btc_total: None,
                    rate_btc: None,
                    success,
                    status: if success { "Settled".to_string() } else { "Invalid".to_string() },
                    from: source.clone(),
                    to: destination.clone(),
                    buyer: destination.clone(),
                    seller: source.clone(),
                    dispenser_tx: None,
                    machine_closed: false,
                });
                push_pair(&mut transfers, source, destination, asset, qty);
            }
            "POOL_MATCH" => {
                if let Some((action, legs)) = pool_match_movements(params, success) {
                    actions.push(action);
                    transfers.extend(legs);
                }
            }
            "CREDIT" => {
                if has_pool_match {
                    continue;
                }
                if let (Some(dest), Some(amount)) = (destination.or(source), qty) {
                    if !asset.is_empty() {
                        transfers.push(XcpTransfer {
                            address: dest,
                            asset,
                            amount,
                            incoming: true,
                        });
                    }
                }
            }
            "DEBIT" => {
                if has_pool_match {
                    continue;
                }
                if let (Some(src), Some(amount)) = (source.or(destination), qty) {
                    if !asset.is_empty() {
                        transfers.push(XcpTransfer {
                            address: src,
                            asset,
                            amount,
                            incoming: false,
                        });
                    }
                }
            }
            "ISSUANCE" => {
                let headline = match (qty.as_deref(), asset.as_str()) {
                    (Some(amount), asset) if !asset.is_empty() => {
                        format!("{amount} {asset} issued")
                    }
                    (_, asset) if !asset.is_empty() => format!("{asset} issued"),
                    _ => "Issuance".to_string(),
                };
                actions.push(XcpAction {
                    method: "issuance".to_string(),
                    headline,
                    asset: asset.clone(),
                    amount: qty.clone(),
                    btc_total: None,
                    rate_btc: None,
                    success,
                    status: if success { "Settled".to_string() } else { "Invalid".to_string() },
                    from: None,
                    to: source.clone(),
                    buyer: source.clone(),
                    seller: None,
                    dispenser_tx: None,
                    machine_closed: false,
                });
                if let (Some(dest), Some(amount)) = (source, qty) {
                    if !asset.is_empty() {
                        transfers.push(XcpTransfer {
                            address: dest,
                            asset,
                            amount,
                            incoming: true,
                        });
                    }
                }
            }
            "FAIRMINT" => {
                let dest = destination.or(source);
                let headline = match (qty.as_deref(), asset.as_str()) {
                    (Some(amount), asset) if !asset.is_empty() => {
                        format!("{amount} {asset} minted")
                    }
                    _ => "Fairmint".to_string(),
                };
                actions.push(XcpAction {
                    method: "fairmint".to_string(),
                    headline,
                    asset: asset.clone(),
                    amount: qty.clone(),
                    btc_total: None,
                    rate_btc: None,
                    success,
                    status: if success { "Settled".to_string() } else { "Invalid".to_string() },
                    from: None,
                    to: dest.clone(),
                    buyer: dest.clone(),
                    seller: None,
                    dispenser_tx: None,
                    machine_closed: false,
                });
                if let (Some(dest), Some(amount)) = (dest, qty) {
                    if !asset.is_empty() {
                        transfers.push(XcpTransfer {
                            address: dest,
                            asset,
                            amount,
                            incoming: true,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    (actions, transfers)
}

fn pool_match_movements(params: &Value, success: bool) -> Option<(XcpAction, Vec<XcpTransfer>)> {
    let forward = string_field(params, "forward_asset")?;
    let backward = string_field(params, "backward_asset")?;
    let forward_qty = string_field(params, "forward_quantity_normalized").map(|s| trim_qty(&s))?;
    let backward_qty =
        string_field(params, "backward_quantity_normalized").map(|s| trim_qty(&s))?;
    let source = string_field(params, "source")?;
    let bought_token = is_quote_name(&backward) && !is_quote_name(&forward);
    let sold_token = is_quote_name(&forward) && !is_quote_name(&backward);
    let mut headline = if bought_token {
        format!("Bought {forward_qty} {forward} with {backward_qty} {backward}")
    } else if sold_token {
        format!("Sold {backward_qty} {backward} for {forward_qty} {forward}")
    } else {
        format!("Swapped {backward_qty} {backward} for {forward_qty} {forward}")
    };
    if let Some(fee) = string_field(params, "fee_quantity_normalized").map(|s| trim_qty(&s)) {
        if fee != "0" {
            // AMM fee is taken from the inbound (backward) leg, not always XCP.
            let fee_asset = string_field(params, "fee_asset").unwrap_or_else(|| backward.clone());
            headline.push_str(&format!(" · {fee} {fee_asset} pool fee"));
        }
    }
    let token = if !is_quote_name(&forward) { forward.clone() } else { backward.clone() };
    let action = XcpAction {
        method: "pool_swap".to_string(),
        headline,
        asset: token,
        amount: None,
        btc_total: None,
        rate_btc: None,
        success,
        status: if success { "Settled".to_string() } else { "Invalid".to_string() },
        from: Some(source.clone()),
        to: None,
        buyer: bought_token.then(|| source.clone()),
        seller: sold_token.then(|| source.clone()),
        dispenser_tx: None,
        machine_closed: false,
    };
    let transfers = vec![
        XcpTransfer {
            address: source.clone(),
            asset: forward,
            amount: forward_qty,
            incoming: true,
        },
        XcpTransfer { address: source, asset: backward, amount: backward_qty, incoming: false },
    ];
    Some((action, transfers))
}

fn is_quote_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("XCP") || name.eq_ignore_ascii_case("BTC")
}

fn push_pair(
    transfers: &mut Vec<XcpTransfer>,
    source: Option<String>,
    destination: Option<String>,
    asset: String,
    qty: Option<String>,
) {
    let Some(amount) = qty else { return };
    if asset.is_empty() {
        return;
    }
    if let Some(src) = source {
        transfers.push(XcpTransfer {
            address: src,
            asset: asset.clone(),
            amount: amount.clone(),
            incoming: false,
        });
    }
    if let Some(dest) = destination {
        transfers.push(XcpTransfer { address: dest, asset, amount, incoming: true });
    }
}

fn first_asset_from_core(core: Option<&Value>) -> String {
    if let Some(direct) = core.and_then(asset_name_from_params) {
        return direct;
    }
    let Some(events) = core.and_then(|c| c.get("events")).and_then(|v| v.as_array()) else {
        return String::new();
    };
    for event in events {
        let params = event.get("params").unwrap_or(&Value::Null);
        if let Some(asset) = asset_name_from_params(params) {
            return asset;
        }
    }
    String::new()
}

fn asset_name_from_params(params: &Value) -> Option<String> {
    const KEYS: [&str; 6] =
        ["asset", "get_asset", "give_asset", "forward_asset", "asset_a", "asset_b"];
    let mut fallback = None;
    for key in KEYS {
        let Some(name) = string_field(params, key) else {
            continue;
        };
        if name.is_empty() || name.eq_ignore_ascii_case("BTC") {
            continue;
        }
        if name.eq_ignore_ascii_case("XCP") {
            fallback = fallback.or(Some(name));
            continue;
        }
        return Some(name);
    }
    fallback
}

fn normalized_qty(params: &Value) -> Option<String> {
    const KEYS: [&str; 4] = [
        "quantity_normalized",
        "dispense_quantity_normalized",
        "give_normalized",
        "get_normalized",
    ];
    for key in KEYS {
        if let Some(qty) = params.get(key).and_then(|v| v.as_str()).map(trim_qty) {
            if !qty.is_empty() {
                return Some(qty);
            }
        }
    }
    None
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn int_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
    })
}

fn format_btc_sats(sats: u64) -> String {
    let whole = sats / 100_000_000;
    let frac = sats % 100_000_000;
    if frac == 0 {
        return whole.to_string();
    }
    trim_qty(&format!("{whole}.{frac:08}"))
}

fn rate_btc_from_sats(btc_sats: u64, base_units: u64) -> Option<String> {
    if base_units == 0 {
        return None;
    }
    Some(format_btc_sats(btc_sats.saturating_mul(100_000_000) / base_units))
}

fn fallback_status(message_name: &str, valid: Option<bool>) -> String {
    if valid == Some(false) {
        return "Invalid".to_string();
    }
    match message_name {
        "dispense" | "send" | "enhanced_send" | "mpma_send" | "issuance" | "fairmint" => {
            "Settled".to_string()
        }
        _ => title_case(message_name),
    }
}

fn core_is_xcp(core: Option<&Value>) -> bool {
    core.and_then(|c| c.get("transaction_type"))
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty() && s != "unknown")
        .unwrap_or(false)
}

fn trim_qty(raw: &str) -> String {
    if !raw.contains('.') {
        return raw.to_string();
    }
    let trimmed = raw.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() { "0".to_string() } else { trimmed.to_string() }
}

pub fn title_case(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn asset_letter(asset: &str) -> char {
    asset
        .chars()
        .find(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('X')
}

pub fn short_hash(hash: &str) -> String {
    if hash.len() <= 14 {
        return hash.to_string();
    }
    format!("{}…{}", &hash[..8], &hash[hash.len() - 6..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispense_matches_xcp_io_shape() {
        let core = json!({
            "transaction_type": "dispense",
            "valid": true,
            "btc_amount": 615888,
            "events": [
                {
                    "event": "DISPENSER_UPDATE",
                    "params": {
                        "tx_hash": "462c8c9c3ccd3fd2f234febf3c359e7ab935c0e0fba7ffee62eeeeb1321a9321",
                        "give_remaining": 0,
                        "status": 10
                    }
                },
                {
                    "event": "DISPENSE",
                    "params": {
                        "asset": "XCP",
                        "dispense_quantity": 11200000000u64,
                        "dispense_quantity_normalized": "112.00000000",
                        "btc_amount": 615888,
                        "btc_amount_normalized": "0.00615888",
                        "source": "1dispenser",
                        "destination": "bc1pbuyer",
                        "dispenser_tx_hash": "462c8c9c3ccd3fd2f234febf3c359e7ab935c0e0fba7ffee62eeeeb1321a9321"
                    }
                }
            ]
        });
        let view = view_from_core(&core).expect("xcp");
        let action = &view.actions[0];
        assert_eq!(action.headline, "112 XCP bought from a dispenser for 0.00615888 BTC");
        assert_eq!(action.status, "Settled");
        assert_eq!(action.rate_btc.as_deref(), Some("0.00005499"));
        assert_eq!(action.buyer.as_deref(), Some("bc1pbuyer"));
        assert_eq!(action.seller.as_deref(), Some("1dispenser"));
        assert!(action.machine_closed);
    }

    #[test]
    fn pool_match_shows_user_buy_and_deltas() {
        let core = json!({
            "transaction_type": "order",
            "valid": true,
            "source": "bc1pzmkd2zs4y47aggezal2yj8djpp2lht80gv8dfzy4cnvf3yh35t0sde2nyd",
            "events": [
                {
                    "event": "OPEN_ORDER",
                    "params": {
                        "get_asset": "MSGA",
                        "give_asset": "XCP",
                        "source": "bc1pzmkd2zs4y47aggezal2yj8djpp2lht80gv8dfzy4cnvf3yh35t0sde2nyd"
                    }
                },
                {
                    "event": "POOL_MATCH",
                    "params": {
                        "source": "bc1pzmkd2zs4y47aggezal2yj8djpp2lht80gv8dfzy4cnvf3yh35t0sde2nyd",
                        "forward_asset": "MSGA",
                        "backward_asset": "XCP",
                        "forward_quantity_normalized": "245192.64660789",
                        "backward_quantity_normalized": "5.00000000",
                        "fee_quantity_normalized": "0.02500000"
                    }
                }
            ]
        });
        let view = view_from_core(&core).expect("xcp");
        assert_eq!(view.actions.len(), 1);
        let action = &view.actions[0];
        assert_eq!(action.method, "pool_swap");
        assert_eq!(action.headline, "Bought 245192.64660789 MSGA with 5 XCP · 0.025 XCP pool fee");
        assert_eq!(action.asset, "MSGA");
        assert_eq!(
            action.buyer.as_deref(),
            Some("bc1pzmkd2zs4y47aggezal2yj8djpp2lht80gv8dfzy4cnvf3yh35t0sde2nyd")
        );
        let deltas = transfers_for_action(action, &view.transfers);
        assert_eq!(deltas.len(), 2);
        assert!(deltas[0].incoming);
        assert_eq!(deltas[0].asset, "MSGA");
        assert_eq!(deltas[0].amount, "245192.64660789");
        assert!(!deltas[1].incoming);
        assert_eq!(deltas[1].asset, "XCP");
        assert_eq!(deltas[1].amount, "5");
    }

    #[test]
    fn order_uses_get_asset_for_icon() {
        let core = json!({
            "transaction_type": "order",
            "valid": true,
            "source": "bc1qissuer",
            "events": [
                {
                    "event": "OPEN_ORDER",
                    "params": {
                        "get_asset": "MSGA",
                        "give_asset": "XCP",
                        "source": "bc1qissuer"
                    }
                }
            ]
        });
        let view = view_from_core(&core).expect("xcp");
        assert_eq!(view.actions[0].asset, "MSGA");
    }
}
