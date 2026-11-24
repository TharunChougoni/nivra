//! Explicit offline paper simulation. No model subscription or market client is constructed.
use super::*;
use nivra_core::{execution::Instrument, finance::Amount};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
mod export;
mod receipts;
pub(crate) use receipts::*;
mod tape;
pub(crate) use tape::*;

pub(crate) fn scanner_archive_entry(
    config: &Path,
    output: &Path,
    max_snapshots: usize,
) -> std::process::ExitCode {
    use std::io::Write;
    let export = || -> Result<Value> {
        let raw = crate::read_bounded(config, 32_768).map_err(|_| "config_unreadable")?;
        let c: Config = serde_json::from_slice(&raw).map_err(|_| "config_invalid")?;
        c.validate()?;
        let _lock = rollover::controller_lock(&c)?;
        let c = rollover::current(&c)?;
        let (_, state) = rollover::bound_state(&c)?;
        let book: nivra_core::execution::Portfolio = serde_json::from_str(&state)?;
        if !book.closed {
            return Err("scanner_archive_requires_closed_book".into());
        }
        let snapshots = session_feature_snapshots(
            &c.directory,
            c.session,
            Some(c.previous_session),
            max_snapshots,
            2_000,
            20,
            millis(),
        )
        .map_err(Fail::new)?;
        let bytes = serde_json::to_vec(&snapshots)?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("candidate_dataset_too_large".into());
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(output)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(json!({
            "session": c.session,
            "snapshots": snapshots.len(),
            "first_observed_ms": snapshots.first().map(|row| row.observed_ms),
            "last_observed_ms": snapshots.last().map(|row| row.observed_ms),
            "sha256": digest(&bytes),
        }))
    };
    match export() {
        Ok(summary) => {
            println!("{summary}");
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("scanner_archive_failed: {}", failure_code(error.as_ref()));
            std::process::ExitCode::FAILURE
        }
    }
}
