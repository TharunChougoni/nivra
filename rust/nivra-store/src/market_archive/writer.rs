use super::Result;
use chrono::NaiveDate;
use postgres::{
    Client, Transaction,
    types::{Json, Type},
};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub struct Receipt<'a> {
    pub requested_ms: i64,
    pub received_ms: i64,
    pub sha256: [u8; 32],
    /// Original archived envelope, including the untouched provider body.
    pub body: &'a [u8],
}

impl<'a> Receipt<'a> {
    pub fn new(requested_ms: i64, received_ms: i64, body: &'a [u8]) -> Self {
        Self {
            requested_ms,
            received_ms,
            sha256: Sha256::digest(body).into(),
            body,
        }
    }

    fn compress(&self) -> Result<Vec<u8>> {
        if self.requested_ms < 0 || self.requested_ms > self.received_ms || self.body.is_empty() {
            return Err("archive_receipt_invalid".into());
        }
        if self.sha256 != <[u8; 32]>::from(Sha256::digest(self.body)) {
            return Err("archive_receipt_conflict".into());
        }
        // The provider already bounds each response. Level 1 keeps writes inexpensive.
        Ok(zstd::bulk::compress(self.body, 1)?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i16)]
pub enum QuotePurpose {
    Scanner = 1,
    ExecutionOrProtection = 2,
    ScannerMetadata = 3,
    ResearchObservation = 4,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct QuoteObservation {
    pub instrument_key: String,
    pub source_ms: i64,
    pub ltp: i64,
    pub bid: Option<i64>,
    pub ask: Option<i64>,
    pub bid_qty: Option<i64>,
    pub ask_qty: Option<i64>,
    pub volume: i64,
    pub lower_circuit: i64,
    pub upper_circuit: i64,
}

pub struct QuoteBatch<'a> {
    pub receipt: Receipt<'a>,
    pub purpose: QuotePurpose,
    pub observations: Vec<QuoteObservation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i16)]
pub enum CandleUnit {
    Daily = 2,
    Minute = 3,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct CandleBar {
    pub day: NaiveDate,
    pub bar_ms: i64,
    pub o: i64,
    pub h: i64,
    pub l: i64,
    pub c: i64,
    pub volume: i64,
    pub oi: i64,
}

pub struct CandleBatch<'a> {
    pub receipt: Receipt<'a>,
    pub instrument_key: String,
    pub unit: CandleUnit,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub bars: Vec<CandleBar>,
}

#[derive(Clone, Copy)]
pub enum FailedAttempt {
    Unavailable,
    Retryable,
}

/// Return the new receipt id or verify that the existing evidence is identical.
fn insert_or_verify(
    tx: &mut Transaction<'_>,
    kind: i16,
    receipt: &Receipt<'_>,
    compressed: &[u8],
) -> Result<Option<i64>> {
    let hash = receipt.sha256.as_slice();
    let inserted = tx.query_typed_opt(
        "INSERT INTO receipts (kind, requested_ms, received_ms, sha256, body)
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (sha256) DO NOTHING RETURNING id",
        &[
            (&kind, Type::INT2),
            (&receipt.requested_ms, Type::INT8),
            (&receipt.received_ms, Type::INT8),
            (&hash, Type::BYTEA),
            (&compressed, Type::BYTEA),
        ],
    )?;
    if let Some(row) = inserted {
        return Ok(Some(row.get(0)));
    }
    // After ON CONFLICT waits for a concurrent insert, the next statement sees it.
    let existing = tx.query_typed_one(
        "SELECT kind, requested_ms, received_ms, body FROM receipts WHERE sha256 = $1",
        &[(&hash, Type::BYTEA)],
    )?;
    let body: &[u8] = existing.get(3);
    if existing.get::<_, i16>(0) != kind
        || existing.get::<_, i64>(1) != receipt.requested_ms
        || existing.get::<_, i64>(2) != receipt.received_ms
        || zstd::bulk::decompress(body, receipt.body.len())? != receipt.body
    {
        return Err("archive_receipt_conflict".into());
    }
    Ok(None)
}

fn insert_instruments(tx: &mut Transaction<'_>, keys: &[&str]) -> Result<()> {
    // A common lock order prevents deadlocks when two new batches overlap.
    tx.execute_typed(
        "INSERT INTO instruments (key)
         SELECT DISTINCT key FROM unnest($1::text[]) AS incoming(key) ORDER BY key
         ON CONFLICT (key) DO NOTHING",
        &[(&keys, Type::TEXT_ARRAY)],
    )?;
    Ok(())
}

/// Insert one complete quote receipt. Returns false on a verified retry.
pub fn write_quotes(db: &mut Client, batch: &QuoteBatch<'_>) -> Result<bool> {
    let compressed = batch.receipt.compress()?;
    let mut tx = db.transaction()?;
    let Some(receipt_id) = insert_or_verify(&mut tx, 1, &batch.receipt, &compressed)? else {
        tx.commit()?;
        return Ok(false);
    };
    let keys: Vec<&str> = batch
        .observations
        .iter()
        .map(|row| row.instrument_key.as_str())
        .collect();
    insert_instruments(&mut tx, &keys)?;
    let rows = Json(&batch.observations);
    tx.execute_typed(
        "INSERT INTO quote_obs
            (instrument_id, receipt_ms, source_ms, purpose, ltp, bid, ask, bid_qty, ask_qty,
             volume, lower_circuit, upper_circuit, receipt_id)
         SELECT i.id, $2, q.source_ms, $3, q.ltp, q.bid, q.ask, q.bid_qty, q.ask_qty,
                q.volume, q.lower_circuit, q.upper_circuit, $4
         FROM jsonb_to_recordset($1) AS q(
            instrument_key text, source_ms bigint, ltp bigint, bid bigint, ask bigint,
            bid_qty bigint, ask_qty bigint, volume bigint, lower_circuit bigint, upper_circuit bigint
         )
         JOIN instruments i ON i.key = q.instrument_key",
        &[
            (&rows, Type::JSONB),
            (&batch.receipt.received_ms, Type::INT8),
            (&(batch.purpose as i16), Type::INT2),
            (&receipt_id, Type::INT8),
        ],
    )?;
    tx.commit()?;
    Ok(true)
}

const CANDLE_COLUMNS: &str =
    "day date, bar_ms bigint, o bigint, h bigint, l bigint, c bigint, volume bigint, oi bigint";

fn insert_bars(
    tx: &mut Transaction<'_>,
    batch: &CandleBatch<'_>,
    instrument_id: i32,
    receipt_id: i64,
) -> Result<()> {
    let (table, time_column, minute_column) = match batch.unit {
        CandleUnit::Daily => ("daily_bars", "day", ""),
        CandleUnit::Minute => ("minute_bars", "bar_ms", ", bar_ms"),
    };
    let rows = Json(&batch.bars);
    let inserted = tx.execute(
        &format!("INSERT INTO {table} (instrument_id, receipt_id, day, o, h, l, c, volume, oi{minute_column})
                  SELECT $1, $2, day, o, h, l, c, volume, oi{minute_column}
                  FROM jsonb_to_recordset($3) AS b({CANDLE_COLUMNS}) ORDER BY {time_column}
                  ON CONFLICT (instrument_id, {time_column}) DO NOTHING"),
        &[&instrument_id, &receipt_id, &rows],
    )?;
    if inserted == batch.bars.len() as u64 {
        return Ok(());
    }
    // Overlapping history windows may repeat a bar. Compare its values while
    // preserving the first receipt as provenance.
    let matches: i64 = tx
        .query_one(
            &format!(
                "SELECT count(*) FROM jsonb_to_recordset($2) AS b({CANDLE_COLUMNS}) JOIN {table} t
                  ON t.instrument_id = $1 AND t.{time_column} = b.{time_column}
                  AND (t.day, t.o, t.h, t.l, t.c, t.volume, t.oi) =
                      (b.day, b.o, b.h, b.l, b.c, b.volume, b.oi)"
            ),
            &[&instrument_id, &rows],
        )?
        .get(0);
    if matches as usize != batch.bars.len() {
        return Err("archive_candle_conflict".into());
    }
    Ok(())
}

/// Insert a candle receipt, its bars, and coverage for the inclusive request window.
pub fn write_candles(db: &mut Client, batch: &CandleBatch<'_>) -> Result<bool> {
    let days = batch.end.signed_duration_since(batch.start).num_days();
    if !(0..3660).contains(&days)
        || batch
            .bars
            .iter()
            .any(|bar| bar.day < batch.start || bar.day > batch.end)
    {
        return Err("archive_candle_range_invalid".into());
    }
    let compressed = batch.receipt.compress()?;
    let mut tx = db.transaction()?;
    let Some(receipt_id) =
        insert_or_verify(&mut tx, batch.unit as i16, &batch.receipt, &compressed)?
    else {
        tx.commit()?;
        return Ok(false);
    };
    insert_instruments(&mut tx, &[&batch.instrument_key])?;
    let instrument_id = tx
        .query_one(
            "SELECT id FROM instruments WHERE key = $1",
            &[&batch.instrument_key],
        )?
        .get(0);
    insert_bars(&mut tx, batch, instrument_id, receipt_id)?;
    let mut observed: Vec<NaiveDate> = batch.bars.iter().map(|bar| bar.day).collect();
    observed.sort_unstable();
    observed.dedup();
    tx.execute(
        "INSERT INTO coverage (instrument_id, unit, day, status, attempted_ms, receipt_id)
         SELECT $1, $2, $3::date + offset_days,
                CASE WHEN $3::date + offset_days = ANY($5) THEN 'observed' ELSE 'empty' END,
                $6, $7
         FROM generate_series(0, $4::date - $3::date) AS offset_days
         ON CONFLICT (instrument_id, unit, day) DO UPDATE
         SET status = EXCLUDED.status, attempted_ms = EXCLUDED.attempted_ms, receipt_id = EXCLUDED.receipt_id
         WHERE (EXCLUDED.status = 'observed', EXCLUDED.attempted_ms)
             > (coverage.status = 'observed', coverage.attempted_ms)",
        &[&instrument_id, &(batch.unit as i16), &batch.start, &batch.end,
          &observed, &batch.receipt.requested_ms, &receipt_id],
    )?;
    tx.commit()?;
    Ok(true)
}

/// Record a provider failure without claiming that a candle body was received.
pub fn write_failed_attempt(
    db: &mut Client,
    key: &str,
    unit: CandleUnit,
    day: NaiveDate,
    attempted_ms: i64,
    failure: FailedAttempt,
) -> Result<()> {
    let status = match failure {
        FailedAttempt::Unavailable => "unavailable",
        FailedAttempt::Retryable => "failed_retryable",
    };
    let mut tx = db.transaction()?;
    insert_instruments(&mut tx, &[key])?;
    tx.execute(
        "INSERT INTO coverage (instrument_id, unit, day, status, attempted_ms)
         SELECT id, $2, $3, $4, $5 FROM instruments WHERE key = $1
         ON CONFLICT (instrument_id, unit, day) DO UPDATE
         SET status = EXCLUDED.status, attempted_ms = EXCLUDED.attempted_ms, receipt_id = NULL
         WHERE coverage.status <> 'observed' AND EXCLUDED.attempted_ms > coverage.attempted_ms",
        &[&key, &(unit as i16), &day, &status, &attempted_ms],
    )?;
    tx.commit()?;
    Ok(())
}
