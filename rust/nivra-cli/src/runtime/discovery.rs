//! Frozen scanner evidence for Luna. Screening observations never authorize fills.
use super::*;
use nivra_core::execution::{Instrument, Scanner};
use serde_json::{Value, json};

mod projection;
mod settings;
pub(crate) use projection::*;
pub(crate) use settings::*;

pub(crate) fn snapshot(
    c: &Config,
    instruments: &[Instrument],
    settings: &Settings,
    now: i64,
) -> Result<Value> {
    if !c.directory.join("paper.sqlite").exists() {
        return Ok(project(&Scanner::default(), instruments, settings, now));
    }
    let db = Connection::open_with_flags(
        c.directory.join("paper.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    use rusqlite::OptionalExtension;
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='scanner')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(project(&Scanner::default(), instruments, settings, now));
    }
    let Some(bytes) = db
        .query_row("SELECT body FROM scanner WHERE id=1", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    else {
        return Ok(project(&Scanner::default(), instruments, settings, now));
    };
    if bytes.len() > 16_000_000 {
        return Err("planner_scanner_snapshot_invalid".into());
    }
    let scan: Scanner = serde_json::from_str(&bytes)?;
    Ok(project(&scan, instruments, settings, now))
}

/// The same upper median used by scanner triggers (#20).
pub(crate) fn median(mut values: Vec<f64>) -> f64 {
    values.retain(|v| v.is_finite());
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

#[cfg(test)]
mod thesis_tests;

#[cfg(test)]
mod executable_quote_tests;
#[cfg(test)]
mod strategy_tests;
