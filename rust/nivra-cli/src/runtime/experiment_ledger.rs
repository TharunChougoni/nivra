//! Append-only experiment trials. A successful research job is execution
//! evidence only; this ledger never records that as an edge.
use super::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
pub(crate) mod research;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS experiment_trials(
  trial_id TEXT PRIMARY KEY,
  body TEXT NOT NULL,
  recorded_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS experiment_trial_events(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  trial_id TEXT NOT NULL,
  event TEXT NOT NULL,
  at_ms INTEGER NOT NULL
);";

pub(super) fn file_path(directory: &std::path::Path) -> std::path::PathBuf {
    directory.join("model").join("experiment-trials.sqlite")
}

fn path(c: &Config) -> std::path::PathBuf {
    file_path(&c.directory)
}

fn open(c: &Config) -> Result<Connection> {
    open_directory(&c.directory)
}

pub(super) fn open_directory(directory: &std::path::Path) -> Result<Connection> {
    open_file(&file_path(directory))
}

/// The ledger schema in a private SQLite file of the caller's choosing.
pub(super) fn open_file(file: &std::path::Path) -> Result<Connection> {
    let parent = file.parent().ok_or("experiment_ledger_path_invalid")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let exists = file.exists();
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let db = Connection::open_with_flags(file, flags)?;
    if !exists {
        fs::set_permissions(file, fs::Permissions::from_mode(0o600))?;
    }
    db.execute_batch(SCHEMA)?;
    db.execute_batch(super::lane_queue::SCHEMA)?;
    db.pragma_update(None, "synchronous", "FULL")?;
    Ok(db)
}

pub(super) fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn trial_id(parts: &[&str]) -> String {
    digest(parts.join("\0").as_bytes())
}
