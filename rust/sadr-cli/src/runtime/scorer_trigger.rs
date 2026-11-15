//! Scorer-trigger replies executed only through the policy-gated orchestrator
//! write/replay API. The runtime calls this after close through a durable,
//! once-per-session admission; no automatic promotion or order path exists.
use super::orchestrator_model::{OrchestratorModelSettings, Trigger};
use super::planner::{self, CallGuard, PlannerConfig};
use super::*;
use chrono::NaiveDate;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use sadr_cli::orchestrator::{Orchestrator, OrchestratorConfig};
use sadr_cli::scorer_compare::{self, ScorerReplayReport, ScorerReplaySettings};
use sadr_cli::strategy_lab::ReplayReport;
use sadr_core::execution::Portfolio;
use sadr_core::finance::FeeArithmetic;
use sadr_core::strategy::{FeatureSnapshot, StrategySpec};
use sadr_sandbox::ScorerSandboxConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const SPEC_PATH: &str = "proposals/fixtures/spec.json";
const SNAPSHOTS_PATH: &str = "proposals/fixtures/snapshots.json";
const SCORER_PATH: &str = "proposals/scorer/scorer.py";
const PARAMS_PATH: &str = "proposals/scorer/params.json";
const OUT_PATH: &str = "proposals/scorer/report.json";

pub(crate) trait ScorerModel {
    fn complete(&mut self, system_prompt: &str) -> Result<String>;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScorerReply {
    actions: Vec<ScorerAction>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayPublish {
    scorer: String,
    params: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
enum ScorerAction {
    SandboxWrite {
        path: String,
        text: String,
    },
    Replay {
        spec: String,
        snapshots: String,
        out: String,
        /// Workspace-relative Python scorer written by this reply. Both scorer
        /// and params are present for real scorer replay; absent keeps the
        /// legacy StrategySpec replay path for existing callers and tests.
        #[serde(default)]
        scorer: Option<String>,
        #[serde(default)]
        params: Option<String>,
        #[serde(default)]
        publish: Option<ReplayPublish>,
    },
}

impl ScorerAction {
    fn tool(&self) -> &'static str {
        match self {
            Self::SandboxWrite { .. } => "sandbox_write",
            Self::Replay { .. } => "replay",
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct ScorerRun {
    pub actions_run: usize,
    pub written: Vec<PathBuf>,
    /// Legacy StrategySpec replay results.
    pub replays: Vec<ReplayReport>,
    /// Python scorer replay reports, one per executed scorer replay.
    pub scorer_replays: Vec<ScorerReplayReport>,
    pub published: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub(crate) struct PublishTargets {
    pub scorer: PathBuf,
    pub params: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct HostContext {
    pub session: String,
    pub spec: String,
    pub snapshots: String,
    pub scorer: String,
    pub params: String,
    pub out: String,
    pub publish_required: bool,
}

/// Run one scorer trigger reply through the existing policy-gated orchestrator
/// API without publishing shadow artifacts.
#[cfg(test)]
pub(crate) fn run_scorer(
    settings: &OrchestratorModelSettings,
    orchestrator: &Orchestrator,
    model: &mut dyn ScorerModel,
) -> Result<ScorerRun> {
    run_scorer_with_publish(settings, orchestrator, model, None, None)
}

/// Run one scorer trigger reply, optionally with host fixture paths and shadow
/// publish destinations. Publication happens only after a successful replay and
/// only for paths written by this run.
#[cfg(test)]
pub(crate) fn run_scorer_with_publish(
    settings: &OrchestratorModelSettings,
    orchestrator: &Orchestrator,
    model: &mut dyn ScorerModel,
    host: Option<&HostContext>,
    publish_targets: Option<&PublishTargets>,
) -> Result<ScorerRun> {
    run_scorer_loop(
        settings,
        orchestrator,
        model,
        host,
        publish_targets,
        None,
        None,
        1,
    )
}

/// Run the real scorer loop: write a Python scorer, execute it over the real
/// snapshots, then feed an unqualified report back to the model until it
/// qualifies or the turn/replay budgets are exhausted. Publication is allowed
/// only for a qualified report.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_scorer_with_replay(
    settings: &OrchestratorModelSettings,
    orchestrator: &Orchestrator,
    model: &mut dyn ScorerModel,
    host: Option<&HostContext>,
    publish_targets: Option<&PublishTargets>,
    replay: &ScorerReplaySettings,
    sandbox: &ScorerSandboxConfig,
    costs: Option<&FeeArithmetic>,
) -> Result<ScorerRun> {
    let max_turns = settings.role_for(Trigger::Scorer).max_turns.unwrap_or(10);
    run_scorer_loop(
        settings,
        orchestrator,
        model,
        host,
        publish_targets,
        Some((replay, sandbox)),
        costs,
        max_turns,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_scorer_loop(
    settings: &OrchestratorModelSettings,
    orchestrator: &Orchestrator,
    model: &mut dyn ScorerModel,
    host: Option<&HostContext>,
    publish_targets: Option<&PublishTargets>,
    replay: Option<(&ScorerReplaySettings, &ScorerSandboxConfig)>,
    costs: Option<&FeeArithmetic>,
    max_turns: usize,
) -> Result<ScorerRun> {
    if !settings.enabled
        || !settings
            .triggers
            .get(&Trigger::Scorer)
            .is_some_and(|trigger| trigger.enabled)
    {
        return Err("scorer_trigger_disabled".into());
    }
    if settings.sandbox.network {
        return Err("scorer_sandbox_network_forbidden".into());
    }
    // #222: the orchestrator workspace may opt in to outbound network for
    // reviewed scanner scripts. The post-close scorer sandbox never does.
    if replay.is_some_and(|(_, sandbox)| sandbox.network) {
        return Err("scorer_sandbox_network_forbidden".into());
    }

    let prompt = settings.load_prompt(Trigger::Scorer)?;
    let host_context = host.map(serde_json::to_string).transpose()?;
    let role = settings.role_for(Trigger::Scorer);
    let max_actions = role.max_turns.unwrap_or(usize::MAX);
    let max_replays = replay
        .map(|(settings, _)| settings.max_replays)
        .unwrap_or(1);
    let mut run = ScorerRun::default();
    let mut feedback: Option<String> = None;
    let mut scorer_replays = 0usize;
    for _ in 0..max_turns.max(1) {
        let mut turn_prompt = match &host_context {
            Some(host) => format!("{}\nHost context: {}", prompt.text, host),
            None => prompt.text.clone(),
        };
        if let Some(feedback) = &feedback {
            turn_prompt.push_str("\nPrevious scorer replay report: ");
            turn_prompt.push_str(feedback);
            turn_prompt.push_str(
                "\nRewrite the scorer and parameters to fix every reported failure. Include exactly one replay action with the written scorer and params paths, and publish only when the replay qualifies.",
            );
        }
        let reply_text = model.complete(&turn_prompt)?;
        let reply: ScorerReply =
            serde_json::from_str(&reply_text).map_err(|_| "scorer_reply_invalid")?;

        if reply.actions.len() > max_actions {
            return Err("scorer_too_many_actions".into());
        }
        let replay_count = reply
            .actions
            .iter()
            .filter(|action| matches!(action, ScorerAction::Replay { .. }))
            .count();
        if replay_count > 1 {
            return Err("scorer_too_many_replays".into());
        }
        // The legacy one-reply helper keeps its whole-batch refusal. The real
        // feedback loop may receive a write-only reply and ask again.
        if replay.is_none() && publish_targets.is_some() && replay_count != 1 {
            return Err("scorer_replay_required".into());
        }
        let written_paths: std::collections::BTreeSet<&str> = reply
            .actions
            .iter()
            .filter_map(|action| match action {
                ScorerAction::SandboxWrite { path, .. } => Some(path.as_str()),
                ScorerAction::Replay { .. } => None,
            })
            .collect();
        for action in &reply.actions {
            if !role
                .tools
                .iter()
                .any(|allowed| allowed.as_str() == action.tool())
            {
                return Err("scorer_tool_not_allowed".into());
            }
            match action {
                ScorerAction::SandboxWrite { path, .. } => orchestrator.validate_write(path)?,
                ScorerAction::Replay {
                    spec,
                    snapshots,
                    out,
                    scorer,
                    params,
                    publish,
                } => {
                    orchestrator.validate_replay(spec, snapshots, out)?;
                    match (scorer.as_deref(), params.as_deref()) {
                        (Some(scorer), Some(params)) => {
                            if replay.is_none() {
                                return Err("scorer_replay_settings_missing".into());
                            }
                            orchestrator.validate_read(scorer)?;
                            orchestrator.validate_read(params)?;
                            if !written_paths.contains(scorer) || !written_paths.contains(params) {
                                return Err("scorer_replay_unwritten".into());
                            }
                            if let Some(publication) = publish
                                && (publication.scorer != scorer || publication.params != params)
                            {
                                return Err("scorer_publish_mismatch".into());
                            }
                        }
                        (None, None) => {}
                        _ => return Err("scorer_replay_fields_incomplete".into()),
                    }
                    match (publish, publish_targets) {
                        (None, None) => {}
                        (None, Some(_)) => return Err("scorer_publish_targets_missing".into()),
                        (Some(_), None) => return Err("scorer_publish_targets_missing".into()),
                        (Some(publication), Some(_)) => {
                            if publication.scorer.trim().is_empty()
                                || publication.params.trim().is_empty()
                                || Path::new(&publication.scorer).is_absolute()
                                || Path::new(&publication.params).is_absolute()
                                || Path::new(&publication.scorer)
                                    .components()
                                    .any(|part| matches!(part, std::path::Component::ParentDir))
                                || Path::new(&publication.params)
                                    .components()
                                    .any(|part| matches!(part, std::path::Component::ParentDir))
                            {
                                return Err("scorer_publish_path_invalid".into());
                            }
                            if !written_paths.contains(publication.scorer.as_str())
                                || !written_paths.contains(publication.params.as_str())
                            {
                                return Err("scorer_publish_unwritten".into());
                            }
                        }
                    }
                }
            }
        }

        let mut pending_report: Option<ScorerReplayReport> = None;
        let mut replay_error: Option<String> = None;
        let mut did_replay = false;
        for action in reply.actions {
            run.actions_run += 1;
            match action {
                ScorerAction::SandboxWrite { path, text } => {
                    orchestrator.write_note(path.as_str(), &text)?;
                    run.written.push(PathBuf::from(path));
                }
                ScorerAction::Replay {
                    spec,
                    snapshots,
                    out,
                    scorer,
                    params,
                    publish,
                } => {
                    let publication = publish.as_ref().map(|publish| {
                        (
                            PathBuf::from(&publish.scorer),
                            PathBuf::from(&publish.params),
                        )
                    });
                    if let Some((scorer, params)) = &publication
                        && (!run.written.contains(scorer) || !run.written.contains(params))
                    {
                        return Err("scorer_publish_unwritten".into());
                    }
                    if let (Some(scorer), Some(params)) = (scorer, params) {
                        let (replay_settings, sandbox) =
                            replay.ok_or("scorer_replay_settings_missing")?;
                        did_replay = true;
                        scorer_replays += 1;
                        let source = orchestrator.read_artifact(&scorer)?;
                        let params_bytes = orchestrator.read_artifact(&params)?;
                        let snapshots_bytes = orchestrator.read_artifact(&snapshots)?;
                        let snapshots: Vec<FeatureSnapshot> =
                            serde_json::from_slice(&snapshots_bytes)
                                .map_err(|_| "scorer_replay_input_invalid")?;
                        match scorer_compare::replay_scorer(
                            replay_settings,
                            sandbox,
                            &source,
                            &params_bytes,
                            &snapshots,
                            costs,
                        ) {
                            Ok(report) => {
                                orchestrator
                                    .write_note(&out, &serde_json::to_string_pretty(&report)?)?;
                                if report.qualified()
                                    && let Some((scorer, params)) = &publication
                                {
                                    let targets =
                                        publish_targets.ok_or("scorer_publish_targets_missing")?;
                                    publish_artifact(orchestrator, scorer, &targets.scorer)?;
                                    publish_artifact(orchestrator, params, &targets.params)?;
                                    write_qualification_sidecar(&targets.scorer, &report)?;
                                    run.published.push(targets.scorer.clone());
                                    run.published.push(targets.params.clone());
                                }
                                pending_report = Some(report);
                            }
                            Err(reason) => {
                                replay_error = Some(json!({"error":reason}).to_string());
                            }
                        }
                    } else {
                        let report = orchestrator.run_replay(&spec, &snapshots, &out)?;
                        run.replays.push(report);
                        if let Some((scorer, params)) = publication {
                            let targets =
                                publish_targets.ok_or("scorer_publish_targets_missing")?;
                            publish_artifact(orchestrator, &scorer, &targets.scorer)?;
                            publish_artifact(orchestrator, &params, &targets.params)?;
                            run.published.push(targets.scorer.clone());
                            run.published.push(targets.params.clone());
                        }
                    }
                }
            }
        }

        if let Some(report) = pending_report {
            let qualified = report.qualified();
            if !qualified {
                let max_feedback_chars = replay
                    .map(|(settings, _)| settings.max_feedback_chars)
                    .unwrap_or(4096);
                feedback = Some(scorer_feedback(&report, max_feedback_chars));
            }
            run.scorer_replays.push(report);
            if qualified {
                break;
            }
        } else if let Some(error) = replay_error {
            feedback = Some(error);
        } else if replay.is_some() && !did_replay {
            feedback = Some(json!({"error":"scorer_replay_required"}).to_string());
        }
        if replay.is_some() && scorer_replays >= max_replays {
            break;
        }
    }
    Ok(run)
}

fn scorer_feedback(report: &ScorerReplayReport, max_chars: usize) -> String {
    let failures: Vec<_> = report
        .ticks
        .iter()
        .filter_map(|tick| {
            tick.failure.map(|failure| {
                json!({"observed_ms":tick.observed_ms,"failure":failure,"top_n_overlap":tick.top_n_overlap})
            })
        })
        .take(8)
        .collect();
    let horizons: Vec<_> = report
        .horizons
        .iter()
        .map(|horizon| {
            json!({
                "horizon_minutes":horizon.horizon_minutes,
                "ticks":horizon.ticks,
                "selected_rows":horizon.selected_rows,
                "mean_forward_return_bps":horizon.mean_forward_return_bps,
                "missing_forward_marks":horizon.missing_forward_marks
            })
        })
        .collect();
    let summary = json!({
        "status":report.status,
        "reason":report.reason,
        "report_hash":report.report_hash,
        "dataset_hash":report.dataset_hash,
        "scorer_sha256":report.scorer_sha256,
        "params_sha256":report.params_sha256,
        "snapshots":report.snapshots,
        "valid_snapshots":report.valid_snapshots,
        "failed_snapshots":report.failed_snapshots,
        "min_valid_snapshots":report.min_valid_snapshots,
        "mean_top_n_overlap":report.mean_top_n_overlap,
        "mean_kendall":report.mean_kendall,
        "mean_spearman":report.mean_spearman,
        "horizons":horizons,
        "failures":failures
    })
    .to_string();
    if summary.len() <= max_chars {
        return summary;
    }
    let end = summary
        .char_indices()
        .map(|(index, character)| index + character.len_utf8())
        .take_while(|end| *end <= max_chars)
        .last()
        .unwrap_or(0);
    summary[..end].to_owned()
}

fn qualification_sidecar_path(published: &Path) -> Result<PathBuf> {
    let name = published
        .file_name()
        .ok_or("scorer_publish_path_invalid")?
        .to_string_lossy();
    Ok(published.with_file_name(format!("{name}.qualification.json")))
}

fn write_qualification_sidecar(published: &Path, report: &ScorerReplayReport) -> Result<()> {
    let summary = json!({
        "schema":1,
        "qualified":report.qualified(),
        "status":report.status,
        "reason":report.reason,
        "report_hash":report.report_hash,
        "dataset_hash":report.dataset_hash,
        "scorer_sha256":report.scorer_sha256,
        "params_sha256":report.params_sha256,
        "snapshots":report.snapshots,
        "valid_snapshots":report.valid_snapshots,
        "failed_snapshots":report.failed_snapshots,
        "min_valid_snapshots":report.min_valid_snapshots
    });
    write_private(
        &qualification_sidecar_path(published)?,
        serde_json::to_string_pretty(&summary)?.as_bytes(),
    )
}

fn publish_artifact(
    orchestrator: &Orchestrator,
    workspace_path: &Path,
    destination: &Path,
) -> Result<()> {
    let bytes = orchestrator.read_artifact(workspace_path)?;
    write_private(destination, &bytes)
}

fn write_private(destination: &Path, bytes: &[u8]) -> Result<()> {
    let parent = destination.parent().ok_or("scorer_publish_path_invalid")?;
    fs::create_dir_all(parent)?;
    let temporary = destination.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, destination)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// True only once the session is closed and the configured post-close minute
/// (minutes after midnight IST) has been reached.
pub(crate) fn scorer_due(minute_of_day: u32, session_closed: bool, after_minute: u32) -> bool {
    session_closed && minute_of_day >= after_minute
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Admission {
    session: NaiveDate,
    binding: String,
    configuration_hash: String,
    prompt_sha256: String,
    model_id: String,
    after_minute: u32,
    admitted_ms: i64,
    deadline_ms: i64,
}

enum Admitted {
    Run(Admission),
    Settled(Value),
}

/// A terminal scorer failure carries a bounded code and optional non-secret
/// diagnostics (for example the snapshot window/counts) for the durable status.
type ScorerResult = std::result::Result<ScorerRun, (String, Option<Value>)>;

pub(crate) struct Worker {
    cancel: Arc<AtomicBool>,
    receiver: std::sync::mpsc::Receiver<ScorerResult>,
    admission: Admission,
    settled: bool,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn log(status: &str, reason: &str) {
    eprintln!("scorer_trigger status={status} reason={reason}");
}

fn bounded_reason(reason: &str) -> String {
    let reason = reason.trim();
    if !reason.is_empty()
        && reason.len() <= 160
        && reason
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        reason.to_owned()
    } else {
        "scorer_failed".into()
    }
}

fn table_exists(db: &Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='scorer_control')",
        [],
        |r| r.get(0),
    )?)
}

fn load(db: &Connection) -> Result<Option<(Admission, String, Option<String>)>> {
    if !table_exists(db)? {
        return Ok(None);
    }
    let row: Option<(String, String, Option<String>)> = db
        .query_row(
            "SELECT body,status,result FROM scorer_control WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((body, status, result)) = row else {
        return Ok(None);
    };
    if body.len() > 65536
        || result
            .as_ref()
            .is_some_and(|result| result.len() > 1_000_000)
    {
        return Err("scorer_admission_invalid".into());
    }
    Ok(Some((serde_json::from_str(&body)?, status, result)))
}

fn save(
    db: &Connection,
    admission: &Admission,
    status: &str,
    result: Option<&Value>,
) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS scorer_control(id INTEGER PRIMARY KEY CHECK(id=1),body TEXT NOT NULL,status TEXT NOT NULL,result TEXT);",
    )?;
    let body = serde_json::to_string(admission)?;
    let result = result.map(serde_json::to_string).transpose()?;
    db.execute(
        "INSERT INTO scorer_control(id,body,status,result) VALUES(1,?,?,?)
         ON CONFLICT(id) DO UPDATE SET body=excluded.body,status=excluded.status,result=excluded.result",
        rusqlite::params![body, status, result],
    )?;
    Ok(())
}

fn update_status(
    db: &Connection,
    admission: &Admission,
    status: &str,
    result: Option<&Value>,
) -> Result<()> {
    let Some((saved, current, _)) = load(db)? else {
        return Err("scorer_admission_missing".into());
    };
    if saved.session != admission.session
        || saved.binding != admission.binding
        || saved.configuration_hash != admission.configuration_hash
        || current != "reserved"
    {
        return Err("scorer_admission_changed".into());
    }
    let result = result.map(serde_json::to_string).transpose()?;
    db.execute(
        "UPDATE scorer_control SET status=?,result=? WHERE id=1 AND status='reserved'",
        rusqlite::params![status, result],
    )?;
    Ok(())
}

fn status(db: &Connection, c: &Config) -> Result<Value> {
    let Some((admission, status, result)) = load(db)? else {
        return Ok(json!({"status":"not_admitted"}));
    };
    if admission.session != c.session || admission.binding != book_binding(c)? {
        return Ok(json!({"status":"admission_binding_mismatch"}));
    }
    let trials = super::experiment_ledger::list(c).unwrap_or_default();
    Ok(json!({
        "status":status,
        "reason":result.as_ref().and_then(|value| serde_json::from_str::<Value>(value).ok()).and_then(|value| value["reason"].as_str().map(str::to_owned)),
        "after_minute":admission.after_minute,
        "admitted_ms":admission.admitted_ms,
        "deadline_ms":admission.deadline_ms,
        "model_id":admission.model_id,
        "result":result.as_ref().and_then(|value| serde_json::from_str::<Value>(value).ok()),
        "experiment_trials":trials.len(),
        "experiment_edge_qualified":trials.iter().any(|trial| trial.edge_qualified)
    }))
}

fn config_hash(cfg: &PlannerConfig) -> Result<String> {
    Ok(digest(&serde_json::to_vec(cfg)?))
}

fn record_skip(
    db: &Connection,
    c: &Config,
    cfg: Option<&PlannerConfig>,
    after_minute: u32,
    reason: &str,
    now: i64,
) -> Result<Value> {
    let admission = Admission {
        session: c.session,
        binding: book_binding(c)?,
        configuration_hash: cfg.map(config_hash).transpose()?.unwrap_or_default(),
        prompt_sha256: String::new(),
        model_id: cfg
            .map(|cfg| cfg.role(Trigger::Scorer).model_id)
            .unwrap_or_default(),
        after_minute,
        admitted_ms: now,
        deadline_ms: now,
    };
    save(db, &admission, "skipped", Some(&json!({"reason":reason})))?;
    log("skip", reason);
    status(db, c)
}

fn admit(
    db: &mut Connection,
    c: &Config,
    cfg: Option<&PlannerConfig>,
    p: &Portfolio,
    now: i64,
) -> Result<Admitted> {
    let binding = book_binding(c)?;
    if let Some((admission, current, result)) = load(db)?
        && admission.session == c.session
        && admission.binding == binding
    {
        if current == "reserved" {
            update_status(
                db,
                &admission,
                "interrupted",
                Some(&json!({"reason":"process_restart_no_replay"})),
            )?;
            super::experiment_ledger::record_scorer_outcome(
                c,
                &admission.session.to_string(),
                &admission.configuration_hash,
                &admission.prompt_sha256,
                "interrupted",
                &json!({"reason":"process_restart_no_replay"}),
                now,
            )?;
            log("interrupted", "process_restart_no_replay");
            return Ok(Admitted::Settled(status(db, c)?));
        }
        // A configuration-error skip is not a completed run. Keep it re-admittable
        // after the operator corrects the config and applies it, but only re-admit
        // when the configuration actually changed. Logging/re-admitting an
        // unchanged config every tick is the live loop fixed here (#159).
        let reason = result
            .as_deref()
            .and_then(|body| serde_json::from_str::<Value>(body).ok())
            .and_then(|body| body["reason"].as_str().map(str::to_owned));
        let configuration_error = (current == "skipped" || current == "configuration_error")
            && reason.as_deref() == Some("after_minute_unset");
        if configuration_error {
            let current_hash = cfg.map(config_hash).transpose()?.unwrap_or_default();
            if admission.configuration_hash == current_hash {
                return Ok(Admitted::Settled(status(db, c)?));
            }
            log("re_admit", "after_minute_corrected");
            db.execute("DELETE FROM scorer_control WHERE id=1", [])?;
        } else {
            return Ok(Admitted::Settled(status(db, c)?));
        }
    }
    let Some(cfg) = cfg.filter(|cfg| cfg.enabled) else {
        return Ok(Admitted::Settled(record_skip(
            db,
            c,
            None,
            0,
            "planner_disabled",
            now,
        )?));
    };
    let Some(settings) = cfg
        .orchestrator
        .as_ref()
        .filter(|settings| settings.enabled)
    else {
        return Ok(Admitted::Settled(record_skip(
            db,
            c,
            Some(cfg),
            0,
            "orchestrator_disabled",
            now,
        )?));
    };
    let Some(trigger) = settings.triggers.get(&Trigger::Scorer) else {
        return Ok(Admitted::Settled(record_skip(
            db,
            c,
            Some(cfg),
            0,
            "trigger_missing",
            now,
        )?));
    };
    if !trigger.enabled {
        return Ok(Admitted::Settled(record_skip(
            db,
            c,
            Some(cfg),
            trigger.after_minute.unwrap_or(0),
            "trigger_disabled",
            now,
        )?));
    }
    let Some(after_minute) = trigger.after_minute.or(settings.scorer_after_minute) else {
        // A configuration error must not become a terminal skip: after the
        // operator fixes the config the same session may still be admitted.
        // Persist the configuration hash so an unchanged bad config is silent on
        // the next tick instead of logging and re-admitting forever (#159).
        let admission = Admission {
            session: c.session,
            binding: binding.clone(),
            configuration_hash: config_hash(cfg)?,
            prompt_sha256: String::new(),
            model_id: cfg.role(Trigger::Scorer).model_id,
            after_minute: 0,
            admitted_ms: now,
            deadline_ms: now,
        };
        save(
            db,
            &admission,
            "configuration_error",
            Some(&json!({"reason":"after_minute_unset"})),
        )?;
        log("config_error", "after_minute_unset");
        return Ok(Admitted::Settled(json!({
            "status":"configuration_error",
            "reason":"after_minute_unset",
            "recovery":"set orchestrator.triggers.scorer.after_minute or orchestrator.scorer_after_minute, then run config-apply"
        })));
    };
    let closed = p.closed && c.directory.join("report.json").is_file();
    if local(now).date_naive() != c.session || !scorer_due(minute(now), closed, after_minute) {
        return Ok(Admitted::Settled(json!({
            "status":"awaiting_close",
            "reason":if local(now).date_naive()!=c.session {"different_session"} else {"not_due"},
            "session_closed":closed,
            "after_minute":after_minute
        })));
    }
    let role = cfg.role(Trigger::Scorer);
    if !role.enabled {
        return Ok(Admitted::Settled(record_skip(
            db,
            c,
            Some(cfg),
            after_minute,
            "role_disabled",
            now,
        )?));
    }
    let budget = planner::budget_status(c, "scorer", &role);
    if budget["remaining"].as_u64() == Some(0) {
        return Ok(Admitted::Settled(record_skip(
            db,
            c,
            Some(cfg),
            after_minute,
            "budget_exhausted",
            now,
        )?));
    }
    let prompt = match settings.load_prompt(Trigger::Scorer) {
        Ok(prompt) => prompt,
        Err(_) => {
            return Ok(Admitted::Settled(record_skip(
                db,
                c,
                Some(cfg),
                after_minute,
                "prompt_unavailable",
                now,
            )?));
        }
    };
    let deadline_ms = now
        .checked_add(role.run_budget_ms() as i64)
        .ok_or("scorer_deadline")?;
    let admission = Admission {
        session: c.session,
        binding,
        configuration_hash: config_hash(cfg)?,
        prompt_sha256: prompt.sha256,
        model_id: role.model_id,
        after_minute,
        admitted_ms: now,
        deadline_ms,
    };
    save(db, &admission, "reserved", None)?;
    log("admitted", "due");
    Ok(Admitted::Run(admission))
}

fn summarize(run: &ScorerRun) -> Value {
    json!({
        "actions_run":run.actions_run,
        "written":run.written,
        "replay_count":run.replays.len(),
        "replays":run.replays.iter().map(|report| json!({
            "spec_hash":report.spec_hash,
            "dataset_hash":report.dataset_hash,
            "ticks":report.ticks,
            "failed_ticks":report.failed_ticks,
            "selected_rows":report.selected_rows
        })).collect::<Vec<_>>(),
        "scorer_replay_count":run.scorer_replays.len(),
        "scorer_replays":run.scorer_replays.iter().map(|report| json!({
            "status":report.status,
            "reason":report.reason,
            "qualified":report.qualified(),
            "report_hash":report.report_hash,
            "dataset_hash":report.dataset_hash,
            "scorer_sha256":report.scorer_sha256,
            "params_sha256":report.params_sha256,
            "snapshots":report.snapshots,
            "valid_snapshots":report.valid_snapshots,
            "failed_snapshots":report.failed_snapshots,
            "min_valid_snapshots":report.min_valid_snapshots,
            "mean_top_n_overlap":report.mean_top_n_overlap,
            "mean_kendall":report.mean_kendall,
            "mean_spearman":report.mean_spearman
        })).collect::<Vec<_>>(),
        "qualified":run.scorer_replays.last().is_some_and(ScorerReplayReport::qualified),
        "published":run.published
    })
}

fn settle(
    db: &Connection,
    c: &Config,
    admission: &Admission,
    result: ScorerResult,
    now: i64,
) -> Result<()> {
    let (status, result, reason) = match result {
        Ok(run)
            if now >= admission.admitted_ms
                && now <= admission.deadline_ms
                && local(now).date_naive() == c.session =>
        {
            ("completed", summarize(&run), "completed".to_owned())
        }
        Ok(_) => (
            "unavailable",
            json!({"reason":"deadline"}),
            "deadline".to_owned(),
        ),
        Err((reason, detail)) => {
            let reason = bounded_reason(&reason);
            let mut result = json!({"reason":reason});
            if let Some(detail) = detail {
                result["detail"] = detail;
            }
            ("unavailable", result, reason)
        }
    };
    update_status(db, admission, status, Some(&result))?;
    super::experiment_ledger::record_scorer_outcome(
        c,
        &admission.session.to_string(),
        &admission.configuration_hash,
        &admission.prompt_sha256,
        status,
        &result,
        now,
    )?;
    log(status, &reason);
    Ok(())
}

/// Snapshot extraction failure with bounded evidence for the durable status.
#[derive(Debug)]
struct SnapshotError {
    reason: String,
    detail: Value,
}
impl std::fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.reason)
    }
}
impl std::error::Error for SnapshotError {}

/// Bounded read-only evidence about the snapshot window: how many recent
/// `wide_quotes` rows existed, their purpose mix and time window. Best effort;
/// a diagnostic failure never changes the terminal reason.
fn snapshot_detail(c: &Config, replay: &ScorerReplaySettings) -> Option<Value> {
    let limit = replay
        .max_snapshots
        .saturating_add(replay.lookback_samples)
        .min(10_000);
    let db = Connection::open_with_flags(
        c.directory.join("paper.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    db.pragma_update(None, "query_only", true).ok()?;
    let mut statement = db
        .prepare(
            "SELECT at,body FROM ticks WHERE event IN ('wide_quotes','scanner_metadata') ORDER BY id DESC LIMIT ?1",
        )
        .ok()?;
    let rows: Vec<(i64, String)> = statement
        .query_map([limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))
        .ok()?
        .filter_map(std::result::Result::ok)
        .collect();
    let mut scanner = 0usize;
    let mut metadata = 0usize;
    let mut protection = 0usize;
    let mut unknown = 0usize;
    for (_, raw) in &rows {
        match serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|value| value["purpose"].as_str().map(str::to_owned))
            .as_deref()
        {
            Some("scanner") => scanner += 1,
            Some("scanner_metadata") => metadata += 1,
            Some("execution_or_protection") => protection += 1,
            _ => unknown += 1,
        }
    }
    Some(json!({
        "source":"session_paper_quotes_read_only",
        "window_rows":rows.len(),
        "scanner_rows":scanner,
        "protection_rows":protection,
        "unknown_purpose_rows":unknown,
        "scanner_metadata_rows":metadata,
        "newest_ms":rows.first().map(|row| row.0),
        "oldest_ms":rows.last().map(|row| row.0),
        "max_snapshots":replay.max_snapshots,
        "lookback_samples":replay.lookback_samples,
        "max_age_ms":replay.max_age_ms,
    }))
}

fn run_real(
    c: &Config,
    cfg: &PlannerConfig,
    admission: &Admission,
    cancel: &Arc<AtomicBool>,
) -> Result<ScorerRun> {
    let settings = cfg
        .orchestrator
        .as_ref()
        .ok_or("scorer_orchestrator_missing")?;
    let replay = settings
        .triggers
        .get(&Trigger::Scorer)
        .and_then(|trigger| trigger.replay.clone())
        .unwrap_or_default();
    let snapshots = super::paper_replay::scorer_snapshots(
        &c.directory,
        c.session,
        replay.max_snapshots,
        replay.max_age_ms,
        replay.lookback_samples,
    )
    .map_err(|reason| -> Box<dyn std::error::Error> {
        Box::new(SnapshotError {
            detail: snapshot_detail(c, &replay).unwrap_or(Value::Null),
            reason,
        })
    })?;
    let workspace = c.directory.join("model/orchestrator");
    fs::create_dir_all(&workspace)?;
    let orchestrator = Orchestrator::new(&workspace, OrchestratorConfig::default())?;
    prepare_fixtures(&orchestrator, &snapshots)?;
    let publish = cfg
        .discovery
        .as_ref()
        .and_then(|discovery| discovery.shadow_scorer.as_ref())
        .filter(|scorer| scorer.enabled)
        .map(|scorer| PublishTargets {
            scorer: PathBuf::from(&scorer.scorer_path),
            params: PathBuf::from(&scorer.params_path),
        });
    let host = HostContext {
        session: c.session.to_string(),
        spec: SPEC_PATH.into(),
        snapshots: SNAPSHOTS_PATH.into(),
        scorer: SCORER_PATH.into(),
        params: PARAMS_PATH.into(),
        out: OUT_PATH.into(),
        publish_required: publish.is_some(),
    };
    let sandbox = cfg
        .discovery
        .as_ref()
        .and_then(|discovery| discovery.scorer_sandbox.clone())
        .unwrap_or_default();
    let mut model = RealScorer {
        c,
        cfg,
        cancel: Arc::clone(cancel),
        started_ms: admission.admitted_ms,
        deadline_ms: admission.deadline_ms,
    };
    run_scorer_with_replay(
        settings,
        &orchestrator,
        &mut model,
        Some(&host),
        publish.as_ref(),
        &replay,
        &sandbox,
        c.paper_costs.as_ref(),
    )
}

fn prepare_fixtures(orchestrator: &Orchestrator, snapshots: &[FeatureSnapshot]) -> Result<()> {
    let spec = serde_json::to_string(&StrategySpec::default())?;
    let snapshots = serde_json::to_string(snapshots)?;
    orchestrator.write_note(SPEC_PATH, &spec)?;
    orchestrator.write_note(SNAPSHOTS_PATH, &snapshots)?;
    Ok(())
}

struct RealScorer<'a> {
    c: &'a Config,
    cfg: &'a PlannerConfig,
    cancel: Arc<AtomicBool>,
    started_ms: i64,
    deadline_ms: i64,
}
impl ScorerModel for RealScorer<'_> {
    fn complete(&mut self, system_prompt: &str) -> Result<String> {
        let instructions = format!("{}\n\n{system_prompt}", super::harness::objective());
        let mut role = self.cfg.role(Trigger::Scorer);
        let path = role
            .model_config
            .as_deref()
            .or(self.cfg.model_config.as_deref())
            .ok_or("scorer_model_config_required")?;
        // The scorer returns one JSON action document, not provider tool calls.
        role.tools.clear();
        let mut backend = planner::Subscription::new(path);
        let raw = super::harness::run(
            self.c,
            self.cfg,
            "scorer",
            &role,
            &instructions,
            &json!({"trigger":{"kind":"post_close"}}),
            &mut backend,
            &CallGuard {
                cancel: Some(&self.cancel),
                deadline: Some(self.deadline_ms),
                not_before: Some(self.started_ms),
                configuration: Some((self.c, self.cfg)),
                journal_required: std::cell::Cell::new(true),
                minimum_prior_calls: 0,
                invocation_id: None,
                context_hash: None,
                clock: &millis,
                duration: None,
                started_wall_ms: None,
            },
        )?;
        if let Ok(value) = serde_json::from_str::<Value>(&raw)
            && let Some(output) = value["output"].as_array()
        {
            return super::harness::final_text(output);
        }
        Ok(raw)
    }
}

/// Called after close. A restart preserves an admitted reservation as
/// interrupted and never replays it. Errors affect only this advisory lane.
pub(crate) fn tick(
    db: &mut Connection,
    c: &Config,
    cfg: Option<&PlannerConfig>,
    p: &Portfolio,
    now: i64,
    worker: &mut Option<Worker>,
) -> Result<Value> {
    if let Some(mut active) = worker.take() {
        match active.receiver.try_recv() {
            Ok(result) => {
                if !active.settled {
                    settle(db, c, &active.admission, result, now)?;
                    active.settled = true;
                }
                return status(db, c);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                if !active.settled {
                    settle(
                        db,
                        c,
                        &active.admission,
                        Err(("worker_lost".into(), None)),
                        now,
                    )?;
                    active.settled = true;
                }
                return status(db, c);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                let changed = cfg.is_none_or(|cfg| {
                    config_hash(cfg)
                        .map(|hash| hash != active.admission.configuration_hash)
                        .unwrap_or(true)
                });
                if !active.settled
                    && (now > active.admission.deadline_ms
                        || local(now).date_naive() != c.session
                        || changed)
                {
                    active.cancel.store(true, Ordering::Release);
                    settle(
                        db,
                        c,
                        &active.admission,
                        Err(("cancelled_or_expired".into(), None)),
                        now,
                    )?;
                    active.settled = true;
                }
                *worker = Some(active);
                return status(db, c);
            }
        }
    }
    match admit(db, c, cfg, p, now)? {
        Admitted::Settled(value) => Ok(value),
        Admitted::Run(admission) => {
            let (sender, receiver) = std::sync::mpsc::channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let current = status(db, c)?;
            let (c, cfg, admission_copy, worker_cancel) = (
                c.clone(),
                cfg.expect("admitted config").clone(),
                admission.clone(),
                Arc::clone(&cancel),
            );
            thread::spawn(move || {
                let result = run_real(&c, &cfg, &admission_copy, &worker_cancel).map_err(|error| {
                    match error.downcast::<SnapshotError>() {
                        Ok(snapshot) => (snapshot.reason, Some(snapshot.detail)),
                        Err(error) => (error.to_string(), None),
                    }
                });
                let _ = sender.send(result);
            });
            *worker = Some(Worker {
                cancel,
                receiver,
                admission,
                settled: false,
            });
            Ok(current)
        }
    }
}

#[cfg(test)]
mod tests;
