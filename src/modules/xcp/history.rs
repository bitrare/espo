//! Native Counterparty histories. Keep original rows, including message indexes:
//! a single transaction can contain multiple sends or fills.
use super::core::{self, CoreFetch, CoreList};
use serde_json::{Value, json};

pub const RESOURCES: &[&str] = &[
    "issuances",
    "sends",
    "dispensers",
    "dispenses",
    "orders",
    "matches",
    "dividends",
    "destructions",
    "subassets",
    "fairminters",
    "fairmints",
    "credits",
    "debits",
    "pool_matches",
    "pool_deposits",
    "pool_withdrawals",
    "pool_history",
];

pub fn label(resource: &str) -> &str {
    match resource {
        "matches" => "DEX matches",
        "pool_matches" => "Pool swaps",
        "pool_deposits" => "Pool deposits",
        "pool_withdrawals" => "Pool withdrawals",
        "pool_history" => "Pool price history",
        "fairminters" => "Fairminters",
        "fairmints" => "Fairmints",
        "issuances" => "Issuances",
        "sends" => "Sends",
        "dispensers" => "Dispensers",
        "dispenses" => "Dispenses",
        "orders" => "Orders",
        "dividends" => "Dividends",
        "destructions" => "Destructions",
        "subassets" => "Subassets",
        "credits" => "Credits",
        "debits" => "Debits",
        _ => resource,
    }
}

pub fn valid_status(resource: &str, status: &str) -> bool {
    status == "all"
        || match resource {
            "orders" => ["open", "expired", "filled", "cancelled"].contains(&status),
            "matches" => ["pending", "completed", "expired"].contains(&status),
            "dispensers" => ["open", "closed", "closing", "open_empty_address"].contains(&status),
            _ => false,
        }
}

pub fn fetch(
    asset: &str,
    resource: &str,
    quote: &str,
    status: &str,
    offset: usize,
    limit: usize,
) -> CoreFetch<CoreList> {
    if !RESOURCES.contains(&resource) || !valid_status(resource, status) {
        return CoreFetch::NotFound;
    }
    let sort =
        if ["issuances", "sends", "dispensers", "dispenses", "orders", "matches", "dividends"]
            .contains(&resource)
        {
            "sort=block_index:DESC"
        } else {
            ""
        };
    let extra = if status != "all" { format!("{sort}&status={status}") } else { sort.to_string() };
    if let Some(suffix) = resource.strip_prefix("pool_") {
        let suffix = if suffix == "history" { "price_history" } else { suffix };
        core::fetch_path_list(
            &format!("/pools/{}/{}/{suffix}", core::encode_path(asset), core::encode_path(quote)),
            "",
            offset,
            limit,
        )
    } else {
        core::fetch_asset_list(asset, resource, &extra, offset, limit)
    }
}

pub async fn rpc(payload: Value) -> Value {
    let Some(asset) = payload
        .get("asset")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return json!({"ok":false,"error":"asset required"});
    };
    let resource = payload.get("type").and_then(Value::as_str).unwrap_or("sends");
    let status = payload.get("status").and_then(Value::as_str).unwrap_or("all");
    if !RESOURCES.contains(&resource) || !valid_status(resource, status) {
        return json!({"ok":false,"error":"invalid type or status","types":RESOURCES});
    }
    // Reject malformed pagination rather than silently wrapping or ignoring it.
    let number = |key: &str, default: u64| -> Option<u64> {
        payload.get(key).map_or(Some(default), Value::as_u64)
    };
    let (Some(offset), Some(limit)) = (number("offset", 0), number("limit", 50)) else {
        return json!({"ok":false,"error":"offset and limit must be non-negative integers"});
    };
    if limit == 0 || limit > 200 || offset > 10_000_000 {
        return json!({"ok":false,"error":"limit must be 1..200; offset must be 0..10000000"});
    }
    let (asset, resource, status, quote) = (
        asset.to_string(),
        resource.to_string(),
        status.to_string(),
        payload.get("quote_asset").and_then(Value::as_str).unwrap_or("XCP").to_string(),
    );
    let args = (asset.clone(), resource.clone(), status.clone(), quote.clone());
    match tokio::task::spawn_blocking(move || {
        fetch(&args.0, &args.1, &args.3, &args.2, offset as usize, limit as usize)
    })
    .await
    {
        Ok(CoreFetch::Ok(list)) => {
            json!({"ok":true,"asset":asset,"type":resource,"quote_asset":quote,"status":status,"offset":offset,"limit":limit,"total":list.total,"result_count":list.items.len(),"has_more":offset.saturating_add(list.items.len() as u64)<list.total as u64,"result":list.items})
        }
        Ok(CoreFetch::NotFound) => {
            json!({"ok":false,"error":"not_found","asset":asset,"type":resource})
        }
        _ => json!({"ok":false,"error":"upstream_unavailable","asset":asset,"type":resource}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejects_path_injection_and_bad_pagination_before_fetching() {
        for payload in [
            json!({"asset":"DJPEPE","type":"../config"}),
            json!({"asset":"DJPEPE","type":"sends","status":"open"}),
            json!({"asset":"DJPEPE","limit":-1}),
            json!({"asset":"DJPEPE","offset":"1"}),
            json!({"asset":"DJPEPE","limit":201}),
        ] {
            assert_eq!(rpc(payload).await["ok"], false);
        }
    }
}
