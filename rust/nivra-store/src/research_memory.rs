//! The autonomous research record: `research/research.sqlite` under the
//! sessions root, outside every dated book, plus immutable blobs under
//! `research/objects/sha256/`. SQLite owns ideas, experiments, findings,
//! skills, leases and budgets. The job queue owns run status; TimescaleDB
//! owns market data. Nothing here holds bulk quotes or account payloads.
use nivra_core::autoresearch::{DayLimits, DaySpend, mechanism_key};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

mod control;
mod objects;
mod records;
pub use objects::{BackupReport, verify};
pub use records::*;
use records::{
    ATTEMPT, CAMPAIGN, EXPERIMENT, FINDING, SKILL, attempt_row, campaign_row, experiment_row,
    finding_row, idea_row, skill_row,
};
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

const SCHEMA: &str = include_str!("research_memory/schema.sql");

/// The open research record.
pub struct Store {
    root: PathBuf,
    db: Connection,
}

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
        let spent = control::day_spend(&tx, s.day)?;
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
