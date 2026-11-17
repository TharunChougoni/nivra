//! Reopen account-scoped market-read journals.
//!
//! The caller supplies the connection. This module does not open a path, copy
//! rows into another namespace, reset or refund a reservation, or become a
//! replacement allowance authority. Real journals are never opened here.

use rusqlite::Connection;
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

fn is_scope(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    value.len() <= 128
        && first.is_ascii_lowercase()
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
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
    }
}
