//! Offline configured research-proposal binding.
//!
//! The completion is injected. This module does not call a provider, read an API
//! key, execute a script, or authorize trading. It revalidates the private config,
//! refuses before creating the proposal directory, and reserves the frontier call
//! in the shared model journal. Existing rows are kept. Failed and cancelled calls
//! are not refunded. The config hash is a Rust canonical digest of the reviewed
//! document, not a pydantic `model_dump_json` byte image. Native subscription
//! configs are refused before any journal file is created.

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, limits::Limit};
use sadr_core::{
    native, research_evidence::EvidenceTools, research_prompt::HistoricalPlan,
    research_signal::DailyReview,
};
use serde::Serialize;
use serde_json::{Map, Value, json, value::RawValue};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fmt, fs,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use crate::research_router::{self, CallFault, ProposalInput};

const EMPTY_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS model_calls (id INTEGER PRIMARY KEY, day TEXT,
            model TEXT, reserved INTEGER, status TEXT, response_hash TEXT);
        CREATE TABLE IF NOT EXISTS extraction_cache (key TEXT PRIMARY KEY, output TEXT);
        CREATE TABLE IF NOT EXISTS model_metrics (call_id INTEGER PRIMARY KEY,
            latency_ms INTEGER, actual_micro_usd INTEGER, receipt_hash TEXT,
            input_tokens INTEGER, output_tokens INTEGER);";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Private,
    Unavailable,
    Cancelled,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Private => "Private research proposal configuration unavailable",
            Self::Unavailable => {
                "Configured research proposal unavailable; preserve artifacts and spend"
            }
            Self::Cancelled => "research_proposal_cancelled",
        })
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug)]
enum Fault {
    Cancelled,
    Other,
}

pub struct Config {
    plan: HistoricalPlan,
    frontier: Spec,
    cheap: Spec,
    daily: u64,
    monthly: u64,
    daily_calls: Option<u64>,
    database: PathBuf,
    database_text: String,
    native: bool,
    canonical: Vec<u8>,
    hash: String,
}
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResearchProposalConfig(redacted, no provider, no authority)")
    }
}
impl Config {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        parse_config(raw).map_err(|_| Error::Unavailable)
    }
    pub fn hash(&self) -> &str {
        &self.hash
    }
    pub fn plan_hash(&self) -> &str {
        self.plan.hash()
    }
    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical
    }
    pub fn database(&self) -> &Path {
        &self.database
    }
}

pub struct RuntimeInput<'a> {
    pub config: &'a Config,
    pub original: &'a DailyReview,
    pub destination: &'a Path,
    pub confirmed_config_hash: &'a str,
    pub now: &'a str,
    pub expected_identity: Option<(u64, u64)>,
}

#[derive(Clone)]
struct Spec {
    provider: String,
    model: String,
    max_cost: u64,
    pricing_asof: String,
    canonical: Vec<u8>,
}

struct Bind<'a> {
    path: &'a Path,
    path_text: &'a str,
    spec: &'a Spec,
    daily: u64,
    monthly: u64,
    daily_calls: Option<u64>,
    identity: (u64, u64),
    now: &'a str,
    prefix: RefCell<(u64, String)>,
}

/// Read an owned mode-0600 config and require the confirmed hash.
/// The error text is fixed and does not include file bytes.
pub fn load_config(path: &Path, confirmed: &str) -> Result<Config, Error> {
    let raw = read_private(path).map_err(|_| Error::Private)?;
    let config = Config::parse(&raw).map_err(|_| Error::Private)?;
    if config.hash != confirmed {
        return Err(Error::Private);
    }
    Ok(config)
}

/// Reserve one frontier completion under the reviewed quotas, then stage the attempt.
/// A false guard or a storage change preserves created artifacts and spend.
pub fn run_configured_research_proposal(
    input: &RuntimeInput<'_>,
    guard: &mut dyn FnMut() -> bool,
    complete: &mut dyn FnMut(&str) -> Result<Vec<u8>, CallFault>,
) -> Result<Value, Error> {
    match run_inner(input, guard, complete) {
        Ok(value) => Ok(value),
        Err(Fault::Cancelled) => Err(Error::Cancelled),
        Err(Fault::Other) => Err(Error::Unavailable),
    }
}

fn run_inner(
    input: &RuntimeInput<'_>,
    guard: &mut dyn FnMut() -> bool,
    complete: &mut dyn FnMut(&str) -> Result<Vec<u8>, CallFault>,
) -> Result<Value, Fault> {
    let config = Config::parse(input.config.canonical_json()).map_err(|_| Fault::Other)?;
    let original = DailyReview::parse(input.original.original_json()).map_err(|_| Fault::Other)?;
    if config.native || config.hash != input.confirmed_config_hash {
        return Err(Fault::Other);
    }
    preflight(&config, &original, input.now, input.destination)?;
    let previous = model_storage(&config.database, false)?;
    if input
        .expected_identity
        .is_some_and(|id| previous != Some(id))
    {
        return Err(Fault::Other);
    }
    model_schema(&config.database)?;
    if !affordable(&config.cheap, config.daily, config.monthly)
        || !affordable(&config.frontier, config.daily, config.monthly)
    {
        return Err(Fault::Other);
    }
    if !guard() || model_storage(&config.database, false)? != previous {
        return Err(Fault::Other);
    }
    model_schema(&config.database)?;
    let identity = model_storage(&config.database, true)?.ok_or(Fault::Other)?;
    if !guard() || model_storage(&config.database, false)? != Some(identity) {
        return Err(Fault::Other);
    }
    model_schema(&config.database)?;
    if file_len(&config.database)? == 0 {
        init_schema(&config.database, identity)?;
    }
    let opened = fingerprint(&config.database, None)?;
    let bind = Bind {
        path: &config.database,
        path_text: &config.database_text,
        spec: &config.frontier,
        daily: config.daily,
        monthly: config.monthly,
        daily_calls: config.daily_calls,
        identity,
        now: input.now,
        prefix: RefCell::new(opened),
    };
    if verify_storage(&bind, guard).is_err() {
        return Err(Fault::Other);
    }
    let frontier = plan_frontier(&config.plan)?;
    let proposed = research_router::propose_research_script(
        &ProposalInput {
            plan: &config.plan,
            original: &original,
            frontier: &frontier,
            distinct_from_cheap: true,
            destination: input.destination,
            confirmed_hash: config.plan.hash(),
            now: input.now,
            configuration_hash: Some(config.hash()),
        },
        &mut || verify_storage(&bind, guard).is_ok(),
        &mut |prompt| invoke(&bind, prompt, complete),
    );
    match proposed {
        Ok(value) => finish(&bind, guard, value, &config.hash),
        Err(research_router::Error::Cancelled) => Err(Fault::Cancelled),
        Err(_) => Err(Fault::Other),
    }
}

fn finish(
    bind: &Bind<'_>,
    guard: &mut dyn FnMut() -> bool,
    mut value: Value,
    hash: &str,
) -> Result<Value, Fault> {
    let extended = fingerprint(bind.path, Some(bind.prefix.borrow().clone()))?;
    *bind.prefix.borrow_mut() = extended;
    verify_storage(bind, guard)?;
    value
        .as_object_mut()
        .ok_or(Fault::Other)?
        .insert("config_hash".into(), json!(hash));
    Ok(value)
}

fn preflight(
    config: &Config,
    original: &DailyReview,
    now: &str,
    destination: &Path,
) -> Result<(), Fault> {
    let received = original.report()["received_at"]
        .as_str()
        .ok_or(Fault::Other)?;
    let fields = native::decode(config.plan.canonical_json()).map_err(|_| Fault::Other)?;
    if fields["capture_id"].as_str() != Some(original.capture_hash()) || !current(now, received) {
        return Err(Fault::Other);
    }
    config
        .plan
        .historical_proposal_prompt(original, None)
        .map_err(|_| Fault::Other)?;
    if let Some(tools) = fields.get("tools") {
        let raw = serde_json::to_vec(tools).map_err(|_| Fault::Other)?;
        EvidenceTools::parse(&raw)
            .and_then(|tools| tools.available(now))
            .map_err(|_| Fault::Other)?;
        config
            .plan
            .historical_catalog_prompt(original)
            .map_err(|_| Fault::Other)?;
    }
    explicit_path(destination)?;
    if exists(destination)? || destination == config.database {
        return Err(Fault::Other);
    }
    let _parent = open_dir(destination.parent().ok_or(Fault::Other)?)?;
    Ok(())
}

fn invoke(
    bind: &Bind<'_>,
    prompt: &str,
    complete: &mut dyn FnMut(&str) -> Result<Vec<u8>, CallFault>,
) -> Result<Vec<u8>, CallFault> {
    let id = reserve(bind).map_err(|_| CallFault::Failed)?;
    let result = complete(prompt);
    let mark = match &result {
        Ok(bytes) => match response_hash(bytes) {
            Ok(hash) => Mark::Received(hash),
            Err(()) => Mark::Failed,
        },
        Err(CallFault::Cancelled) => Mark::Cancelled,
        Err(CallFault::Failed) => Mark::Failed,
    };
    let prefix = bind.prefix.borrow().clone();
    if fingerprint(bind.path, Some(prefix)).is_err() {
        let _ = settle(bind, id, &Mark::Failed);
        return Err(CallFault::Failed);
    }
    if settle(bind, id, &mark).is_err() {
        return Err(CallFault::Failed);
    }
    if let Mark::Received(_) = mark {
        let current = bind.prefix.borrow().clone();
        match fingerprint(bind.path, Some(current)) {
            Ok(extended) => *bind.prefix.borrow_mut() = extended,
            Err(_) => return Err(CallFault::Failed),
        }
        result
    } else if matches!(mark, Mark::Cancelled) {
        Err(CallFault::Cancelled)
    } else {
        Err(CallFault::Failed)
    }
}

enum Mark {
    Received(String),
    Failed,
    Cancelled,
}

fn current(now: &str, received: &str) -> bool {
    let Ok(now) = DateTime::parse_from_rfc3339(now) else {
        return false;
    };
    let Ok(received) = DateTime::parse_from_rfc3339(received) else {
        return false;
    };
    now >= received
}

fn affordable(spec: &Spec, daily: u64, monthly: u64) -> bool {
    if spec.provider == "fake" {
        return true;
    }
    if spec.pricing_asof == "unverified" || spec.max_cost > daily.min(monthly) {
        return false;
    }
    !(matches!(spec.provider.as_str(), "xai" | "opencode")
        && spec.max_cost == 0
        && !(spec.provider == "opencode" && spec.model.ends_with("-free")))
}

fn plan_frontier(plan: &HistoricalPlan) -> Result<Value, Fault> {
    let fields = native::decode(plan.canonical_json()).map_err(|_| Fault::Other)?;
    fields.get("frontier").cloned().ok_or(Fault::Other)
}

fn verify_storage(bind: &Bind<'_>, guard: &mut dyn FnMut() -> bool) -> Result<(), Fault> {
    if !guard() || model_storage(bind.path, false)? != Some(bind.identity) {
        return Err(Fault::Other);
    }
    let db = open_db(bind.path, false)?;
    database_list(&db, bind.path_text)?;
    private_file(bind.path)?;
    sidecars(bind.path)?;
    let prefix = bind.prefix.borrow().clone();
    fingerprint(bind.path, Some(prefix))?;
    Ok(())
}

fn reserve(bind: &Bind<'_>) -> Result<i64, Fault> {
    if model_storage(bind.path, false)? != Some(bind.identity) {
        return Err(Fault::Other);
    }
    let mut db = open_db(bind.path, true)?;
    database_list(&db, bind.path_text)?;
    let day = billing_day(bind)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| Fault::Other)?;
    let (used, count, month) = usage(&tx, &day)?;
    if bind.spec.provider != "fake" && exhausted(bind, used, count, month)? {
        return Err(Fault::Other);
    }
    tx.execute(
        "INSERT INTO model_calls(day,model,reserved,status) VALUES(?1,?2,?3,'reserved')",
        rusqlite::params![day, bind.spec.model, i64_cost(bind.spec.max_cost)?],
    )
    .map_err(|_| Fault::Other)?;
    let id = tx.last_insert_rowid();
    tx.commit().map_err(|_| Fault::Other)?;
    Ok(id)
}

fn billing_day(bind: &Bind<'_>) -> Result<String, Fault> {
    if bind.spec.provider == "fake" {
        let stamp = DateTime::parse_from_rfc3339(bind.now).map_err(|_| Fault::Other)?;
        return Ok(stamp.date_naive().format("%Y-%m-%d").to_string());
    }
    // Workspace chrono has no clock feature. The civil UTC day still comes from the system clock.
    utc_civil_day()
}

fn utc_civil_day() -> Result<String, Fault> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Fault::Other)?
        .as_millis();
    let millis = i64::try_from(millis).map_err(|_| Fault::Other)?;
    let stamp = DateTime::<Utc>::from_timestamp_millis(millis).ok_or(Fault::Other)?;
    Ok(stamp.date_naive().format("%Y-%m-%d").to_string())
}

fn usage(db: &Connection, day: &str) -> Result<(i64, i64, i64), Fault> {
    let month = day.get(..7).ok_or(Fault::Other)?;
    let row: (i64, i64) = db
        .query_row(
            "SELECT COALESCE(SUM(reserved),0), COUNT(*) FROM model_calls WHERE day=?1",
            [day],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| Fault::Other)?;
    let month_used: i64 = db
        .query_row(
            "SELECT COALESCE(SUM(reserved),0) FROM model_calls WHERE substr(day,1,7)=?1",
            [month],
            |row| row.get(0),
        )
        .map_err(|_| Fault::Other)?;
    Ok((row.0, row.1, month_used))
}

fn exhausted(bind: &Bind<'_>, used: i64, count: i64, month: i64) -> Result<bool, Fault> {
    if bind.daily_calls.is_none_or(|calls| calls == 0) || bind.spec.pricing_asof == "unverified" {
        return Err(Fault::Other);
    }
    let cost = i64_cost(bind.spec.max_cost)?;
    let daily = i64_cost(bind.daily)?;
    let monthly = i64_cost(bind.monthly)?;
    let calls = bind.daily_calls.ok_or(Fault::Other)?;
    Ok(count >= i64_cost(calls)?
        || used.checked_add(cost).is_none_or(|sum| sum > daily)
        || month.checked_add(cost).is_none_or(|sum| sum > monthly))
}

fn i64_cost(value: u64) -> Result<i64, Fault> {
    i64::try_from(value).map_err(|_| Fault::Other)
}

fn settle(bind: &Bind<'_>, id: i64, mark: &Mark) -> Result<(), Fault> {
    if model_storage(bind.path, false)? != Some(bind.identity) {
        return Err(Fault::Other);
    }
    let mut db = open_db(bind.path, true)?;
    database_list(&db, bind.path_text)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| Fault::Other)?;
    let row: (String, Option<String>) = tx
        .query_row(
            "SELECT status, response_hash FROM model_calls WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| Fault::Other)?;
    if row.0 != "reserved" || row.1.is_some() {
        return Err(Fault::Other);
    }
    let (status, hash) = match mark {
        Mark::Received(hash) => ("received", Some(hash.as_str())),
        Mark::Failed => ("failed", None),
        Mark::Cancelled => ("cancelled", None),
    };
    let changed = tx
        .execute(
            "UPDATE model_calls SET status=?1, response_hash=?2 WHERE id=?3 AND status='reserved' AND response_hash IS NULL",
            rusqlite::params![status, hash, id],
        )
        .map_err(|_| Fault::Other)?;
    if changed != 1 {
        return Err(Fault::Other);
    }
    tx.commit().map_err(|_| Fault::Other)?;
    Ok(())
}

fn response_hash(bytes: &[u8]) -> Result<String, ()> {
    let value = native::decode(bytes).map_err(|_| ())?;
    let text = python_json(&value, true).map_err(|_| ())?;
    Ok(digest(text.as_bytes()))
}

fn init_schema(path: &Path, identity: (u64, u64)) -> Result<(), Fault> {
    if model_storage(path, false)? != Some(identity) || file_len(path)? != 0 {
        return Err(Fault::Other);
    }
    let db = open_db(path, true)?;
    db.execute_batch(SCHEMA).map_err(|_| Fault::Other)?;
    drop(db);
    model_schema(path)?;
    Ok(())
}

fn model_schema(path: &Path) -> Result<(), Fault> {
    model_storage(path, false)?;
    if !exists(path)? || file_len(path)? == 0 {
        return Ok(());
    }
    let db = open_db(path, false)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))
        .map_err(|_| Fault::Other)?;
    schema_objects(&db)?;
    columns(&db, "model_calls", CALLS)?;
    columns(&db, "extraction_cache", CACHE)?;
    columns(&db, "model_metrics", METRICS)?;
    scan_calls(&db, deadline)?;
    Ok(())
}

const CALLS: &[(&str, &str, i64)] = &[
    ("id", "INTEGER", 1),
    ("day", "TEXT", 0),
    ("model", "TEXT", 0),
    ("reserved", "INTEGER", 0),
    ("status", "TEXT", 0),
    ("response_hash", "TEXT", 0),
];
const CACHE: &[(&str, &str, i64)] = &[("key", "TEXT", 1), ("output", "TEXT", 0)];
const METRICS: &[(&str, &str, i64)] = &[
    ("call_id", "INTEGER", 1),
    ("latency_ms", "INTEGER", 0),
    ("actual_micro_usd", "INTEGER", 0),
    ("receipt_hash", "TEXT", 0),
    ("input_tokens", "INTEGER", 0),
    ("output_tokens", "INTEGER", 0),
];

fn schema_objects(db: &Connection) -> Result<(), Fault> {
    let mut stmt = db
        .prepare("SELECT type,name,sql FROM sqlite_master")
        .map_err(|_| Fault::Other)?;
    let mut rows = stmt.query([]).map_err(|_| Fault::Other)?;
    let mut tables = Vec::new();
    while let Some(row) = rows.next().map_err(|_| Fault::Other)? {
        let kind: String = row.get(0).map_err(|_| Fault::Other)?;
        let name: String = row.get(1).map_err(|_| Fault::Other)?;
        let sql: Option<String> = row.get(2).map_err(|_| Fault::Other)?;
        if kind == "table" {
            tables.push(name);
        } else if !(kind == "index" && sql.is_none() && name.starts_with("sqlite_autoindex_")) {
            return Err(Fault::Other);
        }
    }
    tables.sort();
    if tables != ["extraction_cache", "model_calls", "model_metrics"] {
        return Err(Fault::Other);
    }
    Ok(())
}

fn columns(db: &Connection, table: &str, expected: &[(&str, &str, i64)]) -> Result<(), Fault> {
    let sql = match table {
        "model_calls" => "PRAGMA table_xinfo(model_calls)",
        "extraction_cache" => "PRAGMA table_xinfo(extraction_cache)",
        "model_metrics" => "PRAGMA table_xinfo(model_metrics)",
        _ => return Err(Fault::Other),
    };
    let mut stmt = db.prepare(sql).map_err(|_| Fault::Other)?;
    let mut rows = stmt.query([]).map_err(|_| Fault::Other)?;
    for (name, kind, pk) in expected {
        let row = rows.next().map_err(|_| Fault::Other)?.ok_or(Fault::Other)?;
        let got_name: String = row.get(1).map_err(|_| Fault::Other)?;
        let got_type: String = row.get(2).map_err(|_| Fault::Other)?;
        let not_null: i64 = row.get(3).map_err(|_| Fault::Other)?;
        let default_value: Option<String> = row.get(4).map_err(|_| Fault::Other)?;
        let got_pk: i64 = row.get(5).map_err(|_| Fault::Other)?;
        let hidden: i64 = row.get(6).map_err(|_| Fault::Other)?;
        if &got_name != name
            || &got_type != kind
            || not_null != 0
            || default_value.is_some()
            || &got_pk != pk
            || hidden != 0
        {
            return Err(Fault::Other);
        }
    }
    if rows.next().map_err(|_| Fault::Other)?.is_some() {
        return Err(Fault::Other);
    }
    Ok(())
}

fn scan_calls(db: &Connection, deadline: Instant) -> Result<(), Fault> {
    let mut stmt = db
        .prepare("SELECT id,day,model,reserved,status,response_hash FROM model_calls LIMIT 100001")
        .map_err(|_| Fault::Other)?;
    let mut rows = stmt.query([]).map_err(|_| Fault::Other)?;
    let mut count = 0u64;
    while let Some(row) = rows.next().map_err(|_| Fault::Other)? {
        count += 1;
        if count > 100_000 || Instant::now() >= deadline || !valid_call(row)? {
            return Err(Fault::Other);
        }
    }
    if Instant::now() >= deadline {
        return Err(Fault::Other);
    }
    Ok(())
}

fn valid_call(row: &rusqlite::Row<'_>) -> Result<bool, Fault> {
    let id = match row.get_ref(0).map_err(|_| Fault::Other)? {
        rusqlite::types::ValueRef::Integer(id) if id > 0 => id,
        _ => return Ok(false),
    };
    let _ = id;
    let day = text_ref(row, 1)?;
    let model = text_ref(row, 2)?;
    let reserved = match row.get_ref(3).map_err(|_| Fault::Other)? {
        rusqlite::types::ValueRef::Integer(n) if n >= 0 => n,
        _ => return Ok(false),
    };
    let _ = reserved;
    let status = text_ref(row, 4)?.ok_or(Fault::Other)?;
    let hash = match row.get_ref(5).map_err(|_| Fault::Other)? {
        rusqlite::types::ValueRef::Null => None,
        rusqlite::types::ValueRef::Text(bytes) => {
            Some(std::str::from_utf8(bytes).map_err(|_| Fault::Other)?)
        }
        _ => return Ok(false),
    };
    let day = day.ok_or(Fault::Other)?;
    let model = model.ok_or(Fault::Other)?;
    Ok(valid_day(day)
        && !model.is_empty()
        && matches!(status, "reserved" | "failed" | "cancelled" | "received")
        && (status == "received") == hash.is_some_and(hex_hash)
        && (status == "received" || hash.is_none()))
}

fn text_ref<'a>(row: &'a rusqlite::Row<'a>, index: usize) -> Result<Option<&'a str>, Fault> {
    match row.get_ref(index).map_err(|_| Fault::Other)? {
        rusqlite::types::ValueRef::Text(bytes) => {
            Ok(Some(std::str::from_utf8(bytes).map_err(|_| Fault::Other)?))
        }
        _ => Ok(None),
    }
}

fn valid_day(value: &str) -> bool {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .is_some_and(|day| day.format("%Y-%m-%d").to_string() == value)
}

fn fingerprint(path: &Path, prefix: Option<(u64, String)>) -> Result<(u64, String), Fault> {
    model_storage(path, false)?;
    model_schema(path)?;
    let mut hasher = Sha256::new();
    let (expected_count, expected_hash) = prefix.unwrap_or((0, EMPTY_HASH.to_owned()));
    if expected_count == 0 && expected_hash != EMPTY_HASH {
        return Err(Fault::Other);
    }
    if !exists(path)? || file_len(path)? == 0 {
        if 0 < expected_count {
            return Err(Fault::Other);
        }
        return Ok((0, EMPTY_HASH.to_owned()));
    }
    let db = open_db(path, false)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))
        .map_err(|_| Fault::Other)?;
    db.execute_batch("BEGIN").map_err(|_| Fault::Other)?;
    let mut stmt = db
        .prepare("SELECT * FROM model_calls ORDER BY rowid LIMIT 100001")
        .map_err(|_| Fault::Other)?;
    let mut rows = stmt.query([]).map_err(|_| Fault::Other)?;
    let mut count = 0u64;
    let mut size = 0usize;
    while let Some(row) = rows.next().map_err(|_| Fault::Other)? {
        count += 1;
        let line = row_line(row)?;
        size += line.len();
        if count > 100_000 || size > 16_000_000 || Instant::now() >= deadline {
            return Err(Fault::Other);
        }
        hasher.update(&line);
        hasher.update(b"\n");
        if count == expected_count && hex_bytes(&hasher.clone().finalize()) != expected_hash {
            return Err(Fault::Other);
        }
    }
    if Instant::now() >= deadline || count < expected_count {
        return Err(Fault::Other);
    }
    Ok((count, hex_bytes(&hasher.finalize())))
}

fn row_line(row: &rusqlite::Row<'_>) -> Result<Vec<u8>, Fault> {
    let mut values = Vec::new();
    let mut index = 0;
    loop {
        match row.get_ref(index) {
            Ok(value) => values.push(sql_json(value)?),
            Err(rusqlite::Error::InvalidColumnIndex(_)) => break,
            Err(_) => return Err(Fault::Other),
        }
        index += 1;
    }
    let text = python_json(&Value::Array(values), false).map_err(|_| Fault::Other)?;
    Ok(text.into_bytes())
}

fn sql_json(value: rusqlite::types::ValueRef<'_>) -> Result<Value, Fault> {
    Ok(match value {
        rusqlite::types::ValueRef::Null => Value::Null,
        rusqlite::types::ValueRef::Integer(n) => json!(n),
        rusqlite::types::ValueRef::Text(bytes) => {
            json!(std::str::from_utf8(bytes).map_err(|_| Fault::Other)?)
        }
        _ => return Err(Fault::Other),
    })
}

fn open_db(path: &Path, write: bool) -> Result<Connection, Fault> {
    let flags = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW
    };
    let db = Connection::open_with_flags(path, flags).map_err(|_| Fault::Other)?;
    db.busy_timeout(Duration::from_millis(500))
        .map_err(|_| Fault::Other)?;
    db.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .map_err(|_| Fault::Other)?;
    db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 1_000_000)
        .map_err(|_| Fault::Other)?;
    if !write {
        db.execute_batch("PRAGMA query_only=ON")
            .map_err(|_| Fault::Other)?;
    }
    Ok(db)
}

fn database_list(db: &Connection, expected: &str) -> Result<(), Fault> {
    let mut stmt = db
        .prepare("PRAGMA database_list")
        .map_err(|_| Fault::Other)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(|_| Fault::Other)?;
    let mut main = Vec::new();
    for row in rows {
        let (name, file) = row.map_err(|_| Fault::Other)?;
        if name != "main" && name != "temp" {
            return Err(Fault::Other);
        }
        if name == "main" {
            main.push(file);
        }
    }
    if main != [expected.to_owned()] {
        return Err(Fault::Other);
    }
    Ok(())
}

fn model_storage(path: &Path, create: bool) -> Result<Option<(u64, u64)>, Fault> {
    explicit_path(path)?;
    let _parent = open_dir(path.parent().ok_or(Fault::Other)?)?;
    sidecars(path)?;
    if !exists(path)? {
        if orphan_sidecar(path)? {
            return Err(Fault::Other);
        }
        if !create {
            return Ok(None);
        }
        create_private(path)?;
    }
    private_file(path).map(Some)
}

fn orphan_sidecar(path: &Path) -> Result<bool, Fault> {
    for suffix in ["-wal", "-shm", "-journal"] {
        if exists(&sidecar_path(path, suffix)?)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sidecars(path: &Path) -> Result<(), Fault> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = sidecar_path(path, suffix)?;
        match fs::symlink_metadata(&sidecar) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(Fault::Other),
            Ok(_) => {
                private_file(&sidecar)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Fault::Other),
        }
    }
    Ok(())
}

fn sidecar_path(path: &Path, suffix: &str) -> Result<PathBuf, Fault> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(Fault::Other)?;
    Ok(path
        .parent()
        .ok_or(Fault::Other)?
        .join(format!("{name}{suffix}")))
}

fn create_private(path: &Path) -> Result<(), Fault> {
    let parent = open_dir(path.parent().ok_or(Fault::Other)?)?;
    let name = path.file_name().ok_or(Fault::Other)?;
    let flags = rustix::fs::OFlags::RDWR
        | rustix::fs::OFlags::CREATE
        | rustix::fs::OFlags::EXCL
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let _file = fs::File::from(
        rustix::fs::openat(
            &parent,
            name,
            flags,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map_err(|_| Fault::Other)?,
    );
    Ok(())
}

fn private_file(path: &Path) -> Result<(u64, u64), Fault> {
    let parent = open_dir(path.parent().ok_or(Fault::Other)?)?;
    let name = path.file_name().ok_or(Fault::Other)?;
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC;
    let file = fs::File::from(
        rustix::fs::openat(&parent, name, flags, rustix::fs::Mode::empty())
            .map_err(|_| Fault::Other)?,
    );
    let meta = file.metadata().map_err(|_| Fault::Other)?;
    if !meta.is_file() || meta.uid() != uid()? || meta.mode() & 0o7777 != 0o600 || meta.nlink() != 1
    {
        return Err(Fault::Other);
    }
    Ok((meta.dev(), meta.ino()))
}

fn explicit_path(path: &Path) -> Result<(), Fault> {
    if !path.is_absolute()
        || path.components().count() < 4
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Fault::Other);
    }
    let mut walked = PathBuf::new();
    for component in path.components() {
        walked.push(component);
        if fs::symlink_metadata(&walked)
            .ok()
            .is_some_and(|meta| meta.file_type().is_symlink())
        {
            return Err(Fault::Other);
        }
    }
    let parent = path.parent().ok_or(Fault::Other)?;
    let meta = fs::metadata(parent).map_err(|_| Fault::Other)?;
    if !meta.is_dir() || meta.uid() != uid()? {
        return Err(Fault::Other);
    }
    Ok(())
}

fn open_dir(path: &Path) -> Result<fs::File, Fault> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Fault::Other);
    }
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut file = fs::File::from(
        rustix::fs::open("/", flags, rustix::fs::Mode::empty()).map_err(|_| Fault::Other)?,
    );
    for component in path.components() {
        if let Component::Normal(name) = component {
            file = fs::File::from(
                rustix::fs::openat(&file, name, flags, rustix::fs::Mode::empty())
                    .map_err(|_| Fault::Other)?,
            );
        }
    }
    let meta = file.metadata().map_err(|_| Fault::Other)?;
    if !meta.is_dir() || meta.uid() != uid()? || meta.mode() & 0o7777 != 0o700 {
        return Err(Fault::Other);
    }
    Ok(file)
}

fn exists(path: &Path) -> Result<bool, Fault> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(Fault::Other),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(Fault::Other),
    }
}

fn file_len(path: &Path) -> Result<u64, Fault> {
    Ok(fs::symlink_metadata(path).map_err(|_| Fault::Other)?.len())
}

fn uid() -> Result<u32, Fault> {
    Ok(fs::metadata("/proc/self").map_err(|_| Fault::Other)?.uid())
}

fn read_private(path: &Path) -> Result<Vec<u8>, Fault> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Fault::Other);
    }
    let mut walked = PathBuf::new();
    for component in path.components() {
        walked.push(component);
        if fs::symlink_metadata(&walked)
            .ok()
            .is_some_and(|meta| meta.file_type().is_symlink())
        {
            return Err(Fault::Other);
        }
    }
    let parent = open_any_dir(path.parent().ok_or(Fault::Other)?)?;
    let name = path.file_name().ok_or(Fault::Other)?;
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC;
    let file = fs::File::from(
        rustix::fs::openat(&parent, name, flags, rustix::fs::Mode::empty())
            .map_err(|_| Fault::Other)?,
    );
    let meta = file.metadata().map_err(|_| Fault::Other)?;
    if !meta.is_file()
        || meta.uid() != uid()?
        || meta.mode() & 0o7777 != 0o600
        || meta.nlink() != 1
        || meta.len() == 0
        || meta.len() > 1_000_000
    {
        return Err(Fault::Other);
    }
    let mut raw = Vec::new();
    file.take(1_000_001)
        .read_to_end(&mut raw)
        .map_err(|_| Fault::Other)?;
    if raw.len() > 1_000_000 {
        return Err(Fault::Other);
    }
    native::decode(&raw).map_err(|_| Fault::Other)?;
    Ok(raw)
}

fn open_any_dir(path: &Path) -> Result<fs::File, Fault> {
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut file = fs::File::from(
        rustix::fs::open("/", flags, rustix::fs::Mode::empty()).map_err(|_| Fault::Other)?,
    );
    for component in path.components() {
        if let Component::Normal(name) = component {
            file = fs::File::from(
                rustix::fs::openat(&file, name, flags, rustix::fs::Mode::empty())
                    .map_err(|_| Fault::Other)?,
            );
        }
    }
    Ok(file)
}

fn parse_config(raw: &[u8]) -> Result<Config, ()> {
    let root = native::decode(raw).map_err(|_| ())?;
    let obj = root.as_object().ok_or(())?;
    if obj.len() != 3 || !obj.contains_key("proposal") || !obj.contains_key("models") {
        return Err(());
    }
    let database_text = obj
        .get("model_database")
        .and_then(Value::as_str)
        .ok_or(())?
        .to_owned();
    let database = PathBuf::from(&database_text);
    explicit_path(&database).map_err(|_| ())?;
    let proposal = serde_json::to_vec(obj.get("proposal").ok_or(())?).map_err(|_| ())?;
    let plan = HistoricalPlan::parse(&proposal).map_err(|_| ())?;
    let models = obj.get("models").and_then(Value::as_object).ok_or(())?;
    let parsed = parse_models(models)?;
    let plan_frontier = plan_frontier(&plan).map_err(|_| ())?;
    let frontier_value: Value =
        serde_json::from_slice(&parsed.frontier.canonical).map_err(|_| ())?;
    if frontier_value != plan_frontier {
        return Err(());
    }
    let canonical = canonical_config(
        &plan,
        &parsed.cheap,
        &parsed.frontier,
        parsed.daily,
        parsed.monthly,
        parsed.daily_calls,
        &database_text,
    )?;
    let hash = digest(&canonical);
    Ok(Config {
        plan,
        frontier: parsed.frontier,
        cheap: parsed.cheap,
        daily: parsed.daily,
        monthly: parsed.monthly,
        daily_calls: parsed.daily_calls,
        database,
        database_text,
        native: false,
        canonical,
        hash,
    })
}

struct ModelBudget {
    cheap: Spec,
    frontier: Spec,
    daily: u64,
    monthly: u64,
    daily_calls: Option<u64>,
}

fn parse_models(models: &Map<String, Value>) -> Result<ModelBudget, ()> {
    let keys: Vec<&str> = models.keys().map(String::as_str).collect();
    let allowed = [
        "cheap",
        "daily_calls",
        "daily_micro_usd",
        "frontier",
        "monthly_micro_usd",
        "subscription",
        "subscription_journal",
    ];
    if keys.iter().any(|key| !allowed.contains(key))
        || ![
            "cheap",
            "daily_calls",
            "daily_micro_usd",
            "frontier",
            "monthly_micro_usd",
        ]
        .iter()
        .all(|key| models.contains_key(*key))
    {
        return Err(());
    }
    let cheap = parse_spec(models.get("cheap").ok_or(())?)?;
    let frontier = parse_spec(models.get("frontier").ok_or(())?)?;
    let daily = strict_u64(models.get("daily_micro_usd").ok_or(())?)?;
    let monthly = strict_u64(models.get("monthly_micro_usd").ok_or(())?)?;
    let daily_calls = match models.get("daily_calls").ok_or(())? {
        Value::Null => None,
        other => {
            let calls = strict_u64(other)?;
            if calls == 0 {
                return Err(());
            }
            Some(calls)
        }
    };
    let native = cheap.provider == "xai_subscription" || frontier.provider == "xai_subscription";
    let subscribed =
        models.contains_key("subscription") || models.contains_key("subscription_journal");
    if native || subscribed || daily_calls.is_none() {
        // Structural native documents stay unparsed as runnable configs.
        // Opening a grant directory or an existing subscription journal is out of this slice.
        return Err(());
    }
    Ok(ModelBudget {
        cheap,
        frontier,
        daily,
        monthly,
        daily_calls,
    })
}

fn parse_spec(value: &Value) -> Result<Spec, ()> {
    let obj = value.as_object().ok_or(())?;
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let allowed = [
        "max_cost_micro_usd",
        "max_input_chars",
        "max_output_tokens",
        "model",
        "pricing_asof",
        "prompt_version",
        "provider",
        "schema_version",
        "subscription_account_hash",
        "timeout_seconds",
    ];
    if keys.iter().any(|key| !allowed.contains(key))
        || !obj.contains_key("provider")
        || !obj.contains_key("model")
    {
        return Err(());
    }
    let provider = obj
        .get("provider")
        .and_then(Value::as_str)
        .ok_or(())?
        .to_owned();
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .ok_or(())?
        .to_owned();
    if ![
        "fake",
        "xai",
        "opencode",
        "codex",
        "grok_build",
        "xai_subscription",
    ]
    .contains(&provider.as_str())
        || model.is_empty()
    {
        return Err(());
    }
    let prompt_version = optional_string(obj, "prompt_version", "v1")?;
    let schema_version = optional_string(obj, "schema_version", "v1")?;
    let timeout = optional_u64(obj, "timeout_seconds", 30)?;
    let max_input = optional_u64(obj, "max_input_chars", 24000)?;
    let max_output = optional_u64(obj, "max_output_tokens", 1200)?;
    let max_cost = optional_u64(obj, "max_cost_micro_usd", 0)?;
    let pricing_asof = optional_string(obj, "pricing_asof", "unverified")?;
    let account = match obj.get("subscription_account_hash") {
        None | Some(Value::Null) => None,
        Some(Value::String(hash)) if hex_hash(hash) => Some(hash.clone()),
        _ => return Err(()),
    };
    if !(1..=30).contains(&timeout)
        || max_input == 0
        || max_output == 0
        || (provider == "xai_subscription") != account.is_some()
    {
        return Err(());
    }
    let canonical = serde_json::to_vec(&CanonSpec {
        provider: &provider,
        model: &model,
        prompt_version: &prompt_version,
        schema_version: &schema_version,
        timeout_seconds: timeout,
        max_input_chars: max_input,
        max_output_tokens: max_output,
        max_cost_micro_usd: max_cost,
        pricing_asof: &pricing_asof,
        subscription_account_hash: account.as_deref(),
    })
    .map_err(|_| ())?;
    Ok(Spec {
        provider,
        model,
        max_cost,
        pricing_asof,
        canonical,
    })
}

fn optional_string(obj: &Map<String, Value>, key: &str, default: &str) -> Result<String, ()> {
    match obj.get(key) {
        None => Ok(default.to_owned()),
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(()),
    }
}

fn optional_u64(obj: &Map<String, Value>, key: &str, default: u64) -> Result<u64, ()> {
    match obj.get(key) {
        None => Ok(default),
        Some(value) => strict_u64(value),
    }
}

fn strict_u64(value: &Value) -> Result<u64, ()> {
    value.as_u64().ok_or(())
}

#[derive(Serialize)]
struct CanonSpec<'a> {
    provider: &'a str,
    model: &'a str,
    prompt_version: &'a str,
    schema_version: &'a str,
    timeout_seconds: u64,
    max_input_chars: u64,
    max_output_tokens: u64,
    max_cost_micro_usd: u64,
    pricing_asof: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    subscription_account_hash: Option<&'a str>,
}

fn canonical_config(
    plan: &HistoricalPlan,
    cheap: &Spec,
    frontier: &Spec,
    daily: u64,
    monthly: u64,
    daily_calls: Option<u64>,
    database: &str,
) -> Result<Vec<u8>, ()> {
    let proposal =
        RawValue::from_string(String::from_utf8(plan.canonical_json().to_vec()).map_err(|_| ())?)
            .map_err(|_| ())?;
    let cheap = RawValue::from_string(String::from_utf8(cheap.canonical.clone()).map_err(|_| ())?)
        .map_err(|_| ())?;
    let frontier =
        RawValue::from_string(String::from_utf8(frontier.canonical.clone()).map_err(|_| ())?)
            .map_err(|_| ())?;
    serde_json::to_vec(&CanonConfig {
        proposal: &proposal,
        models: CanonModels {
            cheap: &cheap,
            frontier: &frontier,
            daily_micro_usd: daily,
            monthly_micro_usd: monthly,
            daily_calls,
        },
        model_database: database,
    })
    .map_err(|_| ())
}

#[derive(Serialize)]
struct CanonConfig<'a> {
    proposal: &'a RawValue,
    models: CanonModels<'a>,
    model_database: &'a str,
}

#[derive(Serialize)]
struct CanonModels<'a> {
    cheap: &'a RawValue,
    frontier: &'a RawValue,
    daily_micro_usd: u64,
    monthly_micro_usd: u64,
    daily_calls: Option<u64>,
}

fn digest(bytes: &[u8]) -> String {
    hex_bytes(&Sha256::digest(bytes))
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn python_json(value: &Value, spaced: bool) -> Result<String, ()> {
    let mut out = String::new();
    write_json(&mut out, value, spaced)?;
    Ok(out)
}

fn write_json(out: &mut String, value: &Value, spaced: bool) -> Result<(), ()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => push_string(out, text),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                    if spaced {
                        out.push(' ');
                    }
                }
                write_json(out, item, spaced)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                    if spaced {
                        out.push(' ');
                    }
                }
                push_string(out, key);
                out.push(':');
                if spaced {
                    out.push(' ');
                }
                write_json(out, &map[*key], spaced)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn push_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 || !ch.is_ascii() => {
                let mut buf = [0u16; 2];
                for unit in ch.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}
