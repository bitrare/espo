use super::{
    archive::{self, ArchiveState, Boundary, Core},
    engine::{self, Asset, Movement},
    ranges::{Run, Stack},
    storage,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use rocksdb::{DB, Direction, IteratorMode, WriteBatch};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

#[derive(Clone)]
pub struct Shared {
    pub db: Arc<DB>,
    pub gate: Arc<RwLock<()>>,
}
fn error(db: &DB, key: &str, value: Option<&str>) {
    let mut b = WriteBatch::default();
    if let Some(v) = value {
        let _ = storage::put(&mut b, key, &json!({"message":v,"at":archive::now()}));
    } else {
        b.delete(key);
    }
    let _ = storage::commit(db, b);
}
pub(super) fn disk_check(path: &PathBuf) -> Result<()> {
    let out = std::process::Command::new("df").args(["-Pk"]).arg(path).output()?;
    ensure!(out.status.success(), "disk availability check failed");
    let out = String::from_utf8(out.stdout)?;
    let available = out
        .lines()
        .last()
        .and_then(|s| s.split_whitespace().nth(3))
        .context("disk check parse")?
        .parse::<u64>()?;
    ensure!(available > 64 * 1024 * 1024, "less than 64 GiB available; archive paused");
    Ok(())
}
fn reconcile(shared: &Shared, core: &mut Core) -> Result<()> {
    let Some(tip) = storage::read::<Boundary>(&shared.db, "archive/tip")? else {
        return Ok(());
    };
    let current = core.boundary(tip.height)?;
    if current.hash == tip.hash
        && current.messages_hash == tip.messages_hash
        && current.last == tip.last
    {
        return Ok(());
    }
    let mut common = None;
    for height in (tip.height.saturating_sub(287)..tip.height).rev() {
        let Some(old) =
            storage::read::<Boundary>(&shared.db, &format!("archive/block/{height:010}"))?
        else {
            break;
        };
        let now = core.boundary(height)?;
        if old.hash == now.hash && old.messages_hash == now.messages_hash && old.last == now.last {
            common = Some(old);
            break;
        }
    }
    let common = common.context(
        "reorg or Core reparse exceeds 288-block undo; rebuild XCP history in a fresh directory",
    )?;
    let _guard = shared.gate.write().map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
    while let Some(p) = storage::read::<Boundary>(&shared.db, "projection/tip")? {
        if p.height <= common.height {
            break;
        }
        storage::rollback(&shared.db, p.height)?;
    }
    let mut b = WriteBatch::default();
    b.delete_range(archive::raw_key(common.last + 1), "raw0".to_string());
    b.delete_range(
        format!("archive/block/{:010}", common.height + 1),
        "archive/block0".to_string(),
    );
    let mut state: ArchiveState =
        storage::read(&shared.db, "archive/state")?.context("archive state missing")?;
    state.target = common.clone();
    state.downloaded_events = common.last + 1;
    state.checked_at = archive::now();
    storage::put(&mut b, "archive/state", &state)?;
    storage::put(&mut b, "archive/tip", &common)?;
    b.delete("projection/error");
    storage::commit(&shared.db, b)?;
    Ok(())
}
fn archive_loop(shared: &Shared, url: &str, confirmations: u64, path: &PathBuf) -> Result<()> {
    disk_check(path)?;
    let mut core = Core::new(url.into());
    core.ready()?;
    // After bootstrap has been structurally validated, shallow forks are handled
    // before bootstrap's fixed-target check can reject the changed target.
    reconcile(shared, &mut core)?;
    archive::bootstrap(shared.db.clone(), url.into(), confirmations)?;
    let mut checked = std::time::Instant::now();
    loop {
        let state: ArchiveState =
            storage::read(&shared.db, "archive/state")?.context("archive not initialized")?;
        let previous = storage::read::<Boundary>(&shared.db, "archive/tip")?;
        let first = previous.as_ref().map_or(0, |b| b.last + 1);
        if first > state.target.last {
            break;
        }
        let (boundary, _) = archive::block_from(&shared.db, first)?;
        if let Some(prev) = previous {
            ensure!(
                boundary.height == prev.height + 1 && boundary.parent == prev.hash,
                "archive block hash chain discontinuity"
            );
        } else {
            ensure!(
                boundary.height == super::GENESIS && boundary.first == 0,
                "archive genesis mismatch"
            );
        }
        let mut b = WriteBatch::default();
        storage::put(&mut b, "archive/tip", &boundary)?;
        storage::put(&mut b, &format!("archive/block/{:010}", boundary.height), &boundary)?;
        storage::commit(&shared.db, b)?;
        if checked.elapsed() > Duration::from_secs(60) {
            disk_check(path)?;
            archive::check_target(&mut core, &state)?;
            checked = std::time::Instant::now();
        }
    }
    error(&shared.db, "archive/error", None);
    loop {
        disk_check(path)?;
        let ready = core.ready()?;
        reconcile(shared, &mut core)?;
        let mut tip: Boundary =
            storage::read(&shared.db, "archive/tip")?.context("no validated archive tip")?;
        let target = archive::integer(&ready, "counterparty_height")?.saturating_sub(confirmations);
        while tip.height < target {
            let next = archive::follow_block(&shared.db, &mut core, tip.height + 1, &tip)?;
            let (verified, _) = archive::block_from(&shared.db, next.first)?;
            ensure!(
                verified.hash == next.hash && verified.last == next.last,
                "follow block mismatch"
            );
            let mut b = WriteBatch::default();
            storage::put(&mut b, "archive/tip", &next)?;
            storage::put(&mut b, &format!("archive/block/{:010}", next.height), &next)?;
            storage::commit(&shared.db, b)?;
            tip = next;
        }
        let mut b = WriteBatch::default();
        storage::put(&mut b, "archive/heartbeat", &archive::now())?;
        storage::commit(&shared.db, b)?;
        std::thread::sleep(Duration::from_secs(15));
    }
}
fn projection_step(shared: &Shared) -> Result<bool> {
    let _guard = shared.gate.write().map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
    let Some(available) = storage::read::<Boundary>(&shared.db, "archive/tip")? else {
        return Ok(false);
    };
    let tip = storage::read::<Boundary>(&shared.db, "projection/tip")?;
    let first = tip.as_ref().map_or(0, |b| b.last + 1);
    if first > available.last {
        return Ok(false);
    }
    let (boundary, events) = archive::block_from(&shared.db, first)?;
    engine::project(&shared.db, &boundary, &events).with_context(|| {
        format!(
            "projection stopped at block {} (events {}..{})",
            boundary.height, boundary.first, boundary.last
        )
    })?;
    error(&shared.db, "projection/error", None);
    Ok(true)
}
pub fn status(db: &DB) -> Result<Value> {
    let archive = storage::read::<ArchiveState>(db, "archive/state")?;
    let raw_tip = storage::read::<Boundary>(db, "archive/tip")?;
    let tip = storage::read::<Boundary>(db, "projection/tip")?;
    let archive_error = storage::read::<Value>(db, "archive/error")?;
    let projection_error = storage::read::<Value>(db, "projection/error")?;
    let heartbeat = storage::read::<u64>(db, "archive/heartbeat")?;
    let fresh = heartbeat.is_some_and(|h| archive::now().saturating_sub(h) < 180);
    let caught_up = tip
        .as_ref()
        .zip(raw_tip.as_ref())
        .is_some_and(|(a, b)| a.height == b.height && a.hash == b.hash)
        && archive.as_ref().zip(raw_tip.as_ref()).is_some_and(|(a, t)| {
            a.complete && a.target.height == t.height && a.target.hash == t.hash
        })
        && fresh
        && archive_error.is_none()
        && projection_error.is_none();
    let phase = if archive_error.is_some() {
        "archive_paused"
    } else if projection_error.is_some() {
        "projection_paused"
    } else if caught_up {
        "following"
    } else if archive.as_ref().is_none_or(|s| !s.complete) {
        "downloading_history"
    } else {
        "replaying_history"
    };
    let generation = tip.as_ref().map(|t| {
        format!("{}:{}:{}:{}:{}", super::RULESET, t.height, t.hash, t.last, t.messages_hash)
    });
    Ok(
        json!({"ok":true,"network":"mainnet","ruleset":super::RULESET,"core_version":"11.3.0","source_commit":super::SOURCE_COMMIT,"phase":phase,"complete":caught_up,"fresh":fresh,"verified":false,"verification":"quantity reconciliation is enforced per projected block; full historical compatibility and live balance audit are separate release gates","generation":generation,"numbering_tip":tip,"archive_tip":raw_tip,"download":archive,"archive_error":archive_error,"projection_error":projection_error,"last_source_check":heartbeat,"method_page":"/counterparty/numbering"}),
    )
}
/// Operator audit certificates are local files, never accepted over RPC. They
/// apply to one asset and expire on its next movement or a changed chain anchor.
fn asset_context(db: &DB, mut context: Value, asset: &str) -> Result<Value> {
    context["verification_scope"] = json!({"asset":asset});
    let path = db.path().join("audits").join(format!("{}.json", engine::escaped(asset)));
    if !path.exists() { return Ok(context); }
    ensure!(std::fs::metadata(&path)?.len() <= 16384, "asset audit certificate too large");
    let audit: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let height = archive::integer(&audit["checkpoint"], "height")?;
    let anchor = storage::read::<Boundary>(db, &format!("archive/block/{height:010}"))?;
    let prefix = engine::history_prefix(asset);
    let end = format!("{prefix}~");
    let latest = db.iterator(IteratorMode::From(end.as_bytes(), Direction::Reverse))
        .next().transpose()?.filter(|(k, _)| k.starts_with(prefix.as_bytes()))
        .map(|(_, v)| serde_json::from_slice::<Movement>(&v)).transpose()?;
    let meta = storage::read::<Asset>(db, &engine::asset_key(asset))?;
    let passed = ["all_serial_histories", "legacy_ordered_holdings", "current_ordered_holdings",
        "unique_serials", "core_balances", "core_escrows", "core_supply", "core_height_window"]
        .iter().all(|name| audit["checks"][*name] == true);
    let anchored = anchor.is_some_and(|b| b.hash == audit["checkpoint"]["hash"]
        && b.messages_hash == audit["checkpoint"]["messages_hash"]);
    let unchanged = latest.is_some_and(|m| audit["last_movement"]["event_index"] == m.event_index.to_string()
        && audit["last_movement"]["leg"] == m.leg);
    let identity = audit["schema"] == 1 && audit["asset"] == asset
        && audit["network"] == "mainnet" && audit["ruleset"] == super::RULESET
        && audit["source_commit"] == super::SOURCE_COMMIT
        && meta.is_some_and(|a| !a.divisible && audit["issued_units"].as_u64() == Some(a.next_serial - 1));
    let current = context["complete"] == true && context["numbering_tip"]["height"].as_u64().is_some_and(|h| h >= height);
    context["verified"] = json!(identity && passed && anchored && unchanged && current);
    context["asset_audit"] = audit;
    context["verification"] = if context["verified"] == true {
        json!(format!("{asset}: all original serial histories, ordered holdings, and Core supply/locations audited; no asset movement since the audited checkpoint"))
    } else {
        json!(format!("{asset}: the saved audit does not cover this current asset/chain state"))
    };
    Ok(context)
}

fn canonical(db: &DB, input: &str) -> Result<String> {
    ensure!(!input.is_empty() && input.len() <= 255, "asset required (up to 255 characters)");
    if storage::read::<Asset>(db, &engine::asset_key(input))?.is_some() {
        return Ok(input.into());
    }
    let alias = input
        .strip_prefix('A')
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or(input);
    storage::read(db, &format!("alias/{}", engine::escaped(alias)))?
        .context("asset not present at numbering checkpoint")
}
fn api_movement(m: Movement) -> Value {
    json!({"asset":m.asset,"block":m.block,"event_index":m.event_index.to_string(),"leg":m.leg,"tx_hash":m.tx_hash,"event_reference":m.event_reference,"reason":m.reason,"from":m.from,"to":m.to,"ranges":m.units.api()})
}
fn api_range(mut v: Value) -> Result<Value> {
    let r: Run = serde_json::from_value(v["run"].take())?;
    v["range"] = Stack(vec![r]).api()[0].clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("run");
    }
    Ok(v)
}
fn query(shared: &Shared, request: Value) -> Result<Value> {
    let _guard = shared.gate.read().map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
    let db = &shared.db;
    let mut context = status(db)?;
    let method = request["method"].as_str().context("method required")?;
    if method == "get_numbering_status" {
        if let Some(input) = request["params"]["asset"].as_str() {
            // Keep global download progress readable before this asset is issued.
            if let Ok(asset) = canonical(db, input) { context = asset_context(db, context, &asset)?; }
        }
        return Ok(context);
    }
    let p = &request["params"];
    ensure!(
        context["complete"] == true || p["allow_partial"] == true,
        "numbering_incomplete: request allow_partial=true to inspect the reported historical checkpoint"
    );
    ensure!(!context["generation"].is_null(), "numbering_not_started");
    let asset = if method == "get_address_units" || method == "get_transfer_units" {
        p["asset"].as_str().map(|s| canonical(db, s)).transpose()?
    } else {
        Some(canonical(db, p["asset"].as_str().context("asset required")?)?)
    };
    if let Some(asset) = &asset { context = asset_context(db, context, asset)?; }
    let asset_id = asset
        .as_ref()
        .map(|a| storage::read::<Asset>(db, &engine::asset_key(a)).map(|a| a.map(|a| a.asset_id)))
        .transpose()?
        .flatten();
    let serial = if method == "get_unit" || method == "get_unit_history" {
        Some(archive::integer(p, "serial")?)
    } else {
        None
    };
    if let Some(asset) = &asset {
        let a = storage::read::<Asset>(db, &engine::asset_key(asset))?.context("missing asset")?;
        ensure!(!a.divisible || serial.is_some(), "asset is divisible at this checkpoint");
        if let Some(serial) = serial {
            ensure!(serial > 0 && serial < a.next_serial, "unit not issued at checkpoint");
        }
    }
    if method == "get_unit" {
        let serial = serial.unwrap();
        ensure!(serial > 0, "serial starts at one");
        let prefix = engine::range_prefix(asset.as_ref().unwrap());
        let start = format!("{prefix}{serial:020}");
        let mut iter = db.iterator(IteratorMode::From(start.as_bytes(), Direction::Reverse));
        let (k, v) = iter.next().context("unit not issued at checkpoint")??;
        ensure!(k.starts_with(prefix.as_bytes()), "unit not issued at checkpoint");
        let row: Value = serde_json::from_slice(&v)?;
        let run: Run = serde_json::from_value(row["run"].clone())?;
        ensure!(run.contains(serial), "unit not issued at checkpoint");
        return Ok(
            json!({"ok":true,"context":context,"asset":asset,"asset_id":asset_id,"serial":serial.to_string(),"location":row["location"]}),
        );
    }
    let prefix = match method {
        "get_asset_units" => engine::range_prefix(asset.as_ref().unwrap()),
        "get_unit_history" => engine::history_prefix(asset.as_ref().unwrap()),
        "get_address_units" => {
            let address = p["address"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 150)
                .context("address required")?;
            format!(
                "owner/{}/{}",
                engine::escaped(address),
                asset.as_ref().map_or(String::new(), |a| format!("{}/", engine::escaped(a)))
            )
        }
        "get_transfer_units" => {
            let txid = p["txid"]
                .as_str()
                .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .context("64-character txid required")?;
            format!("tx/{}/", engine::escaped(&txid.to_lowercase()))
        }
        _ => anyhow::bail!("unknown numbering method"),
    };
    let limit = p["limit"].as_u64().unwrap_or(50).clamp(1, 200) as usize;
    let scope = json!([method, prefix, asset, serial]).to_string();
    let after = if let Some(cursor) = p["cursor"].as_str() {
        ensure!(cursor.len() < 4096, "invalid cursor");
        let c: Value = serde_json::from_slice(&hex::decode(cursor)?)?;
        ensure!(
            c["generation"] == context["generation"] && c["scope"] == scope,
            "stale_cursor: restart pagination at the current checkpoint"
        );
        Some(c["key"].as_str().context("cursor key missing")?.to_string())
    } else {
        None
    };
    let start = after.as_deref().unwrap_or(&prefix);
    let mut items = Vec::new();
    let mut last = None;
    let mut has_more = false;
    let mut scanned = 0;
    for entry in db.iterator(IteratorMode::From(start.as_bytes(), Direction::Forward)) {
        let (k, v) = entry?;
        if !k.starts_with(prefix.as_bytes()) {
            break;
        }
        let key = String::from_utf8(k.to_vec())?;
        if after.as_ref() == Some(&key) {
            continue;
        }
        if items.len() >= limit || scanned >= 4000 {
            has_more = true;
            break;
        }
        scanned += 1;
        last = Some(key);
        if method == "get_unit_history" || method == "get_transfer_units" {
            let m: Movement = if method == "get_transfer_units" {
                let key: String = serde_json::from_slice(&v)?;
                storage::read(db, &key)?.context("dangling transfer index")?
            } else {
                serde_json::from_slice(&v)?
            };
            if asset.as_ref().is_some_and(|a| a != &m.asset)
                || serial.is_some_and(|s| !m.units.0.iter().any(|r| r.contains(s)))
            {
                continue;
            }
            items.push(api_movement(m));
        } else {
            items.push(api_range(serde_json::from_slice(&v)?)?);
        }
    }
    let cursor = if has_more {
        Some(hex::encode(serde_json::to_vec(
            &json!({"generation":context["generation"],"scope":scope,"key":last}),
        )?))
    } else {
        None
    };
    Ok(
        json!({"ok":true,"context":context,"asset":asset,"asset_id":asset_id,"serial":serial.map(|s|s.to_string()),"items":items,"next_cursor":cursor,"has_more":has_more}),
    )
}
async fn rpc(State(shared): State<Shared>, Json(request): Json<Value>) -> Json<Value> {
    let result = tokio::task::spawn_blocking(move || query(&shared, request)).await;
    Json(match result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => json!({"ok":false,"error":e.to_string()}),
        Err(e) => json!({"ok":false,"error":e.to_string()}),
    })
}
async fn get_status(State(shared): State<Shared>) -> Json<Value> {
    Json(status(&shared.db).unwrap_or_else(|e| json!({"ok":false,"error":e.to_string()})))
}
pub async fn run(
    data_dir: PathBuf,
    url: String,
    confirmations: u64,
    listen: std::net::SocketAddr,
) -> Result<()> {
    ensure!(listen.ip().is_loopback(), "history API must bind to loopback");
    let shared =
        Shared { db: Arc::new(storage::open(&data_dir)?), gate: Arc::new(RwLock::new(())) };
    let archive_shared = shared.clone();
    std::thread::spawn(move || {
        loop {
            if let Err(e) = archive_loop(&archive_shared, &url, confirmations, &data_dir) {
                let message = format!("{e:#}");
                eprintln!("[xcp-history] {message}");
                error(&archive_shared.db, "archive/error", Some(&message));
            }
            std::thread::sleep(Duration::from_secs(30));
        }
    });
    let projection_shared = shared.clone();
    std::thread::spawn(move || {
        let mut replay_batch = 0u8;
        loop {
            match projection_step(&projection_shared) {
                Ok(true) => {
                    replay_batch += 1;
                    if replay_batch == 32 {
                        // projection_step has released the write lock. Let queued
                        // HTTP readers run before the next replay batch reacquires it.
                        std::thread::sleep(Duration::from_millis(1));
                        replay_batch = 0;
                    }
                }
                Ok(false) => std::thread::sleep(Duration::from_secs(1)),
                Err(e) => {
                    let message = format!("{e:#}");
                    eprintln!("[xcp-history] {message}");
                    error(&projection_shared.db, "projection/error", Some(&message));
                    std::thread::sleep(Duration::from_secs(30));
                }
            }
        }
    });
    let app = Router::new()
        .route("/status", get(get_status))
        .route("/query", post(rpc))
        .with_state(shared);
    axum::serve(tokio::net::TcpListener::bind(listen).await?, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_reads_are_explicit_and_cursors_are_checkpoint_bound() {
        let t = tempfile::TempDir::new().unwrap();
        let db = Arc::new(storage::open(&t.path().join("db")).unwrap());
        let shared = Shared { db: db.clone(), gate: Arc::new(RwLock::new(())) };
        let mut b = WriteBatch::default();
        storage::put(
            &mut b,
            "projection/tip",
            &Boundary {
                height: 300000,
                hash: "h".into(),
                parent: "p".into(),
                first: 10,
                last: 20,
                messages_hash: "m".into(),
            },
        )
        .unwrap();
        storage::put(
            &mut b,
            &engine::asset_key("CARD"),
            &Asset {
                name: "CARD".into(),
                asset_id: "35155".into(),
                longname: None,
                divisible: false,
                next_serial: 3,
                epoch: 0,
            },
        )
        .unwrap();
        for serial in 1..=2 {
            storage::put(&mut b,&format!("{}{:020}",engine::range_prefix("CARD"),serial),&json!({"asset":"CARD","run":{"first":serial,"last":serial},"location":{"kind":"address","id":"alice"},"stack_position":(serial-1).to_string()})).unwrap();
        }
        storage::commit(&db, b).unwrap();
        assert!(
            query(&shared, json!({"method":"get_asset_units","params":{"asset":"CARD"}}))
                .unwrap_err()
                .to_string()
                .contains("incomplete")
        );
        let first=query(&shared,json!({"method":"get_asset_units","params":{"asset":"CARD","limit":1,"allow_partial":true}})).unwrap();
        assert_eq!(first["items"].as_array().unwrap().len(), 1);
        assert_eq!(first["has_more"], true);
        assert_eq!(first["items"][0]["range"]["first"], "1");
        for serial in ["0", "3"] {
            assert!(query(&shared,json!({"method":"get_unit_history","params":{"asset":"CARD","serial":serial,"allow_partial":true}})).unwrap_err().to_string().contains("not issued"));
        }
        let cursor = first["next_cursor"].clone();
        let second=query(&shared,json!({"method":"get_asset_units","params":{"asset":"CARD","limit":1,"allow_partial":true,"cursor":cursor}})).unwrap();
        assert_eq!(second["items"][0]["range"]["first"], "2");
        let mut b = WriteBatch::default();
        storage::put(
            &mut b,
            "projection/tip",
            &Boundary {
                height: 300000,
                hash: "h".into(),
                parent: "p".into(),
                first: 10,
                last: 20,
                messages_hash: "changed".into(),
            },
        )
        .unwrap();
        storage::commit(&db, b).unwrap();
        assert!(query(&shared,json!({"method":"get_asset_units","params":{"asset":"CARD","allow_partial":true,"cursor":cursor}})).unwrap_err().to_string().contains("stale_cursor"));
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[test]
    fn asset_audit_is_scoped_and_expires_on_movement_or_reorg() {
        let t = tempfile::TempDir::new().unwrap();
        let db = storage::open(&t.path().join("db")).unwrap();
        let tip = Boundary {height:300000, hash:"h".into(), parent:"p".into(), first:10,last:20,messages_hash:"m".into()};
        let m = Movement {asset:"CARD".into(),block:300000,event_index:19,leg:0,tx_hash:None,event_reference:None,reason:"issuance".into(),from:None,to:engine::Location::address("alice"),units:Stack(vec![Run {first:1,last:2}])};
        let mut b = WriteBatch::default();
        storage::put(&mut b,"archive/block/0000300000",&tip).unwrap();
        storage::put(&mut b,&engine::asset_key("CARD"),&Asset {name:"CARD".into(),asset_id:"35155".into(),longname:None,divisible:false,next_serial:3,epoch:0}).unwrap();
        storage::put(&mut b,&format!("{}{:020}/{:06}",engine::history_prefix("CARD"),19,0),&m).unwrap();
        storage::commit(&db,b).unwrap();
        let audit=json!({"schema":1,"asset":"CARD","network":"mainnet","ruleset":super::super::RULESET,"source_commit":super::super::SOURCE_COMMIT,"checkpoint":tip,"last_movement":{"event_index":"19","leg":0},"issued_units":2,"checks":{"all_serial_histories":true,"legacy_ordered_holdings":true,"current_ordered_holdings":true,"unique_serials":true,"core_balances":true,"core_escrows":true,"core_supply":true,"core_height_window":true}});
        std::fs::create_dir(db.path().join("audits")).unwrap();
        std::fs::write(db.path().join("audits").join(format!("{}.json",engine::escaped("CARD"))),serde_json::to_vec(&audit).unwrap()).unwrap();
        let context=json!({"complete":true,"verified":false,"numbering_tip":tip});
        assert_eq!(asset_context(&db,context.clone(),"CARD").unwrap()["verified"],true);
        assert_eq!(asset_context(&db,context.clone(),"OTHER").unwrap()["verified"],false);
        let mut incomplete=context.clone(); incomplete["complete"]=json!(false);
        assert_eq!(asset_context(&db,incomplete,"CARD").unwrap()["verified"],false);
        let mut b=WriteBatch::default(); let mut fork=tip.clone();fork.hash="fork".into();
        storage::put(&mut b,"archive/block/0000300000",&fork).unwrap();storage::commit(&db,b).unwrap();
        assert_eq!(asset_context(&db,context.clone(),"CARD").unwrap()["verified"],false);
        let mut b=WriteBatch::default();storage::put(&mut b,"archive/block/0000300000",&tip).unwrap();
        let mut moved=m.clone();moved.event_index=20;
        storage::put(&mut b,&format!("{}{:020}/{:06}",engine::history_prefix("CARD"),20,0),&moved).unwrap();storage::commit(&db,b).unwrap();
        assert_eq!(asset_context(&db,context,"CARD").unwrap()["verified"],false);
    }
}
