//! The record's typed rows and the readers that map SQLite rows onto them.
use super::{Error, Result};
use chrono::NaiveDate;
use nivra_core::autoresearch::{DayLimits, Decision, Regime, Score, Stage};
use serde::Serialize;

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

pub(super) fn date(text: String) -> rusqlite::Result<NaiveDate> {
    text.parse().map_err(|_| rusqlite::Error::InvalidQuery)
}

pub(super) fn campaign_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Campaign> {
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
pub(super) const CAMPAIGN: &str = "SELECT id,name,segment,manifest_digest,development_from,development_to,selection_from,trial_cap,status,created_at_ms FROM campaign";

pub(super) fn idea_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Idea> {
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
pub(super) const IDEA: &str = "SELECT id,campaign_id,family,regime,mechanism_key,hypothesis,falsifier,comparator,parent_experiment_id,created_at_ms,status,repairs FROM idea";

pub(super) fn experiment_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Experiment> {
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
pub(super) const EXPERIMENT: &str = "SELECT id,campaign_id,idea_id,parent_id,stage,source_path,source_blob,params_blob,manifest_blob,experiment_key,ordinal,status,score,verdict,reason,report_blob,interpreted,created_at_ms FROM experiment";

pub(super) fn attempt_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Attempt> {
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
pub(super) const ATTEMPT: &str = "SELECT id,experiment_id,attempt_no,idempotency_key,job_id,job_revision,lease_generation,deadline_ms,state,failure FROM attempt";

pub(super) fn finding_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Finding> {
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
pub(super) const FINDING: &str = "SELECT id,idea_id,experiment_id,kind,claim,scope,counterevidence,next_test,evidence_digest,created_at_ms FROM finding";

pub(super) fn skill_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillRevision> {
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
pub(super) const SKILL: &str = "SELECT name,digest,parent_digest,finding_ids,status,decision_reason,reviewer,created_at_ms FROM skill_revision";
