//! Read-only baseline Python ledger compatibility. Extended schemas fail closed.
use crate::StoreError;
use rusqlite::{Connection, limits::Limit, types::ValueRef};
use sadr_core::{
    candidate::Candidate,
    finance::{Amount, Money, Price, Side},
    time::Timestamp,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

const MAX_ROWS: usize = 10_000;
const MAX_TEXT: usize = 65_536;
const MAX_TOTAL_TEXT: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct LegacyCandidate {
    original_json: String,
    original_hash: String,
    candidate: Candidate,
}

impl LegacyCandidate {
    pub fn parse(original_json: String, original_hash: String) -> Result<Self, StoreError> {
        if original_json.len() > MAX_TEXT {
            return Err(StoreError::Bound);
        }
        if original_hash.len() != 64
            || !original_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || Sha256::digest(original_json.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
                != original_hash
        {
            return Err(StoreError::CandidateHash);
        }
        let candidate = serde_json::from_str(&original_json).map_err(|_| StoreError::Candidate)?;
        Ok(Self {
            original_json,
            original_hash,
            candidate,
        })
    }
    pub fn original_json(&self) -> &str {
        &self.original_json
    }
    pub fn original_hash(&self) -> &str {
        &self.original_hash
    }
    pub fn candidate(&self) -> &Candidate {
        &self.candidate
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState {
    Reserved,
    PartFilled,
    Filled,
    CancelPending,
    Cancelled,
    Closed,
}

impl IntentState {
    fn parse(text: &str) -> Result<Self, StoreError> {
        match text {
            "RESERVED" => Ok(Self::Reserved),
            "PART_FILLED" => Ok(Self::PartFilled),
            "FILLED" => Ok(Self::Filled),
            "CANCEL_PENDING" => Ok(Self::CancelPending),
            "CANCELLED" => Ok(Self::Cancelled),
            "CLOSED" => Ok(Self::Closed),
            _ => Err(StoreError::Rows),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Intent {
    id: String,
    candidate: LegacyCandidate,
    lots: u64,
    reserved: Money,
    state: IntentState,
    created: Timestamp,
}

impl Intent {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn candidate(&self) -> &LegacyCandidate {
        &self.candidate
    }
    pub fn lots(&self) -> u64 {
        self.lots
    }
    pub fn reserved(&self) -> Money {
        self.reserved
    }
    pub fn state(&self) -> IntentState {
        self.state
    }
    pub fn created(&self) -> Timestamp {
        self.created
    }
}

#[derive(Debug, Clone)]
pub struct LedgerSnapshot {
    cash: Money,
    initial: Price,
    halted: bool,
    intents: BTreeMap<String, Intent>,
    holdings: BTreeMap<(String, usize), i64>,
    reserved: Money,
    fills: usize,
    metadata: BTreeMap<String, String>,
    event_kinds: Vec<String>,
}

impl LedgerSnapshot {
    pub fn cash(&self) -> Money {
        self.cash
    }
    pub fn initial(&self) -> Price {
        self.initial
    }
    pub fn halted(&self) -> bool {
        self.halted
    }
    pub fn intents(&self) -> &BTreeMap<String, Intent> {
        &self.intents
    }
    pub fn holdings(&self, intent: &str, leg: usize) -> i64 {
        self.holdings
            .get(&(intent.to_owned(), leg))
            .copied()
            .unwrap_or(0)
    }
    pub fn reserved(&self) -> Money {
        self.reserved
    }
    pub fn fill_count(&self) -> usize {
        self.fills
    }
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.metadata
    }
    pub fn event_kinds(&self) -> &[String] {
        &self.event_kinds
    }
}

/// Consume a caller-owned connection and return only a verified immutable
/// projection. No path is opened, no SQL from the database is executed, and no
/// write or recovery is performed. The read transaction is rolled back before
/// return; the connection is closed on every path. File/account/session identity
/// and WAL/sidecar ownership remain caller obligations, not facts proven here.
/// File-backed connections must be read-only; active writers are invalid inputs.
pub fn inspect_connection(mut db: Connection) -> Result<LedgerSnapshot, StoreError> {
    db.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .map_err(|_| StoreError::Snapshot)?;
    if !db.is_autocommit() {
        return Err(StoreError::Snapshot);
    }
    db.busy_timeout(Duration::from_millis(250))
        .map_err(|_| StoreError::Snapshot)?;
    db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 1_000_000)
        .map_err(|_| StoreError::Snapshot)?;
    db.execute_batch(
        "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA read_uncommitted=OFF;",
    )
    .map_err(|_| StoreError::Snapshot)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))
        .map_err(|_| StoreError::Snapshot)?;
    let transaction = db.transaction().map_err(|_| StoreError::Snapshot)?;
    let result = inspect(&transaction, deadline);
    // Drop's fallback also rolls back on any SQL/validation error.
    transaction.rollback().map_err(|_| StoreError::Snapshot)?;
    result
}

type Column = (&'static str, &'static str, i64, Option<&'static str>, i64);
const ACCOUNT: &[Column] = &[
    ("id", "INTEGER", 0, None, 1),
    ("cash", "TEXT", 0, None, 0),
    ("initial", "TEXT", 0, None, 0),
    ("halted", "INTEGER", 1, Some("0"), 0),
];
const INTENTS: &[Column] = &[
    ("id", "TEXT", 0, None, 1),
    ("hash", "TEXT", 1, None, 0),
    ("candidate", "TEXT", 1, None, 0),
    ("lots", "INTEGER", 1, None, 0),
    ("reserved", "TEXT", 1, None, 0),
    ("state", "TEXT", 1, None, 0),
    ("created", "TEXT", 1, None, 0),
];
const FILLS: &[Column] = &[
    ("id", "TEXT", 0, None, 1),
    ("intent", "TEXT", 1, None, 0),
    ("leg", "INTEGER", 1, None, 0),
    ("side", "TEXT", 1, None, 0),
    ("units", "INTEGER", 1, None, 0),
    ("price", "TEXT", 1, None, 0),
    ("fee", "TEXT", 1, None, 0),
    ("stamp", "TEXT", 1, None, 0),
];
const EVENTS: &[Column] = &[
    ("seq", "INTEGER", 0, None, 1),
    ("kind", "TEXT", 1, None, 0),
    ("payload", "TEXT", 1, None, 0),
];
const METADATA: &[Column] = &[("key", "TEXT", 0, None, 1), ("value", "TEXT", 1, None, 0)];

fn schema(db: &Connection) -> Result<(), StoreError> {
    // Caller-supplied connections must not let TEMP/attached schemas shadow the
    // baseline objects. No URI, ATTACH or caller SQL is interpreted here.
    let mut stmt = db
        .prepare("PRAGMA database_list")
        .map_err(|_| StoreError::Schema)?;
    let mut databases = stmt.query([]).map_err(|_| StoreError::Schema)?;
    let database = databases
        .next()
        .map_err(|_| StoreError::Schema)?
        .ok_or(StoreError::Schema)?;
    let filename: String = database.get(2).map_err(|_| StoreError::Schema)?;
    if !filename.is_empty() && !db.is_readonly("main").map_err(|_| StoreError::Schema)? {
        return Err(StoreError::Schema);
    }
    if database
        .get::<_, String>(1)
        .map_err(|_| StoreError::Schema)?
        != "main"
        || databases.next().map_err(|_| StoreError::Schema)?.is_some()
    {
        return Err(StoreError::Schema);
    }
    let expected = [
        ("account", ACCOUNT),
        ("intents", INTENTS),
        ("fills", FILLS),
        ("events", EVENTS),
        ("metadata", METADATA),
    ];
    let mut stmt = db
        .prepare("SELECT type,name,sql FROM sqlite_schema LIMIT 33")
        .map_err(|_| StoreError::Schema)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Schema)?;
    let mut tables = Vec::new();
    let mut count = 0;
    while let Some(row) = rows.next().map_err(|_| StoreError::Schema)? {
        count += 1;
        if count > 32 {
            return Err(StoreError::Schema);
        }
        let kind: String = row.get(0).map_err(|_| StoreError::Schema)?;
        let name: String = row.get(1).map_err(|_| StoreError::Schema)?;
        let sql: Option<String> = row.get(2).map_err(|_| StoreError::Schema)?;
        if kind == "table" && expected.iter().any(|(n, _)| *n == name) {
            if !sql.as_deref().is_some_and(|s| {
                s.trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE TABLE ")
            }) {
                return Err(StoreError::Schema);
            }
            tables.push(name);
        } else if !(kind == "index" && sql.is_none() && name.starts_with("sqlite_autoindex_")) {
            return Err(StoreError::Schema);
        }
    }
    if tables.len() != expected.len() {
        return Err(StoreError::Schema);
    }
    for (name, columns) in expected {
        let mut stmt = db
            .prepare(&format!("PRAGMA table_xinfo({name})"))
            .map_err(|_| StoreError::Schema)?;
        let mut rows = stmt.query([]).map_err(|_| StoreError::Schema)?;
        for (index, (n, t, not_null, default, pk)) in columns.iter().enumerate() {
            let row = rows
                .next()
                .map_err(|_| StoreError::Schema)?
                .ok_or(StoreError::Schema)?;
            let got: (i64, String, String, i64, Option<String>, i64, i64) = (
                row.get(0).map_err(|_| StoreError::Schema)?,
                row.get(1).map_err(|_| StoreError::Schema)?,
                row.get(2).map_err(|_| StoreError::Schema)?,
                row.get(3).map_err(|_| StoreError::Schema)?,
                row.get(4).map_err(|_| StoreError::Schema)?,
                row.get(5).map_err(|_| StoreError::Schema)?,
                row.get(6).map_err(|_| StoreError::Schema)?,
            );
            if got
                != (
                    index as i64,
                    (*n).into(),
                    (*t).into(),
                    *not_null,
                    default.map(str::to_owned),
                    *pk,
                    0,
                )
            {
                return Err(StoreError::Schema);
            }
        }
        if rows.next().map_err(|_| StoreError::Schema)?.is_some() {
            return Err(StoreError::Schema);
        }
    }
    Ok(())
}

fn text(row: &rusqlite::Row<'_>, index: usize, total: &mut usize) -> Result<String, StoreError> {
    match row.get_ref(index).map_err(|_| StoreError::Rows)? {
        ValueRef::Text(value) if value.len() <= MAX_TEXT => {
            *total = total.checked_add(value.len()).ok_or(StoreError::Bound)?;
            if *total > MAX_TOTAL_TEXT {
                return Err(StoreError::Bound);
            }
            String::from_utf8(value.to_vec()).map_err(|_| StoreError::Rows)
        }
        ValueRef::Text(_) => Err(StoreError::Bound),
        _ => Err(StoreError::Rows),
    }
}

fn integer(row: &rusqlite::Row<'_>, index: usize) -> Result<i64, StoreError> {
    row.get(index).map_err(|_| StoreError::Rows)
}
fn amount(value: &str) -> Result<Money, StoreError> {
    Money::parse(value).map_err(|_| StoreError::Rows)
}
fn positive(value: &str) -> Result<Price, StoreError> {
    Price::parse(value).map_err(|_| StoreError::Rows)
}
fn bounded(count: usize, deadline: Instant) -> Result<(), StoreError> {
    if count > MAX_ROWS || Instant::now() >= deadline {
        Err(StoreError::Bound)
    } else {
        Ok(())
    }
}

fn inspect(db: &Connection, deadline: Instant) -> Result<LedgerSnapshot, StoreError> {
    schema(db)?;
    let mut total = 0;
    let mut stmt = db
        .prepare("SELECT id,cash,initial,halted FROM account LIMIT 2")
        .map_err(|_| StoreError::Rows)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Rows)?;
    let row = rows
        .next()
        .map_err(|_| StoreError::Rows)?
        .ok_or(StoreError::Rows)?;
    if integer(row, 0)? != 1 || !matches!(integer(row, 3)?, 0 | 1) {
        return Err(StoreError::Rows);
    }
    let cash = amount(&text(row, 1, &mut total)?)?;
    let initial = positive(&text(row, 2, &mut total)?)?;
    let halted = integer(row, 3)? == 1;
    if rows.next().map_err(|_| StoreError::Rows)?.is_some() {
        return Err(StoreError::Rows);
    }
    let mut snapshot = LedgerSnapshot {
        cash,
        initial,
        halted,
        intents: BTreeMap::new(),
        holdings: BTreeMap::new(),
        reserved: Money::ZERO,
        fills: 0,
        metadata: BTreeMap::new(),
        event_kinds: Vec::new(),
    };
    let mut stmt = db.prepare("SELECT id,hash,candidate,lots,reserved,state,created FROM intents ORDER BY id LIMIT 10001").map_err(|_| StoreError::Rows)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Rows)?;
    while let Some(row) = rows.next().map_err(|_| StoreError::Rows)? {
        bounded(snapshot.intents.len() + 1, deadline)?;
        let id = text(row, 0, &mut total)?;
        let hash = text(row, 1, &mut total)?;
        let candidate = LegacyCandidate::parse(text(row, 2, &mut total)?, hash)?;
        let lots = u64::try_from(integer(row, 3)?).map_err(|_| StoreError::Rows)?;
        if lots == 0 {
            return Err(StoreError::Rows);
        }
        let reserved = amount(&text(row, 4, &mut total)?)?;
        let state = IntentState::parse(&text(row, 5, &mut total)?)?;
        let created = Timestamp::parse(&text(row, 6, &mut total)?).map_err(|_| StoreError::Rows)?;
        if !(candidate.candidate().created_at() <= created
            && created < candidate.candidate().valid_until())
        {
            return Err(StoreError::Rows);
        }
        snapshot.reserved = Money::try_from(
            snapshot
                .reserved
                .amount()
                .checked_add(reserved.amount())
                .map_err(|_| StoreError::Arithmetic)?,
        )
        .map_err(|_| StoreError::Arithmetic)?;
        let intent = Intent {
            id: id.clone(),
            candidate,
            lots,
            reserved,
            state,
            created,
        };
        if snapshot.intents.insert(id, intent).is_some() {
            return Err(StoreError::Rows);
        }
    }
    let mut cash_check = initial.amount();
    let mut stmt = db.prepare("SELECT id,intent,leg,side,units,price,fee,stamp FROM fills ORDER BY stamp,id LIMIT 10001").map_err(|_| StoreError::Rows)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Rows)?;
    while let Some(row) = rows.next().map_err(|_| StoreError::Rows)? {
        snapshot.fills += 1;
        bounded(snapshot.fills, deadline)?;
        let _id = text(row, 0, &mut total)?;
        let id = text(row, 1, &mut total)?;
        let leg = usize::try_from(integer(row, 2)?).map_err(|_| StoreError::Rows)?;
        let side = match text(row, 3, &mut total)?.as_str() {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            _ => return Err(StoreError::Rows),
        };
        let units = integer(row, 4)?;
        let price = positive(&text(row, 5, &mut total)?)?;
        let fee = amount(&text(row, 6, &mut total)?)?;
        let stamp = Timestamp::parse(&text(row, 7, &mut total)?).map_err(|_| StoreError::Rows)?;
        let candidate = snapshot
            .intents
            .get(&id)
            .ok_or(StoreError::Rows)?
            .candidate
            .candidate();
        let contract = candidate.legs().get(leg).ok_or(StoreError::Rows)?;
        contract
            .instrument()
            .validate_order(units, price)
            .map_err(|_| StoreError::Rows)?;
        if side == contract.side()
            && (!(candidate.created_at() <= stamp && stamp < candidate.valid_until())
                || (side == Side::Buy && price > contract.limit_price())
                || (side == Side::Sell && price < contract.limit_price()))
        {
            return Err(StoreError::Rows);
        }
        let delta = if side == Side::Buy { units } else { -units };
        let holding = snapshot.holdings.entry((id, leg)).or_default();
        *holding = holding.checked_add(delta).ok_or(StoreError::Arithmetic)?;
        let turnover = price
            .amount()
            .checked_mul(Amount::from_integer(units))
            .map_err(|_| StoreError::Arithmetic)?;
        cash_check = if side == Side::Sell {
            cash_check.checked_add(turnover)
        } else {
            cash_check.checked_sub(turnover)
        }
        .and_then(|a| a.checked_sub(fee.amount()))
        .map_err(|_| StoreError::Arithmetic)?;
    }
    if cash_check != cash.amount() {
        return Err(StoreError::Rows);
    }
    for intent in snapshot.intents.values() {
        let mut positions = Vec::new();
        let mut fully_filled = true;
        for (leg, contract) in intent.candidate.candidate().legs().iter().enumerate() {
            let qty = i128::from(snapshot.holdings(&intent.id, leg));
            let maximum = i128::from(intent.lots) * i128::from(contract.instrument().lot_size());
            fully_filled &= qty.abs() == maximum;
            if (contract.side() == Side::Buy && qty < 0)
                || (contract.side() == Side::Sell && qty > 0)
                || qty.abs() > maximum
            {
                return Err(StoreError::Rows);
            }
            positions.push(qty);
        }
        let flat = positions.iter().all(|q| *q == 0);
        let has_reservation = intent.reserved > Money::ZERO;
        let valid_state = match intent.state {
            IntentState::Reserved => flat && has_reservation,
            IntentState::Filled => fully_filled && has_reservation,
            IntentState::PartFilled => !flat && !fully_filled && has_reservation,
            IntentState::CancelPending => has_reservation,
            IntentState::Cancelled => flat != has_reservation,
            IntentState::Closed => flat && !has_reservation,
        };
        if !valid_state || (positions.len() == 2 && positions[1].abs() > positions[0]) {
            return Err(StoreError::Rows);
        }
    }
    let mut stmt = db
        .prepare("SELECT key,value FROM metadata ORDER BY key LIMIT 10001")
        .map_err(|_| StoreError::Rows)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Rows)?;
    while let Some(row) = rows.next().map_err(|_| StoreError::Rows)? {
        bounded(snapshot.metadata.len() + 1, deadline)?;
        let key = text(row, 0, &mut total)?;
        let value = text(row, 1, &mut total)?;
        if snapshot.metadata.insert(key, value).is_some() {
            return Err(StoreError::Rows);
        }
    }
    let mut stmt = db
        .prepare("SELECT seq,kind FROM events ORDER BY seq LIMIT 10001")
        .map_err(|_| StoreError::Rows)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Rows)?;
    while let Some(row) = rows.next().map_err(|_| StoreError::Rows)? {
        bounded(snapshot.event_kinds.len() + 1, deadline)?;
        if integer(row, 0)? <= 0 {
            return Err(StoreError::Rows);
        }
        snapshot.event_kinds.push(text(row, 1, &mut total)?);
    }
    bounded(0, deadline)?;
    Ok(snapshot)
}
