//! Read-only dated pristine-book guard; no installation/session/service authority.
use rusqlite::{Connection, OptionalExtension, limits::Limit};
use sadr_core::native;
use serde_json::Value;
use std::{
    fmt,
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Version,
    Unavailable,
    Missing,
    Invalid,
    NonPristine,
    Capital,
    Policy,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Version => "unsupported_pristine_upgrade_version",
            Self::Unavailable => "pristine_state_unavailable",
            Self::Missing => "missing original state",
            Self::Invalid => "invalid original state",
            Self::NonPristine => "non-pristine book: upgrade refused",
            Self::Capital => "unexpected original capital",
            Self::Policy => "unexpected prepared wide book",
        })
    }
}
impl std::error::Error for Error {}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
pub fn validate_body(version: &str, body: &[u8]) -> Result<(), Error> {
    if !["v0.4.1", "v0.4.2"].contains(&version) {
        return Err(Error::Version);
    }
    let state = native::decode(body).map_err(|_| Error::Invalid)?;
    if ["halted", "stopped", "closed", "units", "fills", "positions"]
        .iter()
        .any(|key| state.get(key).is_some_and(truthy))
    {
        return Err(Error::NonPristine);
    }
    // Reject malformed false-like fields rather than accepting Python truthiness as a schema.
    for key in ["halted", "stopped", "closed"] {
        if state.get(key).is_some_and(|v| !v.is_boolean()) {
            return Err(Error::Invalid);
        }
    }
    if state.get("units").is_some_and(|v| v.as_u64() != Some(0))
        || state.get("fills").is_some_and(|v| !v.is_array())
        || state.get("positions").is_some_and(|v| !v.is_object())
    {
        return Err(Error::Invalid);
    }
    let expected = if version == "v0.4.2" {
        "100000"
    } else {
        "50000"
    };
    if state.get("cash").and_then(Value::as_str) != Some(expected) {
        return Err(Error::Capital);
    }
    if version == "v0.4.2"
        && (state.get("policy").and_then(Value::as_str) != Some(sadr_core::execution::POLICY)
            || state.get("high_water").and_then(Value::as_str) != Some(expected))
    {
        return Err(Error::Policy);
    }
    Ok(())
}
/// Consume a caller-supplied read-only connection and close its read snapshot before return.
/// Original path/inode/account binding and the dated installer clock/service guards
/// remain the caller's responsibility. A passing verdict does not authorize a swap.
pub fn inspect(mut db: Connection, version: &str) -> Result<(), Error> {
    db.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .map_err(|_| Error::Unavailable)?;
    if !db.is_autocommit() || !db.is_readonly("main").map_err(|_| Error::Unavailable)? {
        return Err(Error::Unavailable);
    }
    db.busy_timeout(Duration::from_millis(250))
        .map_err(|_| Error::Unavailable)?;
    db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 1_000_000)
        .map_err(|_| Error::Unavailable)?;
    db.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;")
        .map_err(|_| Error::Unavailable)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))
        .map_err(|_| Error::Unavailable)?;
    let tx = db.transaction().map_err(|_| Error::Unavailable)?;
    let result = (|| {
        let kind: Option<String> = tx
            .query_row(
                "SELECT type FROM sqlite_schema WHERE name='state'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| Error::Unavailable)?;
        if kind.as_deref() != Some("table") {
            return Err(Error::Invalid);
        }
        let row: Option<String> = tx
            .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
            .optional()
            .map_err(|_| Error::Unavailable)?;
        validate_body(version, row.ok_or(Error::Missing)?.as_bytes())
    })();
    tx.rollback().map_err(|_| Error::Unavailable)?;
    result
}
