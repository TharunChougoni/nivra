//! Frozen manifests, submission and the loop's own bounded job queue. The
//! model names an idea, a workspace script and params; everything else that
//! sets a score comes from the frozen campaign.
use super::{AutoGrant, AutoresearchSettings, SubmitArgs, research_root};
use crate::runtime::harness::research::{
    self as harness, Admit, Admitted, BacktestArgs, CandidateComputeSettings, ResearchSettings,
};
use crate::runtime::planner::PlannerConfig;
use crate::runtime::workspace::{self, WorkspaceOp, WorkspaceRoot, WorkspaceScope};
use crate::runtime::{Config, Result, digest};
use chrono::{Duration, NaiveDate};
use nivra_core::autoresearch::{
    Comparator, Decision, ExperimentIdentity, RESEARCH_CAPITAL_RUPEES, Rule, Score, Stage, keep,
};
use nivra_core::finance::{Amount, FeeArithmetic};
use nivra_store::market_archive::{BarWindow, ist_midnight};
use nivra_store::research_jobs::{self as queue, BacktestWindows};
use nivra_store::research_memory::{
    Campaign, Experiment, NewCampaign, Outcome, Reserved, Store, Submission,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Component, PathBuf};

/// Every bar experiment of a campaign is scored on exactly these inputs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BarManifest {
    pub protocol: u32,
    pub abi: String,
    pub window: BarWindow,
    pub splits: BacktestWindows,
    pub capital: Amount,
    pub costs: FeeArithmetic,
    pub comparator: String,
    pub sandbox: nivra_sandbox::SandboxConfig,
}

/// A quote replay's frozen tape window, capital, costs and comparator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QuoteManifest {
    pub protocol: u32,
    pub abi: String,
    pub dataset_id: String,
    pub selection_from_ms: i64,
    pub selection_to_ms: i64,
    pub capital: Amount,
    pub costs: FeeArithmetic,
    pub comparator: String,
}

/// Canonical JSON for hashing: keys sorted, compact. Floats are kept as the
/// evaluator receives them.
fn canonical(value: &Value) -> Result<Vec<u8>> {
    let mut value = value.clone();
    value.sort_all_objects();
    Ok(serde_json::to_vec(&value)?)
}

/// The research capital per sleeve.
fn capital() -> Amount {
    Amount::from_integer(RESEARCH_CAPITAL_RUPEES)
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

/// The bar manifest a campaign ending on `to` freezes.
pub(super) fn campaign_manifest(
    c: &Config,
    cfg: &PlannerConfig,
    settings: &AutoresearchSettings,
    to: NaiveDate,
) -> Result<BarManifest> {
    let campaign = &settings.campaign;
    let from = to - Duration::days(i64::from(campaign.lookback_days));
    let selection_from = to - Duration::days(i64::from(campaign.selection_days));
    let (_, candidate) = loop_settings(c, cfg)?;
    let day_after = to.succ_opt().ok_or("autoresearch_campaign_invalid")?;
    Ok(BarManifest {
        protocol: nivra_core::autoresearch::PROTOCOL_VERSION,
        abi: nivra_research::archive_bars::ABI.into(),
        window: BarWindow {
            unit: campaign.unit,
            from,
            to,
            instruments: campaign.instruments.clone().map(|mut keys| {
                keys.sort_unstable();
                keys.dedup();
                keys
            }),
        },
        splits: BacktestWindows {
            training_from_ms: ist_midnight(from),
            training_to_ms: ist_midnight(selection_from) - 1,
            selection_from_ms: ist_midnight(selection_from),
            selection_to_ms: ist_midnight(day_after) - 1,
        },
        capital: capital(),
        costs: c.paper_costs.clone().ok_or("paper_costs_required")?,
        comparator: "cash".into(),
        sandbox: candidate.sandbox,
    })
}

/// The named campaign, created and frozen on first use.
pub(super) fn ensure_campaign(
    store: &mut Store,
    c: &Config,
    cfg: &PlannerConfig,
    settings: &AutoresearchSettings,
    to: NaiveDate,
    now: i64,
) -> Result<Campaign> {
    let manifest = campaign_manifest(c, cfg, settings, to)?;
    let bytes = canonical(&serde_json::to_value(&manifest)?)?;
    Ok(store.ensure_campaign(&NewCampaign {
        name: &settings.campaign.name,
        segment: super::SEGMENT,
        manifest: &bytes,
        development_from: manifest.window.from,
        development_to: manifest.window.to,
        selection_from: to - Duration::days(i64::from(settings.campaign.selection_days)),
        trial_cap: settings.campaign.trial_cap,
        now,
    })?)
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
        require_flat: stage == Stage::QuoteReplay,
    }
}

/// Compare a score with cash, the pinned parent and the cell incumbent,
/// each only when it was scored on the same frozen manifest.
pub(super) fn judge(
    store: &Store,
    cfg: &PlannerConfig,
    settings: &AutoresearchSettings,
    experiment: &Experiment,
    score: &Score,
) -> Result<Decision> {
    let mut comparators = vec![Comparator::Cash];
    let idea = store
        .idea(experiment.idea_id)?
        .ok_or("research_idea_unknown")?;
    let incumbent = store
        .cells(experiment.campaign_id)?
        .into_iter()
        .find(|cell| cell.family == idea.family && cell.regime == idea.regime.as_str())
        .map(|cell| cell.incumbent_experiment_id);
    for id in experiment.parent_id.into_iter().chain(incumbent) {
        if let Some(other) = store.experiment(id)?
            && other.id != experiment.id
            && other.manifest_blob == experiment.manifest_blob
            && let Some(score) = other.score
            && !comparators
                .iter()
                .any(|c| matches!(c, Comparator::Experiment { id, .. } if *id == other.id))
        {
            comparators.push(Comparator::Experiment {
                id: other.id,
                score,
            });
        }
    }
    Ok(keep(
        score,
        &comparators,
        rule(cfg, settings, experiment.stage),
    ))
}

/// The test-split score of a verified bar report.
pub(crate) fn bar_score(report: &[u8]) -> Result<Score> {
    #[derive(Deserialize)]
    struct Split {
        mark_to_market_pnl: Amount,
        max_drawdown_bps: i64,
        closed_trades: u64,
        open_positions: u64,
    }
    #[derive(Deserialize)]
    struct Report {
        test: Split,
    }
    let report: Report =
        serde_json::from_slice(report).map_err(|_| "archive_bars_report_invalid")?;
    Ok(Score {
        net_pnl: report.test.mark_to_market_pnl,
        max_drawdown_bps: report.test.max_drawdown_bps,
        closed_trades: report.test.closed_trades,
        open_positions: report.test.open_positions,
    })
}

/// Score a verified bar report and build the outcome to commit.
pub(super) fn bar_outcome(
    store: &Store,
    cfg: &PlannerConfig,
    settings: &AutoresearchSettings,
    experiment: &Experiment,
    report: &[u8],
) -> Result<Outcome> {
    let score = match bar_score(report) {
        Ok(score) => score,
        Err(_) => {
            return Ok(Outcome::Invalid {
                reason: "archive_bars_report_invalid".into(),
            });
        }
    };
    let decision = judge(store, cfg, settings, experiment, &score)?;
    Ok(Outcome::Scored {
        report_blob: store.put_object(report)?,
        score,
        decision,
    })
}

/// Admit an experiment's frozen bar job to the loop queue. Admitting the same
/// experiment again returns its existing job.
pub(super) fn admit_bar(
    c: &Config,
    cfg: &PlannerConfig,
    store: &Store,
    experiment: &Experiment,
    goal: &str,
    now: i64,
) -> Result<std::result::Result<(String, u32), Value>> {
    let code = store.object(&experiment.source_blob)?;
    let params: Value = serde_json::from_slice(&store.object(&experiment.params_blob)?)?;
    let manifest: BarManifest = serde_json::from_slice(&store.object(&experiment.manifest_blob)?)?;
    let archive = c.market_archive.as_ref().ok_or("archive_config_missing")?;
    let (research, mut candidate) = loop_settings(c, cfg)?;
    candidate.sandbox = manifest.sandbox.clone();
    let request = harness::bar_request(
        archive,
        manifest.window,
        manifest.splits,
        manifest.capital,
        manifest.costs,
        params,
        &candidate,
    )
    .ok_or("candidate_archive_bars_invalid")?;
    let input = serde_json::to_vec(&request)?;
    let root = research_root(c)?;
    let mut db = harness::open_queue(&research, &root, true)?;
    let admitted = harness::admit(&mut Admit {
        settings: &research,
        candidate: &candidate,
        db: &mut db,
        authority: &root,
        binding: loop_binding(),
        session: loop_session(store, experiment)?,
        task_key: &task_key(experiment),
        parent_turn_id: "autoresearch",
        model_id: "autoresearch",
        goal,
        dataset_id: harness::ARCHIVE_BARS_DATASET_ID,
        policy_sha256: experiment.manifest_blob.clone(),
        code: &code,
        input: &input,
        limits: None,
        now,
    })?;
    Ok(match admitted {
        Admitted::Queued(record) => Ok((record.spec.task_id, record.spec.revision)),
        Admitted::Refused(value) => Err(value),
    })
}

/// Replay a quote-stage experiment inside the call. An accepted replay
/// registers in the library as every replay does.
pub(super) fn run_quote(
    c: &Config,
    cfg: &PlannerConfig,
    settings: &AutoresearchSettings,
    store: &Store,
    experiment: &Experiment,
    now: i64,
    preempted: &dyn Fn() -> bool,
) -> Result<(Outcome, Value)> {
    let manifest: QuoteManifest =
        serde_json::from_slice(&store.object(&experiment.manifest_blob)?)?;
    let idea = store
        .idea(experiment.idea_id)?
        .ok_or("research_idea_unknown")?;
    let source = String::from_utf8(store.object(&experiment.source_blob)?)
        .map_err(|_| "research_source_unreadable")?;
    let params: Value = serde_json::from_slice(&store.object(&experiment.params_blob)?)?;
    let mut book = c.clone();
    book.capital = Some(manifest.capital);
    let value = harness::backtest_submit(
        &book,
        cfg,
        "autoresearch",
        &format!("autoresearch-{}", experiment.id),
        BacktestArgs {
            task_key: task_key(experiment),
            hypothesis: idea.hypothesis.chars().take(1000).collect(),
            source,
            params,
            dataset_id: manifest.dataset_id.clone(),
            training_from_ms: manifest.selection_from_ms - 2,
            training_to_ms: manifest.selection_from_ms - 1,
            selection_from_ms: manifest.selection_from_ms,
            selection_to_ms: manifest.selection_to_ms,
            archive_daily_window: None,
            as_of_receipt_id: None,
            archive_bars: None,
        },
        now,
        preempted,
    )?;
    let reason = || {
        value["reason"]
            .as_str()
            .unwrap_or("not_dispatched")
            .to_owned()
    };
    if value["status"] != "completed" {
        let reason = reason();
        return Ok((
            if reason == "strategy_replay_preempted" {
                Outcome::Preempted { reason }
            } else if reason.contains("unavailable") || reason.contains("calendar") {
                Outcome::Failed { reason }
            } else {
                Outcome::Invalid { reason }
            },
            value,
        ));
    }
    let sha = value["report_sha256"]
        .as_str()
        .filter(|sha| nivra_core::autoresearch::sha256_ok(sha))
        .ok_or("strategy_replay_report_unreadable")?;
    let bytes = crate::read_bounded(
        &book
            .directory
            .join("model/strategy-replays")
            .join(format!("{sha}.json")),
        32 * 1024 * 1024,
    )
    .map_err(|_| "strategy_replay_report_unreadable")?;
    if digest(&bytes) != sha {
        return Err("strategy_replay_report_unreadable".into());
    }
    #[derive(Deserialize)]
    struct Summary {
        net_pnl: Amount,
        worst_drawdown: Amount,
        trades: u64,
        open_positions: u64,
    }
    let summary: Summary = serde_json::from_value(value["summary"].clone())
        .map_err(|_| "strategy_replay_report_unreadable")?;
    let drawdown = summary
        .worst_drawdown
        .checked_mul(Amount::from_integer(10_000))
        .and_then(|d| d.checked_floor_ratio(manifest.capital))
        .map_err(|_| "strategy_replay_report_unreadable")?;
    let score = Score {
        net_pnl: summary.net_pnl,
        max_drawdown_bps: i64::try_from(drawdown).unwrap_or(i64::MAX),
        closed_trades: summary.trades,
        open_positions: summary.open_positions,
    };
    let decision = judge(store, cfg, settings, experiment, &score)?;
    Ok((
        Outcome::Scored {
            report_blob: store.put_object(&bytes)?,
            score,
            decision,
        },
        value,
    ))
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
    let (manifest, runner, abi) = match stage {
        Stage::Bars => (
            store.object(&campaign.manifest_digest)?,
            harness::runtime_sha256()?,
            nivra_research::archive_bars::ABI,
        ),
        Stage::QuoteReplay => {
            let strategy = candidate
                .strategy
                .as_ref()
                .filter(|s| s.enabled)
                .ok_or("candidate_strategy_disabled")?;
            let window = harness::RecentArchiveWindow::resolve(c, strategy, now)?;
            let manifest = QuoteManifest {
                protocol: nivra_core::autoresearch::PROTOCOL_VERSION,
                abi: crate::runtime::strategy::ABI.into(),
                dataset_id: harness::RECENT_ARCHIVE_DATASET_ID.into(),
                selection_from_ms: window.selection_from_ms,
                selection_to_ms: window.selection_to_ms,
                capital: capital(),
                costs: c.paper_costs.clone().ok_or("paper_costs_required")?,
                comparator: "cash".into(),
            };
            let worker = cfg.effective_strategy_worker();
            let runner = digest(
                format!(
                    "{}\n{}",
                    worker.python_path.display(),
                    worker.env_sha256.as_deref().unwrap_or("stdlib")
                )
                .as_bytes(),
            );
            (
                canonical(&serde_json::to_value(&manifest)?)?,
                runner,
                crate::runtime::strategy::ABI,
            )
        }
    };
    let source_blob = store.put_object(source.as_bytes())?;
    let params_blob = store.put_object(&canonical(&params)?)?;
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
            let (outcome, replay) =
                run_quote(c, cfg, settings, store, &experiment, now, preempted)?;
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
    research: ResearchSettings,
    root: PathBuf,
}

impl QueueJobs {
    pub(crate) fn new(c: &Config, cfg: &PlannerConfig) -> Result<Self> {
        Ok(Self {
            research: loop_settings(c, cfg)?.0,
            root: research_root(c)?,
        })
    }
}

impl Jobs for QueueJobs {
    fn run(&mut self, task_id: &str, revision: u32, stop: &dyn Fn() -> bool) -> Result<JobEnd> {
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
