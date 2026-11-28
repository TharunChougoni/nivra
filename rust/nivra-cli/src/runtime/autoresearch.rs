//! The autonomous research loop. Rust keeps the experiment log and the
//! clock; pi gets one falsifiable hypothesis and one editable strategy per
//! turn. The record lives in `<sessions_root>/research/research.sqlite`, the
//! drafts in `<sessions_root>/research/workspace/`, both outside every book.
use super::planner::PlannerConfig;
use super::{Config, Result, rollover};
use nivra_core::autoresearch::{Regime, Stage, slug_ok};
use nivra_store::research_memory::{HistoryQuery, NewFinding, NewIdea, Store};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

pub(crate) mod cli;
pub(crate) mod coordinator;
pub(crate) mod jobs;
mod settings;
mod skills;
#[cfg(test)]
mod tests;

pub(crate) use coordinator::{Step, StepReport, status, step};
pub(crate) use jobs::QueueJobs;
pub(crate) use settings::AutoresearchSettings;

/// The research segment every Phase 2 campaign belongs to.
pub(crate) const SEGMENT: &str = "nse_eq";
/// The one lease both entry points share.
pub(crate) const LEASE: &str = "coordinator";

/// `<sessions_root>/research`.
pub(crate) fn research_root(c: &Config) -> Result<PathBuf> {
    Ok(rollover::root(c)?.join("research"))
}

pub(crate) fn open(c: &Config) -> Result<Store> {
    Ok(Store::open(&research_root(c)?)?)
}

/// The loop's settings when the owner switched it on.
pub(crate) fn enabled(cfg: &PlannerConfig) -> Option<&AutoresearchSettings> {
    cfg.autoresearch
        .as_ref()
        .filter(|settings| settings.enabled)
}

/// Which step of the loop an autonomous turn serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnKind {
    /// Record one idea, write one strategy, submit it.
    Proposal,
    /// Read one verdict, record findings, repair an invalid source once.
    Interpretation,
    /// Approve or reject learned skill drafts. Runs on the review lane.
    SkillReview,
}

impl TurnKind {
    /// The tools the turn may call: pi name, then the Rust executor. `None`
    /// is answered by the lane host itself.
    pub(crate) fn tools(self) -> &'static [(&'static str, Option<&'static str>)] {
        match self {
            Self::Proposal => &[
                ("research_history", Some("research_history")),
                ("idea_record", Some("idea_record")),
                ("list", Some("workspace_list")),
                ("read", Some("workspace_read")),
                ("write", Some("workspace_write")),
                ("run", Some("workspace_run")),
                ("backtest", Some("experiment_submit")),
            ],
            Self::Interpretation => &[
                ("research_history", Some("research_history")),
                ("finding_record", Some("finding_record")),
                ("list", Some("workspace_list")),
                ("read", Some("workspace_read")),
                ("write", Some("workspace_write")),
                ("backtest", Some("experiment_submit")),
            ],
            Self::SkillReview => &[
                ("research_history", Some("research_history")),
                ("skill_verdict", None),
            ],
        }
    }
}

/// What an autonomous turn holds while it runs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AutoGrant {
    pub generation: i64,
    pub kind: TurnKind,
}

fn function(name: &str, description: &str, properties: Value, required: Value) -> Value {
    json!({"type":"function","name":name,"description":description,"strict":true,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
}

/// The memory tools every research turn sees, and the experiment submit the
/// autonomous turns publish as `backtest`.
pub(crate) fn tool_definitions() -> Vec<Value> {
    let id = || json!({"type":["integer","null"],"minimum":1});
    let text = |max: u32| json!({"type":"string","minLength":1,"maxLength":max});
    vec![
        function(
            "research_history",
            "Read the research record that persists across sessions: prior ideas with their status, experiments including losers, verdicts and reasons, findings, cell incumbents, approved procedures and the trial allowance. Filter by family, mechanism or parent; page with before_id. Read only.",
            json!({"family":{"type":["string","null"],"maxLength":32},"mechanism":{"type":["string","null"],"maxLength":300},
                "parent_experiment_id":id(),"before_id":id(),"limit":{"type":["integer","null"],"minimum":1,"maximum":20}}),
            json!([
                "family",
                "mechanism",
                "parent_experiment_id",
                "before_id",
                "limit"
            ]),
        ),
        function(
            "idea_record",
            "Record one falsifiable idea in the active campaign before you write code: the mechanism, what would falsify it, the family, the regime it expects and an optional parent experiment to mutate. Returns the idea id, similar earlier attempts and the remaining trial allowance. A repeat of an open idea returns the earlier one.",
            json!({"family":{"type":"string","minLength":1,"maxLength":32,"description":"Lowercase slug, for example trend, reversal or breakout."},
                "regime":{"type":"string","enum":["trend","range","unknown"]},
                "mechanism":text(300),"hypothesis":text(1000),"falsifier":text(500),
                "comparator":{"type":"string","enum":["cash","parent"]},
                "parent_experiment_id":id()}),
            json!([
                "family",
                "regime",
                "mechanism",
                "hypothesis",
                "falsifier",
                "comparator",
                "parent_experiment_id"
            ]),
        ),
        function(
            "finding_record",
            "Record one immutable finding about a scored or invalid experiment: what you observed, where it holds, its limitation and the next test. Negative and inconclusive findings are as useful as positive ones. It never sets a score or approves a skill.",
            json!({"experiment_id":{"type":"integer","minimum":1},
                "kind":{"type":"string","enum":["positive","negative","inconclusive","procedure"]},
                "observation":text(600),"scope":text(300),"limitation":text(300),"next_test":text(300)}),
            json!([
                "experiment_id",
                "kind",
                "observation",
                "scope",
                "limitation",
                "next_test"
            ]),
        ),
        function(
            "experiment_submit",
            "Submit one recorded idea for evaluation. Give the idea id, the workspace script path, its params and the stage. The host freezes the campaign's dates, universe, ₹5,000 capital, costs and comparator, reserves one trial and returns the experiment id. stage=bars queues one bounded archive job whose verdict reaches the next turn; stage=quote_replay replays recorded quotes inside this call and returns the verdict. An identical submission returns its earlier record and spends nothing.",
            json!({"idea_id":{"type":"integer","minimum":1},
                "source_path":{"type":"string","minLength":1,"maxLength":256,"description":"scripts/<name>.py in the research workspace."},
                "params":{"type":"string","minLength":2,"maxLength":65536,"description":"A JSON object encoded as a string."},
                "stage":{"type":"string","enum":["bars","quote_replay"]}}),
            json!(["idea_id", "source_path", "params", "stage"]),
        ),
    ]
}

/// The review lane's typed answer for one learned skill draft.
pub(crate) fn skill_verdict_definition() -> Value {
    json!({
        "name":"skill_verdict",
        "description":"Approve or reject one learned skill draft by name and digest. Approve a reproducible procedure that worked on two distinct experiments and stays inside the research tools. Reject a trading-performance claim dressed as a procedure, a draft that conflicts with recorded evidence or one that asks for tools research does not have.",
        "parameters":{"type":"object","properties":{
            "name":{"type":"string","minLength":1,"maxLength":64},
            "digest":{"type":"string","minLength":64,"maxLength":64},
            "approve":{"type":"boolean"},
            "reason":{"type":"string","minLength":1,"maxLength":400},
            "finding_ids":{"type":"array","maxItems":16,"items":{"type":"integer","minimum":1}}
        },"required":["name","digest","approve","reason","finding_ids"],"additionalProperties":false}
    })
}

/// A parsed memory or experiment tool call.
#[derive(Clone, Debug)]
pub(crate) enum MemoryOp {
    History(HistoryArgs),
    Idea(IdeaArgs),
    Finding(FindingArgs),
    Submit(SubmitArgs),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryArgs {
    family: Option<String>,
    mechanism: Option<String>,
    parent_experiment_id: Option<i64>,
    before_id: Option<i64>,
    limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdeaArgs {
    family: String,
    regime: String,
    mechanism: String,
    hypothesis: String,
    falsifier: String,
    comparator: String,
    parent_experiment_id: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FindingArgs {
    experiment_id: i64,
    kind: String,
    observation: String,
    scope: String,
    limitation: String,
    next_test: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubmitArgs {
    pub idea_id: i64,
    pub source_path: String,
    pub params: String,
    pub stage: String,
}

/// Bounded single-line or multi-line text free of credential shapes.
fn text_ok(text: &str, max: usize) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= max
        && !text.chars().any(|ch| ch.is_control() && ch != '\n')
        && super::transcript::redact(&json!(text)).as_str() == Some(text)
}

pub(crate) fn parse_tool(name: &str, args: Value) -> Result<MemoryOp> {
    let invalid = || -> Box<dyn std::error::Error> { "research_memory_args_invalid".into() };
    Ok(match name {
        "research_history" => {
            let a: HistoryArgs = serde_json::from_value(args).map_err(|_| invalid())?;
            if a.family.as_deref().is_some_and(|f| !slug_ok(f, 32))
                || a.mechanism.as_deref().is_some_and(|m| !text_ok(m, 300))
            {
                return Err(invalid());
            }
            MemoryOp::History(a)
        }
        "idea_record" => {
            let a: IdeaArgs = serde_json::from_value(args).map_err(|_| invalid())?;
            if !slug_ok(&a.family, 32)
                || Regime::parse(&a.regime).is_none()
                || !text_ok(&a.mechanism, 300)
                || !text_ok(&a.hypothesis, 1000)
                || !text_ok(&a.falsifier, 500)
                || !matches!(a.comparator.as_str(), "cash" | "parent")
                || (a.comparator == "parent") != a.parent_experiment_id.is_some()
            {
                return Err(invalid());
            }
            MemoryOp::Idea(a)
        }
        "finding_record" => {
            let a: FindingArgs = serde_json::from_value(args).map_err(|_| invalid())?;
            if !matches!(
                a.kind.as_str(),
                "positive" | "negative" | "inconclusive" | "procedure"
            ) || ![
                (&a.observation, 600),
                (&a.scope, 300),
                (&a.limitation, 300),
                (&a.next_test, 300),
            ]
            .iter()
            .all(|(text, max)| text_ok(text, *max))
            {
                return Err(invalid());
            }
            MemoryOp::Finding(a)
        }
        "experiment_submit" => {
            let a: SubmitArgs = serde_json::from_value(args).map_err(|_| invalid())?;
            if Stage::parse(&a.stage).is_none() {
                return Err(invalid());
            }
            MemoryOp::Submit(a)
        }
        _ => return Err("harness_tool_not_allowed".into()),
    })
}

/// The bounded allowance a turn sees beside every memory read.
fn allowance(cfg: &PlannerConfig, store: &Store, now: i64) -> Result<Value> {
    let Some(settings) = enabled(cfg) else {
        return Ok(Value::Null);
    };
    let campaign = store.campaign(&settings.campaign.name)?;
    let spent = store.day_spend(nivra_core::autoresearch::ist_day(now))?;
    let limits = settings.day_limits();
    Ok(json!({
        "campaign":campaign.as_ref().map(|c| &c.name),
        "trials_used":campaign.as_ref().map(|c| store.trials(c.id)).transpose()?,
        "trial_cap":campaign.as_ref().map(|c| c.trial_cap),
        "day_variants_left":limits.variants.saturating_sub(spent.variants),
        "day_turns_left":limits.turns.saturating_sub(spent.turns),
    }))
}

/// Run one memory or experiment tool for a research turn.
pub(crate) fn execute(
    c: &Config,
    cfg: &PlannerConfig,
    grant: Option<AutoGrant>,
    op: MemoryOp,
    now: i64,
    preempted: &dyn Fn() -> bool,
) -> Result<Value> {
    let settings = enabled(cfg).ok_or("autoresearch_disabled")?;
    let mut store = open(c)?;
    let campaign = store.campaign(&settings.campaign.name)?;
    match op {
        MemoryOp::History(a) => {
            let history = store.history(&HistoryQuery {
                campaign_id: campaign.as_ref().map(|c| c.id),
                family: a.family.as_deref(),
                mechanism: a.mechanism.as_deref(),
                parent_experiment_id: a.parent_experiment_id,
                before_id: a.before_id,
                limit: a.limit.unwrap_or(10).min(20),
            })?;
            Ok(
                json!({"history":history,"allowance":allowance(cfg, &store, now)?,
                "evidence_only":true,"execution_authorized":false}),
            )
        }
        MemoryOp::Idea(a) => {
            let campaign = campaign
                .filter(|c| c.status == "active")
                .ok_or("research_campaign_unavailable")?;
            if let Some(parent) = a.parent_experiment_id
                && store
                    .experiment(parent)?
                    .is_none_or(|e| e.campaign_id != campaign.id || e.status != "scored")
            {
                return Err("research_parent_unavailable".into());
            }
            let (idea, existing) = store.record_idea(&NewIdea {
                campaign_id: campaign.id,
                origin_request_key: None,
                family: &a.family,
                regime: Regime::parse(&a.regime).ok_or("research_memory_args_invalid")?,
                mechanism: &a.mechanism,
                hypothesis: &a.hypothesis,
                falsifier: &a.falsifier,
                comparator: &a.comparator,
                parent_experiment_id: a.parent_experiment_id,
                now,
            })?;
            let similar = store.history(&HistoryQuery {
                campaign_id: Some(campaign.id),
                family: Some(&a.family),
                mechanism: None,
                parent_experiment_id: None,
                before_id: Some(idea.id),
                limit: 5,
            })?;
            Ok(
                json!({"idea_id":idea.id,"existing":existing,"status":idea.status,
                "similar":{"ideas":similar.ideas,"experiments":similar.experiments},
                "allowance":allowance(cfg, &store, now)?}),
            )
        }
        MemoryOp::Finding(a) => {
            let experiment = store
                .experiment(a.experiment_id)?
                .ok_or("research_experiment_unknown")?;
            if !matches!(experiment.status.as_str(), "scored" | "invalid" | "failed") {
                return Err("research_experiment_not_finished".into());
            }
            let finding = store.record_finding(&NewFinding {
                experiment_id: a.experiment_id,
                kind: &a.kind,
                claim: &a.observation,
                scope: &a.scope,
                counterevidence: &a.limitation,
                next_test: &a.next_test,
                now,
            })?;
            Ok(json!({"finding_id":finding.id,"evidence_digest":finding.evidence_digest}))
        }
        MemoryOp::Submit(a) => {
            let grant = grant
                .filter(|g| g.kind != TurnKind::SkillReview)
                .ok_or("research_autonomous_turn_required")?;
            jobs::submit(c, cfg, settings, &mut store, grant, &a, now, preempted)
        }
    }
}
