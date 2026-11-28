//! The autonomous research record: `research/research.sqlite` under the
//! sessions root, outside every dated book, plus immutable blobs under
//! `research/objects/sha256/`. SQLite owns ideas, experiments, findings,
//! skills, leases and budgets. The job queue owns run status; TimescaleDB
//! owns market data. Nothing here holds bulk quotes or account payloads.
use chrono::NaiveDate;
use nivra_core::autoresearch::{
    DayLimits, DaySpend, Decision, Regime, Score, Stage, mechanism_key,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

mod objects;
pub use objects::{BackupReport, verify};
#[cfg(test)]
mod tests;

const SCHEMA_VERSION: i64 = 1;

/// A bounded snake_case failure code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self("research_store_unavailable")
    }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self("research_store_io_failed")
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self("research_store_record_invalid")
    }
}
pub type Result<T> = std::result::Result<T, Error>;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS campaign(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  segment TEXT NOT NULL,
  protocol_version INTEGER NOT NULL,
  manifest_digest TEXT NOT NULL,
  development_from TEXT NOT NULL,
  development_to TEXT NOT NULL,
  selection_from TEXT NOT NULL,
  validation_from TEXT, validation_to TEXT,
  holdout_from TEXT, holdout_to TEXT,
  trial_cap INTEGER NOT NULL CHECK(trial_cap > 0),
  finalist_experiment_id INTEGER REFERENCES experiment(id),
  holdout_state TEXT NOT NULL DEFAULT 'unopened' CHECK(holdout_state IN ('unopened','reserved','opened','retired')),
  status TEXT NOT NULL CHECK(status IN ('active','exhausted','closed')),
  created_at_ms INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS campaign_holdout ON campaign(segment, holdout_from, holdout_to)
  WHERE holdout_from IS NOT NULL;
CREATE TABLE IF NOT EXISTS idea(
  id INTEGER PRIMARY KEY,
  campaign_id INTEGER NOT NULL REFERENCES campaign(id),
  origin_request_key TEXT UNIQUE,
  family TEXT NOT NULL,
  regime TEXT NOT NULL CHECK(regime IN ('trend','range','unknown')),
  mechanism_key TEXT NOT NULL,
  hypothesis TEXT NOT NULL,
  falsifier TEXT NOT NULL,
  comparator TEXT NOT NULL,
  parent_experiment_id INTEGER REFERENCES experiment(id),
  status TEXT NOT NULL CHECK(status IN ('open','testing','kept','rejected','invalid','exhausted')),
  repairs INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idea_family ON idea(campaign_id, family, regime);
CREATE TABLE IF NOT EXISTS experiment(
  id INTEGER PRIMARY KEY,
  campaign_id INTEGER NOT NULL REFERENCES campaign(id),
  idea_id INTEGER NOT NULL REFERENCES idea(id),
  parent_id INTEGER REFERENCES experiment(id),
  stage TEXT NOT NULL CHECK(stage IN ('bars','quote_replay')),
  source_path TEXT NOT NULL,
  source_blob TEXT NOT NULL,
  params_blob TEXT NOT NULL,
  manifest_blob TEXT NOT NULL,
  evaluator_digest TEXT NOT NULL,
  experiment_key TEXT NOT NULL UNIQUE,
  ordinal INTEGER NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('reserved','scored','invalid','failed')),
  score TEXT,
  verdict TEXT CHECK(verdict IN ('keep','reject')),
  reason TEXT,
  report_blob TEXT,
  interpreted INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(campaign_id, ordinal)
);
CREATE TABLE IF NOT EXISTS attempt(
  id INTEGER PRIMARY KEY,
  experiment_id INTEGER NOT NULL REFERENCES experiment(id),
  attempt_no INTEGER NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE,
  job_id TEXT,
  job_revision INTEGER,
  lease_generation INTEGER NOT NULL,
  deadline_ms INTEGER NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('reserved','running','succeeded','invalid','failed','preempted')),
  failure TEXT,
  usage TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(experiment_id, attempt_no)
);
CREATE TABLE IF NOT EXISTS finding(
  id INTEGER PRIMARY KEY,
  idea_id INTEGER REFERENCES idea(id),
  experiment_id INTEGER REFERENCES experiment(id),
  kind TEXT NOT NULL CHECK(kind IN ('positive','negative','inconclusive','procedure')),
  claim TEXT NOT NULL,
  scope TEXT NOT NULL,
  counterevidence TEXT NOT NULL,
  next_test TEXT NOT NULL,
  evidence_digest TEXT,
  created_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS skill_revision(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  digest TEXT NOT NULL,
  parent_digest TEXT,
  finding_ids TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('proposed','approved','rejected','retired')),
  decision_reason TEXT,
  reviewer TEXT,
  active_from_ms INTEGER,
  retired_at_ms INTEGER,
  created_at_ms INTEGER NOT NULL,
  UNIQUE(name, digest)
);
CREATE UNIQUE INDEX IF NOT EXISTS skill_one_active ON skill_revision(name) WHERE status = 'approved';
CREATE TABLE IF NOT EXISTS cell(
  campaign_id INTEGER NOT NULL REFERENCES campaign(id),
  segment TEXT NOT NULL,
  family TEXT NOT NULL,
  regime TEXT NOT NULL,
  incumbent_experiment_id INTEGER NOT NULL REFERENCES experiment(id),
  PRIMARY KEY(campaign_id, segment, family, regime)
);
CREATE TABLE IF NOT EXISTS promotion(
  id INTEGER PRIMARY KEY,
  candidate_experiment_id INTEGER NOT NULL REFERENCES experiment(id),
  replay_report_blob TEXT NOT NULL,
  forward_report_blob TEXT,
  policy_version TEXT NOT NULL,
  library_id TEXT,
  state TEXT NOT NULL CHECK(state IN ('pending','completed','refused')),
  created_at_ms INTEGER NOT NULL,
  UNIQUE(candidate_experiment_id, policy_version, replay_report_blob, forward_report_blob)
);
CREATE TABLE IF NOT EXISTS shadow(
  id INTEGER PRIMARY KEY,
  promotion_id INTEGER NOT NULL REFERENCES promotion(id),
  session TEXT NOT NULL,
  ledger_blob TEXT,
  coverage_bps INTEGER,
  state TEXT NOT NULL,
  UNIQUE(promotion_id, session)
);
CREATE TABLE IF NOT EXISTS lease(
  name TEXT PRIMARY KEY,
  owner TEXT NOT NULL,
  pid INTEGER NOT NULL,
  generation INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS reservation(
  key TEXT PRIMARY KEY,
  day TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('variant','turn')),
  calls INTEGER NOT NULL,
  usd_micros INTEGER NOT NULL,
  settled INTEGER NOT NULL DEFAULT 0,
  created_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS reservation_day ON reservation(day, kind);
CREATE TABLE IF NOT EXISTS imported(
  sha256 TEXT NOT NULL,
  destination TEXT NOT NULL,
  origin TEXT NOT NULL,
  at_ms INTEGER NOT NULL,
  PRIMARY KEY(sha256, destination)
);
CREATE TABLE IF NOT EXISTS loop_state(
  id INTEGER PRIMARY KEY CHECK(id = 1),
  body TEXT NOT NULL
);
"#;

/// The open research record.
pub struct Store {
    root: PathBuf,
    db: Connection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdeaStatus {
    Open,
    Testing,
    Kept,
    Rejected,
    Invalid,
    Exhausted,
}

impl IdeaStatus {
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "open" => Self::Open,
            "testing" => Self::Testing,
            "kept" => Self::Kept,
            "rejected" => Self::Rejected,
            "invalid" => Self::Invalid,
            "exhausted" => Self::Exhausted,
            _ => return Err(Error("research_store_record_invalid")),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Campaign {
    pub id: i64,
    pub name: String,
    pub segment: String,
    pub manifest_digest: String,
    pub development_from: NaiveDate,
    pub development_to: NaiveDate,
    pub selection_from: NaiveDate,
    pub trial_cap: u32,
    pub status: String,
    pub created_at_ms: i64,
}

pub struct NewCampaign<'a> {
    pub name: &'a str,
    pub segment: &'a str,
    /// The frozen campaign manifest, stored as an object.
    pub manifest: &'a [u8],
    pub development_from: NaiveDate,
    pub development_to: NaiveDate,
    pub selection_from: NaiveDate,
    pub trial_cap: u32,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Idea {
    pub id: i64,
    pub campaign_id: i64,
    pub family: String,
    pub regime: Regime,
    pub mechanism_key: String,
    pub hypothesis: String,
    pub falsifier: String,
    pub comparator: String,
    pub parent_experiment_id: Option<i64>,
    pub status: IdeaStatus,
    pub repairs: u32,
    pub created_at_ms: i64,
}

pub struct NewIdea<'a> {
    pub campaign_id: i64,
    pub origin_request_key: Option<&'a str>,
    pub family: &'a str,
    pub regime: Regime,
    /// The mechanism in the model's words; its normalised key detects repeats.
    pub mechanism: &'a str,
    pub hypothesis: &'a str,
    pub falsifier: &'a str,
    pub comparator: &'a str,
    pub parent_experiment_id: Option<i64>,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Experiment {
    pub id: i64,
    pub campaign_id: i64,
    pub idea_id: i64,
    pub parent_id: Option<i64>,
    pub stage: Stage,
    pub source_path: String,
    pub source_blob: String,
    pub params_blob: String,
    pub manifest_blob: String,
    pub experiment_key: String,
    /// The trial number within the campaign, from 1.
    pub ordinal: u32,
    pub status: String,
    pub score: Option<Score>,
    pub verdict: Option<String>,
    pub reason: Option<String>,
    pub report_blob: Option<String>,
    pub interpreted: bool,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Attempt {
    pub id: i64,
    pub experiment_id: i64,
    pub attempt_no: u32,
    pub idempotency_key: String,
    pub job_id: Option<String>,
    pub job_revision: Option<u32>,
    pub lease_generation: i64,
    pub deadline_ms: i64,
    pub state: String,
    pub failure: Option<String>,
}

/// One submission. The blobs are written by `put_object` first.
pub struct Submission<'a> {
    pub idea_id: i64,
    pub stage: Stage,
    pub source_path: &'a str,
    pub source_blob: &'a str,
    pub params_blob: &'a str,
    pub manifest_blob: &'a str,
    pub evaluator_digest: &'a str,
    pub experiment_key: &'a str,
    pub lease_generation: i64,
    pub deadline_ms: i64,
    pub day: NaiveDate,
    pub limits: DayLimits,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reserved {
    /// A new trial with its first attempt, its variant slot and its key.
    New(Experiment, Attempt),
    /// The same key ran before: its record, no new trial.
    Existing(Experiment),
    Refused(&'static str),
}

/// How an attempt ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Scored {
        score: Score,
        decision: Decision,
        report_blob: String,
    },
    /// The source or its output broke the protocol. One repair is allowed.
    Invalid { reason: String },
    /// Infrastructure failed; the frozen experiment may run again as a new attempt.
    Failed { reason: String },
    /// A decision turn or host pressure stopped it; it runs again later.
    Preempted { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub id: i64,
    pub idea_id: Option<i64>,
    pub experiment_id: Option<i64>,
    pub kind: String,
    pub claim: String,
    pub scope: String,
    pub counterevidence: String,
    pub next_test: String,
    pub evidence_digest: Option<String>,
    pub created_at_ms: i64,
}

pub struct NewFinding<'a> {
    pub experiment_id: i64,
    pub kind: &'a str,
    pub claim: &'a str,
    pub scope: &'a str,
    pub counterevidence: &'a str,
    pub next_test: &'a str,
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SkillRevision {
    pub name: String,
    pub digest: String,
    pub parent_digest: Option<String>,
    pub finding_ids: Vec<i64>,
    pub status: String,
    pub decision_reason: Option<String>,
    pub reviewer: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Cell {
    pub segment: String,
    pub family: String,
    pub regime: String,
    pub incumbent_experiment_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub owner: String,
    pub generation: i64,
    pub expires_at_ms: i64,
}

/// A bounded history read.
pub struct HistoryQuery<'a> {
    pub campaign_id: Option<i64>,
    pub family: Option<&'a str>,
    pub mechanism: Option<&'a str>,
    pub parent_experiment_id: Option<i64>,
    /// Only records with ids below this cursor; `None` starts at the newest.
    pub before_id: Option<i64>,
    pub limit: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct History {
    pub ideas: Vec<Idea>,
    pub experiments: Vec<Experiment>,
    pub findings: Vec<Finding>,
    pub incumbents: Vec<Cell>,
    pub skills: Vec<SkillRevision>,
    /// Pass as `before_id` for the next page; `None` when exhausted.
    pub next_before_id: Option<i64>,
}

fn date(text: String) -> rusqlite::Result<NaiveDate> {
    text.parse().map_err(|_| rusqlite::Error::InvalidQuery)
}

fn campaign_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Campaign> {
    Ok(Campaign {
        id: row.get(0)?,
        name: row.get(1)?,
        segment: row.get(2)?,
        manifest_digest: row.get(3)?,
        development_from: date(row.get(4)?)?,
        development_to: date(row.get(5)?)?,
        selection_from: date(row.get(6)?)?,
        trial_cap: row.get(7)?,
        status: row.get(8)?,
        created_at_ms: row.get(9)?,
    })
}
const CAMPAIGN: &str = "SELECT id,name,segment,manifest_digest,development_from,development_to,selection_from,trial_cap,status,created_at_ms FROM campaign";

fn idea_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Idea> {
    let regime: String = row.get(3)?;
    let status: String = row.get(10)?;
    Ok(Idea {
        id: row.get(0)?,
        campaign_id: row.get(1)?,
        family: row.get(2)?,
        regime: Regime::parse(&regime).ok_or(rusqlite::Error::InvalidQuery)?,
        mechanism_key: row.get(4)?,
        hypothesis: row.get(5)?,
        falsifier: row.get(6)?,
        comparator: row.get(7)?,
        parent_experiment_id: row.get(8)?,
        created_at_ms: row.get(9)?,
        status: IdeaStatus::parse(&status).map_err(|_| rusqlite::Error::InvalidQuery)?,
        repairs: row.get(11)?,
    })
}
const IDEA: &str = "SELECT id,campaign_id,family,regime,mechanism_key,hypothesis,falsifier,comparator,parent_experiment_id,created_at_ms,status,repairs FROM idea";

fn experiment_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Experiment> {
    let stage: String = row.get(4)?;
    let score: Option<String> = row.get(12)?;
    Ok(Experiment {
        id: row.get(0)?,
        campaign_id: row.get(1)?,
        idea_id: row.get(2)?,
        parent_id: row.get(3)?,
        stage: Stage::parse(&stage).ok_or(rusqlite::Error::InvalidQuery)?,
        source_path: row.get(5)?,
        source_blob: row.get(6)?,
        params_blob: row.get(7)?,
        manifest_blob: row.get(8)?,
        experiment_key: row.get(9)?,
        ordinal: row.get(10)?,
        status: row.get(11)?,
        score: score
            .map(|text| serde_json::from_str(&text))
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        verdict: row.get(13)?,
        reason: row.get(14)?,
        report_blob: row.get(15)?,
        interpreted: row.get(16)?,
        created_at_ms: row.get(17)?,
    })
}
const EXPERIMENT: &str = "SELECT id,campaign_id,idea_id,parent_id,stage,source_path,source_blob,params_blob,manifest_blob,experiment_key,ordinal,status,score,verdict,reason,report_blob,interpreted,created_at_ms FROM experiment";

fn attempt_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Attempt> {
    Ok(Attempt {
        id: row.get(0)?,
        experiment_id: row.get(1)?,
        attempt_no: row.get(2)?,
        idempotency_key: row.get(3)?,
        job_id: row.get(4)?,
        job_revision: row.get(5)?,
        lease_generation: row.get(6)?,
        deadline_ms: row.get(7)?,
        state: row.get(8)?,
        failure: row.get(9)?,
    })
}
const ATTEMPT: &str = "SELECT id,experiment_id,attempt_no,idempotency_key,job_id,job_revision,lease_generation,deadline_ms,state,failure FROM attempt";

fn finding_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Finding> {
    Ok(Finding {
        id: row.get(0)?,
        idea_id: row.get(1)?,
        experiment_id: row.get(2)?,
        kind: row.get(3)?,
        claim: row.get(4)?,
        scope: row.get(5)?,
        counterevidence: row.get(6)?,
        next_test: row.get(7)?,
        evidence_digest: row.get(8)?,
        created_at_ms: row.get(9)?,
    })
}
const FINDING: &str = "SELECT id,idea_id,experiment_id,kind,claim,scope,counterevidence,next_test,evidence_digest,created_at_ms FROM finding";

fn skill_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillRevision> {
    let ids: String = row.get(3)?;
    Ok(SkillRevision {
        name: row.get(0)?,
        digest: row.get(1)?,
        parent_digest: row.get(2)?,
        finding_ids: serde_json::from_str(&ids).map_err(|_| rusqlite::Error::InvalidQuery)?,
        status: row.get(4)?,
        decision_reason: row.get(5)?,
        reviewer: row.get(6)?,
        created_at_ms: row.get(7)?,
    })
}
const SKILL: &str = "SELECT name,digest,parent_digest,finding_ids,status,decision_reason,reviewer,created_at_ms FROM skill_revision";

impl Store {
    /// Open or create `<root>/research.sqlite` with WAL and foreign keys.
    pub fn open(root: &Path) -> Result<Self> {
        if !root.is_absolute() {
            return Err(Error("research_store_path_invalid"));
        }
        objects::private_dir(root)?;
        let path = root.join("research.sqlite");
        if !path.exists() {
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
        }
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_file() {
            return Err(Error("research_store_path_invalid"));
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let db = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::from_millis(2_000))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(Error("research_store_schema_newer"));
        }
        db.execute_batch(SCHEMA)?;
        db.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self {
            root: root.to_path_buf(),
            db,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store `bytes` as an immutable object and return its digest.
    pub fn put_object(&self, bytes: &[u8]) -> Result<String> {
        objects::put(&self.root, bytes)
    }

    /// Read an object back, verifying its digest.
    pub fn object(&self, sha256: &str) -> Result<Vec<u8>> {
        objects::get(&self.root, sha256)
    }

    /// The named campaign, created on first use with its frozen manifest.
    /// An existing campaign keeps its original bounds.
    pub fn ensure_campaign(&mut self, new: &NewCampaign<'_>) -> Result<Campaign> {
        let manifest = self.put_object(new.manifest)?;
        self.db.execute(
            "INSERT OR IGNORE INTO campaign(name,segment,protocol_version,manifest_digest,development_from,development_to,selection_from,trial_cap,status,created_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'active',?9)",
            params![
                new.name,
                new.segment,
                nivra_core::autoresearch::PROTOCOL_VERSION,
                manifest,
                new.development_from.to_string(),
                new.development_to.to_string(),
                new.selection_from.to_string(),
                new.trial_cap,
                new.now
            ],
        )?;
        self.campaign(new.name)?
            .ok_or(Error("research_store_unavailable"))
    }

    pub fn campaign(&self, name: &str) -> Result<Option<Campaign>> {
        Ok(self
            .db
            .query_row(&format!("{CAMPAIGN} WHERE name=?1"), [name], campaign_row)
            .optional()?)
    }

    /// Raise or lower a campaign's trial cap; a cap at or below the trials
    /// already scored exhausts it.
    pub fn set_trial_cap(&mut self, campaign_id: i64, cap: u32) -> Result<()> {
        self.db.execute(
            "UPDATE campaign SET trial_cap=?2 WHERE id=?1",
            params![campaign_id, cap.max(1)],
        )?;
        Ok(())
    }

    /// Record an idea. A repeat of an open or testing idea's mechanism and
    /// parent in the same campaign returns the earlier idea instead.
    pub fn record_idea(&mut self, new: &NewIdea<'_>) -> Result<(Idea, bool)> {
        let key = mechanism_key(new.mechanism);
        if key.is_empty() {
            return Err(Error("research_idea_mechanism_invalid"));
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                &format!(
                    "{IDEA} WHERE campaign_id=?1 AND mechanism_key=?2 AND parent_experiment_id IS ?3 ORDER BY id DESC LIMIT 1"
                ),
                params![new.campaign_id, key, new.parent_experiment_id],
                idea_row,
            )
            .optional()?;
        if let Some(idea) = existing {
            return Ok((idea, true));
        }
        if let Some(request) = new.origin_request_key
            && let Some(idea) = tx
                .query_row(
                    &format!("{IDEA} WHERE origin_request_key=?1"),
                    [request],
                    idea_row,
                )
                .optional()?
        {
            return Ok((idea, true));
        }
        tx.execute(
            "INSERT INTO idea(campaign_id,origin_request_key,family,regime,mechanism_key,hypothesis,falsifier,comparator,parent_experiment_id,status,created_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'open',?10)",
            params![
                new.campaign_id,
                new.origin_request_key,
                new.family,
                new.regime.as_str(),
                key,
                new.hypothesis,
                new.falsifier,
                new.comparator,
                new.parent_experiment_id,
                new.now
            ],
        )?;
        let id = tx.last_insert_rowid();
        let idea = tx.query_row(&format!("{IDEA} WHERE id=?1"), [id], idea_row)?;
        tx.commit()?;
        Ok((idea, false))
    }

    pub fn idea(&self, id: i64) -> Result<Option<Idea>> {
        Ok(self
            .db
            .query_row(&format!("{IDEA} WHERE id=?1"), [id], idea_row)
            .optional()?)
    }

    pub fn experiment(&self, id: i64) -> Result<Option<Experiment>> {
        Ok(self
            .db
            .query_row(&format!("{EXPERIMENT} WHERE id=?1"), [id], experiment_row)
            .optional()?)
    }

    /// Scored trials in a campaign, abandoned and failed ones included.
    pub fn trials(&self, campaign_id: i64) -> Result<u32> {
        Ok(self.db.query_row(
            "SELECT count(*) FROM experiment WHERE campaign_id=?1",
            [campaign_id],
            |row| row.get(0),
        )?)
    }

    /// Reserve the trial, the day's variant slot and the submission key in
    /// one transaction, before any job exists.
    pub fn reserve(&mut self, s: &Submission<'_>) -> Result<Reserved> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = tx
            .query_row(
                &format!("{EXPERIMENT} WHERE experiment_key=?1"),
                [s.experiment_key],
                experiment_row,
            )
            .optional()?
        {
            return Ok(Reserved::Existing(existing));
        }
        let Some(idea) = tx
            .query_row(&format!("{IDEA} WHERE id=?1"), [s.idea_id], idea_row)
            .optional()?
        else {
            return Ok(Reserved::Refused("research_idea_unknown"));
        };
        let repair = match idea.status {
            IdeaStatus::Open => false,
            IdeaStatus::Invalid if idea.repairs == 0 => true,
            IdeaStatus::Invalid => return Ok(Reserved::Refused("research_idea_repair_used")),
            _ => return Ok(Reserved::Refused("research_idea_not_open")),
        };
        let (status, cap): (String, u32) = tx.query_row(
            "SELECT status,trial_cap FROM campaign WHERE id=?1",
            [idea.campaign_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let trials: u32 = tx.query_row(
            "SELECT count(*) FROM experiment WHERE campaign_id=?1",
            [idea.campaign_id],
            |row| row.get(0),
        )?;
        if status != "active" || trials >= cap {
            return Ok(Reserved::Refused("trial_allowance_spent"));
        }
        let spent = day_spend(&tx, s.day)?;
        if let Some(reason) = s.limits.variant_refusal(&spent) {
            return Ok(Reserved::Refused(reason));
        }
        tx.execute(
            "INSERT INTO experiment(campaign_id,idea_id,parent_id,stage,source_path,source_blob,params_blob,manifest_blob,evaluator_digest,experiment_key,ordinal,status,created_at_ms,updated_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'reserved',?12,?12)",
            params![
                idea.campaign_id,
                idea.id,
                idea.parent_experiment_id,
                s.stage.as_str(),
                s.source_path,
                s.source_blob,
                s.params_blob,
                s.manifest_blob,
                s.evaluator_digest,
                s.experiment_key,
                trials + 1,
                s.now
            ],
        )?;
        let experiment_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO attempt(experiment_id,attempt_no,idempotency_key,lease_generation,deadline_ms,state,created_at_ms,updated_at_ms)
             VALUES(?1,1,?2,?3,?4,'reserved',?5,?5)",
            params![
                experiment_id,
                format!("{}:1", s.experiment_key),
                s.lease_generation,
                s.deadline_ms,
                s.now
            ],
        )?;
        tx.execute(
            "INSERT INTO reservation(key,day,kind,calls,usd_micros,settled,created_at_ms) VALUES(?1,?2,'variant',0,0,1,?3)",
            params![format!("variant:{}", s.experiment_key), s.day.to_string(), s.now],
        )?;
        tx.execute(
            "UPDATE idea SET status='testing', repairs=repairs+?2 WHERE id=?1",
            params![idea.id, i64::from(repair)],
        )?;
        let experiment = tx.query_row(
            &format!("{EXPERIMENT} WHERE id=?1"),
            [experiment_id],
            experiment_row,
        )?;
        let attempt = tx.query_row(
            &format!("{ATTEMPT} WHERE experiment_id=?1"),
            [experiment_id],
            attempt_row,
        )?;
        tx.commit()?;
        Ok(Reserved::New(experiment, attempt))
    }

    /// The latest attempt of an experiment.
    pub fn attempt(&self, experiment_id: i64) -> Result<Option<Attempt>> {
        Ok(self
            .db
            .query_row(
                &format!("{ATTEMPT} WHERE experiment_id=?1 ORDER BY attempt_no DESC LIMIT 1"),
                [experiment_id],
                attempt_row,
            )
            .optional()?)
    }

    /// Bind the queue job to an attempt. Only the current lease generation
    /// may write it; `false` means a newer holder owns the loop.
    pub fn bind_job(
        &mut self,
        attempt_id: i64,
        job_id: &str,
        revision: u32,
        generation: i64,
        now: i64,
    ) -> Result<bool> {
        if !self.generation_current(generation)? {
            return Ok(false);
        }
        Ok(self.db.execute(
            "UPDATE attempt SET job_id=?2, job_revision=?3, state='running', lease_generation=?4, updated_at_ms=?5
             WHERE id=?1 AND state IN ('reserved','running')",
            params![attempt_id, job_id, revision, generation, now],
        )? == 1)
    }

    /// Start the next attempt of a frozen experiment after a preemption or an
    /// infrastructure failure. It spends no trial and no variant slot.
    pub fn next_attempt(
        &mut self,
        experiment_id: i64,
        generation: i64,
        deadline_ms: i64,
        now: i64,
    ) -> Result<Attempt> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (key, next): (String, u32) = tx.query_row(
            "SELECT e.experiment_key, coalesce(max(a.attempt_no),0)+1 FROM experiment e LEFT JOIN attempt a ON a.experiment_id=e.id WHERE e.id=?1 AND e.status='reserved'",
            [experiment_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        tx.execute(
            "INSERT INTO attempt(experiment_id,attempt_no,idempotency_key,lease_generation,deadline_ms,state,created_at_ms,updated_at_ms)
             VALUES(?1,?2,?3,?4,?5,'reserved',?6,?6)",
            params![experiment_id, next, format!("{key}:{next}"), generation, deadline_ms, now],
        )?;
        let attempt = tx.query_row(
            &format!("{ATTEMPT} WHERE experiment_id=?1 AND attempt_no=?2"),
            params![experiment_id, next],
            attempt_row,
        )?;
        tx.commit()?;
        Ok(attempt)
    }

    /// Commit how an attempt ended. A score commits the verdict, the idea's
    /// status and, on a keep, the cell incumbent in the same transaction.
    pub fn finish(
        &mut self,
        attempt_id: i64,
        generation: i64,
        outcome: &Outcome,
        now: i64,
    ) -> Result<bool> {
        if !self.generation_current(generation)? {
            return Ok(false);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(attempt) = tx
            .query_row(&format!("{ATTEMPT} WHERE id=?1"), [attempt_id], attempt_row)
            .optional()?
        else {
            return Err(Error("research_attempt_unknown"));
        };
        if !matches!(attempt.state.as_str(), "reserved" | "running") {
            return Ok(false);
        }
        let experiment = tx.query_row(
            &format!("{EXPERIMENT} WHERE id=?1"),
            [attempt.experiment_id],
            experiment_row,
        )?;
        let (attempt_state, failure) = match outcome {
            Outcome::Scored { .. } => ("succeeded", None),
            Outcome::Invalid { reason } => ("invalid", Some(reason.as_str())),
            Outcome::Failed { reason } => ("failed", Some(reason.as_str())),
            Outcome::Preempted { reason } => ("preempted", Some(reason.as_str())),
        };
        tx.execute(
            "UPDATE attempt SET state=?2, failure=?3, updated_at_ms=?4 WHERE id=?1",
            params![attempt_id, attempt_state, failure, now],
        )?;
        match outcome {
            Outcome::Scored {
                score,
                decision,
                report_blob,
            } => {
                tx.execute(
                    "UPDATE experiment SET status='scored', score=?2, verdict=?3, reason=?4, report_blob=?5, updated_at_ms=?6 WHERE id=?1",
                    params![
                        experiment.id,
                        serde_json::to_string(score)?,
                        decision.verdict.as_str(),
                        decision.reason,
                        report_blob,
                        now
                    ],
                )?;
                let kept = decision.verdict == nivra_core::autoresearch::Verdict::Keep;
                tx.execute(
                    "UPDATE idea SET status=?2 WHERE id=?1",
                    params![experiment.idea_id, if kept { "kept" } else { "rejected" }],
                )?;
                if kept {
                    tx.execute(
                        "INSERT INTO cell(campaign_id,segment,family,regime,incumbent_experiment_id)
                         SELECT i.campaign_id, c.segment, i.family, i.regime, ?2 FROM idea i JOIN campaign c ON c.id=i.campaign_id WHERE i.id=?1
                         ON CONFLICT(campaign_id,segment,family,regime) DO UPDATE SET incumbent_experiment_id=excluded.incumbent_experiment_id",
                        params![experiment.idea_id, experiment.id],
                    )?;
                }
            }
            Outcome::Invalid { reason } => {
                tx.execute(
                    "UPDATE experiment SET status='invalid', reason=?2, updated_at_ms=?3 WHERE id=?1",
                    params![experiment.id, reason, now],
                )?;
                tx.execute(
                    "UPDATE idea SET status=CASE WHEN repairs=0 THEN 'invalid' ELSE 'exhausted' END WHERE id=?1",
                    [experiment.idea_id],
                )?;
            }
            Outcome::Failed { .. } | Outcome::Preempted { .. } => {}
        }
        tx.commit()?;
        Ok(true)
    }

    /// Give up on a frozen experiment whose attempts keep failing. The trial
    /// stays counted.
    pub fn abandon(&mut self, experiment_id: i64, reason: &str, now: i64) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute(
            "UPDATE experiment SET status='failed', reason=?2, interpreted=1, updated_at_ms=?3 WHERE id=?1 AND status='reserved'",
            params![experiment_id, reason, now],
        )?;
        tx.execute(
            "UPDATE idea SET status='exhausted' WHERE id=(SELECT idea_id FROM experiment WHERE id=?1)",
            [experiment_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The campaign's experiment that has not reached a verdict, if any.
    pub fn unfinished(&self, campaign_id: i64) -> Result<Option<(Experiment, Attempt)>> {
        let Some(experiment) = self
            .db
            .query_row(
                &format!(
                    "{EXPERIMENT} WHERE campaign_id=?1 AND status='reserved' ORDER BY id LIMIT 1"
                ),
                [campaign_id],
                experiment_row,
            )
            .optional()?
        else {
            return Ok(None);
        };
        let attempt = self
            .attempt(experiment.id)?
            .ok_or(Error("research_attempt_missing"))?;
        Ok(Some((experiment, attempt)))
    }

    /// The oldest verdict no interpretation turn has seen yet.
    pub fn awaiting_interpretation(&self, campaign_id: i64) -> Result<Option<Experiment>> {
        Ok(self
            .db
            .query_row(
                &format!("{EXPERIMENT} WHERE campaign_id=?1 AND status IN ('scored','invalid') AND interpreted=0 ORDER BY id LIMIT 1"),
                [campaign_id],
                experiment_row,
            )
            .optional()?)
    }

    pub fn mark_interpreted(&mut self, experiment_id: i64) -> Result<()> {
        self.db.execute(
            "UPDATE experiment SET interpreted=1 WHERE id=?1",
            [experiment_id],
        )?;
        Ok(())
    }

    /// Failed attempts since `since_ms` and since the last attempt that ran
    /// to an end.
    pub fn consecutive_failures(&self, since_ms: i64) -> Result<u32> {
        let mut statement = self
            .db
            .prepare("SELECT state FROM attempt WHERE state NOT IN ('reserved','running','preempted') AND updated_at_ms >= ?1 ORDER BY updated_at_ms DESC, id DESC LIMIT 16")?;
        let states = statement.query_map([since_ms], |row| row.get::<_, String>(0))?;
        let mut count = 0;
        for state in states {
            if state? != "failed" {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    /// Scored non-improvements since the family's last keep.
    pub fn family_stall(&self, campaign_id: i64, family: &str) -> Result<u32> {
        let mut statement = self.db.prepare(
            "SELECT e.verdict FROM experiment e JOIN idea i ON i.id=e.idea_id
             WHERE e.campaign_id=?1 AND i.family=?2 AND e.status='scored' ORDER BY e.id DESC LIMIT 16",
        )?;
        let verdicts =
            statement.query_map(params![campaign_id, family], |row| row.get::<_, String>(0))?;
        let mut count = 0;
        for verdict in verdicts {
            if verdict? == "keep" {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    /// An immutable finding about one experiment. It never sets a score.
    pub fn record_finding(&mut self, new: &NewFinding<'_>) -> Result<Finding> {
        let tx = self.db.transaction()?;
        let (idea_id, evidence): (i64, Option<String>) = tx
            .query_row(
                "SELECT idea_id, coalesce(report_blob, source_blob) FROM experiment WHERE id=?1",
                [new.experiment_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(Error("research_experiment_unknown"))?;
        tx.execute(
            "INSERT INTO finding(idea_id,experiment_id,kind,claim,scope,counterevidence,next_test,evidence_digest,created_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                idea_id,
                new.experiment_id,
                new.kind,
                new.claim,
                new.scope,
                new.counterevidence,
                new.next_test,
                evidence,
                new.now
            ],
        )?;
        let id = tx.last_insert_rowid();
        let finding = tx.query_row(&format!("{FINDING} WHERE id=?1"), [id], finding_row)?;
        tx.commit()?;
        Ok(finding)
    }

    pub fn finding(&self, id: i64) -> Result<Option<Finding>> {
        Ok(self
            .db
            .query_row(&format!("{FINDING} WHERE id=?1"), [id], finding_row)
            .optional()?)
    }

    pub fn cells(&self, campaign_id: i64) -> Result<Vec<Cell>> {
        let mut statement = self.db.prepare(
            "SELECT segment,family,regime,incumbent_experiment_id FROM cell WHERE campaign_id=?1 ORDER BY family, regime",
        )?;
        let rows = statement.query_map([campaign_id], |row| {
            Ok(Cell {
                segment: row.get(0)?,
                family: row.get(1)?,
                regime: row.get(2)?,
                incumbent_experiment_id: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Newest-first prior ideas, experiments (losers included), findings,
    /// incumbents and approved procedures, bounded and paged.
    pub fn history(&self, query: &HistoryQuery<'_>) -> Result<History> {
        let limit = query.limit.clamp(1, 50) as i64;
        let before = query.before_id.unwrap_or(i64::MAX);
        let mechanism = query.mechanism.map(mechanism_key);
        let mut statement = self.db.prepare(&format!(
            "{IDEA} WHERE (?1 IS NULL OR campaign_id=?1) AND (?2 IS NULL OR family=?2)
             AND (?3 IS NULL OR mechanism_key=?3) AND (?4 IS NULL OR parent_experiment_id=?4)
             AND id < ?5 ORDER BY id DESC LIMIT ?6"
        ))?;
        let ideas: Vec<Idea> = statement
            .query_map(
                params![
                    query.campaign_id,
                    query.family,
                    mechanism,
                    query.parent_experiment_id,
                    before,
                    limit
                ],
                idea_row,
            )?
            .collect::<rusqlite::Result<_>>()?;
        let mut history = History {
            next_before_id: (ideas.len() as i64 == limit)
                .then(|| ideas.last().map(|idea| idea.id))
                .flatten(),
            ..History::default()
        };
        for idea in &ideas {
            let mut statement = self.db.prepare(&format!(
                "{EXPERIMENT} WHERE idea_id=?1 ORDER BY id DESC LIMIT 4"
            ))?;
            history.experiments.extend(
                statement
                    .query_map([idea.id], experiment_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
            let mut statement = self.db.prepare(&format!(
                "{FINDING} WHERE idea_id=?1 ORDER BY id DESC LIMIT 4"
            ))?;
            history.findings.extend(
                statement
                    .query_map([idea.id], finding_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
        history.ideas = ideas;
        if let Some(campaign) = query.campaign_id {
            history.incumbents = self.cells(campaign)?;
        }
        history.skills = self.skills(Some("approved"))?;
        Ok(history)
    }

    /// Stage a learned skill draft for review. A repeat digest is a no-op.
    pub fn propose_skill(
        &mut self,
        name: &str,
        bytes: &[u8],
        finding_ids: &[i64],
        now: i64,
    ) -> Result<(SkillRevision, bool)> {
        let digest = self.put_object(bytes)?;
        let parent: Option<String> = self
            .db
            .query_row(
                "SELECT digest FROM skill_revision WHERE name=?1 AND status='approved'",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        let inserted = self.db.execute(
            "INSERT OR IGNORE INTO skill_revision(name,digest,parent_digest,finding_ids,status,created_at_ms) VALUES(?1,?2,?3,?4,'proposed',?5)",
            params![name, digest, parent, serde_json::to_string(finding_ids)?, now],
        )? == 1;
        let skill = self.db.query_row(
            &format!("{SKILL} WHERE name=?1 AND digest=?2"),
            params![name, digest],
            skill_row,
        )?;
        Ok((skill, inserted))
    }

    /// Skill revisions, optionally in one status, oldest first.
    pub fn skills(&self, status: Option<&str>) -> Result<Vec<SkillRevision>> {
        let mut statement = self.db.prepare(&format!(
            "{SKILL} WHERE (?1 IS NULL OR status=?1) ORDER BY id"
        ))?;
        Ok(statement
            .query_map([status], skill_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Record a review decision. Approval retires the name's previous active
    /// revision, so one revision per name stays active.
    pub fn review_skill(
        &mut self,
        name: &str,
        digest: &str,
        approve: bool,
        reason: &str,
        reviewer: &str,
        now: i64,
    ) -> Result<SkillRevision> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM skill_revision WHERE name=?1 AND digest=?2",
                params![name, digest],
                |row| row.get(0),
            )
            .optional()?;
        if status.as_deref() != Some("proposed") {
            return Err(Error("research_skill_not_proposed"));
        }
        if approve {
            tx.execute(
                "UPDATE skill_revision SET status='retired', retired_at_ms=?2 WHERE name=?1 AND status='approved'",
                params![name, now],
            )?;
        }
        tx.execute(
            "UPDATE skill_revision SET status=?3, decision_reason=?4, reviewer=?5, active_from_ms=CASE WHEN ?3='approved' THEN ?6 END
             WHERE name=?1 AND digest=?2",
            params![
                name,
                digest,
                if approve { "approved" } else { "rejected" },
                reason,
                reviewer,
                now
            ],
        )?;
        let skill = tx.query_row(
            &format!("{SKILL} WHERE name=?1 AND digest=?2"),
            params![name, digest],
            skill_row,
        )?;
        tx.commit()?;
        Ok(skill)
    }

    /// What `day` has used. Unsettled turn reservations count at their
    /// reserved worst case.
    pub fn day_spend(&self, day: NaiveDate) -> Result<DaySpend> {
        day_spend(&self.db, day)
    }

    /// Reserve one turn's worst-case calls and cost before the provider is
    /// asked. `Ok(Err(reason))` means today's ceilings refuse it.
    pub fn reserve_turn(
        &mut self,
        key: &str,
        day: NaiveDate,
        calls: u64,
        usd_micros: i64,
        limits: &DayLimits,
        now: i64,
    ) -> Result<std::result::Result<(), &'static str>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let spent = day_spend(&tx, day)?;
        if let Some(reason) = limits.turn_refusal(&spent, calls, usd_micros) {
            return Ok(Err(reason));
        }
        tx.execute(
            "INSERT INTO reservation(key,day,kind,calls,usd_micros,settled,created_at_ms) VALUES(?1,?2,'turn',?3,?4,0,?5)",
            params![key, day.to_string(), calls as i64, usd_micros, now],
        )?;
        tx.commit()?;
        Ok(Ok(()))
    }

    /// Replace a turn reservation with its actual usage. Settling twice is a
    /// no-op, so one call is charged once.
    pub fn settle_turn(&mut self, key: &str, calls: u64, usd_micros: i64) -> Result<bool> {
        Ok(self.db.execute(
            "UPDATE reservation SET calls=?2, usd_micros=?3, settled=1 WHERE key=?1 AND kind='turn' AND settled=0",
            params![key, calls as i64, usd_micros],
        )? == 1)
    }

    /// Settle every reservation a dead turn left open at its worst case.
    pub fn settle_abandoned_turns(&mut self, except: Option<&str>) -> Result<usize> {
        Ok(self.db.execute(
            "UPDATE reservation SET settled=1 WHERE kind='turn' AND settled=0 AND key IS NOT ?1",
            [except],
        )?)
    }

    /// Take or renew the named lease. A live holder keeps it even past its
    /// expiry; a dead holder's lease is reclaimed with a new generation.
    pub fn acquire_lease(
        &mut self,
        name: &str,
        owner: &str,
        pid: u32,
        now: i64,
        ttl_ms: i64,
        alive: &dyn Fn(u32) -> bool,
    ) -> Result<Option<Lease>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(String, u32, i64, i64)> = tx
            .query_row(
                "SELECT owner,pid,generation,expires_at_ms FROM lease WHERE name=?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let generation = match current {
            None => 1,
            Some((held, _, generation, _)) if held == owner => generation,
            Some((_, held_pid, generation, expires))
                if held_pid == 0 || (now >= expires && held_pid != pid && !alive(held_pid)) =>
            {
                generation + 1
            }
            Some(_) => return Ok(None),
        };
        let expires_at_ms = now.saturating_add(ttl_ms);
        tx.execute(
            "INSERT INTO lease(name,owner,pid,generation,expires_at_ms) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(name) DO UPDATE SET owner=excluded.owner, pid=excluded.pid, generation=excluded.generation, expires_at_ms=excluded.expires_at_ms",
            params![name, owner, pid, generation, expires_at_ms],
        )?;
        tx.commit()?;
        Ok(Some(Lease {
            owner: owner.to_owned(),
            generation,
            expires_at_ms,
        }))
    }

    /// Give the lease back between steps so the other entry point can take
    /// it. The next holder gets a new generation.
    pub fn release_lease(&mut self, name: &str, owner: &str) -> Result<()> {
        self.db.execute(
            "UPDATE lease SET expires_at_ms=0, pid=0 WHERE name=?1 AND owner=?2",
            params![name, owner],
        )?;
        Ok(())
    }

    fn generation_current(&self, generation: i64) -> Result<bool> {
        let current: Option<i64> = self
            .db
            .query_row(
                "SELECT generation FROM lease WHERE name='coordinator'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(current.is_none_or(|current| current == generation))
    }

    /// The loop's last published state, for the dashboard.
    pub fn loop_state(&self) -> Result<Option<Value>> {
        let body: Option<String> = self
            .db
            .query_row("SELECT body FROM loop_state WHERE id=1", [], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(body.map(|text| serde_json::from_str(&text)).transpose()?)
    }

    pub fn set_loop_state(&mut self, body: &Value) -> Result<()> {
        self.db.execute(
            "INSERT INTO loop_state(id,body) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
            [serde_json::to_string(body)?],
        )?;
        Ok(())
    }

    /// Record that `sha256` was imported to `destination`; `false` when it
    /// already was.
    pub fn record_import(
        &mut self,
        sha256: &str,
        destination: &str,
        origin: &str,
        now: i64,
    ) -> Result<bool> {
        Ok(self.db.execute(
            "INSERT OR IGNORE INTO imported(sha256,destination,origin,at_ms) VALUES(?1,?2,?3,?4)",
            params![sha256, destination, origin, now],
        )? == 1)
    }

    pub fn imported(&self, sha256: &str) -> Result<bool> {
        Ok(self
            .db
            .query_row(
                "SELECT 1 FROM imported WHERE sha256=?1 LIMIT 1",
                [sha256],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// The latest experiments in a campaign, newest first.
    pub fn recent_experiments(&self, campaign_id: i64, limit: usize) -> Result<Vec<Experiment>> {
        let mut statement = self.db.prepare(&format!(
            "{EXPERIMENT} WHERE campaign_id=?1 ORDER BY id DESC LIMIT ?2"
        ))?;
        Ok(statement
            .query_map(params![campaign_id, limit as i64], experiment_row)?
            .collect::<rusqlite::Result<_>>()?)
    }
}

fn day_spend(db: &Connection, day: NaiveDate) -> Result<DaySpend> {
    Ok(db.query_row(
        "SELECT coalesce(sum(kind='variant'),0), coalesce(sum(kind='turn'),0), coalesce(sum(calls),0), coalesce(sum(usd_micros),0)
         FROM reservation WHERE day=?1",
        [day.to_string()],
        |row| {
            Ok(DaySpend {
                variants: row.get(0)?,
                turns: row.get(1)?,
                calls: row.get::<_, i64>(2)? as u64,
                usd_micros: row.get(3)?,
            })
        },
    )?)
}
