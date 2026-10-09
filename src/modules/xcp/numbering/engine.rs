use super::{
    archive::{Boundary, Event, integer, string},
    ranges::{Run, Stack},
    storage::{self, BlockTx},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub asset_id: String,
    pub longname: Option<String>,
    pub divisible: bool,
    pub next_serial: u64,
    pub epoch: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Location {
    pub kind: String,
    pub id: String,
    pub controller: Option<String>,
    pub ledger: Option<String>,
}
impl Location {
    pub fn address(id: &str) -> Self {
        Self {
            kind: "address".into(),
            id: id.into(),
            controller: Some(id.into()),
            ledger: Some(format!("address:{id}")),
        }
    }
    pub fn escrow(kind: &str, id: &str, controller: Option<String>) -> Self {
        Self { kind: kind.into(), id: id.into(), controller, ledger: None }
    }
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind, self.id)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holding {
    pub location: Location,
    pub units: Stack,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Movement {
    pub asset: String,
    pub block: u64,
    pub event_index: u64,
    pub leg: u32,
    pub tx_hash: Option<String>,
    pub event_reference: Option<String>,
    pub reason: String,
    pub from: Option<Location>,
    pub to: Location,
    pub units: Stack,
}
#[derive(Clone)]
struct Move {
    asset: String,
    from: Location,
    to: Location,
    quantity: u64,
    reverse: bool,
}
pub fn escaped(s: &str) -> String {
    hex::encode(s.as_bytes())
}
pub fn asset_key(asset: &str) -> String {
    format!("asset/{}", escaped(asset))
}
pub fn holding_prefix(asset: &str) -> String {
    format!("holding/{}/", escaped(asset))
}
pub fn history_prefix(asset: &str) -> String {
    format!("history/{}/", escaped(asset))
}
pub fn range_prefix(asset: &str) -> String {
    format!("range/{}/", escaped(asset))
}
fn hold_key(asset: &str, loc: &Location) -> String {
    format!("{}{}", holding_prefix(asset), escaped(&loc.key()))
}
fn reference(e: &Event) -> Result<String> {
    string(&e.params, "event")
        .or_else(|_| string(&e.params, "tx_hash"))
        .or_else(|_| e.tx_hash.clone().context("missing operation reference"))
}
fn valid(p: &Value) -> bool {
    p["status"] == "valid"
}
fn meta(tx: &BlockTx<'_>, asset: &str) -> Result<Option<Asset>> {
    tx.get(&asset_key(asset))
}
fn numbered(tx: &BlockTx<'_>, asset: &str) -> Result<bool> {
    if asset == "BTC" || asset == "XCP" {
        return Ok(false);
    }
    Ok(!meta(tx, asset)?
        .with_context(|| format!("unknown divisibility for {asset}"))?
        .divisible)
}
fn numeric_id(name: &str) -> Result<String> {
    if let Some(n) = name
        .strip_prefix('A')
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return Ok(n.parse::<u64>()?.to_string());
    }
    let mut id = 0u64;
    for b in name.bytes() {
        ensure!(b.is_ascii_uppercase(), "invalid canonical asset name");
        id = id
            .checked_mul(26)
            .and_then(|i| i.checked_add(u64::from(b - b'A')))
            .context("asset id overflow")?;
    }
    Ok(id.to_string())
}
fn update_asset(tx: &mut BlockTx<'_>, p: &Value, reset: bool) -> Result<()> {
    let name = string(p, "asset")?;
    let divisible = p["divisible"].as_bool().context("missing historical divisibility")?;
    let mut a = meta(tx, &name)?.unwrap_or(Asset {
        name: name.clone(),
        asset_id: numeric_id(&name)?,
        longname: None,
        divisible,
        next_serial: 1,
        epoch: 0,
    });
    if reset {
        a.epoch += 1;
    } else {
        ensure!(a.divisible == divisible, "divisibility changed without reset for {name}");
    }
    a.divisible = divisible;
    if let Some(long) = p["asset_longname"].as_str() {
        a.longname = Some(long.into());
        tx.set(format!("alias/{}", escaped(long)), &name)?;
    }
    tx.set(format!("alias/{}", escaped(&a.asset_id)), &name)?;
    tx.set(asset_key(&name), &a)
}
fn ledger_location(p: &Value) -> Result<Location> {
    if let Some(utxo) = p["utxo"].as_str().filter(|s| !s.is_empty()) {
        return Ok(Location {
            kind: "utxo".into(),
            id: utxo.into(),
            controller: p["utxo_address"].as_str().map(str::to_owned),
            ledger: Some(format!("utxo:{utxo}")),
        });
    }
    Ok(Location::address(&string(p, "address")?))
}
fn action(e: &Event) -> Result<String> {
    string(&e.params, if e.event == "CREDIT" { "calling_function" } else { "action" })
}
fn record(tx: &BlockTx<'_>, kind: &str, id: &str) -> Result<Value> {
    tx.get(&format!("record/{kind}/{}", escaped(id)))?
        .with_context(|| format!("missing {kind} reference {id}"))
}
fn order_location(tx: &BlockTx<'_>, id: &str) -> Result<Location> {
    let p = record(tx, "order", id)?;
    Ok(Location::escrow("order", id, p["source"].as_str().map(str::to_owned)))
}
fn dispenser_location(tx: &BlockTx<'_>, id: &str) -> Result<Location> {
    let p = record(tx, "dispenser", id)?;
    Ok(Location::escrow("dispenser", id, p["source"].as_str().map(str::to_owned)))
}
fn pool(p: &Value) -> Result<Location> {
    Ok(Location::escrow(
        "pool",
        &format!("{}:{}", string(p, "asset_a")?, string(p, "asset_b")?),
        None,
    ))
}
fn fair_parent(tx: &BlockTx<'_>, reference: &str) -> Result<String> {
    if let Some(p) = tx.get::<Value>(&format!("record/fairmint/{}", escaped(reference)))? {
        return string(&p, "fairminter_tx_hash");
    }
    record(tx, "fairminter", reference)?;
    Ok(reference.into())
}
fn fair_location(tx: &BlockTx<'_>, id: &str) -> Result<Location> {
    let p = record(tx, "fairminter", id)?;
    Ok(Location {
        kind: "fairmint_escrow".into(),
        id: id.into(),
        controller: p["source"].as_str().map(str::to_owned),
        ledger: Some("address:1CounterpartyXXXXXXXXXXXXXXXUWLpVr".into()),
    })
}
fn lookup<'a>(
    events: &'a [Event],
    after: u64,
    name: &str,
    pred: impl Fn(&Value) -> bool,
) -> Result<&'a Event> {
    events
        .iter()
        .find(|e| e.event_index > after && e.event == name && pred(&e.params))
        .with_context(|| format!("missing {name} context after event {after}"))
}
struct Engine<'a> {
    tx: BlockTx<'a>,
    height: u64,
    leg: u32,
    actual: BTreeMap<(String, String), i128>,
    expected: BTreeMap<(String, String), i128>,
}
impl<'a> Engine<'a> {
    fn holding(&self, asset: &str, loc: &Location) -> Result<Holding> {
        Ok(self
            .tx
            .get(&hold_key(asset, loc))?
            .unwrap_or(Holding { location: loc.clone(), units: Stack::default() }))
    }
    fn write_holding(&mut self, asset: &str, h: Holding) -> Result<()> {
        let key = hold_key(asset, &h.location);
        if let Some(old) = self.tx.get::<Holding>(&key)? {
            for r in old.units.0 {
                self.tx.delete(format!("{}{:020}", range_prefix(asset), r.first.min(r.last)));
                if let Some(owner) = &old.location.controller {
                    self.tx.delete(format!(
                        "owner/{}/{}/{:020}",
                        escaped(owner),
                        escaped(asset),
                        r.first.min(r.last)
                    ));
                }
            }
        }
        let mut position = 0u64;
        for r in &h.units.0 {
            let row = json!({"asset":asset,"run":r,"location":h.location,"stack_position":position.to_string()});
            self.tx
                .set(format!("{}{:020}", range_prefix(asset), r.first.min(r.last)), &row)?;
            if let Some(owner) = &h.location.controller {
                self.tx.set(
                    format!(
                        "owner/{}/{}/{:020}",
                        escaped(owner),
                        escaped(asset),
                        r.first.min(r.last)
                    ),
                    &row,
                )?;
            }
            position += r.len();
        }
        if h.units.0.is_empty() {
            self.tx.delete(key);
        } else {
            self.tx.set(key, &h)?;
        }
        Ok(())
    }
    fn delta(&mut self, asset: &str, loc: &Location, q: i128) {
        if let Some(ledger) = &loc.ledger {
            *self.actual.entry((asset.into(), ledger.clone())).or_default() += q;
        }
    }
    fn log(
        &mut self,
        e: &Event,
        asset: &str,
        from: Option<Location>,
        to: Location,
        units: Stack,
        reason: &str,
    ) -> Result<()> {
        for run in units.0 {
            let m = Movement {
                asset: asset.into(),
                block: self.height,
                event_index: e.event_index,
                leg: self.leg,
                tx_hash: e.tx_hash.clone(),
                event_reference: e.params["event"]
                    .as_str()
                    .or_else(|| e.params["id"].as_str())
                    .or_else(|| e.params["tx_hash"].as_str())
                    .map(str::to_owned),
                reason: reason.into(),
                from: from.clone(),
                to: to.clone(),
                units: Stack(vec![run]),
            };
            let key = format!("{}{:020}/{:06}", history_prefix(asset), e.event_index, self.leg);
            self.tx.set(key.clone(), &m)?;
            if let Some(hash) = &e.tx_hash {
                self.tx.set(
                    format!("tx/{}/{:020}/{:06}", escaped(hash), e.event_index, self.leg),
                    &key,
                )?;
            }
            if let Some(reference) = m
                .event_reference
                .as_ref()
                .filter(|r| r.len() == 64 && r.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                self.tx.set(
                    format!("tx/{}/{:020}/{:06}", escaped(reference), e.event_index, self.leg),
                    &key,
                )?;
            }
            self.leg += 1;
        }
        Ok(())
    }
    fn move_units(&mut self, e: &Event, m: Move, reason: &str) -> Result<()> {
        if m.quantity == 0 || !numbered(&self.tx, &m.asset)? {
            return Ok(());
        }
        let mut source = self.holding(&m.asset, &m.from)?;
        if reason == "utxo move" {
            ensure!(
                source.units.len() == m.quantity,
                "UTXO_MOVE must move the complete asset balance"
            );
        }
        let units = source.units.take(m.quantity, m.reverse).with_context(|| {
            format!("{} {} from {} at event {}", reason, m.asset, m.from.key(), e.event_index)
        })?;
        self.write_holding(&m.asset, source)?;
        let mut dest = self.holding(&m.asset, &m.to)?;
        dest.location = m.to.clone();
        dest.units.append(units.clone());
        self.write_holding(&m.asset, dest)?;
        self.delta(&m.asset, &m.from, -i128::from(m.quantity));
        self.delta(&m.asset, &m.to, i128::from(m.quantity));
        self.log(e, &m.asset, Some(m.from), m.to, units, reason)
    }
    fn mint(&mut self, e: &Event, asset: &str, to: Location, q: u64, reason: &str) -> Result<()> {
        if q == 0 || !numbered(&self.tx, asset)? {
            return Ok(());
        }
        let mut a = meta(&self.tx, asset)?.unwrap();
        let next = a.next_serial.checked_add(q).context("serial counter overflow")?;
        let units = Stack(vec![Run { first: a.next_serial, last: next - 1 }]);
        a.next_serial = next;
        self.tx.set(asset_key(asset), &a)?;
        let mut dest = self.holding(asset, &to)?;
        dest.units.append(units.clone());
        self.write_holding(asset, dest)?;
        self.delta(asset, &to, i128::from(q));
        self.log(e, asset, None, to, units, reason)
    }
}
/// Whole-block planning handles MPMA's credit-before-debit ordering and the
/// implementation-only temporary address used when opening an empty dispenser.
fn direct_moves(
    tx: &BlockTx<'_>,
    events: &[Event],
) -> Result<(BTreeMap<u64, Vec<Move>>, BTreeSet<u64>)> {
    let mut groups: BTreeMap<(String, String, String), Vec<&Event>> = BTreeMap::new();
    for e in events.iter().filter(|e| e.event == "CREDIT" || e.event == "DEBIT") {
        let a = action(e)?;
        if [
            "send",
            "mpma send",
            "sweep",
            "dividend",
            "attach to utxo",
            "detach from utxo",
            "utxo move",
            "open dispenser empty addr",
        ]
        .contains(&a.as_str())
        {
            groups
                .entry((a, reference(e)?, string(&e.params, "asset")?))
                .or_default()
                .push(e);
        }
    }
    let mut planned = BTreeMap::new();
    let mut skipped = BTreeSet::new();
    for ((act, reference, asset), items) in groups {
        if !numbered(tx, &asset)? {
            continue;
        }
        let debits: Vec<_> = items.iter().filter(|e| e.event == "DEBIT").collect();
        let credits: Vec<_> = items.iter().filter(|e| e.event == "CREDIT").collect();
        let first = items[0].event_index;
        let mut moves = Vec::new();
        if act == "open dispenser empty addr" {
            let d = debits.first().context("empty dispenser without debit")?;
            let p = &lookup(events, first, "OPEN_DISPENSER", |p| {
                p["tx_hash"] == reference && p["asset"] == asset
            })?
            .params;
            moves.push(Move {
                asset,
                from: ledger_location(&d.params)?,
                to: Location::escrow(
                    "dispenser",
                    &reference,
                    p["source"].as_str().map(str::to_owned),
                ),
                quantity: integer(&d.params, "quantity")?,
                reverse: true,
            });
        } else {
            let total_d: u128 = debits
                .iter()
                .map(|e| integer(&e.params, "quantity").map(u128::from))
                .collect::<Result<Vec<_>>>()?
                .iter()
                .sum();
            let total_c: u128 = credits
                .iter()
                .map(|e| integer(&e.params, "quantity").map(u128::from))
                .collect::<Result<Vec<_>>>()?
                .iter()
                .sum();
            ensure!(total_d == total_c, "unbalanced {act} {reference} {asset}");
            if act == "utxo move"
                || debits
                    .iter()
                    .map(|d| ledger_location(&d.params).map(|l| l.key()))
                    .collect::<Result<BTreeSet<_>>>()?
                    .len()
                    > 1
            {
                let mut used = BTreeSet::new();
                for c in credits {
                    let d = debits
                        .iter()
                        .rev()
                        .find(|d| {
                            d.event_index < c.event_index
                                && !used.contains(&d.event_index)
                                && d.params["quantity"] == c.params["quantity"]
                        })
                        .context("unpaired UTXO move")?;
                    used.insert(d.event_index);
                    moves.push(Move {
                        asset: asset.clone(),
                        from: ledger_location(&d.params)?,
                        to: ledger_location(&c.params)?,
                        quantity: integer(&c.params, "quantity")?,
                        reverse: act != "utxo move",
                    });
                }
                ensure!(used.len() == debits.len(), "unconsumed UTXO debit");
            } else {
                let from =
                    ledger_location(&debits.first().context("transfer without debit")?.params)?;
                ensure!(
                    debits
                        .iter()
                        .all(|d| ledger_location(&d.params).is_ok_and(|l| l.key() == from.key())),
                    "multiple debit sources require an explicit adapter: {act}"
                );
                for c in credits {
                    moves.push(Move {
                        asset: asset.clone(),
                        from: from.clone(),
                        to: ledger_location(&c.params)?,
                        quantity: integer(&c.params, "quantity")?,
                        reverse: true,
                    });
                }
            }
        }
        for e in items {
            skipped.insert(e.event_index);
        }
        planned.insert(first, moves);
    }
    Ok((planned, skipped))
}

pub fn project(db: &rocksdb::DB, boundary: &Boundary, events: &[Event]) -> Result<()> {
    let mut engine = Engine {
        tx: BlockTx::new(db),
        height: boundary.height,
        leg: 0,
        actual: BTreeMap::new(),
        expected: BTreeMap::new(),
    };
    if let Some(previous) = engine.tx.get::<Boundary>("projection/tip")? {
        ensure!(
            boundary.height == previous.height + 1
                && boundary.parent == previous.hash
                && boundary.first == previous.last + 1,
            "projection parent discontinuity"
        );
    } else {
        ensure!(
            boundary.height == super::GENESIS && boundary.first == 0,
            "projection must start at Core genesis"
        );
    }
    let mut budgets: BTreeMap<(String, String), u64> = BTreeMap::new();
    // Resolve forward references only for context; all balance mutations below
    // still execute in the source ledger order.
    for e in events {
        let p = &e.params;
        let kind = match e.event.as_str() {
            "OPEN_ORDER" => Some("order"),
            "OPEN_DISPENSER" => Some("dispenser"),
            "ORDER_MATCH" => Some("match"),
            "NEW_FAIRMINTER" => Some("fairminter"),
            "NEW_FAIRMINT" => Some("fairmint"),
            _ => None,
        };
        if let Some(kind) = kind {
            if p["status"].as_str().is_some_and(|s| s.starts_with("invalid")) {
                continue;
            }
            let id = if kind == "match" { string(p, "id")? } else { string(p, "tx_hash")? };
            engine.tx.set(format!("record/{kind}/{}", escaped(&id)), p)?;
        }
        if (e.event == "ASSET_ISSUANCE" || e.event == "RESET_ISSUANCE") && valid(p) {
            let asset = string(p, "asset")?;
            if meta(&engine.tx, &asset)?.is_none() {
                update_asset(&mut engine.tx, p, false)?;
            }
            if p["divisible"] == false {
                let q = integer(p, "quantity")?;
                let entry = budgets.entry((asset, string(p, "tx_hash")?)).or_default();
                *entry = entry.checked_add(q).context("issuance budget overflow")?;
            }
        }
    }
    // Metadata must be available before planning sends. Divisible ledger legs
    // (including historical null-address sends) never enter unit numbering.
    let (mut direct, skip) = direct_moves(&engine.tx, events)?;
    let mut pool_done = BTreeSet::new();
    let mut final_pools: BTreeMap<String, Value> = BTreeMap::new();
    for e in events {
        let p = &e.params;
        if e.event == "OPEN_POOL" || e.event == "POOL_UPDATE" {
            final_pools.insert(pool(p)?.key(), p.clone());
        }
        const KNOWN: &[&str] = &[
            "NEW_BLOCK",
            "BLOCK_PARSED",
            "NEW_TRANSACTION",
            "TRANSACTION_PARSED",
            "CREDIT",
            "DEBIT",
            "ASSET_CREATION",
            "ASSET_ISSUANCE",
            "RESET_ISSUANCE",
            "ASSET_DESTRUCTION",
            "ASSET_TRANSFER",
            "ASSET_DIVIDEND",
            "SEND",
            "ENHANCED_SEND",
            "MPMA_SEND",
            "SWEEP",
            "INVALID_SWEEP",
            "ATTACH_TO_UTXO",
            "DETACH_FROM_UTXO",
            "UTXO_MOVE",
            "OPEN_ORDER",
            "ORDER_UPDATE",
            "ORDER_FILLED",
            "ORDER_MATCH",
            "ORDER_MATCH_UPDATE",
            "ORDER_EXPIRATION",
            "ORDER_MATCH_EXPIRATION",
            "BTC_PAY",
            "INVALID_CANCEL",
            "OPEN_DISPENSER",
            "REFILL_DISPENSER",
            "DISPENSER_UPDATE",
            "DISPENSE",
            "OPEN_POOL",
            "POOL_UPDATE",
            "POOL_MATCH",
            "NEW_POOL_DEPOSIT",
            "NEW_POOL_WITHDRAWAL",
            "NEW_FAIRMINTER",
            "FAIRMINTER_UPDATE",
            "NEW_FAIRMINT",
        ];
        if !KNOWN.contains(&e.event.as_str())
            && !p["status"].as_str().is_some_and(|s| s.starts_with("invalid"))
        {
            for field in
                ["asset", "asset_a", "asset_b", "forward_asset", "backward_asset", "give_asset"]
            {
                if let Some(asset) = p[field].as_str() {
                    if numbered(&engine.tx, asset)? {
                        bail!(
                            "unsupported numbered semantic event {} at {}",
                            e.event,
                            e.event_index
                        );
                    }
                }
            }
        }
        if let Some(moves) = direct.remove(&e.event_index) {
            for m in moves {
                engine.move_units(e, m, &action(e)?)?;
            }
        }
        if e.event == "ASSET_ISSUANCE" && valid(p) {
            update_asset(&mut engine.tx, p, false)?;
        }
        if e.event == "RESET_ISSUANCE" && valid(p) {
            update_asset(&mut engine.tx, p, true)?;
        }
        if e.event == "ORDER_UPDATE" {
            let id = string(p, "tx_hash")?;
            let mut old = record(&engine.tx, "order", &id)?;
            if let (Some(o), Some(n)) = (old.as_object_mut(), p.as_object()) {
                o.extend(n.clone());
            }
            engine.tx.set(format!("record/order/{}", escaped(&id)), &old)?;
        }
        if e.event == "ORDER_MATCH" && p["status"] == "pending" {
            let id = string(p, "id")?;
            for (side, txside) in [("forward", "tx0"), ("backward", "tx1")] {
                let asset = string(p, &format!("{side}_asset"))?;
                if !numbered(&engine.tx, &asset)? {
                    continue;
                }
                let from = order_location(&engine.tx, &string(p, &format!("{txside}_hash"))?)?;
                let to = Location::escrow("pending_btc_match", &id, from.controller.clone());
                engine.move_units(
                    e,
                    Move {
                        asset,
                        from,
                        to,
                        quantity: integer(p, &format!("{side}_quantity"))?,
                        reverse: false,
                    },
                    "reserve pending BTC match",
                )?;
            }
        }
        if e.event == "ORDER_MATCH_UPDATE" && p["status"] == "expired" {
            let id = string(p, "id")?;
            let old = record(&engine.tx, "match", &id)?;
            for (side, txside) in [("forward", "tx0"), ("backward", "tx1")] {
                let asset = string(&old, &format!("{side}_asset"))?;
                if !numbered(&engine.tx, &asset)? {
                    continue;
                }
                let has_refund = events.iter().any(|c| {
                    c.event == "CREDIT"
                        && c.params["event"] == id
                        && c.params["asset"] == asset
                        && c.params["calling_function"]
                            .as_str()
                            .is_some_and(|a| a == "order expired" || a == "order cancelled")
                });
                if !has_refund {
                    let to = order_location(&engine.tx, &string(&old, &format!("{txside}_hash"))?)?;
                    let from = Location::escrow("pending_btc_match", &id, to.controller.clone());
                    engine.move_units(
                        e,
                        Move {
                            asset,
                            from,
                            to,
                            quantity: integer(&old, &format!("{side}_quantity"))?,
                            reverse: false,
                        },
                        "return expired BTC match to order",
                    )?;
                }
            }
        }
        // Fairminter-funded pools have no new wallet debit. The earlier block
        // action released these exact units from the fairminter's escrow.
        if e.event == "NEW_POOL_DEPOSIT" && valid(p) {
            let id = string(p, "tx_hash")?;
            for side in ["a", "b"] {
                let asset = string(p, &format!("asset_{side}"))?;
                if !numbered(&engine.tx, &asset)? {
                    continue;
                }
                let release = Location::escrow("fairmint_release", &id, None);
                let q = integer(p, &format!("quantity_{side}"))?;
                if engine.holding(&asset, &release)?.units.len() > 0 {
                    engine.move_units(
                        e,
                        Move { asset, from: release, to: pool(p)?, quantity: q, reverse: true },
                        "fairminter pool deposit",
                    )?;
                }
            }
        }
        if e.event == "ASSET_DESTRUCTION" && valid(p) && p["tag"] == "soft cap not reached" {
            let asset = string(p, "asset")?;
            if numbered(&engine.tx, &asset)? {
                let id = string(p, "tx_hash")?;
                engine.move_units(
                    e,
                    Move {
                        asset,
                        from: Location::escrow("fairmint_release", &id, None),
                        to: Location::escrow("destroyed", &id, None),
                        quantity: integer(p, "quantity")?,
                        reverse: true,
                    },
                    "failed fairmint soft cap",
                )?;
            }
        }
        if e.event != "CREDIT" && e.event != "DEBIT" {
            continue;
        }
        let asset = string(p, "asset")?;
        let act = action(e)?;
        let id = reference(e)?;
        let q = integer(p, "quantity")?;
        let is_numbered = numbered(&engine.tx, &asset)?;
        if is_numbered {
            let loc = ledger_location(p)?;
            let sign = if e.event == "CREDIT" { 1 } else { -1 };
            *engine
                .expected
                .entry((asset.clone(), loc.ledger.context("ledger key")?))
                .or_default() += i128::from(q) * sign;
        }
        // A pool credit may be divisible while the input is numbered.
        if e.event == "CREDIT" && act == "pool match" {
            let match_e = lookup(events, e.event_index, "POOL_MATCH", |m| {
                m["order_tx_hash"] == id
                    && m["forward_asset"] == asset
                    && m["forward_quantity"] == p["quantity"]
            })?;
            ensure!(pool_done.insert(match_e.event_index), "pool match reused");
            let m = &match_e.params;
            ensure!(valid(m), "invalid pool match has credit");
            let reserve = pool(m)?;
            let give = string(m, "backward_asset")?;
            let from = order_location(&engine.tx, &id)?;
            engine.move_units(
                e,
                Move {
                    asset: give,
                    from,
                    to: reserve.clone(),
                    quantity: integer(m, "backward_quantity")?,
                    reverse: true,
                },
                "pool match input (fee included)",
            )?;
            engine.move_units(
                e,
                Move {
                    asset: asset.clone(),
                    from: reserve,
                    to: ledger_location(p)?,
                    quantity: q,
                    reverse: true,
                },
                "pool match output",
            )?;
            continue;
        }
        if !is_numbered || q == 0 || skip.contains(&e.event_index) {
            continue;
        }
        let wallet = ledger_location(p)?;
        if e.event == "DEBIT" {
            let (to, reverse) = match act.as_str() {
                "open order" => (order_location(&engine.tx, &id)?, true),
                "open dispenser" => (dispenser_location(&engine.tx, &id)?, true),
                "refill dispenser" => {
                    let r =
                        lookup(events, e.event_index, "REFILL_DISPENSER", |r| r["tx_hash"] == id)?;
                    (
                        dispenser_location(&engine.tx, &string(&r.params, "dispenser_tx_hash")?)?,
                        false,
                    )
                }
                "pool deposit" => {
                    let r = lookup(events, e.event_index, "NEW_POOL_DEPOSIT", |r| {
                        r["tx_hash"] == id && valid(r)
                    })?;
                    (pool(&r.params)?, true)
                }
                "destroy" | "reset destroy" => (
                    Location::escrow(
                        if act == "reset destroy" { "retired" } else { "destroyed" },
                        &id,
                        None,
                    ),
                    true,
                ),
                "unescrowed fairmint" | "unescrowed premint" => {
                    let from = fair_location(&engine.tx, &id)?;
                    engine.move_units(
                        e,
                        Move {
                            asset,
                            from,
                            to: Location::escrow("fairmint_release", &id, None),
                            quantity: q,
                            reverse: false,
                        },
                        &act,
                    )?;
                    continue;
                }
                _ => bail!(
                    "unsupported numbered debit action {act} at {} for {asset}",
                    e.event_index
                ),
            };
            engine.move_units(e, Move { asset, from: wallet, to, quantity: q, reverse }, &act)?;
        } else {
            let budget = budgets.get_mut(&(asset.clone(), id.clone()));
            let mint_action = [
                "issuance",
                "reset issuance",
                "fairmint",
                "escrowed fairmint",
                "premint",
                "escrowed premint",
                "escrowed pool liquidity",
                "fairmint commission",
            ]
            .contains(&act.as_str());
            if mint_action && budget.as_ref().is_some_and(|b| **b >= q) {
                *budget.unwrap() -= q;
                let to = if act.starts_with("escrowed") {
                    let parent = fair_parent(&engine.tx, &id)?;
                    fair_location(&engine.tx, &parent)?
                } else {
                    wallet
                };
                engine.mint(e, &asset, to, q, &act)?;
                continue;
            }
            let from = match act.as_str() {
                "cancel order" => order_location(&engine.tx, &id)?,
                "filled" => {
                    // Core book fills reference the opposite order in the credit.
                    // The immediately following update identifies the refunded order;
                    // pool fills use that same update but reference their own order.
                    let update = events
                        .iter()
                        .find(|next| next.event_index > e.event_index)
                        .context("filled refund without following order update")?;
                    ensure!(
                        update.event == "ORDER_UPDATE" && update.params["status"] == "filled",
                        "filled refund is not followed by a filled order update"
                    );
                    let refunded_id = string(&update.params, "tx_hash")?;
                    let order = record(&engine.tx, "order", &refunded_id)?;
                    ensure!(
                        order["give_asset"] == asset && order["source"] == p["address"],
                        "filled refund asset/address differs from updated order"
                    );
                    let remaining = integer(&update.params, "give_remaining")?;
                    ensure!(
                        remaining == q || remaining == 0,
                        "filled refund quantity differs from order update"
                    );
                    order_location(&engine.tx, &refunded_id)?
                }
                "order expired" | "order cancelled" => {
                    Location::escrow("pending_btc_match", &id, wallet.controller.clone())
                }
                "order match" => {
                    let m = record(&engine.tx, "match", &id)?;
                    // Core emits the forward credit, then the backward credit.
                    // Asset/address/quantity alone cannot distinguish same-asset
                    // self-matches. Use the ledger leg order and validate its fields.
                    let credits: Vec<_> = events.iter().filter(|c| {
                        c.event == "CREDIT" && c.params["event"] == id
                            && c.params["calling_function"] == "order match"
                    }).collect();
                    ensure!(m["status"] == "completed" && credits.len() == 2,
                        "completed order match must have two credit legs");
                    let leg = credits.iter().position(|c| c.event_index == e.event_index)
                        .context("order match credit not found")?;
                    let (side, source, destination) = if leg == 0 {
                        ("forward", "tx0_hash", "tx1_address")
                    } else {
                        ("backward", "tx1_hash", "tx0_address")
                    };
                    ensure!(m[format!("{side}_asset")] == asset
                        && integer(&m, &format!("{side}_quantity"))? == q
                        && m[destination] == p["address"],
                        "order match credit differs from its recorded leg");
                    order_location(&engine.tx, &string(&m, source)?)?
                }
                "btcpay" => {
                    let r = lookup(events, e.event_index, "BTC_PAY", |r| {
                        r["tx_hash"] == id && valid(r)
                    })?;
                    Location::escrow(
                        "pending_btc_match",
                        &string(&r.params, "order_match_id")?,
                        wallet.controller.clone(),
                    )
                }
                "dispense" => {
                    let d = lookup(events, e.event_index, "DISPENSE", |r| {
                        r["tx_hash"] == id
                            && r["asset"] == asset
                            && r["destination"] == p["address"]
                            && r["dispense_quantity"] == p["quantity"]
                    })?;
                    dispenser_location(&engine.tx, &string(&d.params, "dispenser_tx_hash")?)?
                }
                "close dispenser" => {
                    let d = lookup(events, e.event_index, "DISPENSER_UPDATE", |r| {
                        r["asset"] == asset && r["status"] == 10 && r["give_remaining"] == 0
                    })?;
                    dispenser_location(&engine.tx, &string(&d.params, "tx_hash")?)?
                }
                "dispenser close" | "Closed: Max dispenses reached" => {
                    let d = lookup(events, e.event_index, "DISPENSE", |r| {
                        r["tx_hash"] == id && r["asset"] == asset && r["source"] == p["address"]
                    })?;
                    dispenser_location(&engine.tx, &string(&d.params, "dispenser_tx_hash")?)?
                }
                "pool withdraw" => {
                    let r = lookup(events, e.event_index, "NEW_POOL_WITHDRAWAL", |r| {
                        r["tx_hash"] == id && valid(r)
                    })?;
                    pool(&r.params)?
                }
                "unescrowed fairmint" | "premint" | "fairmint commission" => {
                    let parent = fair_parent(&engine.tx, &id)?;
                    Location::escrow("fairmint_release", &parent, None)
                }
                _ => bail!(
                    "unsupported numbered credit action {act} at {} for {asset}",
                    e.event_index
                ),
            };
            engine.move_units(
                e,
                Move { asset, from, to: wallet, quantity: q, reverse: true },
                &act,
            )?;
        }
    }
    for (_, p) in final_pools {
        let loc = pool(&p)?;
        for side in ["a", "b"] {
            let asset = string(&p, &format!("asset_{side}"))?;
            if numbered(&engine.tx, &asset)? {
                ensure!(
                    engine.holding(&asset, &loc)?.units.len()
                        == integer(&p, &format!("reserve_{side}"))?,
                    "pool reserve quantity mismatch for {asset}"
                );
            }
        }
    }
    for (key, q) in budgets {
        ensure!(q == 0, "issuance credit mismatch {key:?}: {q} units not allocated");
    }
    engine.actual.retain(|_, v| *v != 0);
    engine.expected.retain(|_, v| *v != 0);
    ensure!(
        engine.actual == engine.expected,
        "Core quantity reconciliation failed at {}: expected {:?}, numbered {:?}",
        boundary.height,
        engine.expected,
        engine.actual
    );
    // Release containers must never leak a balance across a completed block.
    for (_, value) in &engine.tx.changes {
        if let Some(value) = value {
            if let Ok(h) = serde_json::from_slice::<Holding>(value) {
                ensure!(
                    h.location.kind != "fairmint_release" || h.units.len() == 0,
                    "unallocated fairmint release"
                );
            }
        }
    }
    engine.tx.set("projection/tip".into(), boundary)?;
    engine.tx.set(format!("block/{:010}", boundary.height), boundary)?;
    engine.tx.commit(boundary.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    pub(super) fn db() -> (TempDir, rocksdb::DB) {
        let t = TempDir::new().unwrap();
        let db = storage::open(&t.path().join("db")).unwrap();
        (t, db)
    }
    pub(super) fn ledger(
        kind: &str,
        asset: &str,
        who: &str,
        q: u64,
        act: &str,
        id: &str,
    ) -> (String, Value) {
        let mut p = json!({"asset":asset,"address":who,"quantity":q,"event":id,"tx_index":1});
        p[if kind == "CREDIT" { "calling_function" } else { "action" }] = json!(act);
        (kind.into(), p)
    }
    pub(super) fn issue(asset: &str, q: u64) -> Vec<(String, Value)> {
        vec![
            (
                "ASSET_ISSUANCE".into(),
                json!({"asset":asset,"quantity":q,"divisible":false,"status":"valid","tx_hash":"mint","source":"alice","issuer":"alice"}),
            ),
            ledger("CREDIT", asset, "alice", q, "issuance", "mint"),
        ]
    }
    pub(super) fn apply(db: &rocksdb::DB, rows: Vec<(String, Value)>) -> Result<Boundary> {
        let prev = storage::read::<Boundary>(db, "projection/tip")?;
        let height = prev.as_ref().map_or(super::super::GENESIS, |b| b.height + 1);
        let first = prev.as_ref().map_or(0, |b| b.last + 1);
        let parent = prev.as_ref().map_or("parent".into(), |b| b.hash.clone());
        let boundary = Boundary {
            height,
            hash: format!("hash-{height}"),
            parent,
            first,
            last: first + rows.len() as u64 + 1,
            messages_hash: format!("messages-{height}"),
        };
        let mut events = vec![Event {
            event_index: first,
            event: "NEW_BLOCK".into(),
            params: json!({}),
            tx_hash: None,
            block_index: height,
        }];
        for (i, (event, params)) in rows.into_iter().enumerate() {
            events.push(Event {
                event_index: first + i as u64 + 1,
                event,
                params,
                tx_hash: Some("transaction".into()),
                block_index: height,
            });
        }
        events.push(Event {
            event_index: boundary.last,
            event: "BLOCK_PARSED".into(),
            params: json!({}),
            tx_hash: None,
            block_index: height,
        });
        project(db, &boundary, &events)?;
        Ok(boundary)
    }
    pub(super) fn units(db: &rocksdb::DB, asset: &str, loc: &Location) -> Vec<u64> {
        let h: Option<Holding> = storage::read(db, &hold_key(asset, loc)).unwrap();
        h.map_or(vec![], |h| {
            h.units
                .0
                .into_iter()
                .flat_map(|r| {
                    let n = r.len();
                    (0..n).map(move |i| if r.last >= r.first { r.first + i } else { r.first - i })
                })
                .collect()
        })
    }
    #[test]
    fn ordinary_mpma_self_send_and_atomic_failure() {
        let (_t, db) = db();
        apply(&db, issue("CARD", 5)).unwrap();
        apply(
            &db,
            vec![
                ledger("CREDIT", "CARD", "bob", 2, "mpma send", "multi"),
                ledger("CREDIT", "CARD", "alice", 1, "mpma send", "multi"),
                ledger("DEBIT", "CARD", "alice", 3, "mpma send", "multi"),
            ],
        )
        .unwrap();
        assert_eq!(units(&db, "CARD", &Location::address("bob")), vec![5, 4]);
        assert_eq!(units(&db, "CARD", &Location::address("alice")), vec![1, 2, 3]);
        let tip = storage::read::<Boundary>(&db, "projection/tip").unwrap().unwrap();
        assert!(
            apply(&db, vec![ledger("DEBIT", "CARD", "alice", 1, "unknown operation", "bad")])
                .is_err()
        );
        assert_eq!(
            storage::read::<Boundary>(&db, "projection/tip").unwrap().unwrap().last,
            tip.last
        );
        assert_eq!(units(&db, "CARD", &Location::address("alice")), vec![1, 2, 3]);
    }
    #[test]
    fn dispenser_refill_preserves_tail_and_close_requires_actual_credit() {
        let (_t, db) = db();
        apply(&db, issue("CARD", 6)).unwrap();
        apply(&db,vec![ledger("DEBIT","CARD","alice",2,"open dispenser","d"),("OPEN_DISPENSER".into(),json!({"tx_hash":"d","asset":"CARD","source":"alice","give_remaining":2,"status":0}))]).unwrap();
        apply(&db,vec![ledger("DEBIT","CARD","alice",2,"refill dispenser","r"),("REFILL_DISPENSER".into(),json!({"tx_hash":"r","dispenser_tx_hash":"d","destination":"alice","dispense_quantity":2}))]).unwrap();
        let d = Location::escrow("dispenser", "d", Some("alice".into()));
        assert_eq!(units(&db, "CARD", &d), vec![6, 5, 3, 4]);
        apply(
            &db,
            vec![("DISPENSER_UPDATE".into(), json!({"tx_hash":"d","asset":"CARD","status":11}))],
        )
        .unwrap();
        assert_eq!(units(&db, "CARD", &d), vec![6, 5, 3, 4]);
        apply(&db,vec![ledger("CREDIT","CARD","alice",4,"close dispenser","close_tx"),("DISPENSER_UPDATE".into(),json!({"tx_hash":"d","asset":"CARD","source":"alice","give_remaining":0,"status":10}))]).unwrap();
        assert_eq!(units(&db, "CARD", &Location::address("alice")), vec![1, 2, 4, 3, 5, 6]);
    }
    #[test]
    fn wrapping_preserves_identity_and_move_order() {
        let (_t, db) = db();
        apply(&db, issue("CARD", 3)).unwrap();
        let utxo = |kind: &str, id: &str, q, act: &str, txid: &str| {
            let (k, mut v) = ledger(kind, "CARD", "", q, act, txid);
            v["address"] = Value::Null;
            v["utxo"] = json!(id);
            v["utxo_address"] = json!("alice");
            (k, v)
        };
        apply(
            &db,
            vec![
                ledger("DEBIT", "CARD", "alice", 2, "attach to utxo", "a"),
                utxo("CREDIT", "a:0", 2, "attach to utxo", "a"),
            ],
        )
        .unwrap();
        apply(
            &db,
            vec![
                utxo("DEBIT", "a:0", 2, "utxo move", "m"),
                utxo("CREDIT", "m:0", 2, "utxo move", "m"),
            ],
        )
        .unwrap();
        apply(
            &db,
            vec![
                utxo("DEBIT", "m:0", 2, "detach from utxo", "d"),
                ledger("CREDIT", "CARD", "bob", 2, "detach from utxo", "d"),
            ],
        )
        .unwrap();
        assert_eq!(units(&db, "CARD", &Location::address("bob")), vec![2, 3]);
    }
    #[test]
    fn pool_spends_order_escrow_including_fee_then_withdraws_current_tail() {
        let (_t, db) = db();
        apply(&db, issue("CARD", 10)).unwrap();
        apply(&db,vec![ledger("DEBIT","CARD","alice",4,"pool deposit","pd"),("OPEN_POOL".into(),json!({"asset_a":"CARD","asset_b":"XCP","reserve_a":4,"reserve_b":1000})),("NEW_POOL_DEPOSIT".into(),json!({"tx_hash":"pd","asset_a":"CARD","asset_b":"XCP","quantity_a":4,"quantity_b":1000,"status":"valid"}))]).unwrap();
        apply(&db,vec![ledger("DEBIT","CARD","alice",2,"open order","order"),("OPEN_ORDER".into(),json!({"tx_hash":"order","source":"alice","give_asset":"CARD","give_quantity":2,"status":"open"})),ledger("CREDIT","XCP","alice",200,"pool match","order"),("POOL_UPDATE".into(),json!({"asset_a":"CARD","asset_b":"XCP","reserve_a":6,"reserve_b":800})),("POOL_MATCH".into(),json!({"order_tx_hash":"order","asset_a":"CARD","asset_b":"XCP","source":"alice","forward_asset":"XCP","forward_quantity":200,"backward_asset":"CARD","backward_quantity":2,"fee_quantity":1,"status":"valid"}))]).unwrap();
        assert_eq!(units(&db, "CARD", &Location::address("alice")), vec![1, 2, 3, 4]);
        apply(
            &db,
            vec![
                ledger("CREDIT", "CARD", "bob", 3, "pool withdraw", "withdraw"),
                (
                    "NEW_POOL_WITHDRAWAL".into(),
                    json!({"tx_hash":"withdraw","asset_a":"CARD","asset_b":"XCP","status":"valid"}),
                ),
                (
                    "POOL_UPDATE".into(),
                    json!({"asset_a":"CARD","asset_b":"XCP","reserve_a":3,"reserve_b":400}),
                ),
            ],
        )
        .unwrap();
        assert_eq!(units(&db, "CARD", &Location::address("bob")), vec![6, 5, 7]);
    }
    #[test]
    fn restart_and_undo_preserve_counter_and_all_indexes() {
        let (t, db) = db();
        let tip = apply(&db, issue("CARD", 5)).unwrap();
        let before: Vec<_> = db.iterator(IteratorMode::Start).map(|r| r.unwrap()).collect();
        let mut rows = vec![
            ledger("DEBIT", "CARD", "alice", 5, "reset destroy", "reset"),
            (
                "RESET_ISSUANCE".into(),
                json!({"asset":"CARD","quantity":2,"divisible":false,"status":"valid","tx_hash":"reset","source":"alice","issuer":"alice"}),
            ),
            ledger("CREDIT", "CARD", "alice", 2, "reset issuance", "reset"),
        ];
        let block = apply(&db, std::mem::take(&mut rows)).unwrap();
        assert_eq!(units(&db, "CARD", &Location::address("alice")), vec![6, 7]);
        drop(db);
        let db = storage::open(&t.path().join("db")).unwrap();
        assert_eq!(units(&db, "CARD", &Location::address("alice")), vec![6, 7]);
        storage::rollback(&db, block.height).unwrap();
        let after: Vec<_> = db.iterator(IteratorMode::Start).map(|r| r.unwrap()).collect();
        assert_eq!(before, after);
        assert_eq!(
            storage::read::<Boundary>(&db, "projection/tip").unwrap().unwrap().height,
            tip.height
        );
    }
    use rocksdb::IteratorMode;
}

#[cfg(test)]
mod legacy_tests {
    use super::*;
    #[test]
    fn every_legacy_serial_history_and_ordered_holding_matches() {
        for asset in ["RAREPEPE", "DJPEPE"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/fixtures/xcp_numbering/legacy_{asset}.json"));
            let f: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let t = tempfile::TempDir::new().unwrap();
            let db = storage::open(&t.path().join("db")).unwrap();
            let mut engine = Engine {
                tx: BlockTx::new(&db),
                height: 0,
                leg: 0,
                actual: BTreeMap::new(),
                expected: BTreeMap::new(),
            };
            update_asset(&mut engine.tx, &json!({"asset":asset,"divisible":false}), false).unwrap();
            for (i, op) in f["operations"].as_array().unwrap().iter().enumerate() {
                engine.height = op["block"].as_u64().unwrap();
                let e = Event {
                    event_index: i as u64,
                    event: "legacy_fixture".into(),
                    params: json!({}),
                    tx_hash: None,
                    block_index: engine.height,
                };
                let to: Location = serde_json::from_value(op["to"].clone()).unwrap();
                let q = op["quantity"].as_u64().unwrap();
                let reason = op["reason"].as_str().unwrap();
                if op["from"].is_null() {
                    engine.mint(&e, asset, to, q, reason).unwrap();
                } else {
                    engine
                        .move_units(
                            &e,
                            Move {
                                asset: asset.into(),
                                from: serde_json::from_value(op["from"].clone()).unwrap(),
                                to,
                                quantity: q,
                                reverse: op["reverse"].as_bool().unwrap(),
                            },
                            reason,
                        )
                        .unwrap();
                }
            }
            let mut histories: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            for (_, bytes) in engine.tx.prefix(&history_prefix(asset)).unwrap() {
                let m: Movement = serde_json::from_slice(&bytes).unwrap();
                let mut entry = json!({"address":m.to.controller.as_ref().unwrap_or(&m.to.id),"fromBlock":m.block});
                if m.to.kind == "order" {
                    entry["orderId"] = json!(m.to.id);
                }
                if m.to.kind == "dispenser" {
                    entry["dispenser"] = json!(m.to.id);
                }
                for r in m.units.0 {
                    for serial in r.first.min(r.last)..=r.first.max(r.last) {
                        histories.entry(format!("#{serial}")).or_default().push(entry.clone());
                    }
                }
            }
            assert_eq!(json!(histories), f["histories"], "every {asset} serial history");
            let mut holdings = BTreeMap::new();
            for (_, bytes) in engine.tx.prefix(&holding_prefix(asset)).unwrap() {
                let h: Holding = serde_json::from_slice(&bytes).unwrap();
                let units: Vec<u64> = h
                    .units
                    .0
                    .into_iter()
                    .flat_map(|r| {
                        let n = r.len();
                        (0..n)
                            .map(move |i| if r.last >= r.first { r.first + i } else { r.first - i })
                    })
                    .collect();
                if !units.is_empty() {
                    holdings.insert(h.location.key(), units);
                }
            }
            assert_eq!(json!(holdings), f["holdings"], "every {asset} ordered holding");
        }
    }
}

#[cfg(test)]
mod core_fixture_tests {
    use super::*;
    fn load(height: u64) -> Vec<Event> {
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/fixtures/xcp_numbering/core-11.3.0/{height}.json"));
        let v: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        let mut rows: Vec<Event> = serde_json::from_value(v["result"].clone()).unwrap();
        rows.sort_by_key(|e| e.event_index);
        assert_eq!(rows.first().unwrap().event, "NEW_BLOCK");
        assert_eq!(rows.last().unwrap().event, "BLOCK_PARSED");
        assert!(rows.windows(2).all(|w| w[1].event_index == w[0].event_index + 1));
        rows
    }
    // These tests retain full source blocks on disk and replay the relevant
    // complete transactions with synthetic pre-block serials. They validate
    // deployed Core event semantics, not historical identity before the block.
    fn replay(
        height: u64,
        asset: &str,
        pool_before: Option<u64>,
    ) -> (tempfile::TempDir, rocksdb::DB, Vec<Event>) {
        let full = load(height);
        let related: BTreeSet<_> = full
            .iter()
            .filter(|e| {
                ["asset", "asset_a", "asset_b", "give_asset", "get_asset"]
                    .iter()
                    .any(|f| e.params[*f] == asset)
            })
            .filter_map(|e| e.tx_hash.clone())
            .collect();
        let events: Vec<_> = full
            .iter()
            .filter(|e| {
                e.event == "NEW_BLOCK"
                    || e.event == "BLOCK_PARSED"
                    || e.tx_hash.as_ref().is_some_and(|h| related.contains(h))
            })
            .cloned()
            .collect();
        let first = full.first().unwrap();
        let last = full.last().unwrap();
        let b = Boundary {
            height,
            hash: string(&first.params, "block_hash").unwrap(),
            parent: string(&first.params, "previous_block_hash").unwrap(),
            first: first.event_index,
            last: last.event_index,
            messages_hash: string(&last.params, "messages_hash").unwrap(),
        };
        let t = tempfile::TempDir::new().unwrap();
        let db = storage::open(&t.path().join("db")).unwrap();
        let mut seed = Engine {
            tx: BlockTx::new(&db),
            height: height - 1,
            leg: 0,
            actual: BTreeMap::new(),
            expected: BTreeMap::new(),
        };
        if !events.iter().any(|e| e.event == "ASSET_ISSUANCE" && e.params["asset"] == asset) {
            update_asset(&mut seed.tx, &json!({"asset":asset,"divisible":false}), false).unwrap();
        }
        let e = Event {
            event_index: 0,
            event: "synthetic_fixture_seed".into(),
            params: json!({}),
            tx_hash: None,
            block_index: height - 1,
        };
        for destruction in events
            .iter()
            .filter(|e| e.event == "ASSET_DESTRUCTION" && e.params["tag"] == "pool_withdraw")
        {
            update_asset(
                &mut seed.tx,
                &json!({"asset":destruction.params["asset"],"divisible":true}),
                false,
            )
            .unwrap();
        }
        if let Some(q) = pool_before {
            seed.mint(
                &e,
                asset,
                Location::escrow("pool", &format!("{asset}:XCP"), None),
                q,
                "fixture seed",
            )
            .unwrap();
        }
        for debit in events.iter().filter(|e| e.event == "DEBIT" && e.params["asset"] == asset) {
            seed.mint(
                &e,
                asset,
                ledger_location(&debit.params).unwrap(),
                integer(&debit.params, "quantity").unwrap(),
                "fixture seed",
            )
            .unwrap();
        }
        seed.tx
            .set(
                "projection/tip".into(),
                &Boundary {
                    height: height - 1,
                    hash: b.parent.clone(),
                    parent: "synthetic ancestor".into(),
                    first: b.first - 1,
                    last: b.first - 1,
                    messages_hash: "synthetic seed".into(),
                },
            )
            .unwrap();
        seed.tx.commit(height - 1).unwrap();
        project(&db, &b, &events).unwrap_or_else(|e| panic!("real {asset} block {height}: {e:#}"));
        (t, db, events)
    }
    #[test]
    fn real_rarepepe_initial_issuance() {
        let (_t, db, events) = replay(428919, "RAREPEPE", None);
        let credit = events
            .iter()
            .find(|e| e.event == "CREDIT" && e.params["asset"] == "RAREPEPE")
            .unwrap();
        let h: Holding =
            storage::read(&db, &hold_key("RAREPEPE", &ledger_location(&credit.params).unwrap()))
                .unwrap()
                .unwrap();
        assert_eq!(h.units, Stack(vec![Run { first: 1, last: 300 }]));
    }
    #[test]
    fn real_attach_detach_and_utxo_move() {
        for (height, asset, reverse) in [
            (965928, "KEKMONTECRLO", true),
            (965949, "A3344466055756786903", false),
            (965150, "A3610466835930246700", true),
        ] {
            let (_t, db, events) = replay(height, asset, None);
            let credit = events
                .iter()
                .find(|e| e.event == "CREDIT" && e.params["asset"] == asset)
                .unwrap();
            let q = integer(&credit.params, "quantity").unwrap();
            let h: Holding =
                storage::read(&db, &hold_key(asset, &ledger_location(&credit.params).unwrap()))
                    .unwrap()
                    .unwrap();
            assert_eq!(
                h.units,
                Stack(vec![if reverse {
                    Run { first: q, last: 1 }
                } else {
                    Run { first: 1, last: q }
                }])
            );
        }
    }
    #[test]
    fn real_indivisible_pool_withdrawal() {
        let (_t, db, events) = replay(965018, "CAKE", Some(20));
        let credit = events
            .iter()
            .find(|e| e.event == "CREDIT" && e.params["asset"] == "CAKE")
            .unwrap();
        let h: Holding =
            storage::read(&db, &hold_key("CAKE", &ledger_location(&credit.params).unwrap()))
                .unwrap()
                .unwrap();
        assert_eq!(h.units, Stack(vec![Run { first: 20, last: 1 }]));
        assert!(
            storage::read::<Holding>(
                &db,
                &hold_key("CAKE", &Location::escrow("pool", "CAKE:XCP", None))
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn real_indivisible_pool_deposit_and_order_funded_swap() {
        let (_t, db, _) = replay(966033, "SIDEEYE", None);
        let loc = Location::escrow("pool", "SIDEEYE:XCP", None);
        let h: Holding = storage::read(&db, &hold_key("SIDEEYE", &loc)).unwrap().unwrap();
        assert_eq!(h.units, Stack(vec![Run { first: 10, last: 1 }]));
        let (_t, db, events) = replay(966056, "SIDEEYE", Some(10));
        let h: Holding = storage::read(&db, &hold_key("SIDEEYE", &loc)).unwrap().unwrap();
        assert_eq!(h.units, Stack(vec![Run { first: 1, last: 15 }]));
        let open = events
            .iter()
            .find(|e| e.event == "OPEN_ORDER" && e.params["give_asset"] == "SIDEEYE")
            .unwrap();
        let wallet = Location::address(open.params["source"].as_str().unwrap());
        assert!(storage::read::<Holding>(&db, &hold_key("SIDEEYE", &wallet)).unwrap().is_none());
    }
}

#[cfg(test)]
mod extension_tests {
    use super::tests::*;
    use super::*;
    #[test]
    fn pending_btc_match_settlement_and_expiration() {
        for settled in [true, false] {
            let (_t, db) = db();
            apply(&db, issue("CARD", 3)).unwrap();
            apply(&db,vec![ledger("DEBIT","CARD","alice",3,"open order","o0"),("OPEN_ORDER".into(),json!({"tx_hash":"o0","source":"alice","give_asset":"CARD","status":"open"})),("ORDER_MATCH".into(),json!({"id":"o0_o1","status":"pending","forward_asset":"CARD","forward_quantity":2,"backward_asset":"BTC","backward_quantity":1000,"tx0_hash":"o0","tx1_hash":"o1","tx0_address":"alice","tx1_address":"bob"}))]).unwrap();
            if settled {
                apply(
                    &db,
                    vec![
                        ledger("CREDIT", "CARD", "bob", 2, "btcpay", "pay"),
                        (
                            "BTC_PAY".into(),
                            json!({"tx_hash":"pay","status":"valid","order_match_id":"o0_o1"}),
                        ),
                    ],
                )
                .unwrap();
                assert_eq!(units(&db, "CARD", &Location::address("bob")), vec![1, 2]);
            } else {
                apply(
                    &db,
                    vec![(
                        "ORDER_MATCH_UPDATE".into(),
                        json!({"id":"o0_o1","order_match_id":"o0_o1","status":"expired"}),
                    )],
                )
                .unwrap();
                assert_eq!(
                    units(&db, "CARD", &Location::escrow("order", "o0", Some("alice".into()))),
                    vec![3, 2, 1]
                );
            }
        }
    }
    #[test]
    fn fairmint_block_release_can_fund_pool_or_destroy_without_new_units() {
        for success in [true, false] {
            let (_t, db) = db();
            let burn = "1CounterpartyXXXXXXXXXXXXXXXUWLpVr";
            apply(&db,vec![("NEW_FAIRMINTER".into(),json!({"tx_hash":"fm","asset":"CARD","source":"alice","status":"open"})),("ASSET_ISSUANCE".into(),json!({"asset":"CARD","divisible":false,"quantity":6,"status":"valid","tx_hash":"fm"})),ledger("CREDIT","CARD",burn,2,"escrowed premint","fm"),ledger("CREDIT","CARD",burn,4,"escrowed pool liquidity","fm")]).unwrap();
            apply(&db,vec![ledger("CREDIT","CARD",burn,3,"escrowed fairmint","mint1"),("NEW_FAIRMINT".into(),json!({"tx_hash":"mint1","asset":"CARD","fairminter_tx_hash":"fm","source":"bob","status":"valid"})),("ASSET_ISSUANCE".into(),json!({"asset":"CARD","divisible":false,"quantity":3,"status":"valid","tx_hash":"mint1"}))]).unwrap();
            let mut rows = vec![ledger("DEBIT", "CARD", burn, 9, "unescrowed fairmint", "fm")];
            if success {
                rows.extend(vec![ledger("CREDIT","CARD","bob",3,"unescrowed fairmint","mint1"),ledger("CREDIT","CARD","alice",2,"premint","fm"),("OPEN_POOL".into(),json!({"asset_a":"CARD","asset_b":"XCP","reserve_a":4,"reserve_b":10000})),("NEW_POOL_DEPOSIT".into(),json!({"tx_hash":"fm","asset_a":"CARD","asset_b":"XCP","quantity_a":4,"quantity_b":10000,"status":"valid"}))]);
            } else {
                rows.push(("ASSET_DESTRUCTION".into(),json!({"tx_hash":"fm","asset":"CARD","quantity":9,"tag":"soft cap not reached","status":"valid"})));
            }
            apply(&db, rows).unwrap();
            assert_eq!(
                storage::read::<Asset>(&db, &asset_key("CARD")).unwrap().unwrap().next_serial,
                10
            );
            if success {
                assert_eq!(
                    units(&db, "CARD", &Location::escrow("pool", "CARD:XCP", None)),
                    vec![4, 3, 2, 1]
                );
                assert_eq!(units(&db, "CARD", &Location::address("bob")), vec![9, 8, 7]);
            } else {
                assert_eq!(
                    units(&db, "CARD", &Location::escrow("destroyed", "fm", None)),
                    vec![9, 8, 7, 6, 5, 4, 3, 2, 1]
                );
            }
        }
    }
    #[test]
    fn multi_input_automatic_detach_and_unknown_pool_effect_fail_closed() {
        let (_t, db) = db();
        apply(&db, issue("CARD", 4)).unwrap();
        let utxo = |kind: &str, id: &str, q, act: &str, txid: &str| {
            let (k, mut v) = ledger(kind, "CARD", "", q, act, txid);
            v["address"] = Value::Null;
            v["utxo"] = json!(id);
            v["utxo_address"] = json!("alice");
            (k, v)
        };
        for id in ["a:0", "b:0"] {
            apply(
                &db,
                vec![
                    ledger("DEBIT", "CARD", "alice", 2, "attach to utxo", id),
                    utxo("CREDIT", id, 2, "attach to utxo", id),
                ],
            )
            .unwrap();
        }
        apply(
            &db,
            vec![
                utxo("DEBIT", "a:0", 2, "detach from utxo", "spend"),
                ledger("CREDIT", "CARD", "bob", 2, "detach from utxo", "spend"),
                utxo("DEBIT", "b:0", 2, "detach from utxo", "spend"),
                ledger("CREDIT", "CARD", "bob", 2, "detach from utxo", "spend"),
            ],
        )
        .unwrap();
        assert_eq!(units(&db, "CARD", &Location::address("bob")), vec![3, 4, 1, 2]);
        assert!(
            apply(
                &db,
                vec![("FUTURE_POOL_EFFECT".into(), json!({"asset_a":"CARD","asset_b":"XCP"}))]
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod filled_refund_tests {
    use super::tests::*;
    use super::*;
    #[test]
    fn real_book_fill_refunds_the_updated_order_not_the_credit_reference() {
        let (_dir, db) = db();
        let value: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/xcp_numbering/core-11.3.0/295004.json"
        ))
        .unwrap();
        let mut events: Vec<Event> = value["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let mut row = row.clone();
                row["block_index"] = json!(295004);
                serde_json::from_value(row).unwrap()
            })
            .collect();
        events.sort_by_key(|e| e.event_index);
        let owner = "1BGSqmCnkdyB7W2T15rSBS4qXBdY3LZn1t";
        let mut rows = issue("MEAT", 324404);
        rows.extend([
            ledger("DEBIT", "MEAT", "alice", 324404, "send", "seed"),
            ledger("CREDIT", "MEAT", owner, 324404, "send", "seed"),
        ]);
        apply(&db, rows).unwrap();
        let mut batch = rocksdb::WriteBatch::default();
        for e in &events {
            if e.event != "ORDER_MATCH" {
                continue;
            }
            let id = e.params["tx0_hash"].as_str().unwrap();
            storage::put(&mut batch, &format!("record/order/{}", escaped(id)), &json!({
                "tx_hash":id, "source":e.params["tx0_address"], "give_asset":"XCP", "status":"open"
            })).unwrap();
        }
        let boundary = Boundary {
            height: 295004,
            first: 75094,
            last: 75116,
            hash: events[0].params["block_hash"].as_str().unwrap().into(),
            parent: events[0].params["previous_block_hash"].as_str().unwrap().into(),
            messages_hash: events.last().unwrap().params["messages_hash"].as_str().unwrap().into(),
        };
        storage::put(
            &mut batch,
            "projection/tip",
            &Boundary {
                height: 295003,
                first: 75092,
                last: 75093,
                hash: boundary.parent.clone(),
                parent: "fixture".into(),
                messages_hash: "fixture".into(),
            },
        )
        .unwrap();
        storage::commit(&db, batch).unwrap();
        project(&db, &boundary, &events).unwrap();
        assert_eq!(units(&db, "MEAT", &Location::address(owner)).len(), 105457);
        assert_eq!(
            units(&db, "MEAT", &Location::address("1QBYgXMTqEQNgoVotQN2iP1sPhHRPEoDHb")).len(),
            218947
        );
        let id = "2ca11ca09a67351968fb0ecb1994eeccacab4614eded94be21c3b4595377bf82";
        assert!(units(&db, "MEAT", &Location::escrow("order", id, Some(owner.into()))).is_empty());
    }
}

#[cfg(test)]
mod divisible_send_tests {
    use super::*;
    #[test]
    fn real_divisible_null_address_send_does_not_enter_numbering() {
        let (_dir, db) = super::tests::db();
        let value: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/xcp_numbering/core-11.3.0/329675.json"
        ))
        .unwrap();
        let mut events: Vec<Event> = value["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let mut row = row.clone();
                row["block_index"] = json!(329675);
                serde_json::from_value(row).unwrap()
            })
            .collect();
        events.sort_by_key(|e| e.event_index);
        let boundary = Boundary {
            height: 329675,
            first: 701116,
            last: 701122,
            hash: events[0].params["block_hash"].as_str().unwrap().into(),
            parent: events[0].params["previous_block_hash"].as_str().unwrap().into(),
            messages_hash: events.last().unwrap().params["messages_hash"].as_str().unwrap().into(),
        };
        let mut tx = BlockTx::new(&db);
        update_asset(&mut tx, &json!({"asset":"SJCX","divisible":true}), false).unwrap();
        tx.set(
            "projection/tip".into(),
            &Boundary {
                height: 329674,
                first: 701099,
                last: 701115,
                hash: boundary.parent.clone(),
                parent: "fixture".into(),
                messages_hash: "fixture".into(),
            },
        )
        .unwrap();
        tx.commit(329674).unwrap();
        project(&db, &boundary, &events).unwrap();
        assert!(BlockTx::new(&db).prefix(&history_prefix("SJCX")).unwrap().is_empty());
        assert_eq!(
            storage::read::<Asset>(&db, &asset_key("SJCX")).unwrap().unwrap().next_serial,
            1
        );
    }
}

#[cfg(test)]
mod same_asset_match_tests {
    use super::tests::*;
    use super::*;

    #[test]
    fn real_bitcorn_match_uses_both_order_escrows() {
        let (_dir, db) = db();
        let value: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/xcp_numbering/core-11.3.0/713886.json"
        )).unwrap();
        let mut events: Vec<Event> = value["result"].as_array().unwrap().iter().map(|row| {
            let mut row = row.clone();
            row["block_index"] = json!(713886);
            serde_json::from_value(row).unwrap()
        }).collect();
        events.sort_by_key(|e| e.event_index);
        let matched = events.iter().find(|e| e.event == "ORDER_MATCH").unwrap().params.clone();
        let owner0 = matched["tx0_address"].as_str().unwrap();
        let owner1 = matched["tx1_address"].as_str().unwrap();
        let mut seed = issue("BITCORN", 1010);
        seed.extend([
            ledger("DEBIT", "BITCORN", "alice", 10, "send", "seed0"),
            ledger("CREDIT", "BITCORN", owner0, 10, "send", "seed0"),
            ledger("DEBIT", "BITCORN", "alice", 1000, "send", "seed1"),
            ledger("CREDIT", "BITCORN", owner1, 1000, "send", "seed1"),
        ]);
        apply(&db, seed).unwrap();
        let boundary = Boundary {
            height: 713886,
            first: events.first().unwrap().event_index,
            last: events.last().unwrap().event_index,
            hash: events[0].params["block_hash"].as_str().unwrap().into(),
            parent: events[0].params["previous_block_hash"].as_str().unwrap().into(),
            messages_hash: events.last().unwrap().params["messages_hash"].as_str().unwrap().into(),
        };
        let mut batch = rocksdb::WriteBatch::default();
        storage::put(&mut batch, "projection/tip", &Boundary {
            height: boundary.height - 1, first: boundary.first - 2, last: boundary.first - 1,
            hash: boundary.parent.clone(), parent: "fixture".into(), messages_hash: "fixture".into(),
        }).unwrap();
        storage::commit(&db, batch).unwrap();
        // Preserve both complete match transactions from the captured full block;
        // synthetic pre-block serials isolate settlement from unrelated assets.
        events.retain(|e| e.event == "NEW_BLOCK" || e.event == "BLOCK_PARSED"
            || e.tx_hash.as_deref() == matched["tx0_hash"].as_str()
            || e.tx_hash.as_deref() == matched["tx1_hash"].as_str());
        project(&db, &boundary, &events).unwrap();
        assert_eq!(units(&db, "BITCORN", &Location::address(owner0)), (1..=1000).rev().collect::<Vec<_>>());
        assert_eq!(units(&db, "BITCORN", &Location::address(owner1)), (1001..=1010).rev().collect::<Vec<_>>());
        for side in ["tx0", "tx1"] {
            assert!(units(&db, "BITCORN", &Location::escrow("order", matched[format!("{side}_hash")].as_str().unwrap(), None)).is_empty());
        }
    }

    #[test]
    fn same_asset_self_match_with_identical_credits_keeps_leg_order() {
        let (_dir, db) = db();
        apply(&db, issue("CARD", 4)).unwrap();
        apply(&db, vec![
            ledger("DEBIT", "CARD", "alice", 2, "open order", "o0"),
            ("OPEN_ORDER".into(), json!({"tx_hash":"o0","source":"alice","give_asset":"CARD","status":"open"})),
            ledger("DEBIT", "CARD", "alice", 2, "open order", "o1"),
            ("OPEN_ORDER".into(), json!({"tx_hash":"o1","source":"alice","give_asset":"CARD","status":"open"})),
            ledger("CREDIT", "CARD", "alice", 2, "order match", "o0_o1"),
            ledger("CREDIT", "CARD", "alice", 2, "order match", "o0_o1"),
            ("ORDER_MATCH".into(), json!({"id":"o0_o1","status":"completed","forward_asset":"CARD","forward_quantity":2,"backward_asset":"CARD","backward_quantity":2,"tx0_hash":"o0","tx1_hash":"o1","tx0_address":"alice","tx1_address":"alice"})),
        ]).unwrap();
        assert_eq!(units(&db, "CARD", &Location::address("alice")), vec![3,4,1,2]);
    }
}
