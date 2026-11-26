//! TimescaleDB archive for original market receipts and normalized observations.

mod history;
mod reader;
mod writer;

pub use history::{
    CoverageWindow, CoveredDays, DailyBar, DailyCoverage, covered_days, daily_bars, daily_coverage,
    daily_instrument_keys, latest_daily_bars, lock_history_download,
};
pub use postgres::Client as ArchiveClient;
pub use reader::{
    QuoteBodies, quote_receipt_bodies, quote_receipt_ids, quote_reference, quote_watermark,
};
pub use writer::{
    CandleBar, CandleBatch, CandleUnit, FailedAttempt, QuoteBatch, QuoteObservation, QuotePurpose,
    Receipt, import_candles, import_quotes, import_unavailable, write_candles,
    write_failed_attempt, write_failed_range, write_quotes,
};

pub use postgres::Client;
use postgres::{Config, NoTls, error::SqlState};
use serde::Serialize;
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const SCHEMA_VERSION: i32 = 1;

/// Advisory lock keys. Each guards one thing, so a long holder of one never
/// blocks another: schema init, a history download pass, quote id ordering.
const INIT_LOCK: i64 = 6_749_301_730_906_321_426;
pub(crate) const HISTORY_DOWNLOAD_LOCK: i64 = 6_749_301_730_906_321_427;
pub(crate) const QUOTE_ORDER_LOCK: i64 = 6_749_301_730_906_321_428;

fn connection_config(url: &str) -> Result<Config> {
    let invalid = "archive_url_invalid";
    if !url.starts_with("postgresql://") && !url.starts_with("postgres://") {
        return Err(invalid.into());
    }
    let config: Config = url.parse().map_err(|_| invalid)?;
    if config.get_password().is_some()
        || config.get_user().is_none_or(str::is_empty)
        || config.get_dbname().is_none_or(str::is_empty)
        || config.get_hosts().is_empty()
    {
        return Err(invalid.into());
    }
    Ok(config)
}

pub fn validate_url(url: &str) -> Result<()> {
    connection_config(url).map(|_| ())
}

pub fn connect(url: &str, password: &str) -> Result<Client> {
    if password.is_empty() {
        return Err("archive_password_empty".into());
    }
    connection_config(url)?
        .password(password)
        .connect_timeout(Duration::from_secs(3))
        .tcp_user_timeout(Duration::from_secs(5))
        .keepalives_idle(Duration::from_secs(5))
        .keepalives_interval(Duration::from_secs(2))
        .keepalives_retries(2)
        .options("-c statement_timeout=3000 -c lock_timeout=2000")
        .connect(NoTls)
        .map_err(|_| "archive_connection_failed".into())
}

/// A data or integrity error is permanent for this receipt.
pub fn invalid_quote_write(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<postgres::Error>()
        .and_then(postgres::Error::code)
        .is_some_and(|code| matches!(&code.code()[..2], "22" | "23"))
        || matches!(
            error.to_string().as_str(),
            "archive_receipt_conflict" | "archive_receipt_invalid"
        )
}

pub fn init(db: &mut Client) -> Result<()> {
    let mut tx = db.transaction()?;
    // Serialize version checks and initial DDL across simultaneous commands.
    tx.query_one("SELECT pg_advisory_xact_lock($1)", &[&INIT_LOCK])?;
    tx.batch_execute(
        "CREATE TABLE IF NOT EXISTS schema_version (
            singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
            version integer NOT NULL
        )",
    )?;
    let version = tx.query_opt("SELECT version FROM schema_version WHERE singleton", &[])?;
    match version.as_ref().map(|row| row.get::<_, i32>(0)) {
        Some(SCHEMA_VERSION) => {}
        Some(_) => return Err("archive_schema_version_unsupported".into()),
        None => {
            tx.batch_execute(include_str!("schema.sql"))?;
            tx.execute(
                "INSERT INTO schema_version (version) VALUES ($1)",
                &[&SCHEMA_VERSION],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Check {
    pub schema_version: i32,
    pub instruments: i64,
    pub receipts: i64,
    pub quote_obs: i64,
    pub daily_bars: i64,
    pub minute_bars: i64,
    pub coverage: i64,
}

pub fn check(db: &mut Client) -> Result<Check> {
    let mut tx = db.build_transaction().read_only(true).start()?;
    let version = tx.query_opt("SELECT version FROM schema_version WHERE singleton", &[]);
    let schema_version = match version {
        Ok(Some(row)) => row.get(0),
        Ok(None) => return Err("archive_schema_missing".into()),
        Err(e) if e.code() == Some(&SqlState::UNDEFINED_TABLE) => {
            return Err("archive_schema_missing".into());
        }
        Err(e) => return Err(e.into()),
    };
    if schema_version != SCHEMA_VERSION {
        return Err("archive_schema_version_unsupported".into());
    }
    let row = tx.query_one(
        "SELECT (SELECT count(*) FROM instruments), (SELECT count(*) FROM receipts),
                (SELECT count(*) FROM quote_obs), (SELECT count(*) FROM daily_bars),
                (SELECT count(*) FROM minute_bars), (SELECT count(*) FROM coverage)",
        &[],
    )?;
    tx.commit()?;
    Ok(Check {
        schema_version,
        instruments: row.get(0),
        receipts: row.get(1),
        quote_obs: row.get(2),
        daily_bars: row.get(3),
        minute_bars: row.get(4),
        coverage: row.get(5),
    })
}
