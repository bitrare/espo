use super::{GENESIS, SOURCE_COMMIT, storage};
use anyhow::{Context, Result, ensure};
use bitcoin::hashes::{Hash, sha256};
use rocksdb::{DB, WriteBatch};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event_index: u64,
    pub event: String,
    pub params: Value,
    pub tx_hash: Option<String>,
    pub block_index: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Boundary {
    pub height: u64,
    pub hash: String,
    pub parent: String,
    pub first: u64,
    pub last: u64,
    pub messages_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveState {
    pub source_commit: String,
    pub network: String,
    pub version: String,
    pub target: Boundary,
    pub next_cursor: Option<u64>,
    pub downloaded_events: u64,
    pub oldest_event: Option<u64>,
    pub complete: bool,
    pub checked_at: u64,
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub struct Core {
    pub url: String,
    agent: ureq::Agent,
    last: Instant,
}
impl Core {
    pub fn new(url: String) -> Self {
        Self {
            url,
            agent: ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build(),
            last: Instant::now() - Duration::from_secs(1),
        }
    }
    pub fn get(&mut self, path: &str) -> Result<Value> {
        let pause = Duration::from_millis(500).saturating_sub(self.last.elapsed());
        std::thread::sleep(pause);
        self.last = Instant::now();
        let value: Value = self.agent.get(&format!("{}{}", self.url, path)).call()?.into_json()?;
        ensure!(
            value.get("result").is_some() && value.get("error").is_none_or(Value::is_null),
            "Core response has no successful result: {path}"
        );
        Ok(value)
    }
    pub fn ready(&mut self) -> Result<Value> {
        let v = self.get("/")?["result"].clone();
        ensure!(v["server_ready"] == true, "Core not ready");
        ensure!(v["network"] == "mainnet", "only mainnet adapter is validated");
        ensure!(
            v["version"] == "11.3.0" && v["current_commit"] == SOURCE_COMMIT,
            "Core adapter version changed; verify before replay"
        );
        Ok(v)
    }
    pub fn boundary(&mut self, height: u64) -> Result<Boundary> {
        let last = self.get(&format!("/blocks/{height}/events?limit=1"))?;
        let e = &last["result"][0];
        ensure!(e["event"] == "BLOCK_PARSED", "block {height} has no final BLOCK_PARSED");
        let first = self.get(&format!("/blocks/{height}/events/NEW_BLOCK?limit=1"))?;
        let n = &first["result"][0];
        ensure!(
            n["event"] == "NEW_BLOCK" && n["params"]["block_index"] == height,
            "missing NEW_BLOCK {height}"
        );
        Ok(Boundary {
            height,
            hash: string(&n["params"], "block_hash")?,
            parent: string(&n["params"], "previous_block_hash")?,
            first: integer(n, "event_index")?,
            last: integer(e, "event_index")?,
            messages_hash: string(&e["params"], "messages_hash")?,
        })
    }
}
pub fn string(v: &Value, key: &str) -> Result<String> {
    v[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("missing string {key}"))
}
pub fn integer(v: &Value, key: &str) -> Result<u64> {
    v[key]
        .as_u64()
        .or_else(|| v[key].as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| anyhow::anyhow!("missing unsigned integer {key}"))
}
pub fn raw_key(index: u64) -> String {
    format!("raw/{index:020}")
}
pub fn validate_page(value: &Value, expected: u64) -> Result<Vec<Event>> {
    let events: Vec<Event> = serde_json::from_value(value["result"].clone())?;
    ensure!(!events.is_empty(), "empty page before genesis");
    let mut cursor = expected;
    for (i, e) in events.iter().enumerate() {
        ensure!(
            e.event_index == cursor,
            "event gap/reorder: expected {cursor}, got {}",
            e.event_index
        );
        ensure!(e.block_index >= GENESIS, "event below genesis");
        if i + 1 < events.len() {
            cursor = cursor.checked_sub(1).context("events after index zero")?;
        }
    }
    let last = events.last().unwrap().event_index;
    if last == 0 {
        ensure!(value["next_cursor"].is_null(), "cursor after genesis");
    } else {
        ensure!(
            value["next_cursor"].as_u64() == Some(last - 1),
            "missing or inconsistent next_cursor"
        );
    }
    Ok(events)
}
/// Persist the exact page and its resume cursor together. Never trust result_count.
pub fn download_page(db: &DB, core: &mut Core, state: &mut ArchiveState) -> Result<()> {
    let expected = state.next_cursor.context("no download cursor")?;
    let page = core.get(&format!("/events?cursor={expected}&limit=1000"))?;
    let events = validate_page(&page, expected)?;
    ensure!(
        events.iter().all(|e| e.block_index <= state.target.height),
        "event beyond fixed target"
    );
    let mut next = state.clone();
    next.oldest_event = events.last().map(|e| e.event_index);
    next.next_cursor = page["next_cursor"].as_u64();
    next.downloaded_events += events.len() as u64;
    next.complete = next.oldest_event == Some(0);
    let mut batch = WriteBatch::default();
    for e in &events {
        storage::put(&mut batch, &raw_key(e.event_index), e)?;
    }
    let digest = sha256::Hash::hash(&serde_json::to_vec(&page)?).to_string();
    storage::put(
        &mut batch,
        &format!("page/{expected:020}"),
        &serde_json::json!({"sha256":digest,"first":expected,"last":next.oldest_event,"next_cursor":next.next_cursor,"target":state.target.height}),
    )?;
    storage::put(&mut batch, "archive/state", &next)?;
    storage::commit(db, batch)?;
    *state = next;
    Ok(())
}
pub fn check_target(core: &mut Core, state: &ArchiveState) -> Result<()> {
    core.ready()?;
    let current = core.boundary(state.target.height)?;
    ensure!(
        current.hash == state.target.hash
            && current.messages_hash == state.target.messages_hash
            && current.first == state.target.first
            && current.last == state.target.last,
        "archive target changed (reorg or Core reparse); archive requires reconciliation"
    );
    Ok(())
}
pub fn bootstrap(db: Arc<DB>, url: String, confirmations: u64) -> Result<()> {
    let mut core = Core::new(url);
    let source = core.ready()?;
    let mut state = if let Some(s) = storage::read::<ArchiveState>(&db, "archive/state")? {
        s
    } else {
        let height = integer(&source, "counterparty_height")?
            .checked_sub(confirmations)
            .context("Core below confirmations")?;
        let target = core.boundary(height)?;
        // Prove both genesis position and network before any archive writes.
        let genesis = core.boundary(GENESIS)?;
        ensure!(genesis.first == 0, "unexpected genesis event boundary");
        ArchiveState {
            source_commit: SOURCE_COMMIT.into(),
            network: "mainnet".into(),
            version: "11.3.0".into(),
            next_cursor: Some(target.last),
            target,
            downloaded_events: 0,
            oldest_event: None,
            complete: false,
            checked_at: now(),
        }
    };
    ensure!(
        state.source_commit == SOURCE_COMMIT
            && state.network == "mainnet"
            && state.version == "11.3.0",
        "stored archive source identity differs; use a new XCP history directory"
    );
    check_target(&mut core, &state)?;
    let mut clear = WriteBatch::default();
    clear.delete("archive/error");
    storage::commit(&db, clear)?;
    let mut checks = Instant::now();
    while !state.complete {
        if checks.elapsed() > Duration::from_secs(60) {
            super::service::disk_check(&db.path().to_path_buf())?;
            check_target(&mut core, &state)?;
            state.checked_at = now();
            checks = Instant::now();
            eprintln!(
                "[xcp-history] archived {} events, oldest {:?}",
                state.downloaded_events, state.oldest_event
            );
        }
        download_page(&db, &mut core, &mut state)?;
    }
    check_target(&mut core, &state)?;
    // The descending download is complete; contiguous blocks are separately validated on replay.
    Ok(())
}
pub fn block_from(db: &DB, first: u64) -> Result<(Boundary, Vec<Event>)> {
    let start: Event = storage::read(db, &raw_key(first))?.context("missing block start")?;
    ensure!(start.event == "NEW_BLOCK", "block does not begin with NEW_BLOCK at {first}");
    let height = start.block_index;
    ensure!(integer(&start.params, "block_index")? == height, "NEW_BLOCK height mismatch");
    let mut events = Vec::new();
    let mut index = first;
    loop {
        let e: Event = storage::read(db, &raw_key(index))?.context("incomplete archived block")?;
        ensure!(e.block_index == height, "block boundary without BLOCK_PARSED");
        let done = e.event == "BLOCK_PARSED";
        events.push(e);
        if done {
            break;
        }
        index += 1;
        ensure!(events.len() < 1_000_000, "unexpected block event count");
    }
    let end = events.last().unwrap();
    let b = Boundary {
        height,
        hash: string(&events[0].params, "block_hash")?,
        parent: string(&events[0].params, "previous_block_hash")?,
        first,
        last: index,
        messages_hash: string(&end.params, "messages_hash")?,
    };
    ensure!(integer(&end.params, "block_index")? == height, "BLOCK_PARSED height mismatch");
    Ok((b, events))
}
/// Add one finalized block in a single atomic archive batch, following Core's cursor.
pub fn follow_block(
    db: &DB,
    core: &mut Core,
    height: u64,
    previous: &Boundary,
) -> Result<Boundary> {
    let boundary = core.boundary(height)?;
    ensure!(
        boundary.parent == previous.hash && boundary.first == previous.last + 1,
        "archive parent/event discontinuity"
    );
    let mut cursor = boundary.last;
    let mut batch = WriteBatch::default();
    loop {
        let v = core.get(&format!("/events?cursor={cursor}&limit=1000"))?;
        let events = validate_page(&v, cursor)?;
        for e in events.iter().filter(|e| e.event_index >= boundary.first) {
            ensure!(e.block_index == height, "block event height mismatch");
            storage::put(&mut batch, &raw_key(e.event_index), e)?;
        }
        if events.last().unwrap().event_index <= boundary.first {
            break;
        }
        cursor = v["next_cursor"].as_u64().context("missing follow cursor")?;
    }
    let current = core.boundary(height)?;
    ensure!(
        current.hash == boundary.hash
            && current.messages_hash == boundary.messages_hash
            && current.last == boundary.last,
        "block changed during read"
    );
    let mut state: ArchiveState =
        storage::read(db, "archive/state")?.context("missing archive state")?;
    state.target = boundary.clone();
    state.checked_at = now();
    state.downloaded_events += boundary.last - boundary.first + 1;
    storage::put(&mut batch, "archive/state", &state)?;
    storage::commit(db, batch)?;
    Ok(boundary)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cursor_is_inclusive_and_gaps_fail() {
        let e = |i| serde_json::json!({"event_index":i,"event":"SEND","params":{},"tx_hash":null,"block_index":400000});
        assert!(
            validate_page(&serde_json::json!({"result":[e(3),e(2)],"next_cursor":1}), 3).is_ok()
        );
        for v in [
            serde_json::json!({"result":[e(3),e(1)],"next_cursor":0}),
            serde_json::json!({"result":[e(3),e(2)],"next_cursor":2}),
            serde_json::json!({"result":[],"next_cursor":null}),
        ] {
            assert!(validate_page(&v, 3).is_err());
        }
    }
}
