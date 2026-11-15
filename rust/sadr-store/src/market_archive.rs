//! Bounded immutable capture archive. IDs bind the exact validated JSON bytes;
//! no receipt or source timestamp is regenerated during storage or loading.
use crate::StoreError;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, limits::Limit};
use sadr_core::market_capture::MarketCapture;
use sha2::{Digest, Sha256};
use std::{fmt, path::Path, time::Duration};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    max_records: u32,
    max_bytes: u64,
}

impl Limits {
    pub fn new(max_records: u32, max_bytes: u64) -> Result<Self, StoreError> {
        if !(1..=100_000).contains(&max_records) || !(1..=10_000_000_000).contains(&max_bytes) {
            return Err(StoreError::Bound);
        }
        Ok(Self {
            max_records,
            max_bytes,
        })
    }
}

pub struct MarketArchive {
    db: Connection,
    limits: Limits,
}

impl fmt::Debug for MarketArchive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MarketArchive(bounded, capture content redacted)")
    }
}

impl MarketArchive {
    pub fn open(path: &Path, limits: Limits) -> Result<Self, StoreError> {
        let db = Connection::open(path).map_err(|_| StoreError::Archive)?;
        db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 20_100_000)
            .map_err(|_| StoreError::Bound)?;
        db.busy_timeout(Duration::from_secs(1))
            .map_err(|_| StoreError::Archive)?;
        db.pragma_update(None, "journal_mode", "WAL")
            .map_err(|_| StoreError::Archive)?;
        db.pragma_update(None, "synchronous", "FULL")
            .map_err(|_| StoreError::Archive)?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS market_captures (id TEXT PRIMARY KEY, payload TEXT NOT NULL)",
        )
        .map_err(|_| StoreError::Archive)?;
        Ok(Self { db, limits })
    }

    /// Append exact validated capture bytes. Identical input is idempotent;
    /// a same-hash/different-payload collision is refused without replacement.
    pub fn append(&mut self, capture: &MarketCapture) -> Result<String, StoreError> {
        let payload = capture.model_dump_json().map_err(|_| StoreError::Archive)?;
        let id = digest(&payload);
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| StoreError::Archive)?;
        let result = (|| {
            let existing = transaction
                .query_row(
                    "SELECT payload FROM market_captures WHERE id=?1",
                    [&id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|_| StoreError::Archive)?;
            if let Some(existing) = existing {
                if existing != payload {
                    return Err(StoreError::ArchiveHash);
                }
                return Ok(());
            }
            let (count, bytes): (i64, i64) = transaction
                .query_row(
                    "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM market_captures",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|_| StoreError::Archive)?;
            if count < 0
                || bytes < 0
                || count >= i64::from(self.limits.max_records)
                || (bytes as u64).saturating_add(payload.len() as u64) > self.limits.max_bytes
            {
                return Err(StoreError::Bound);
            }
            transaction
                .execute(
                    "INSERT INTO market_captures(id,payload) VALUES(?1,?2)",
                    (&id, &payload),
                )
                .map_err(|_| StoreError::Archive)?;
            Ok(())
        })();
        match result {
            Ok(()) => transaction.commit().map_err(|_| StoreError::Archive)?,
            Err(error) => {
                transaction.rollback().map_err(|_| StoreError::Archive)?;
                return Err(error);
            }
        }
        Ok(id)
    }

    pub fn load(&self, id: &str) -> Result<MarketCapture, StoreError> {
        if !valid_hash(id) {
            return Err(StoreError::ArchiveHash);
        }
        let payload: String = self
            .db
            .query_row(
                "SELECT payload FROM market_captures WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .map_err(|_| StoreError::Archive)?;
        if payload.len() > 20_000_000 {
            return Err(StoreError::Bound);
        }
        if digest(&payload) != id {
            return Err(StoreError::ArchiveHash);
        }
        MarketCapture::parse(payload.as_bytes()).map_err(|_| StoreError::Archive)
    }

    pub fn close(self) -> Result<(), StoreError> {
        self.db.close().map_err(|_| StoreError::Archive)
    }
}

fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
