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

    pub(crate) fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown(_))
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

/// Append one trial. The same identity and body is idempotent. A different body
/// for that identity is refused. An edge claim is refused.
pub(crate) fn record(c: &Config, trial: &Trial) -> Result<()> {
    if trial.version != 1
        || trial.trial_id.len() != 64
        || !valid_hash(&trial.trial_id)
        || trial.session.is_empty()
        || trial.session.len() > 32
        || !valid_hash(&trial.source_sha256)
        || !valid_hash(&trial.params_sha256)
        || !valid_hash(&trial.dataset_hash)
        || trial.abi.is_empty()
        || trial.abi.len() > 64
        || trial.hypothesis.len() > 1024
        || trial.hypothesis.chars().any(char::is_control)
        || trial.training_window.is_empty()
        || trial.training_window.len() > 64
        || trial.horizon.is_empty()
        || trial.horizon.len() > 64
        || !trial.state.is_known()
        || trial.reason.is_empty()
        || trial.reason.len() > 96
        || trial.recorded_at_ms < 0
        || (trial.state == "execution_succeeded") != trial.execution_ok
    {
        return Err("experiment_trial_invalid".into());
    }
    if trial.edge_qualified {
        return Err("experiment_edge_not_established".into());
    }
    let body = serde_json::to_string(trial)?;
    let mut db = open(c)?;
    let existing: Option<String> = db
        .query_row(
            "SELECT body FROM experiment_trials WHERE trial_id=?",
            [&trial.trial_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != body {
            return Err("experiment_trial_identity_changed".into());
        }
        return Ok(());
    }
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO experiment_trials(trial_id,body,recorded_at_ms) VALUES(?,?,?)",
        params![trial.trial_id, body, trial.recorded_at_ms],
    )?;
    tx.execute(
        "INSERT INTO experiment_trial_events(trial_id,event,at_ms) VALUES(?,?,?)",
        params![trial.trial_id, "recorded", trial.recorded_at_ms],
    )?;
    tx.commit()?;
    Ok(())
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

pub(crate) fn record_scorer_outcome(
    c: &Config,
    session: &str,
    configuration_hash: &str,
    prompt_sha256: &str,
    status: &str,
    result: &Value,
    now: i64,
) -> Result<()> {
    let replays = result["scorer_replays"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if replays.is_empty() {
        let reason = bounded_reason(
            result["reason"]
                .as_str()
                .filter(|reason| !reason.is_empty())
                .unwrap_or(if status == "completed" {
                    "scorer_completed"
                } else {
                    "scorer_unavailable"
                }),
        );
        let state = if status == "completed" {
            "insufficient_evidence"
        } else {
            "execution_failed"
        };
        return record(
            c,
            &Trial {
                version: 1,
                trial_id: trial_id(&[
                    "scorer-admission",
                    session,
                    configuration_hash,
                    prompt_sha256,
                    &reason,
                ]),
                session: session.to_owned(),
                parent_trial_id: None,
                source_sha256: "0".repeat(64),
                params_sha256: "0".repeat(64),
                dataset_hash: "0".repeat(64),
                abi: "scorer_post_close_v1".into(),
                hypothesis: String::new(),
                training_window: session.to_owned(),
                horizon: "post_close".into(),
                state: state.into(),
                execution_ok: false,
                edge_qualified: false,
                reason,
                recorded_at_ms: now,
            },
        );
    }
    let mut parent = None;
    for (index, replay) in replays.iter().enumerate() {
        let source = replay["scorer_sha256"].as_str().unwrap_or("");
        let params = replay["params_sha256"].as_str().unwrap_or("");
        let dataset = replay["dataset_hash"].as_str().unwrap_or("");
        if !valid_hash(source) || !valid_hash(params) || !valid_hash(dataset) {
            return Err("experiment_trial_invalid".into());
        }
        let valid = replay["valid_snapshots"].as_u64().unwrap_or(0);
        let minimum = replay["min_valid_snapshots"].as_u64().unwrap_or(1);
        let execution_ok = replay["status"] == "ok"
            && replay["failed_snapshots"].as_u64() == Some(0)
            && valid > 0
            && valid >= minimum;
        let state = if execution_ok {
            "execution_succeeded"
        } else if replay["status"] == "ok" {
            "insufficient_evidence"
        } else {
            "execution_failed"
        };
        let reason = bounded_reason(
            replay["reason"]
                .as_str()
                .filter(|reason| !reason.is_empty())
                .unwrap_or(state),
        );
        let id = trial_id(&[
            "scorer-replay",
            session,
            configuration_hash,
            &index.to_string(),
            source,
            params,
            dataset,
        ]);
        record(
            c,
            &Trial {
                version: 1,
                trial_id: id.clone(),
                session: session.to_owned(),
                parent_trial_id: parent.clone(),
                source_sha256: source.to_owned(),
                params_sha256: params.to_owned(),
                dataset_hash: dataset.to_owned(),
                abi: "scorer_replay_v1".into(),
                hypothesis: String::new(),
                training_window: session.to_owned(),
                horizon: "post_close".into(),
                state: state.into(),
                execution_ok,
                edge_qualified: false,
                reason,
                recorded_at_ms: now,
            },
        )?;
        parent = Some(id);
    }
    Ok(())
}

fn bounded_reason(reason: &str) -> String {
    let cleaned = reason
        .chars()
        .filter(|ch| ch.is_ascii_lowercase() || *ch == '_' || ch.is_ascii_digit())
        .take(96)
        .collect::<String>();
    if cleaned.is_empty() {
        "scorer_unavailable".into()
    } else {
        cleaned
    }
}
