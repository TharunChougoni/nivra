//! Append-only experiment trials. A successful scorer process is execution
//! evidence only; this ledger never records that as an edge.
use super::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
pub(crate) mod lifecycle;
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
);
CREATE TABLE IF NOT EXISTS experiment_lifecycle_specs(
  candidate_id TEXT PRIMARY KEY,
  body TEXT NOT NULL,
  recorded_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS experiment_lifecycle_events(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  candidate_id TEXT NOT NULL,
  from_state TEXT,
  to_state TEXT NOT NULL,
  event TEXT NOT NULL,
  evidence_sha256 TEXT NOT NULL,
  at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS experiment_lifecycle_state(
  candidate_id TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  projection TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS experiment_continuations(
  continuation_id TEXT PRIMARY KEY,
  body TEXT NOT NULL,
  recorded_at_ms INTEGER NOT NULL
);";

/// Append-only trial phase. Historic unknown strings stay readable so a later
/// catalog change cannot make the sqlite body unparseable.
#[derive(Clone, Debug, Eq)]
pub(crate) enum TrialState {
    ExecutionFailed,
    ExecutionSucceeded,
    InsufficientEvidence,
    Unknown(String),
}

impl TrialState {
    pub(crate) fn parse(raw: &str) -> Self {
        match raw {
            "execution_failed" => Self::ExecutionFailed,
            "execution_succeeded" => Self::ExecutionSucceeded,
            "insufficient_evidence" => Self::InsufficientEvidence,
            other => Self::Unknown(other.to_owned()),
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::ExecutionFailed => "execution_failed",
            Self::ExecutionSucceeded => "execution_succeeded",
            Self::InsufficientEvidence => "insufficient_evidence",
            Self::Unknown(raw) => raw,
        }
    }
}

impl From<&str> for TrialState {
    fn from(raw: &str) -> Self {
        Self::parse(raw)
    }
}

impl From<String> for TrialState {
    fn from(raw: String) -> Self {
        Self::parse(&raw)
    }
}

impl PartialEq for TrialState {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<str> for TrialState {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for TrialState {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl Serialize for TrialState {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TrialState {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Ok(Self::parse(&String::deserialize(deserializer)?))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Trial {
    pub version: u32,
    pub trial_id: String,
    pub session: String,
    pub parent_trial_id: Option<String>,
    pub source_sha256: String,
    pub params_sha256: String,
    pub dataset_hash: String,
    pub abi: String,
    pub hypothesis: String,
    pub training_window: String,
    pub horizon: String,
    /// `execution_failed`, `execution_succeeded`, or `insufficient_evidence`.
    pub state: TrialState,
    pub execution_ok: bool,
    /// Always false. Execution success is not an edge.
    pub edge_qualified: bool,
    pub reason: String,
    pub recorded_at_ms: i64,
}

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

pub(crate) fn list(c: &Config) -> Result<Vec<Trial>> {
    if !path(c).is_file() {
        return Ok(Vec::new());
    }
    let db = open(c)?;
    let mut statement =
        db.prepare("SELECT body FROM experiment_trials ORDER BY recorded_at_ms,trial_id")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut trials = Vec::new();
    for row in rows {
        trials
            .push(serde_json::from_str(&row?).map_err(|_| Fail::new("experiment_trial_invalid"))?);
    }
    Ok(trials)
}

pub(crate) fn trial_id(parts: &[&str]) -> String {
    digest(parts.join("\0").as_bytes())
}
