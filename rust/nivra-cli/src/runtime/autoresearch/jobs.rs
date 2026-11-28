//! Frozen manifests, submission and the loop's own bounded job queue. The
//! model names an idea, a workspace script and params; everything else that
//! sets a score comes from the frozen campaign.
use super::{AutoGrant, AutoresearchSettings, SubmitArgs, research_root};
use crate::runtime::harness::research::{
    self as harness, CandidateComputeSettings, ResearchSettings,
};
use crate::runtime::planner::PlannerConfig;
use crate::runtime::workspace::{self, WorkspaceOp, WorkspaceRoot, WorkspaceScope};
use crate::runtime::{Config, Result, digest};
use chrono::NaiveDate;
use nivra_core::autoresearch::{ExperimentIdentity, Rule, Stage};

use nivra_store::market_archive::{self, ist_midnight};
use nivra_store::research_jobs as queue;
use nivra_store::research_memory::{Experiment, Reserved, Store, Submission};
use serde_json::{Value, json};
use std::path::{Component, PathBuf};

mod bars;
mod quotes;
#[cfg(test)]
pub(super) use bars::campaign_manifest;
pub(super) use bars::{BarManifest, admit_bar, bar_outcome, ensure_campaign, read_snapshot};
pub(super) use quotes::run_quote;
use quotes::{QuoteManifest, Translation};

/// Canonical JSON for hashing: keys sorted, compact. Floats are kept as the
/// evaluator receives them.
fn canonical(value: &Value) -> Result<Vec<u8>> {
    let mut value = value.clone();
    value.sort_all_objects();
    Ok(serde_json::to_vec(&value)?)
}

/// The research settings with the queue moved under the research root, so
/// loop jobs never mix with a book's own research tasks.
pub(super) fn loop_settings(
    c: &Config,
    cfg: &PlannerConfig,
) -> Result<(ResearchSettings, CandidateComputeSettings)> {
    let mut research = cfg
        .research_tools
        .clone()
        .ok_or("autoresearch_research_tools_missing")?;
    let candidate = research
        .candidate_compute
        .clone()
        .filter(|candidate| candidate.enabled)
        .ok_or("candidate_compute_disabled")?;
    research.queue = research_root(c)?.join("jobs.sqlite");
    Ok((research, candidate))
}

/// The queue task key of an experiment: stable, so a restart finds its job.
pub(super) fn task_key(experiment: &Experiment) -> String {
    format!("ar-{}", &experiment.experiment_key[..40])
}

/// The queue binding every loop job carries instead of a book's.
fn loop_binding() -> String {
    digest(b"nivra-autoresearch-loop")
}

/// The queue's session of a bar experiment: its campaign's last date.
fn loop_session(store: &Store, experiment: &Experiment) -> Result<String> {
    let manifest: BarManifest = serde_json::from_slice(&store.object(&experiment.manifest_blob)?)?;
    Ok(manifest.window.to.to_string())
}

/// The queue task id of a bar experiment's job.
pub(super) fn task_id(store: &Store, experiment: &Experiment) -> Result<String> {
    Ok(queue::scoped_task_id(
        &loop_binding(),
        &loop_session(store, experiment)?,
        &task_key(experiment),
    ))
}

fn read_script(c: &Config, cfg: &PlannerConfig, path: &str) -> Result<String> {
    let rel = workspace::normalize_rel(path)?;
    let mut parts = rel.components();
    let script = matches!(parts.next(), Some(Component::Normal(dir)) if dir == "scripts")
        && rel.extension().is_some_and(|ext| ext == "py")
        && parts.count() == 1;
    if !script {
        return Err("research_source_path_invalid".into());
    }
    let settings = cfg.workspace.as_ref().ok_or("harness_workspace_disabled")?;
    let read = workspace::execute_scoped_with_env(
        &WorkspaceRoot::research(c)?,
        settings,
        WorkspaceScope::ResearchSandbox,
        None,
        WorkspaceOp::Read(path.to_owned()),
    )?;
    Ok(read["text"]
        .as_str()
        .ok_or("research_source_unreadable")?
        .to_owned())
}

/// The keep rule's limits for a stage.
fn rule(cfg: &PlannerConfig, settings: &AutoresearchSettings, stage: Stage) -> Rule {
    Rule {
        max_drawdown_bps: settings.max_drawdown_bps.unwrap_or_else(|| {
            i64::from(
                cfg.strategy_acceptance
                    .clone()
                    .unwrap_or_default()
                    .max_drawdown_bps,
            )
        }),
        require_flat: matches!(stage, Stage::QuoteReplay),
    }
}

fn view(experiment: &Experiment) -> Value {
    json!({"experiment_id":experiment.id,"ordinal":experiment.ordinal,"stage":experiment.stage,
        "status":experiment.status,"verdict":experiment.verdict,"reason":experiment.reason,
        "score":experiment.score,"source_sha256":experiment.source_blob})
}

/// The `backtest` of an autonomous turn: freeze, reserve, then evaluate.
#[allow(clippy::too_many_arguments)]
pub(super) fn submit(
    c: &Config,
    cfg: &PlannerConfig,
    settings: &AutoresearchSettings,
    store: &mut Store,
    grant: AutoGrant,
    args: &SubmitArgs,
    now: i64,
    preempted: &dyn Fn() -> bool,
) -> Result<Value> {
    let stage = Stage::parse(&args.stage).ok_or("research_memory_args_invalid")?;
    if stage == Stage::OptionReplay {
        return Err("option_compare_required".into());
    }
    let campaign = store
        .campaign(&settings.campaign.name)?
        .filter(|campaign| campaign.status == "active")
        .ok_or("research_campaign_unavailable")?;
    let idea = store
        .idea(args.idea_id)?
        .filter(|idea| idea.campaign_id == campaign.id)
        .ok_or("research_idea_unknown")?;
    let (_, candidate) = loop_settings(c, cfg)?;
    let source = read_script(c, cfg, &args.source_path)?;
    if source.len() > candidate.max_source_bytes as usize
        || crate::runtime::transcript::redact(&json!(source)).as_str() != Some(source.as_str())
    {
        return Ok(json!({"status":"not_dispatched","reason":"candidate_source_invalid"}));
    }
    let params: Value =
        serde_json::from_str(&args.params).map_err(|_| "candidate_params_invalid")?;
    if !params.is_object() || args.params.len() > candidate.max_params_bytes as usize {
        return Ok(json!({"status":"not_dispatched","reason":"candidate_params_invalid"}));
    }
    let source_blob = store.put_object(source.as_bytes())?;
    let params_blob = store.put_object(&canonical(&params)?)?;
    let mut exposure = None;
    let (manifest, runner, abi) = match stage {
        Stage::OptionReplay => unreachable!("option rankings use option_compare"),
        Stage::Bars => {
            if store.holdout(campaign.id)?.is_some() {
                return Ok(json!({"status":"not_dispatched","reason":"research_finalist_frozen"}));
            }
            let mut manifest: BarManifest =
                serde_json::from_slice(&store.object(&campaign.manifest_digest)?)
                    .map_err(|_| "autoresearch_campaign_upgrade_required")?;
            if store.holdout_overlaps(manifest.window.from, manifest.window.to)? {
                return Ok(json!({"status":"not_dispatched","reason":"research_holdout_sealed"}));
            }
            let runner = harness::runtime_sha256()?;
            if manifest.runner_sha256.as_deref() != Some(runner.as_str()) {
                return Ok(
                    json!({"status":"not_dispatched","reason":"archive_bars_runner_changed"}),
                );
            }
            let snapshot = read_snapshot(c, &manifest.window)?;
            manifest.snapshot = Some(snapshot.clone());
            let bytes = canonical(&serde_json::to_value(&manifest)?)?;
            store.invalidate_old_bar_scores(campaign.id, &digest(&bytes), now)?;
            if !bars::coverage_complete(c, &manifest, &snapshot)? {
                return Ok(
                    json!({"status":"not_dispatched","reason":"archive_bars_warmup_or_fold_missing"}),
                );
            }
            let declared = |name: &str| {
                params
                    .get(name)
                    .map_or(Some(0), Value::as_u64)
                    .ok_or("archive_bars_horizon_invalid")
            };
            let warmup = snapshot
                .dates
                .partition_point(|day| ist_midnight(*day) < manifest.splits.selection_from_ms)
                .saturating_sub(manifest.embargo_sessions as usize);
            if declared("required_warmup_sessions")? > warmup as u64 {
                return Ok(
                    json!({"status":"not_dispatched","reason":"archive_bars_warmup_unavailable"}),
                );
            }
            if declared("label_horizon_sessions")? > u64::from(manifest.embargo_sessions) {
                return Ok(
                    json!({"status":"not_dispatched","reason":"archive_bars_embargo_too_short"}),
                );
            }
            (bytes, runner, nivra_research::archive_bars::ABI)
        }
        Stage::QuoteReplay => {
            if let Some(previous) = store
                .recent_experiments(campaign.id, campaign.trial_cap as usize)?
                .into_iter()
                .find(|e| {
                    e.stage == Stage::QuoteReplay
                        && e.idea_id == idea.id
                        && e.source_blob == source_blob
                        && e.params_blob == params_blob
                })
            {
                return Ok(json!({"status":"duplicate","experiment":view(&previous)}));
            }
            let parent = idea
                .parent_experiment_id
                .map(|id| store.experiment(id))
                .transpose()?
                .flatten();
            let translation = parent.as_ref().filter(|e| e.stage == Stage::Bars);
            if let Some(parent) = translation {
                if parent.source_blob == source_blob {
                    return Ok(
                        json!({"status":"not_dispatched","reason":"translation_source_reused"}),
                    );
                }
                if !store.holdout(campaign.id)?.is_some_and(|h| {
                    h.finalist == parent.id && h.verdict.as_deref() == Some("keep")
                }) {
                    return Ok(
                        json!({"status":"not_dispatched","reason":"translation_holdout_required"}),
                    );
                }
            }
            let Some(frozen_at) =
                store.freeze_quote_draft(idea.id, &source_blob, &params_blob, now)?
            else {
                return Ok(json!({"status":"not_dispatched","reason":"quote_draft_frozen"}));
            };
            let frozen_at = nivra_core::autoresearch::ist_day(frozen_at);
            let strategy = candidate
                .strategy
                .as_ref()
                .filter(|s| s.enabled)
                .ok_or("candidate_strategy_disabled")?;
            let window = match harness::RecentArchiveWindow::after(c, strategy, frozen_at, now)? {
                Some(window) => window,
                None => {
                    return Ok(
                        json!({"status":"not_dispatched","reason":"waiting_for_unseen_quote_tape"}),
                    );
                }
            };
            let from = window.sessions[0];
            let to = *window
                .sessions
                .last()
                .ok_or("candidate_archive_window_invalid")?;
            if window.sessions.len() < 3
                || from <= frozen_at
                || store.replay_exposed(from, to)?
                || store.holdout_overlaps(from, to)?
            {
                return Ok(
                    json!({"status":"not_dispatched","reason":"waiting_for_unseen_quote_tape"}),
                );
            }
            let archive = c.market_archive.as_ref().ok_or("archive_config_missing")?;
            let mut db = crate::runtime::market_archive::connect(archive)
                .map_err(|_| "archive_connection_failed")?;
            let receipt_id = market_archive::quote_watermark(&mut db)
                .map_err(|_| "archive_quote_read_failed")?;
            if receipt_id == 0 {
                return Ok(
                    json!({"status":"not_dispatched","reason":"waiting_for_unseen_quote_tape"}),
                );
            }
            let calendar = c
                .calendar_at(from)
                .map_err(|_| "candidate_archive_calendar_unavailable")?;
            for day in &window.sessions {
                let date = nivra_core::time::ExchangeDate::parse(&day.to_string())?;
                let (open, close) = calendar
                    .hours(date)?
                    .ok_or("candidate_archive_calendar_unavailable")?;
                let at = |time| {
                    chrono::DateTime::parse_from_rfc3339(&format!("{day}T{time}+05:30"))
                        .map(|t| t.timestamp_millis())
                };
                let (first, last) = (at(open)?, at(close)? - 1);
                let edge_ms = 300_000;
                let starts = market_archive::quote_receipt_ids(
                    &mut db,
                    first,
                    first + edge_ms,
                    receipt_id,
                    1,
                )?;
                let ends = market_archive::quote_receipt_ids(
                    &mut db,
                    last - edge_ms,
                    last,
                    receipt_id,
                    1,
                )?;
                if starts.is_empty() || ends.is_empty() {
                    return Ok(
                        json!({"status":"not_dispatched","reason":"waiting_for_complete_quote_tape"}),
                    );
                }
            }
            let worker = cfg.effective_strategy_worker();
            let runner = digest(&canonical(&json!({
                "worker":worker,
                "runner":crate::runtime::strategy::runner_sha256(),
                "adapter":crate::runtime::strategy::replay::adapter_sha256(),
                "engine":nivra_core::replay::engine_fingerprint(),
            }))?);
            let comparator = if let Some(origin) = translation {
                if let Some(parent_id) = origin.parent_id {
                    let parent = store
                        .experiment(parent_id)?
                        .ok_or("research_parent_unavailable")?;
                    let Some(action) =
                        store.accepted_quote_child(&parent.source_blob, &parent.params_blob)?
                    else {
                        return Ok(
                            json!({"status":"not_dispatched","reason":"quote_parent_version_unavailable"}),
                        );
                    };
                    Some(action.id)
                } else {
                    None
                }
            } else {
                parent
                    .as_ref()
                    .filter(|p| p.stage == Stage::QuoteReplay)
                    .map(|p| p.id)
            };
            let translation = if let Some(parent) = translation {
                let mut origin: BarManifest =
                    serde_json::from_slice(&store.object(&parent.manifest_blob)?)?;
                if origin.window.unit != market_archive::BarUnit::Minute {
                    return Ok(
                        json!({"status":"not_dispatched","reason":"translation_features_unavailable"}),
                    );
                }
                origin.window.to = to;
                Some(Translation {
                    origin_id: parent.id,
                    snapshot: read_snapshot(c, &origin.window)?,
                })
            } else {
                None
            };
            let manifest = QuoteManifest {
                protocol: nivra_core::autoresearch::PROTOCOL_VERSION,
                abi: crate::runtime::strategy::ABI.into(),
                dataset_id: harness::RECENT_ARCHIVE_DATASET_ID.into(),
                selection_from_ms: window.selection_from_ms,
                selection_to_ms: window.selection_to_ms,
                receipt_id,
                sessions: window.sessions,
                capital: c.sleeves.cash.capital,
                sizing: c.sleeves.cash.policy(),
                exit_policy: cfg.exit_policy.clone(),
                worker,
                costs: c.paper_costs.clone().ok_or("paper_costs_required")?,
                max_drawdown_bps: rule(cfg, settings, Stage::QuoteReplay).max_drawdown_bps,
                comparator: idea.comparator.clone(),
                translation,
                comparator_experiment_id: comparator,
            };
            exposure = Some((from, to));
            (
                canonical(&serde_json::to_value(&manifest)?)?,
                runner,
                crate::runtime::strategy::ABI,
            )
        }
    };
    let manifest_blob = store.put_object(&manifest)?;
    let key = ExperimentIdentity {
        stage,
        abi,
        source_sha256: &source_blob,
        params_sha256: &params_blob,
        manifest_sha256: &manifest_blob,
        runner_sha256: &runner,
    }
    .key();
    let reserved = store.reserve(&Submission {
        idea_id: idea.id,
        stage,
        quote_window: exposure,
        source_path: &args.source_path,
        source_blob: &source_blob,
        params_blob: &params_blob,
        manifest_blob: &manifest_blob,
        evaluator_digest: nivra_core::autoresearch::EVALUATOR,
        experiment_key: &key,
        lease_generation: grant.generation,
        deadline_ms: now.saturating_add(i64::from(candidate.wall_millis)),
        day: nivra_core::autoresearch::ist_day(now),
        limits: settings.day_limits(),
        now,
    })?;
    let (experiment, attempt) = match reserved {
        Reserved::Existing(experiment) => {
            return Ok(json!({"status":"duplicate","experiment":view(&experiment),
                "note":"An identical submission ran before; no trial was spent."}));
        }
        Reserved::Refused(reason) => {
            return Ok(json!({"status":"not_dispatched","reason":reason}));
        }
        Reserved::New(experiment, attempt) => (experiment, attempt),
    };
    match stage {
        Stage::OptionReplay => unreachable!("option rankings use option_compare"),
        Stage::Bars => match admit_bar(c, cfg, store, &experiment, &idea.hypothesis, now)? {
            Ok((task_id, revision)) => {
                store.bind_job(attempt.id, &task_id, revision, grant.generation, now)?;
                Ok(json!({"status":"queued","experiment":view(&experiment),
                    "next":"The host runs this job and brings its verdict to the next turn. End the turn now."}))
            }
            Err(value) => Ok(json!({"status":"reserved","experiment":view(&experiment),
                "queue":value,"next":"The host retries the frozen job. End the turn now."})),
        },
        Stage::QuoteReplay => {
            let (outcome, replay) = run_quote(c, cfg, store, &experiment, now, preempted)?;
            store.finish(attempt.id, grant.generation, &outcome, now)?;
            let done = store
                .experiment(experiment.id)?
                .ok_or("research_experiment_unknown")?;
            Ok(json!({"status":done.status,"experiment":view(&done),
                "acceptance":replay["acceptance"],"summary":replay["summary"],
                "reason":replay["reason"],"paper_only":true}))
        }
    }
}

/// How the loop's job ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum JobEnd {
    /// The verified report bytes.
    Report(Vec<u8>),
    Busy,
    /// The strategy failed, overran its wall limit or broke the ABI.
    Invalid(String),
    /// The worker or queue failed; the frozen job may run again.
    Failed(String),
    /// Stopped for a decision turn or host pressure.
    Stopped,
}

/// Drives one admitted job to its end. The controller and the timer use the
/// queue; tests substitute a scripted runner.
pub(crate) trait Jobs {
    fn run(&mut self, task_id: &str, revision: u32, stop: &dyn Fn() -> bool) -> Result<JobEnd>;
}

/// The loop's job queue under the research root.
pub(crate) struct QueueJobs {
    config: Config,
    research: ResearchSettings,
    root: PathBuf,
}

impl QueueJobs {
    pub(crate) fn new(c: &Config, cfg: &PlannerConfig) -> Result<Self> {
        Ok(Self {
            config: c.clone(),
            research: loop_settings(c, cfg)?.0,
            root: research_root(c)?,
        })
    }
}

impl Jobs for QueueJobs {
    fn run(&mut self, task_id: &str, revision: u32, stop: &dyn Fn() -> bool) -> Result<JobEnd> {
        let Some(_slot) = harness::compute_slot(&self.config)? else {
            return Ok(JobEnd::Busy);
        };
        if let Err(code) = harness::drive_task(
            self.research.clone(),
            self.root.clone(),
            task_id.to_owned(),
            revision,
            stop,
        ) {
            return Ok(JobEnd::Failed(code.into()));
        }
        let mut db = harness::open_queue(&self.research, &self.root, false)?;
        let record = queue::load_scoped(&mut db, task_id, revision)
            .map_err(|_| "research_task_unavailable")?;
        Ok(match record.status {
            queue::TaskState::Succeeded => {
                let verified =
                    nivra_research::worker::read_verified_result(&mut db, task_id, revision)
                        .ok()
                        .flatten();
                let report = verified.and_then(|verified| {
                    let (hash, chunks) = verified.result.observations.split_first()?;
                    let report = chunks.concat().into_bytes();
                    (digest(&report) == *hash).then_some(report)
                });
                match report {
                    Some(report) => JobEnd::Report(report),
                    None => JobEnd::Invalid("archive_bars_report_invalid".into()),
                }
            }
            queue::TaskState::Terminated if stop() => JobEnd::Stopped,
            queue::TaskState::Terminated => JobEnd::Invalid("archive_bars_run_failed".into()),
            queue::TaskState::Cancelled => JobEnd::Stopped,
            queue::TaskState::Refused => JobEnd::Failed("research_task_refused".into()),
            _ => JobEnd::Failed("research_task_not_terminal".into()),
        })
    }
}
