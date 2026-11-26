//! Append-only provider evidence and shared history pacing in additive databases.
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{fs, io, os::unix::fs::OpenOptionsExt, path::Path, time::Duration};

pub fn open(path: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput).into());
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(Duration::from_secs(1))?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
    Ok(db)
}

pub struct CandleReceipt<'a> {
    pub range_id: &'a str,
    pub instrument_key: &'a str,
    pub unit: &'a str,
    pub start: &'a str,
    pub end: &'a str,
    pub requested_ms: i64,
    pub received_ms: i64,
    pub body_sha256: &'a str,
    pub body: &'a str,
}

pub fn init_candles(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS candle_receipts (
            range_id TEXT PRIMARY KEY NOT NULL,
            instrument_key TEXT NOT NULL, unit TEXT NOT NULL,
            start_date TEXT NOT NULL, end_date TEXT NOT NULL,
            requested_ms INTEGER NOT NULL, received_ms INTEGER NOT NULL,
            body_sha256 TEXT NOT NULL, body TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS candle_receipts_instrument
            ON candle_receipts(instrument_key, unit, start_date, end_date);",
    )
}

pub fn record_candle_receipt(
    db: &Connection,
    receipt: CandleReceipt<'_>,
) -> rusqlite::Result<bool> {
    // The immutable file is authoritative. An existing row must match it in full.
    let inserted = db.execute(
        "INSERT INTO candle_receipts VALUES (?,?,?,?,?,?,?,?,?)
         ON CONFLICT(range_id) DO NOTHING",
        params![
            receipt.range_id,
            receipt.instrument_key,
            receipt.unit,
            receipt.start,
            receipt.end,
            receipt.requested_ms,
            receipt.received_ms,
            receipt.body_sha256,
            receipt.body
        ],
    )?;
    if inserted == 1 {
        return Ok(true);
    }
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM candle_receipts WHERE range_id=?1
            AND instrument_key=?2 AND unit=?3 AND start_date=?4 AND end_date=?5
            AND requested_ms=?6 AND received_ms=?7 AND body_sha256=?8 AND body=?9)",
        params![
            receipt.range_id,
            receipt.instrument_key,
            receipt.unit,
            receipt.start,
            receipt.end,
            receipt.requested_ms,
            receipt.received_ms,
            receipt.body_sha256,
            receipt.body
        ],
        |row| row.get(0),
    )
}

pub fn init_history_pacing(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS history_pacing (
            scope TEXT PRIMARY KEY NOT NULL,
            next_read_ms INTEGER NOT NULL, cooldown_until_ms INTEGER NOT NULL
        )",
    )
}

/// Atomically reserve a paced start across history processes, or return the
/// existing pace and cooldown clocks. Quote reservations use the original journal.
pub fn pace_history(
    db: &mut Connection,
    scope: &str,
    now: i64,
    interval_ms: i64,
) -> rusqlite::Result<Option<(i64, i64)>> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let previous: Option<(i64, i64)> = tx
        .query_row(
            "SELECT next_read_ms, cooldown_until_ms FROM history_pacing WHERE scope=?",
            [scope],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((next, cooldown)) = previous
        && now < next.max(cooldown)
    {
        return Ok(previous);
    }
    tx.execute(
        "INSERT INTO history_pacing VALUES (?, ?, 0)
         ON CONFLICT(scope) DO UPDATE SET next_read_ms=excluded.next_read_ms",
        params![scope, now.saturating_add(interval_ms)],
    )?;
    tx.commit()?;
    Ok(None)
}

pub fn cool_down_history(db: &Connection, scope: &str, until: i64) -> rusqlite::Result<()> {
    db.execute(
        "UPDATE history_pacing SET cooldown_until_ms=MAX(cooldown_until_ms, ?1) WHERE scope=?2",
        params![until, scope],
    )?;
    Ok(())
}

pub fn init_feed(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS upstox_ltpc_frames (
            id INTEGER PRIMARY KEY,
            received_ms INTEGER NOT NULL, protobuf BLOB NOT NULL
        );",
    )
}

pub fn record_feed_frames(db: &mut Connection, frames: &[(i64, Vec<u8>)]) -> rusqlite::Result<()> {
    let tx = db.transaction()?;
    {
        let mut insert =
            tx.prepare("INSERT INTO upstox_ltpc_frames(received_ms, protobuf) VALUES (?,?)")?;
        for (receipt, frame) in frames {
            insert.execute(params![receipt, frame])?;
        }
    }
    tx.commit()
}
