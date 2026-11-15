//! Bounded read-only loading of a supplied original archive connection.
use crate::{StoreError, archive::LegacyUpstoxCapture};
use rusqlite::{Connection, limits::Limit, types::ValueRef};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct ArchiveLimits {
    max_records: u32,
    max_bytes: u64,
}

impl ArchiveLimits {
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

pub enum CaptureSelector<'a> {
    Id(&'a str),
    Latest,
}

/// No paths, archive creation, writes or clock renewal. The caller binds original
/// file/account/session/sidecar identity. A latest row means insertion rowid order,
/// never largest timestamp or sorted capture hash. Read rollback/close precedes
/// all returned data and errors. Capacity inputs are explicit, not authority.
/// File-backed connections must be read-only; active writers are invalid inputs.
pub fn load_upstox_capture(
    mut db: Connection,
    selector: CaptureSelector<'_>,
    limits: ArchiveLimits,
) -> Result<LegacyUpstoxCapture, StoreError> {
    db.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .map_err(|_| StoreError::Archive)?;
    if !db.is_autocommit() {
        return Err(StoreError::Archive);
    }
    if let CaptureSelector::Id(id) = &selector
        && (id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    {
        return Err(StoreError::ArchiveHash);
    }
    db.busy_timeout(Duration::from_millis(250))
        .map_err(|_| StoreError::Archive)?;
    db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 4_200_000)
        .map_err(|_| StoreError::Archive)?;
    db.execute_batch(
        "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA read_uncommitted=OFF;",
    )
    .map_err(|_| StoreError::Archive)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))
        .map_err(|_| StoreError::Archive)?;
    let transaction = db.transaction().map_err(|_| StoreError::Archive)?;
    let result = load(&transaction, selector, limits, deadline);
    transaction.rollback().map_err(|_| StoreError::Archive)?;
    result
}

fn schema(db: &Connection) -> Result<(), StoreError> {
    let mut stmt = db
        .prepare("PRAGMA database_list")
        .map_err(|_| StoreError::Schema)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Schema)?;
    let row = rows
        .next()
        .map_err(|_| StoreError::Schema)?
        .ok_or(StoreError::Schema)?;
    let filename: String = row.get(2).map_err(|_| StoreError::Schema)?;
    if !filename.is_empty() && !db.is_readonly("main").map_err(|_| StoreError::Schema)? {
        return Err(StoreError::Schema);
    }
    if row.get::<_, String>(1).map_err(|_| StoreError::Schema)? != "main"
        || rows.next().map_err(|_| StoreError::Schema)?.is_some()
    {
        return Err(StoreError::Schema);
    }
    let mut stmt = db
        .prepare("SELECT type,name,sql FROM main.sqlite_schema LIMIT 4")
        .map_err(|_| StoreError::Schema)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Schema)?;
    let mut tables = 0;
    let mut indexes = 0;
    while let Some(row) = rows.next().map_err(|_| StoreError::Schema)? {
        let kind: String = row.get(0).map_err(|_| StoreError::Schema)?;
        let name: String = row.get(1).map_err(|_| StoreError::Schema)?;
        let sql: Option<String> = row.get(2).map_err(|_| StoreError::Schema)?;
        if kind == "table"
            && name == "market_captures"
            && sql.as_deref().is_some_and(|s| {
                s.trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE TABLE ")
            })
        {
            tables += 1;
        } else if kind == "index" && name == "sqlite_autoindex_market_captures_1" && sql.is_none() {
            indexes += 1;
        } else {
            return Err(StoreError::Schema);
        }
    }
    if tables != 1 || indexes != 1 {
        return Err(StoreError::Schema);
    }
    let mut stmt = db
        .prepare("PRAGMA main.table_xinfo(market_captures)")
        .map_err(|_| StoreError::Schema)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Schema)?;
    for expected in [(0, "id", 0, 1), (1, "payload", 1, 0)] {
        let row = rows
            .next()
            .map_err(|_| StoreError::Schema)?
            .ok_or(StoreError::Schema)?;
        let actual: (i64, String, String, i64, Option<String>, i64, i64) = (
            row.get(0).map_err(|_| StoreError::Schema)?,
            row.get(1).map_err(|_| StoreError::Schema)?,
            row.get(2).map_err(|_| StoreError::Schema)?,
            row.get(3).map_err(|_| StoreError::Schema)?,
            row.get(4).map_err(|_| StoreError::Schema)?,
            row.get(5).map_err(|_| StoreError::Schema)?,
            row.get(6).map_err(|_| StoreError::Schema)?,
        );
        if actual
            != (
                expected.0,
                expected.1.into(),
                "TEXT".into(),
                expected.2,
                None,
                expected.3,
                0,
            )
        {
            return Err(StoreError::Schema);
        }
    }
    if rows.next().map_err(|_| StoreError::Schema)?.is_some() {
        return Err(StoreError::Schema);
    }
    Ok(())
}

fn load(
    db: &Connection,
    selector: CaptureSelector<'_>,
    limits: ArchiveLimits,
    deadline: Instant,
) -> Result<LegacyUpstoxCapture, StoreError> {
    schema(db)?;
    let (count, bytes): (i64,i64) = db.query_row(
        "SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM main.market_captures",
        [],|row| Ok((row.get(0)?,row.get(1)?))
    ).map_err(|_| StoreError::Archive)?;
    if count < 0
        || bytes < 0
        || count > i64::from(limits.max_records)
        || bytes as u64 > limits.max_bytes
        || Instant::now() >= deadline
    {
        return Err(StoreError::Bound);
    }
    let (sql, id) = match selector {
        CaptureSelector::Id(id) => (
            "SELECT id,payload FROM main.market_captures WHERE id=?1",
            Some(id),
        ),
        CaptureSelector::Latest => (
            "SELECT id,payload FROM main.market_captures ORDER BY rowid DESC LIMIT 1",
            None,
        ),
    };
    let mut stmt = db.prepare(sql).map_err(|_| StoreError::Archive)?;
    let mut rows = if let Some(id) = id {
        stmt.query([id])
    } else {
        stmt.query([])
    }
    .map_err(|_| StoreError::Archive)?;
    let row = rows
        .next()
        .map_err(|_| StoreError::Archive)?
        .ok_or(StoreError::Archive)?;
    let selected_id = id;
    let id = match row.get_ref(0).map_err(|_| StoreError::Archive)? {
        ValueRef::Text(value) if value.len() == 64 => std::str::from_utf8(value)
            .map_err(|_| StoreError::ArchiveHash)?
            .to_owned(),
        _ => return Err(StoreError::ArchiveHash),
    };
    if selected_id.is_some_and(|selected| selected != id) {
        return Err(StoreError::ArchiveHash);
    }
    let payload = match row.get_ref(1).map_err(|_| StoreError::Archive)? {
        ValueRef::Text(value) if value.len() <= 4_100_000 => std::str::from_utf8(value)
            .map_err(|_| StoreError::Archive)?
            .to_owned(),
        ValueRef::Text(_) => return Err(StoreError::Bound),
        _ => return Err(StoreError::Archive),
    };
    if rows.next().map_err(|_| StoreError::Archive)?.is_some() {
        return Err(StoreError::Archive);
    }
    let capture = LegacyUpstoxCapture::parse(payload, id)?;
    if Instant::now() >= deadline {
        return Err(StoreError::Bound);
    }
    Ok(capture)
}
