//! Reopen synthetic model-usage and account-scoped market-read journals.
//!
//! The caller supplies the connection. This module does not open a path, copy
//! rows into another namespace, reset or refund a reservation, or become a
//! replacement allowance authority. A received model attempt is not a
//! validated decision. Real journals are never opened here.

use rusqlite::{Connection, OptionalExtension};
use std::{error::Error, fmt};

/// Python `SharedReadLimiter` windows: 5/s, 120/min, 1500/30min.
pub const LEGACY_READ_CAPS: ReadCaps = ReadCaps {
    per_second: 5,
    per_minute: 120,
    per_half_hour: 1500,
};

/// Runaway bound only. Windows are never pruned, so a valid historic journal
/// must reopen after growing past the previous 100_000 cap (#214).
const MAX_MARKET_READ_WINDOWS: usize = 10_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelCaps {
    pub daily_micro_usd: i64,
    pub monthly_micro_usd: i64,
    pub daily_calls: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadCaps {
    pub per_second: u32,
    pub per_minute: u32,
    pub per_half_hour: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalSource {
    /// The original journal. `high_water` must equal its current maximum.
    Original { high_water: i64 },
    /// A copy. A high water behind the original is stale and is not adopted.
    Backup { high_water: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptStatus {
    Reserved,
    Failed,
    Cancelled,
    Received,
}

impl AttemptStatus {
    /// Parse a persisted `model_calls.status`. Unknown names stay
    /// `JournalError::Rows` so a corrupt journal cannot be silently rewritten.
    pub fn parse(raw: &str) -> Result<Self, JournalError> {
        match raw {
            "reserved" => Ok(Self::Reserved),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "received" => Ok(Self::Received),
            _ => Err(JournalError::Rows),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Received => "received",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAttempt {
    id: i64,
    day: String,
    model: String,
    reserved: i64,
    status: AttemptStatus,
    response_hash: Option<String>,
}

impl ModelAttempt {
    pub fn id(&self) -> i64 {
        self.id
    }
    pub fn day(&self) -> &str {
        &self.day
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn reserved(&self) -> i64 {
        self.reserved
    }
    pub fn status(&self) -> AttemptStatus {
        self.status
    }
    pub fn response_hash(&self) -> Option<&str> {
        self.response_hash.as_deref()
    }
    /// Receipt of a model response is not a trading or research decision.
    pub fn validated_decision(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelJournal {
    account_hash: String,
    caps: ModelCaps,
    attempts: Vec<ModelAttempt>,
    extraction_rows: u64,
    metric_rows: u64,
}

impl ModelJournal {
    pub fn account_hash(&self) -> &str {
        &self.account_hash
    }
    pub fn caps(&self) -> ModelCaps {
        self.caps
    }
    pub fn attempts(&self) -> &[ModelAttempt] {
        &self.attempts
    }
    pub fn extraction_rows(&self) -> u64 {
        self.extraction_rows
    }
    pub fn metric_rows(&self) -> u64 {
        self.metric_rows
    }
    /// Read-only projection of the native zero-paid quota at this snapshot.
    /// This is not a reservation, provider quota guarantee, or call authority.
    /// Every attempt state counts; no failed/cancelled reservation is refunded.
    pub fn native_quota_available(
        &self,
        account_hash: &str,
        day: &str,
    ) -> Result<bool, JournalError> {
        if !hex64(account_hash) || account_hash != self.account_hash {
            return Err(JournalError::AccountMismatch);
        }
        if !valid_day(day) {
            return Err(JournalError::Rows);
        }
        if self.caps.daily_micro_usd != 0 || self.caps.monthly_micro_usd != 0 {
            return Err(JournalError::ReplacementAllowance);
        }
        let count = self.attempts.iter().filter(|a| a.day == day).count();
        if self
            .caps
            .daily_calls
            .is_some_and(|cap| count as u64 >= cap as u64)
        {
            return Ok(false);
        }
        // Any retained paid reservation in the same month exhausts a zero-paid policy.
        Ok(!self
            .attempts
            .iter()
            .any(|a| a.day[..7] == day[..7] && a.reserved > 0))
    }
    pub fn permits_new_attempt(&self) -> bool {
        false
    }
    pub fn permits_refund(&self) -> bool {
        false
    }
    pub fn allowance_authority(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReadWindow {
    scope: String,
    at: f64,
}

impl ReadWindow {
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub fn at(&self) -> f64 {
        self.at
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarketReadJournal {
    scope: String,
    caps: ReadCaps,
    windows: Vec<ReadWindow>,
}

impl MarketReadJournal {
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub fn caps(&self) -> ReadCaps {
        self.caps
    }
    pub fn windows(&self) -> &[ReadWindow] {
        &self.windows
    }
    pub fn permits_reset(&self) -> bool {
        false
    }
    pub fn allowance_authority(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalError {
    Schema,
    ExtraSchema,
    ReplacementSchema,
    ReplacementAllowance,
    QuotaExhausted,
    StaleBackup,
    NotOriginal,
    AccountMismatch,
    Rows,
    Unavailable,
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Schema => "journal_schema_unsupported",
            Self::ExtraSchema => "journal_extra_schema",
            Self::ReplacementSchema => "journal_replacement_schema",
            Self::ReplacementAllowance => "journal_replacement_allowance",
            Self::QuotaExhausted => "journal_quota_exhausted",
            Self::StaleBackup => "journal_stale_backup",
            Self::NotOriginal => "journal_backup_not_original",
            Self::AccountMismatch => "journal_account_mismatch",
            Self::Rows => "journal_rows_invalid",
            Self::Unavailable => "journal_unavailable",
        })
    }
}

impl Error for JournalError {}

/// Validate and preserve one original model journal. No row is rewritten.
pub fn reopen_model_usage(
    db: &Connection,
    account_hash: &str,
    legacy_caps: ModelCaps,
    proposed_caps: ModelCaps,
    source: JournalSource,
) -> Result<ModelJournal, JournalError> {
    if !db.is_autocommit() || !hex64(account_hash) || !caps_sane(legacy_caps) {
        return Err(JournalError::Unavailable);
    }
    reject_attached(db)?;
    model_schema(db)?;
    if proposed_caps != legacy_caps {
        return Err(JournalError::ReplacementAllowance);
    }
    let attempts = model_attempts(db)?;
    let high_water = attempts.last().map(|attempt| attempt.id).unwrap_or(0);
    accept_source(source, high_water)?;
    Ok(ModelJournal {
        account_hash: account_hash.to_owned(),
        caps: legacy_caps,
        extraction_rows: count_rows(db, "extraction_cache")?,
        metric_rows: count_rows(db, "model_metrics")?,
        attempts,
    })
}

/// Validate and preserve one account-scoped read journal. Windows are not pruned.
pub fn reopen_market_reads(
    db: &Connection,
    account_scope: &str,
    proposed_caps: ReadCaps,
    source: JournalSource,
) -> Result<MarketReadJournal, JournalError> {
    if !db.is_autocommit() || !is_scope(account_scope) {
        return Err(JournalError::Unavailable);
    }
    reject_attached(db)?;
    read_schema(db)?;
    if proposed_caps != LEGACY_READ_CAPS {
        return Err(JournalError::ReplacementAllowance);
    }
    let windows = read_windows(db, account_scope)?;
    let high_water = i64::try_from(windows.len()).map_err(|_| JournalError::Rows)?;
    accept_source(source, high_water)?;
    Ok(MarketReadJournal {
        scope: account_scope.to_owned(),
        caps: LEGACY_READ_CAPS,
        windows,
    })
}

fn accept_source(source: JournalSource, observed: i64) -> Result<(), JournalError> {
    match source {
        JournalSource::Original { high_water } if high_water == observed => Ok(()),
        JournalSource::Original { .. } => Err(JournalError::NotOriginal),
        JournalSource::Backup { high_water } if high_water > observed => {
            Err(JournalError::StaleBackup)
        }
        JournalSource::Backup { .. } => Err(JournalError::NotOriginal),
    }
}

fn reject_attached(db: &Connection) -> Result<(), JournalError> {
    let mut statement = db
        .prepare("PRAGMA database_list")
        .map_err(|_| JournalError::Unavailable)?;
    let mut rows = statement.query([]).map_err(|_| JournalError::Unavailable)?;
    let mut saw_main = false;
    while let Some(row) = rows.next().map_err(|_| JournalError::Unavailable)? {
        let name: String = row.get(1).map_err(|_| JournalError::Unavailable)?;
        if name == "temp" {
            continue;
        }
        if name != "main" {
            return Err(JournalError::ExtraSchema);
        }
        if saw_main {
            return Err(JournalError::ExtraSchema);
        }
        saw_main = true;
    }
    if saw_main {
        Ok(())
    } else {
        Err(JournalError::Unavailable)
    }
}

const MODEL_TABLES: [&str; 3] = ["extraction_cache", "model_calls", "model_metrics"];
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

fn model_schema(db: &Connection) -> Result<(), JournalError> {
    let objects = schema_objects(db)?;
    let mut tables = Vec::new();
    for (kind, name, sql) in &objects {
        if kind == "table" {
            if !MODEL_TABLES.contains(&name.as_str()) {
                return Err(JournalError::ExtraSchema);
            }
            tables.push(name.clone());
        } else if !(kind == "index" && sql.is_none() && name.starts_with("sqlite_autoindex_")) {
            return Err(JournalError::ExtraSchema);
        }
    }
    tables.sort();
    if tables != MODEL_TABLES {
        return Err(JournalError::ReplacementSchema);
    }
    columns(db, "model_calls", CALLS)?;
    columns(db, "extraction_cache", CACHE)?;
    columns(db, "model_metrics", METRICS)?;
    Ok(())
}

fn read_schema(db: &Connection) -> Result<(), JournalError> {
    let objects = schema_objects(db)?;
    let mut tables = Vec::new();
    let mut indexes = Vec::new();
    for (kind, name, sql) in &objects {
        if kind == "table" {
            if name != "read_requests" {
                return Err(JournalError::ExtraSchema);
            }
            tables.push(name.clone());
        } else if kind == "index" && name == "read_window" && sql.is_some() {
            indexes.push(name.clone());
        } else if !(kind == "index" && sql.is_none() && name.starts_with("sqlite_autoindex_")) {
            return Err(JournalError::ExtraSchema);
        }
    }
    if tables != ["read_requests"] || indexes != ["read_window"] {
        return Err(JournalError::ReplacementSchema);
    }
    let mut statement = db
        .prepare("PRAGMA table_xinfo(read_requests)")
        .map_err(|_| JournalError::Schema)?;
    let mut rows = statement.query([]).map_err(|_| JournalError::Schema)?;
    let expected = [("scope", "TEXT", 1), ("at", "REAL", 1)];
    for (name, kind, not_null) in expected {
        let row = rows
            .next()
            .map_err(|_| JournalError::Schema)?
            .ok_or(JournalError::ReplacementSchema)?;
        let got_name: String = row.get(1).map_err(|_| JournalError::Schema)?;
        let got_type: String = row.get(2).map_err(|_| JournalError::Schema)?;
        let got_null: i64 = row.get(3).map_err(|_| JournalError::Schema)?;
        let default_value: Option<String> = row.get(4).map_err(|_| JournalError::Schema)?;
        let pk: i64 = row.get(5).map_err(|_| JournalError::Schema)?;
        let hidden: i64 = row.get(6).map_err(|_| JournalError::Schema)?;
        if got_name != name
            || got_type != kind
            || got_null != not_null
            || default_value.is_some()
            || pk != 0
            || hidden != 0
        {
            return Err(JournalError::ReplacementSchema);
        }
    }
    if rows.next().map_err(|_| JournalError::Schema)?.is_some() {
        return Err(JournalError::ReplacementSchema);
    }
    Ok(())
}

fn schema_objects(db: &Connection) -> Result<Vec<(String, String, Option<String>)>, JournalError> {
    let mut statement = db
        .prepare("SELECT type,name,sql FROM sqlite_master")
        .map_err(|_| JournalError::Schema)?;
    let mut rows = statement.query([]).map_err(|_| JournalError::Schema)?;
    let mut objects = Vec::new();
    while let Some(row) = rows.next().map_err(|_| JournalError::Schema)? {
        objects.push((
            row.get(0).map_err(|_| JournalError::Schema)?,
            row.get(1).map_err(|_| JournalError::Schema)?,
            row.get(2).map_err(|_| JournalError::Schema)?,
        ));
        if objects.len() > 32 {
            return Err(JournalError::ExtraSchema);
        }
    }
    Ok(objects)
}

fn columns(
    db: &Connection,
    table: &str,
    expected: &[(&str, &str, i64)],
) -> Result<(), JournalError> {
    let mut statement = db
        .prepare(&format!("PRAGMA table_xinfo({table})"))
        .map_err(|_| JournalError::Schema)?;
    let mut rows = statement.query([]).map_err(|_| JournalError::Schema)?;
    for (name, kind, pk) in expected {
        let row = rows
            .next()
            .map_err(|_| JournalError::Schema)?
            .ok_or(JournalError::ReplacementSchema)?;
        let got_name: String = row.get(1).map_err(|_| JournalError::Schema)?;
        let got_type: String = row.get(2).map_err(|_| JournalError::Schema)?;
        let not_null: i64 = row.get(3).map_err(|_| JournalError::Schema)?;
        let default_value: Option<String> = row.get(4).map_err(|_| JournalError::Schema)?;
        let got_pk: i64 = row.get(5).map_err(|_| JournalError::Schema)?;
        let hidden: i64 = row.get(6).map_err(|_| JournalError::Schema)?;
        if &got_name != name
            || &got_type != kind
            || not_null != 0
            || default_value.is_some()
            || &got_pk != pk
            || hidden != 0
        {
            return Err(JournalError::ReplacementSchema);
        }
    }
    if rows.next().map_err(|_| JournalError::Schema)?.is_some() {
        return Err(JournalError::ReplacementSchema);
    }
    Ok(())
}

fn model_attempts(db: &Connection) -> Result<Vec<ModelAttempt>, JournalError> {
    let mut statement = db
        .prepare("SELECT id,day,model,reserved,status,response_hash FROM model_calls ORDER BY id")
        .map_err(|_| JournalError::Unavailable)?;
    let mut rows = statement.query([]).map_err(|_| JournalError::Unavailable)?;
    let mut attempts = Vec::new();
    let mut previous = 0;
    while let Some(row) = rows.next().map_err(|_| JournalError::Rows)? {
        if attempts.len() >= 100_000 {
            return Err(JournalError::Rows);
        }
        let id: i64 = row.get(0).map_err(|_| JournalError::Rows)?;
        let day: String = row.get(1).map_err(|_| JournalError::Rows)?;
        let model: String = row.get(2).map_err(|_| JournalError::Rows)?;
        let reserved: i64 = row.get(3).map_err(|_| JournalError::Rows)?;
        let status: String = row.get(4).map_err(|_| JournalError::Rows)?;
        let response_hash: Option<String> = row.get(5).map_err(|_| JournalError::Rows)?;
        let status = AttemptStatus::parse(&status)?;
        if id <= previous
            || !valid_day(&day)
            || model.is_empty()
            || model.len() > 65_536
            || reserved < 0
            || (status == AttemptStatus::Received && !response_hash.as_deref().is_some_and(hex64))
            || (status != AttemptStatus::Received && response_hash.is_some())
        {
            return Err(JournalError::Rows);
        }
        previous = id;
        attempts.push(ModelAttempt {
            id,
            day,
            model,
            reserved,
            status,
            response_hash,
        });
    }
    Ok(attempts)
}

fn read_windows(db: &Connection, account_scope: &str) -> Result<Vec<ReadWindow>, JournalError> {
    let mut statement = db
        .prepare("SELECT scope,at FROM read_requests ORDER BY rowid")
        .map_err(|_| JournalError::Unavailable)?;
    let mut rows = statement.query([]).map_err(|_| JournalError::Unavailable)?;
    let mut windows = Vec::new();
    while let Some(row) = rows.next().map_err(|_| JournalError::Rows)? {
        if windows.len() >= MAX_MARKET_READ_WINDOWS {
            return Err(JournalError::Rows);
        }
        let scope: String = row.get(0).map_err(|_| JournalError::Rows)?;
        let at: f64 = row.get(1).map_err(|_| JournalError::Rows)?;
        if scope != account_scope {
            return Err(JournalError::AccountMismatch);
        }
        if !at.is_finite() {
            return Err(JournalError::Rows);
        }
        windows.push(ReadWindow { scope, at });
    }
    Ok(windows)
}

fn count_rows(db: &Connection, table: &str) -> Result<u64, JournalError> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let count: i64 = db
        .query_row(&sql, [], |row| row.get(0))
        .optional()
        .map_err(|_| JournalError::Unavailable)?
        .ok_or(JournalError::Unavailable)?;
    u64::try_from(count).map_err(|_| JournalError::Rows)
}

fn caps_sane(caps: ModelCaps) -> bool {
    caps.daily_micro_usd >= 0
        && caps.monthly_micro_usd >= 0
        && caps.daily_calls.is_none_or(|calls| calls >= 0)
}

fn hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_scope(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    value.len() <= 128
        && first.is_ascii_lowercase()
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
}

fn valid_day(value: &str) -> bool {
    let mut parts = value.split('-');
    let Some(year) = parts.next() else {
        return false;
    };
    let Some(month) = parts.next() else {
        return false;
    };
    let Some(day) = parts.next() else {
        return false;
    };
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return false;
    }
    let Ok(year) = year.parse::<i32>() else {
        return false;
    };
    let Ok(month) = month.parse::<u32>() else {
        return false;
    };
    let Ok(day) = day.parse::<u32>() else {
        return false;
    };
    let max = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap(year) => 29,
        2 => 28,
        _ => return false,
    };
    (1..=9999).contains(&year) && (1..=max).contains(&day)
}

fn leap(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}
/// Atomically append a zero-paid native attempt to an already validated journal.
/// Caller must hold and verify original-file/account/grant/owner bindings before
/// and after this operation. This function neither opens files nor invokes models.
/// A snapshot supplies continuity/caps, never a replacement allowance namespace.
pub fn reserve_native(
    db: &mut Connection,
    original: &ModelJournal,
    account_hash: &str,
    model: &str,
    day: &str,
) -> Result<i64, JournalError> {
    if !db.is_autocommit()
        || !valid_day(day)
        || model.is_empty()
        || model.len() > 120
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
    {
        return Err(JournalError::Unavailable);
    }
    reject_attached(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| JournalError::Unavailable)?;
    model_schema(&tx)?;
    let attempts = model_attempts(&tx)?;
    if attempts.len() >= 100_000 || !attempts.starts_with(&original.attempts) {
        return Err(JournalError::Rows);
    }
    let id = attempts
        .last()
        .map(|a| a.id)
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(JournalError::Rows)?;
    let current = ModelJournal {
        account_hash: original.account_hash.clone(),
        caps: original.caps,
        attempts,
        extraction_rows: original.extraction_rows,
        metric_rows: original.metric_rows,
    };
    if !current.native_quota_available(account_hash, day)? {
        return Err(JournalError::QuotaExhausted);
    }
    tx.execute("INSERT INTO model_calls(id,day,model,reserved,status,response_hash) VALUES(?1,?2,?3,0,'reserved',NULL)",rusqlite::params![id,day,model]).map_err(|_|JournalError::Unavailable)?;
    // Triggers/foreign schema are rejected above; preserve monotone original row IDs.
    tx.commit().map_err(|_| JournalError::Unavailable)?;
    Ok(id)
}

/// A terminal transport fact, never a validated model decision or billing receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalOutcome {
    Failed,
    Cancelled,
    Received { response_hash: String },
}
/// One-way CAS of the exact reserved row. Duplicate/ambiguous completion refuses;
/// caller reconciles it, never retries invocation. No reservation amount is changed.
/// Original-file/account/grant/lease verification remains a caller requirement.
pub fn finish_model_attempt(
    db: &mut Connection,
    expected: &ModelAttempt,
    outcome: &TerminalOutcome,
) -> Result<(), JournalError> {
    if !db.is_autocommit()
        || expected.status != AttemptStatus::Reserved
        || expected.response_hash.is_some()
    {
        return Err(JournalError::Rows);
    }
    let (status, hash) = match outcome {
        TerminalOutcome::Failed => ("failed", None),
        TerminalOutcome::Cancelled => ("cancelled", None),
        TerminalOutcome::Received { response_hash } if hex64(response_hash) => {
            ("received", Some(response_hash.as_str()))
        }
        TerminalOutcome::Received { .. } => return Err(JournalError::Rows),
    };
    reject_attached(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| JournalError::Unavailable)?;
    model_schema(&tx)?;
    let attempts = model_attempts(&tx)?;
    if attempts.iter().find(|a| a.id == expected.id) != Some(expected) {
        return Err(JournalError::Rows);
    }
    let changed=tx.execute("UPDATE model_calls SET status=?1,response_hash=?2 WHERE id=?3 AND day=?4 AND model=?5 AND reserved=?6 AND status='reserved' AND response_hash IS NULL",rusqlite::params![status,hash,expected.id,expected.day,expected.model,expected.reserved]).map_err(|_|JournalError::Unavailable)?;
    if changed != 1 {
        return Err(JournalError::Rows);
    }
    tx.commit().map_err(|_| JournalError::Unavailable)
}

/// Record a caller's measured latency without changing billing or reservation amounts.
/// Exact original row required; first measurement is immutable except identical replay.
/// Received-response metrics must be recorded before received completion, as in Python.
pub fn record_model_latency(
    db: &mut Connection,
    expected: &ModelAttempt,
    latency_ms: u64,
) -> Result<(), JournalError> {
    let latency = i64::try_from(latency_ms).map_err(|_| JournalError::Rows)?;
    if !db.is_autocommit()
        || expected.status == AttemptStatus::Received
        || expected.response_hash.is_some()
    {
        return Err(JournalError::Rows);
    }
    reject_attached(db)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| JournalError::Unavailable)?;
    model_schema(&tx)?;
    let attempts = model_attempts(&tx)?;
    if attempts.iter().find(|a| a.id == expected.id) != Some(expected) {
        return Err(JournalError::Rows);
    }
    let prior: Option<Option<i64>> = tx
        .query_row(
            "SELECT latency_ms FROM model_metrics WHERE call_id=?1",
            [expected.id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| JournalError::Rows)?;
    if prior.flatten().is_some_and(|old| old != latency) {
        return Err(JournalError::Rows);
    }
    tx.execute("INSERT INTO model_metrics(call_id,latency_ms) VALUES(?1,?2) ON CONFLICT(call_id) DO UPDATE SET latency_ms=excluded.latency_ms",rusqlite::params![expected.id,latency]).map_err(|_|JournalError::Unavailable)?;
    tx.commit().map_err(|_| JournalError::Unavailable)
}

/// Return the exact committed reservation expectation for subsequent CAS outcomes.
/// Commit ambiguity propagates as an error; callers must reconcile, never invoke/retry.
pub fn reserve_native_attempt(
    db: &mut Connection,
    original: &ModelJournal,
    account_hash: &str,
    model: &str,
    day: &str,
) -> Result<ModelAttempt, JournalError> {
    let id = reserve_native(db, original, account_hash, model, day)?;
    Ok(ModelAttempt {
        id,
        day: day.into(),
        model: model.into(),
        reserved: 0,
        status: AttemptStatus::Reserved,
        response_hash: None,
    })
}

/// Expected projection for an already-validated terminal CAS; no storage effects.
pub(crate) fn terminal_projection(
    expected: &ModelAttempt,
    outcome: &TerminalOutcome,
) -> Result<ModelAttempt, JournalError> {
    if expected.status != AttemptStatus::Reserved || expected.response_hash.is_some() {
        return Err(JournalError::Rows);
    }
    let mut result = expected.clone();
    match outcome {
        TerminalOutcome::Failed => result.status = AttemptStatus::Failed,
        TerminalOutcome::Cancelled => result.status = AttemptStatus::Cancelled,
        TerminalOutcome::Received { response_hash } if hex64(response_hash) => {
            result.status = AttemptStatus::Received;
            result.response_hash = Some(response_hash.clone());
        }
        _ => return Err(JournalError::Rows),
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market_read_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE read_requests(scope TEXT NOT NULL, at REAL NOT NULL);
             CREATE INDEX read_window ON read_requests(scope, at);",
        )
        .unwrap();
        db
    }

    #[test]
    fn reopen_market_reads_accepts_more_than_100k_historic_rows() {
        let db = market_read_db();
        let scope = "upstox-market-tharun-upstox";
        let count = 100_001i64;
        {
            let tx = db.unchecked_transaction().unwrap();
            let mut insert = tx
                .prepare("INSERT INTO read_requests(scope, at) VALUES(?1, ?2)")
                .unwrap();
            for i in 0..count {
                // Production-shaped row: configured scope and a finite epoch timestamp.
                insert
                    .execute(rusqlite::params![scope, 1_792_046_340.948 + i as f64])
                    .unwrap();
            }
            drop(insert);
            tx.commit().unwrap();
        }
        let journal = reopen_market_reads(
            &db,
            scope,
            LEGACY_READ_CAPS,
            JournalSource::Original { high_water: count },
        )
        .expect("a valid historic journal larger than 100_000 rows must reopen");
        assert_eq!(journal.scope(), scope);
        assert_eq!(journal.windows().len(), count as usize);
        assert!(journal.windows()[0].at().is_finite());
        assert_eq!(journal.windows()[0].scope(), scope);
        // Historic model_calls.status values keep parsing; unknown names stay rows.
        assert_eq!(
            AttemptStatus::parse("reserved").unwrap(),
            AttemptStatus::Reserved
        );
        assert_eq!(
            AttemptStatus::parse("failed").unwrap(),
            AttemptStatus::Failed
        );
        assert_eq!(
            AttemptStatus::parse("cancelled").unwrap(),
            AttemptStatus::Cancelled
        );
        assert_eq!(
            AttemptStatus::parse("received").unwrap(),
            AttemptStatus::Received
        );
        assert!(matches!(
            AttemptStatus::parse("approval_renewed"),
            Err(JournalError::Rows)
        ));
    }
}
