//! The research lane: bounded research turns beside, never inside, trading.
//!
//! Research runs on its own worker slot, its own session log
//! (`<session>/research-lane`), its own capped usage bucket and its own failure
//! record (#237, #238). It has no financial authority and never parks, backs
//! off or blocks the trading lane. A turn answers either a typed request the
//! trading lane queued (#239) or terminal original evidence; it records one
//! durable turn and persists one idempotent receipt. A failed attempt is
//! terminal for that evidence id and is never replayed.
use super::super::lane_queue::{self, MessageBody, MessageKind, MessageState, TransitionReason};
use super::super::owner_tasks::{
    self, OwnerTask, ResearchEvidenceKind, ResearchEvidenceV1, ResearchIdentity, ResearchOutputV1,
    ResearchReceipt, ResearchScheduleInput,
};
use super::*;
use sadr_store::research_jobs as jobs;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const MAX_ARTIFACT_BYTES: u64 = 1_000_000;
const MAX_ARTIFACT_WRITE_BYTES: usize = 262_144;
const RESEARCH_COMPLETED_VERSION: u32 = 1;
/// The lane log directory beside the trading session log.
pub(crate) const LANE_LOG: &str = "research-lane";
/// Lane turns whose history is carried into the next research prompt.
const LANE_HISTORY: usize = 6;
/// Research results stay deliverable to the trading lane this long.
const RESULT_TTL_MS: i64 = 43_200_000;
const LANE_FAILURE_PATH: &str = "model/research-lane-failure.json";

/// The research lane's own durable log. It shares nothing with the trading
/// log, so a pending research turn can never hold a decision turn.
pub(crate) fn lane_log(root: &Path, session: &str) -> session::SessionLog {
    session::SessionLog::new(root, &format!("{session}/{LANE_LOG}"))
}

/// The log a research turn lives in: the lane log for every lane turn, the
/// trading log only for a turn a pre-lane build already wrote there.
fn log_for(root: &Path, turn: &session::Turn) -> session::SessionLog {
    if turn.request["lane"] == "research" {
        lane_log(root, &turn.session)
    } else {
        session::SessionLog::new(root, &turn.session)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchClockV1 {
    started_wall_ms: i64,
    observed_wall_ms: i64,
    monotonic_elapsed_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResearchCompletedV1 {
    version: u32,
    task: OwnerTask,
    receipt: ResearchReceipt,
    output: Option<ResearchOutputV1>,
    failure: Option<String>,
    evidence: Value,
    clock: ResearchClockV1,
}

fn current_uid() -> u32 {
    fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .unwrap_or(u32::MAX)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn checked_owned_dir(path: &Path, missing: &'static str) -> Result<fs::Metadata> {
    match fs::symlink_metadata(path) {
        Ok(meta)
            if !meta.file_type().is_symlink() && meta.is_dir() && meta.uid() == current_uid() =>
        {
            Ok(meta)
        }
        Ok(_) => Err("owner_research_path_invalid".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(missing.into()),
        Err(_) => Err("owner_research_path_invalid".into()),
    }
}

fn ensure_owned_dir_component(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != current_uid() {
                return Err("owner_research_path_invalid".into());
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|_| "owner_research_path_invalid")?;
            let meta = fs::symlink_metadata(path).map_err(|_| "owner_research_path_invalid")?;
            if meta.file_type().is_symlink()
                || !meta.is_dir()
                || meta.uid() != current_uid()
                || meta.mode() & 0o777 != 0o700
            {
                return Err("owner_research_path_invalid".into());
            }
            Ok(())
        }
        Err(_) => Err("owner_research_path_invalid".into()),
    }
}

/// Resolve the private research directory without following or repairing a
/// predecessor. Every existing component must already be an owned directory;
/// only a missing component is created, with 0700. The final directory must be
/// owned and 0700. A symlinked or non-regular component fails before any mkdir
/// or chmod can touch the outside target.
pub(super) fn research_directory(root: &Path, create: bool) -> Result<PathBuf> {
    let model = root.join("model");
    let directory = model.join("research");
    let root_meta = fs::symlink_metadata(root).map_err(|_| "owner_research_path_invalid")?;
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() || root_meta.uid() != current_uid()
    {
        return Err("owner_research_path_invalid".into());
    }
    if create {
        ensure_owned_dir_component(&model)?;
        ensure_owned_dir_component(&directory)?;
    } else {
        checked_owned_dir(&model, "owner_research_source_missing")?;
        checked_owned_dir(&directory, "owner_research_source_missing")?;
    }
    let meta = checked_owned_dir(&directory, "owner_research_source_missing")?;
    if meta.mode() & 0o777 != 0o700 || meta.uid() != current_uid() {
        return Err("owner_research_path_invalid".into());
    }
    Ok(directory)
}

fn artifact_path(root: &Path, task: &OwnerTask) -> Result<PathBuf> {
    Ok(research_directory(root, false)?.join(format!("{}.json", task.id()?)))
}

fn canonical_value(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<(String, Value)> = object.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut sorted = serde_json::Map::with_capacity(entries.len());
            for (key, value) in entries {
                sorted.insert(key, canonical_value(value));
            }
            Value::Object(sorted)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_value).collect()),
        value => value,
    }
}
pub(super) fn canonical_bytes(value: &Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&canonical_value(value.clone()))?)
}

fn task_for(root: &Path, worker: &Worker) -> Result<(OwnerTask, session::Turn)> {
    let turn = lane_log(root, &worker.session)
        .records()
        .into_iter()
        .find_map(|record| {
            if let session::Body::Turn { turn } = record.body {
                (turn.id == worker.turn_id).then_some(turn)
            } else {
                None
            }
        })
        .ok_or("owner_research_turn_missing")?;
    let task = OwnerTask::from_turn(&turn)?.ok_or("owner_research_binding_invalid")?;
    if task.kind() != "research" || task.research_identity().is_none() {
        return Err("owner_research_binding_invalid".into());
    }
    Ok((task, turn))
}

fn read_source(root: &Path, task: &OwnerTask) -> Result<(Vec<u8>, PathBuf)> {
    let path = artifact_path(root, task)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "owner_research_source_missing"
            } else {
                "owner_research_source_invalid"
            }
        })?;
    let meta = file
        .metadata()
        .map_err(|_| "owner_research_source_invalid")?;
    if !meta.is_file()
        || meta.mode() & 0o777 != 0o600
        || meta.uid() != current_uid()
        || meta.nlink() != 1
        || meta.len() > MAX_ARTIFACT_BYTES
    {
        return Err("owner_research_source_invalid".into());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "owner_research_source_invalid")?;
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err("owner_research_source_invalid".into());
    }
    Ok((bytes, path))
}

fn source_present(root: &Path, task: &OwnerTask) -> Result<bool> {
    match artifact_path(root, task) {
        Ok(path) => Ok(path.is_file()),
        Err(error) if error.to_string() == "owner_research_source_missing" => Ok(false),
        Err(error) => Err(error),
    }
}

fn parse_completed(bytes: &[u8]) -> Result<ResearchCompletedV1> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "owner_research_source_invalid")?;
    let completed: ResearchCompletedV1 =
        serde_json::from_value(value.clone()).map_err(|_| "owner_research_source_invalid")?;
    if completed.version != RESEARCH_COMPLETED_VERSION
        || canonical_bytes(&value).map_err(|_| "owner_research_source_invalid")? != bytes
    {
        return Err("owner_research_source_invalid".into());
    }
    Ok(completed)
}

fn validate_completed(completed: &ResearchCompletedV1, task: &OwnerTask) -> Result<()> {
    let identity = task
        .research_identity()
        .ok_or("owner_research_binding_invalid")?;
    let receipt = &completed.receipt;
    if completed.task != *task
        || receipt.version != 1
        || receipt.evidence_id != identity.evidence_id
        || receipt.model_id.is_empty()
        || receipt.completed_ms <= 0
        || completed.clock.observed_wall_ms != receipt.completed_ms
        || completed.clock.started_wall_ms <= 0
        || completed.clock.monotonic_elapsed_ms < 0
    {
        return Err("owner_research_receipt_conflict".into());
    }
    match (
        completed.output.as_ref(),
        completed.failure.as_deref(),
        receipt.status.as_str(),
    ) {
        (Some(output), None, "completed") => {
            if output.evidence_id != identity.evidence_id
                || receipt.output_sha256 != digest(&serde_json::to_vec(output)?)
            {
                return Err("owner_research_receipt_conflict".into());
            }
        }
        (None, Some(reason), "failed") if !reason.is_empty() && reason.len() <= 160 => {}
        _ => return Err("owner_research_receipt_conflict".into()),
    }
    Ok(())
}

fn validate_artifact_clock(completed: &ResearchCompletedV1, turn: &session::Turn) -> Result<()> {
    if completed.clock.started_wall_ms != turn.started_at_ms
        || completed.clock.observed_wall_ms != completed.receipt.completed_ms
        || completed.clock.monotonic_elapsed_ms < 0
    {
        return Err("owner_research_receipt_conflict".into());
    }
    if completed.receipt.completed_ms < turn.started_at_ms
        && (completed.receipt.status != "failed" || completed.failure.is_none())
    {
        return Err("owner_research_receipt_conflict".into());
    }
    Ok(())
}

fn persist(root: &Path, task: &OwnerTask, completed: &ResearchCompletedV1) -> Result<()> {
    let bytes = canonical_bytes(&serde_json::to_value(completed)?)?;
    if bytes.len() > MAX_ARTIFACT_WRITE_BYTES {
        return Err("owner_research_artifact_bound".into());
    }
    let directory = research_directory(root, true)?;
    let name = format!("{}.json", task.id()?);
    let path = directory.join(&name);
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err("owner_research_source_invalid".into());
        }
        Err(_) => return Err("owner_research_path_invalid".into()),
    };
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| "owner_research_path_invalid")?;
    file.write_all(&bytes)
        .map_err(|_| "owner_research_source_unsynced")?;
    file.sync_all()
        .map_err(|_| "owner_research_source_unsynced")?;
    fs::File::open(&directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "owner_research_source_unsynced")?;
    Ok(())
}

fn research_action(turn: &session::Turn, receipt: &ResearchReceipt) -> session::Action {
    session::action(
        "owner_research",
        &turn.id,
        &serde_json::to_value(receipt).unwrap_or_default(),
    )
}

fn expected_result(
    turn: &session::Turn,
    completed: &ResearchCompletedV1,
    outcome: &str,
) -> Result<session::TurnResult> {
    Ok(session::TurnResult {
        turn_id: turn.id.clone(),
        finished_at_ms: completed.receipt.completed_ms,
        outcome: outcome.into(),
        reason: completed.failure.clone(),
        reply: completed
            .output
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?
            .unwrap_or_default(),
        actions: vec![research_action(turn, &completed.receipt)],
    })
}

/// Settle lane messages from the lane log. Idempotent and crash-safe: the
/// log is the truth, so this re-derives every request state and result
/// message after any crash between the log append and the ledger write.
fn reconcile_research(root: &Path, turn: &session::Turn) {
    if turn.request["lane"] != "research" {
        return;
    }
    if let Err(error) = settle_lane(root, &turn.session, millis()) {
        eprintln!("research_lane_settle_failed reason={error}");
    }
}
fn complete_log(
    root: &Path,
    turn: &session::Turn,
    completed: &ResearchCompletedV1,
    now: i64,
    outcome: &str,
) -> Result<()> {
    let action = research_action(turn, &completed.receipt);
    let actions = session::checked_actions_for(root, &turn.session, &turn.id)?;
    if actions.is_empty() {
        session::commit_action(root, &turn.session, &turn.id, &action)?;
    } else if actions != vec![action] {
        return Err("owner_research_receipt_conflict".into());
    }
    let log = log_for(root, turn);
    let result = expected_result(turn, completed, outcome)?;
    if let Some(existing) = log.result_for(&turn.id) {
        let same_outcome = matches!(
            (existing.outcome.as_str(), result.outcome.as_str()),
            (
                "ok" | "recovered_research_receipt",
                "ok" | "recovered_research_receipt"
            ) | ("failed", "failed")
                | ("interrupted", "interrupted")
        );
        if !same_outcome
            || existing.turn_id != result.turn_id
            || existing.finished_at_ms != result.finished_at_ms
            || existing.reason != result.reason
            || existing.reply != result.reply
            || existing.actions != result.actions
        {
            return Err("owner_research_receipt_conflict".into());
        }
        reconcile_research(root, turn);
        return Ok(());
    }
    log.append(now, session::Body::Result { result })?;
    reconcile_research(root, turn);
    Ok(())
}

fn append_result_only(
    root: &Path,
    turn: &session::Turn,
    now: i64,
    actions: Vec<session::Action>,
) -> Result<()> {
    let log = log_for(root, turn);
    if log.result_for(&turn.id).is_some() {
        reconcile_research(root, turn);
        return Ok(());
    }
    let result = session::TurnResult {
        turn_id: turn.id.clone(),
        finished_at_ms: now,
        outcome: "recovered_action_receipt".into(),
        reason: None,
        reply: String::new(),
        actions,
    };
    log.append(now, session::Body::Result { result })?;
    reconcile_research(root, turn);
    Ok(())
}

/// Recover the one durable research attempt. A persisted artifact wins and is
/// never re-provider-called; an action without an artifact appends the result
/// only; neither means an explicit terminal interruption, never replay.
pub(super) fn recover(root: &Path, turn: &session::Turn, now: i64) -> Result<bool> {
    recover_in(root, turn, now)
}

/// Close a research turn a pre-lane build left pending in the trading log.
pub(super) fn recover_legacy(root: &Path, turn: &session::Turn, now: i64) -> Result<bool> {
    if turn.request["lane"] == "research" {
        return Err("owner_research_binding_invalid".into());
    }
    recover_in(root, turn, now)
}

fn recover_in(root: &Path, turn: &session::Turn, now: i64) -> Result<bool> {
    let Some(task) = OwnerTask::from_turn(turn)? else {
        return Ok(false);
    };
    if task.research_identity().is_none() {
        return Ok(false);
    }
    match read_source(root, &task) {
        Ok((bytes, _)) => {
            let completed = parse_completed(&bytes)?;
            validate_completed(&completed, &task)?;
            if completed.receipt.model_id != turn.request["model_id"].as_str().unwrap_or_default() {
                return Err("owner_research_receipt_conflict".into());
            }
            validate_artifact_clock(&completed, turn)?;
            let outcome = if completed.failure.is_some() {
                "failed"
            } else {
                "recovered_research_receipt"
            };
            complete_log(root, turn, &completed, now, outcome)?;
        }
        Err(error) if error.to_string() == "owner_research_source_missing" => {
            let actions = session::checked_actions_for(root, &turn.session, &turn.id)?;
            if !actions.is_empty() {
                append_result_only(root, turn, now, actions)?;
                return Ok(true);
            }
            let log = log_for(root, turn);
            if log.result_for(&turn.id).is_some() {
                reconcile_research(root, turn);
                return Ok(true);
            }
            log.append(
                now,
                session::Body::Result {
                    result: session::TurnResult {
                        turn_id: turn.id.clone(),
                        finished_at_ms: now,
                        outcome: "interrupted".into(),
                        reason: Some("owner_research_interrupted".into()),
                        reply: String::new(),
                        actions: Vec::new(),
                    },
                },
            )?;
            reconcile_research(root, turn);
        }
        Err(error) => return Err(error),
    }
    Ok(true)
}

pub(super) fn settings_cutoff(c: &Config, cfg: &PlannerConfig, session_end_ms: i64) -> Result<i64> {
    let settings = cfg
        .owner_tasks
        .as_ref()
        .and_then(|settings| settings.research.as_ref())
        .filter(|research| research.enabled)
        .ok_or("owner_research_disabled")?;
    settings.validate(cfg)?;
    if settings.cutoff_minute <= c.execution.session_end_minute {
        return Err("owner_research_cutoff_invalid".into());
    }
    Ok(session_end_ms.saturating_add(
        (i64::from(settings.cutoff_minute) - i64::from(c.execution.session_end_minute))
            .saturating_mul(60_000),
    ))
}

pub(crate) struct EvidenceProjection {
    pub(crate) evidence: Vec<ResearchEvidenceV1>,
    pub(crate) unknown: Vec<&'static str>,
}

fn attempted_evidence_ids(c: &Config, session: &str) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    // Legacy trading-log research turns still count as attempted.
    let records = session::SessionLog::new(&c.directory, session)
        .records()
        .into_iter()
        .chain(lane_log(&c.directory, session).records());
    for record in records {
        let session::Body::Turn { turn } = record.body else {
            continue;
        };
        if turn.trigger != "research" && turn.request.get("owner_task").is_none() {
            continue;
        }
        let Some(task) = OwnerTask::from_turn(&turn)? else {
            continue;
        };
        if let Some(identity) = task.research_identity() {
            ids.insert(identity.evidence_id.clone());
        }
    }
    Ok(ids)
}

fn candidate_evidence(
    db: &mut rusqlite::Connection,
    session: &str,
    evidence: &mut Vec<ResearchEvidenceV1>,
    unknown: &mut Vec<&'static str>,
) {
    let records = match jobs::list_candidates(db, session, None, 100) {
        Ok(records) => records,
        Err(_) => {
            unknown.push("research_queue_unavailable");
            return;
        }
    };
    for record in records {
        match record.status {
            jobs::TaskState::Succeeded => {
                match super::super::harness::research::read_verified_candidate_result(
                    db,
                    &record.spec.task_id,
                    record.spec.revision,
                ) {
                    Ok(Some(result)) => evidence.push(ResearchEvidenceV1 {
                        version: 1,
                        kind: ResearchEvidenceKind::CandidateResult,
                        source_session: record.spec.session.clone(),
                        task_id: record.spec.task_id.clone(),
                        revision: record.spec.revision,
                        result_sha256: result.report_sha256.clone(),
                        lifecycle_candidate_id: Some(result.lifecycle_candidate_id.clone()),
                        maturity_state: None,
                        workspace_refs: Vec::new(),
                    }),
                    Ok(None) => {}
                    Err(_) => unknown.push("research_source_changed"),
                }
            }
            jobs::TaskState::Terminated | jobs::TaskState::Cancelled | jobs::TaskState::Refused => {
                match super::super::harness::research::read_candidate_stopped_failure(
                    db,
                    &record.spec.task_id,
                    record.spec.revision,
                ) {
                    Ok(Some(stopped)) => {
                        if stopped.cause == jobs::CandidateStopCause::OperatorCancel {
                            continue;
                        }
                        let hash = stopped.result_sha256.clone().unwrap_or_else(|| {
                            digest(&serde_json::to_vec(&stopped).unwrap_or_default())
                        });
                        evidence.push(ResearchEvidenceV1 {
                            version: 1,
                            kind: ResearchEvidenceKind::Failure,
                            source_session: record.spec.session.clone(),
                            task_id: record.spec.task_id.clone(),
                            revision: record.spec.revision,
                            result_sha256: hash,
                            lifecycle_candidate_id: Some(stopped.candidate_id),
                            maturity_state: Some(
                                format!("{:?}/{:?}", stopped.cause, stopped.outcome)
                                    .to_ascii_lowercase(),
                            ),
                            workspace_refs: Vec::new(),
                        });
                    }
                    Ok(None) => {}
                    Err(_) => unknown.push("research_source_changed"),
                }
            }
            _ => {}
        }
    }
}

fn registered_evidence(
    db: &mut rusqlite::Connection,
    session: &str,
    evidence: &mut Vec<ResearchEvidenceV1>,
    unknown: &mut Vec<&'static str>,
) {
    let records = match jobs::list_scoped(db, session, None, 100) {
        Ok(records) => records,
        Err(_) => {
            unknown.push("research_queue_unavailable");
            return;
        }
    };
    for record in records {
        if record.status != jobs::TaskState::Succeeded {
            continue;
        }
        match sadr_research::worker::read_verified_result(
            db,
            &record.spec.task_id,
            record.spec.revision,
        ) {
            Ok(Some(result)) => evidence.push(ResearchEvidenceV1 {
                version: 1,
                kind: ResearchEvidenceKind::RegisteredResult,
                source_session: record.spec.session.clone(),
                task_id: record.spec.task_id.clone(),
                revision: record.spec.revision,
                result_sha256: result.result_hash,
                lifecycle_candidate_id: None,
                maturity_state: None,
                workspace_refs: Vec::new(),
            }),
            Ok(None) => {}
            Err(_) => unknown.push("research_source_changed"),
        }
    }
}

fn maturity_evidence(
    c: &Config,
    cfg: &PlannerConfig,
    session: &str,
    evidence: &mut Vec<ResearchEvidenceV1>,
    unknown: &mut Vec<&'static str>,
) {
    let library = match super::super::experiment_ledger::lifecycle::library(&c.directory, session) {
        Ok(library) => library,
        Err(_) => {
            unknown.push("research_lifecycle_unavailable");
            return;
        }
    };
    for item in library["items"].as_array().into_iter().flatten() {
        let Some(state) = item["state"].as_str() else {
            continue;
        };
        if !matches!(state, "eligible" | "rejected" | "insufficient" | "retired") {
            continue;
        }
        let Some(candidate_id) = item["candidate_id"].as_str() else {
            continue;
        };
        let Some(hash) = item["result_sha256"]
            .as_str()
            .filter(|hash| is_sha256(hash))
        else {
            unknown.push("research_source_invalid");
            continue;
        };
        evidence.push(ResearchEvidenceV1 {
            version: 1,
            kind: ResearchEvidenceKind::Maturity,
            source_session: session.to_owned(),
            task_id: candidate_id.to_owned(),
            revision: 1,
            result_sha256: hash.to_owned(),
            lifecycle_candidate_id: Some(candidate_id.to_owned()),
            maturity_state: Some(state.to_owned()),
            workspace_refs: Vec::new(),
        });
    }
    // A CURRENT cross-session continuation is a new terminal fact for the
    // ORIGINAL episode. Its evidence id is derived only from immutable source
    // facts, so a later read, a new admission or current exposure cannot drift
    // it. The existing attempted-id set still enforces one research turn.
    let continuations = match super::super::harness::research::verified_continuations(c, cfg) {
        Ok(continuations) => continuations,
        Err(_) => {
            unknown.push("research_lifecycle_unavailable");
            return;
        }
    };
    for continuation in continuations {
        let Some(candidate_id) = continuation["source_lifecycle_candidate_id"].as_str() else {
            continue;
        };
        let Some(task_id) = continuation["source_task_id"].as_str() else {
            continue;
        };
        let Some(revision) = continuation["source_revision"]
            .as_u64()
            .and_then(|revision| u32::try_from(revision).ok())
        else {
            continue;
        };
        let Some(hash) = continuation["source_result_sha256"]
            .as_str()
            .filter(|hash| is_sha256(hash))
        else {
            unknown.push("research_source_invalid");
            continue;
        };
        let state = continuation["state"].as_str().unwrap_or("unknown");
        evidence.push(ResearchEvidenceV1 {
            version: 1,
            kind: ResearchEvidenceKind::Maturity,
            source_session: continuation["source_session"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            task_id: task_id.to_owned(),
            revision,
            result_sha256: hash.to_owned(),
            lifecycle_candidate_id: Some(candidate_id.to_owned()),
            maturity_state: Some(format!("continuation_{state}")),
            workspace_refs: Vec::new(),
        });
    }
}

/// Pure read-only projection of terminal verified evidence. It creates no
/// queue, task, lease, file or provider call. Evidence ids already attempted in
/// this session are excluded, so one attempt per original id is enforced even
/// across compaction and recovery.
pub(crate) fn due_evidence(c: &Config, cfg: &PlannerConfig) -> Result<EvidenceProjection> {
    let settings = cfg
        .owner_tasks
        .as_ref()
        .and_then(|settings| settings.research.as_ref())
        .filter(|research| research.enabled)
        .ok_or("owner_research_disabled")?;
    settings.validate(cfg)?;
    let session = c.session.to_string();
    let attempted = attempted_evidence_ids(c, &session)?;
    let mut evidence = Vec::new();
    let mut unknown = Vec::new();
    if let Some(research_settings) = cfg.research_tools.as_ref() {
        match super::super::harness::research::open_queue_for_carry(
            research_settings,
            &c.directory.join("model"),
        ) {
            Ok(mut db) => {
                candidate_evidence(&mut db, &session, &mut evidence, &mut unknown);
                registered_evidence(&mut db, &session, &mut evidence, &mut unknown);
            }
            Err(_) => unknown.push("research_queue_unavailable"),
        }
    }
    maturity_evidence(c, cfg, &session, &mut evidence, &mut unknown);
    // Validate every projected record before admission or any paid call. A
    // malformed hash, session, revision or workspace ref is an explicit unknown
    // and never becomes a readable evidence id.
    let mut admitted = Vec::with_capacity(evidence.len());
    for item in evidence {
        if item.validate().is_err() {
            unknown.push("research_source_invalid");
            continue;
        }
        admitted.push(item);
    }
    let mut evidence = admitted;
    unknown.sort_unstable();
    unknown.dedup();
    let mut seen = BTreeSet::new();
    evidence.retain(|item| {
        if attempted.contains(&item.evidence_id().unwrap_or_default()) {
            return false;
        }
        item.evidence_id().is_ok_and(|id| seen.insert(id))
    });
    evidence.sort_by(|left, right| {
        left.kind_order().cmp(&right.kind_order()).then_with(|| {
            left.evidence_id()
                .unwrap_or_default()
                .cmp(&right.evidence_id().unwrap_or_default())
        })
    });
    evidence.truncate(settings.max_evidence);
    Ok(EvidenceProjection { evidence, unknown })
}

pub(super) fn pinned_workspace(
    c: &Config,
    cfg: &PlannerConfig,
    evidence: &mut ResearchEvidenceV1,
) -> Result<Vec<Value>> {
    (|| -> Result<Vec<Value>> {
        let seed = session::seed_for_request(&c.directory, &c.session.to_string())?;
        let Some(references) = seed["workspace_state"]["refs"].as_array() else {
            return Ok(Vec::new());
        };
        if references.is_empty() {
            return Ok(Vec::new());
        }
        let workspace = cfg
            .workspace
            .as_ref()
            .filter(|workspace| workspace.enabled)
            .ok_or("owner_research_workspace_disabled")?;
        let mut pinned = Vec::new();
        for raw in references.iter().take(16) {
            let reference: super::super::workspace::WorkspaceRefV1 =
                serde_json::from_value(raw.clone())
                    .map_err(|_| "owner_research_workspace_invalid")?;
            reference.validate()?;
            let root = if reference.source_session == c.session.to_string() {
                c.directory.clone()
            } else {
                let sessions = c
                    .sessions_root
                    .as_ref()
                    .filter(|root| root.is_absolute())
                    .ok_or("owner_research_workspace_source_invalid")?;
                let sessions_meta = fs::symlink_metadata(sessions)
                    .map_err(|_| "owner_research_workspace_source_missing")?;
                if sessions_meta.file_type().is_symlink() || !sessions_meta.is_dir() {
                    return Err("owner_research_workspace_source_invalid".into());
                }
                let source = sessions.join(&reference.source_session);
                let meta = fs::symlink_metadata(&source)
                    .map_err(|_| "owner_research_workspace_source_missing")?;
                if !meta.is_dir() || meta.file_type().is_symlink() {
                    return Err("owner_research_workspace_source_invalid".into());
                }
                let identity = crate::read_bounded(&source.join("book-identity.json"), 65_536)
                    .map_err(|_| "owner_research_workspace_source_invalid")?;
                let identity: Value = serde_json::from_slice(&identity)
                    .map_err(|_| "owner_research_workspace_source_invalid")?;
                if identity["binding"].as_str() != Some(book_binding(c)?.as_str()) {
                    return Err("owner_research_workspace_source_invalid".into());
                }
                source
            };
            let value = super::super::workspace::read_pinned(&root, workspace, &reference)?;
            pinned.push(value);
        }
        evidence.workspace_refs = references
            .iter()
            .take(16)
            .filter_map(|raw| serde_json::from_value(raw.clone()).ok())
            .collect();
        Ok(pinned)
    })()
    .map_err(|error| super::research_boundary_reason(&error.to_string()).into())
}

/// The lane's own counters: turns and starts from the lane log, calls and
/// provider tokens from the separate `research` usage bucket.
struct LaneUsage {
    turns: usize,
    last_started_ms: Option<i64>,
    calls: u64,
    tokens: u64,
}

fn lane_usage(c: &Config, session: &str) -> Result<LaneUsage> {
    let turns = lane_log(&c.directory, session).turns();
    let (calls, tokens) = if c.directory.join("model/usage-identity.json").exists() {
        let calls = planner::usage(c)?["research"].as_u64().unwrap_or(0);
        let journal = planner::usage_directory(c, true)?;
        (calls, planner::bucket_tokens(&journal, "research")?)
    } else {
        (0, 0)
    };
    Ok(LaneUsage {
        turns: turns.len(),
        last_started_ms: turns.iter().map(|turn| turn.started_at_ms).max(),
        calls,
        tokens,
    })
}

fn lane_cap(lane: &owner_tasks::ResearchLaneSettings, usage: &LaneUsage) -> Option<&'static str> {
    if usage.turns >= lane.max_turns_per_session as usize {
        Some("research_lane_turn_budget_exhausted")
    } else if usage.calls >= u64::from(lane.max_calls_per_session) {
        Some("research_lane_call_budget_exhausted")
    } else if usage.tokens >= lane.max_tokens_per_session {
        Some("research_lane_token_budget_exhausted")
    } else {
        None
    }
}

/// The market close while the market is open; research waits for it unless
/// the lane profile opts into running during market hours.
fn market_window(c: &Config, now: i64, session_end_ms: i64) -> Option<i64> {
    let opens = market_open_ms(c, session_end_ms);
    (now >= opens && now < session_end_ms).then_some(session_end_ms)
}

fn market_open_ms(c: &Config, session_end_ms: i64) -> i64 {
    let open_minutes = i64::from(
        c.execution
            .session_end_minute
            .saturating_sub(c.execution.session_start_minute),
    );
    session_end_ms.saturating_sub(open_minutes.saturating_mul(60_000))
}

/// The quiet-trading trigger (#244): market open, configured and no fill for
/// `quiet_minutes`. It adds a reason to start research, never a bypass.
fn idle_due(
    c: &Config,
    cfg: &PlannerConfig,
    now: i64,
    session_end_ms: i64,
    last_fill_ms: Option<i64>,
) -> bool {
    let Some(idle) = cfg
        .owner_tasks
        .as_ref()
        .and_then(|tasks| tasks.research.as_ref())
        .and_then(|research| research.idle_trigger)
    else {
        return false;
    };
    let open = market_window(c, now, session_end_ms).map(|_| market_open_ms(c, session_end_ms));
    idle.quiet(now, open, last_fill_ms)
}

/// The host-authored request an idle-triggered turn serves. It goes through
/// the same durable queue, capacity and claim as a trading request.
fn idle_request(
    c: &Config,
    cfg: &PlannerConfig,
    seq: u64,
    now: i64,
) -> Result<lane_queue::LaneMessage> {
    let lane = lane_queue::lane_settings(cfg).ok_or("research_lane_settings_missing")?;
    let quiet = cfg
        .owner_tasks
        .as_ref()
        .and_then(|tasks| tasks.research.as_ref())
        .and_then(|research| research.idle_trigger)
        .map_or(0, |idle| idle.quiet_minutes);
    let body = MessageBody::ResearchRequest {
        question: format!(
            "Host idle trigger: no paper fill for at least {quiet} minutes during market hours. Use the quiet time for advisory research that could improve the next paper decisions: review the workspace scripts and recent evidence, test a hypothesis, or prepare a candidate. Paper only; no trade authority."
        ),
        refs: Vec::new(),
    };
    let mut db = lane_queue::open(&c.directory)?;
    let posted = lane_queue::post(
        &mut db,
        &c.session.to_string(),
        &format!("idle-trigger-{seq}"),
        &body,
        now,
        lane.request_ttl_ms,
        lane.max_open_requests,
    )?;
    eprintln!("research_lane_idle_trigger quiet_minutes={quiet}");
    Ok(posted.message)
}

/// Everything admission and the dashboard both project, read once.
struct LaneView {
    input: ResearchScheduleInput,
    projection: EvidenceProjection,
    requests: Vec<lane_queue::LaneMessage>,
}

fn lane_view(
    c: &Config,
    cfg: &PlannerConfig,
    now: i64,
    session_end_ms: i64,
    book_runnable: bool,
    last_fill_ms: Option<i64>,
) -> Result<LaneView> {
    let lane = lane_queue::lane_settings(cfg).ok_or("research_lane_settings_missing")?;
    let cutoff = settings_cutoff(c, cfg, session_end_ms)?;
    let session = c.session.to_string();
    // A corrupt or unreadable lane log is never mistaken for idle (#233).
    let checked = lane_log(&c.directory, &session).pending_checked();
    if checked.turn.is_none() && !checked.idle() {
        return Err("research_lane_log_unavailable".into());
    }
    let pending = checked.turn.is_some();
    let usage = lane_usage(c, &session)?;
    let requests = {
        let db = lane_queue::open(&c.directory)?;
        lane_queue::list(
            &db,
            &session,
            &[MessageKind::ResearchRequest],
            MessageState::Queued,
            now,
            lane.max_open_requests,
        )?
    };
    let projection = due_evidence(c, cfg)?;
    let input = ResearchScheduleInput {
        enabled: true,
        pending,
        book_runnable,
        capped: lane_cap(lane, &usage),
        window_opens_ms: if lane.during_market {
            None
        } else {
            market_window(c, now, session_end_ms)
        },
        next_allowed_ms: usage
            .last_started_ms
            .map(|started| started.saturating_add(lane.min_interval_ms)),
        requests: requests.len(),
        idle_due: idle_due(c, cfg, now, session_end_ms, last_fill_ms),
        source_error: projection
            .unknown
            .first()
            .map(|reason| super::research_boundary_reason(reason)),
        evidence_ids: projection
            .evidence
            .iter()
            .filter_map(|evidence| evidence.evidence_id().ok())
            .collect(),
        unknown: projection
            .unknown
            .iter()
            .map(|reason| super::research_boundary_reason(reason))
            .collect(),
        now_ms: now,
        cutoff_ms: cutoff,
    };
    Ok(LaneView {
        input,
        projection,
        requests,
    })
}

/// Evidence for one queued trading request: the request body hash binds it.
fn request_evidence(
    session: &str,
    request: &lane_queue::LaneMessage,
) -> Result<ResearchEvidenceV1> {
    Ok(ResearchEvidenceV1 {
        version: 1,
        kind: ResearchEvidenceKind::TradingRequest,
        source_session: session.to_owned(),
        task_id: request.id.clone(),
        revision: 1,
        result_sha256: request.body.sha256()?,
        lifecycle_candidate_id: None,
        maturity_state: None,
        workspace_refs: Vec::new(),
    })
}

/// Short lane-only history for the next research prompt. Research never sees
/// the trading conversation; trading never sees this trace.
fn lane_history(log: &session::SessionLog) -> Vec<Value> {
    let mut results = log.results();
    let skip = results.len().saturating_sub(LANE_HISTORY);
    results
        .drain(skip..)
        .map(|result| {
            let summary = serde_json::from_str::<ResearchOutputV1>(&result.reply)
                .map(|output| output.summary.chars().take(300).collect::<String>())
                .unwrap_or_default();
            json!({"turn_id":result.turn_id,"outcome":result.outcome,"reason":result.reason,"summary":summary})
        })
        .collect()
}

/// The research lane slot. It is separate from the trading worker and the
/// aside slot, so research in flight never holds a trading decision.
#[derive(Default)]
pub(crate) struct Lane {
    pub(crate) worker: Option<Worker>,
    settled: bool,
}

impl Lane {
    pub(crate) fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.cancel.store(true, Ordering::Release);
        }
    }
}

/// One research lane tick: finish a returned turn, settle the queue, and admit
/// at most one new turn. Every error stays inside the lane; nothing here
/// writes trading supervisor state, parks trading or arms its backoff.
pub(crate) fn tick_lane(
    c: &Config,
    cfg: &PlannerConfig,
    lane: &mut Lane,
    now: i64,
    session_end_ms: i64,
    book_runnable: bool,
    last_fill_ms: Option<i64>,
) {
    if lane_queue::lane_settings(cfg).is_none() {
        lane.stop();
        return;
    }
    if poll_lane(&c.directory, lane, now) {
        return;
    }
    if lane.worker.is_some() {
        return;
    }
    if !lane.settled {
        match settle_lane(&c.directory, &c.session.to_string(), now) {
            Ok(()) => lane.settled = true,
            Err(error) => {
                record_lane_failure(
                    &c.directory,
                    &c.session.to_string(),
                    "",
                    now,
                    &error.to_string(),
                );
                return;
            }
        }
    }
    match consider_lane(c, cfg, now, session_end_ms, book_runnable, last_fill_ms) {
        Ok(Some(worker)) => lane.worker = Some(worker),
        Ok(None) => {}
        Err(error) => {
            let reason = super::super::failure_code(error.as_ref());
            record_lane_failure(&c.directory, &c.session.to_string(), "", now, &reason);
        }
    }
}

/// Finish a returned lane turn. True when a turn finished this tick.
pub(super) fn poll_lane(root: &Path, lane: &mut Lane, now: i64) -> bool {
    let Some(worker) = lane.worker.as_ref() else {
        return false;
    };
    let reply = match worker.receiver.try_recv() {
        Ok(reply) => reply,
        Err(mpsc::TryRecvError::Empty) => return false,
        Err(mpsc::TryRecvError::Disconnected) => Err("research_lane_worker_disconnected".into()),
    };
    let worker = lane.worker.take().expect("lane worker present");
    match reply {
        Ok(raw) => match reply_text(&raw).filter(|text| !text.trim().is_empty()) {
            Some(text) => {
                finish_reply(root, &worker, now, &text);
            }
            None => {
                finish_failed(root, &worker, now, "owner_research_output_invalid");
            }
        },
        Err(reason) => {
            // The lane's own call allowance ran out. This is a lane cap, not a
            // trading budget event, and it never parks trading.
            let reason = if reason == "planner_budget_exhausted" {
                "research_lane_call_budget_exhausted".to_owned()
            } else {
                reason
            };
            finish_failed(root, &worker, now, &reason);
        }
    }
    true
}

/// Re-derive request states and result messages from the lane log.
fn settle_lane(root: &Path, session: &str, now: i64) -> Result<()> {
    let log = lane_log(root, session);
    let turns = log.turns();
    if turns.is_empty() {
        return Ok(());
    }
    let pending = log.pending().map(|turn| turn.id);
    let mut db = lane_queue::open(root)?;
    let running = lane_queue::list(
        &db,
        session,
        &[MessageKind::ResearchRequest],
        MessageState::Running,
        now,
        usize::MAX,
    )?;
    for request in running {
        let Some(turn_id) = request.turn_id.as_deref() else {
            continue;
        };
        if pending.as_deref() == Some(turn_id) {
            continue;
        }
        let (to, reason) = match log.result_for(turn_id) {
            Some(result)
                if matches!(result.outcome.as_str(), "ok" | "recovered_research_receipt") =>
            {
                (MessageState::Done, TransitionReason::Completed)
            }
            Some(result) if result.outcome == "failed" => {
                (MessageState::Failed, TransitionReason::TurnFailed)
            }
            // Interrupted, or claimed by a turn that never became durable.
            _ => (MessageState::Failed, TransitionReason::Interrupted),
        };
        lane_queue::transition(
            &mut db,
            &request.id,
            MessageState::Running,
            to,
            reason,
            None,
            now,
        )?;
    }
    for turn in turns {
        let Some(result) = log.result_for(&turn.id) else {
            continue;
        };
        if !matches!(result.outcome.as_str(), "ok" | "recovered_research_receipt") {
            continue;
        }
        let Some(task) = OwnerTask::from_turn(&turn)? else {
            continue;
        };
        let Ok(output) = serde_json::from_str::<ResearchOutputV1>(&result.reply) else {
            continue;
        };
        let identity = task
            .research_identity()
            .ok_or("owner_research_binding_invalid")?;
        let research_id = identity.id()?;
        let request_id = task
            .research_evidence()
            .filter(|evidence| evidence.kind == ResearchEvidenceKind::TradingRequest)
            .map(|evidence| evidence.task_id.clone());
        let body = MessageBody::ResearchResult {
            request_id,
            summary: result_summary(&output.summary),
            artifact_ref: format!("model/research/{}.json", task.id()?),
            output_sha256: digest(&serde_json::to_vec(&output)?),
            research_id: research_id.clone(),
        };
        lane_queue::post(
            &mut db,
            session,
            &research_id,
            &body,
            now,
            RESULT_TTL_MS,
            usize::MAX,
        )?;
    }
    Ok(())
}

/// A bounded, redacted one-paragraph summary for the trading lane.
fn result_summary(summary: &str) -> String {
    let redacted = super::transcript::redact(&json!(summary));
    let text: String = redacted
        .as_str()
        .unwrap_or_default()
        .chars()
        .map(|ch| {
            if ch.is_control() && ch != '\n' {
                ' '
            } else {
                ch
            }
        })
        .take(600)
        .collect();
    if text.trim().is_empty() {
        "Research completed; see the artifact.".into()
    } else {
        text
    }
}

fn record_lane_failure(root: &Path, session: &str, turn_id: &str, now: i64, reason: &str) {
    eprintln!("research_lane_failed reason={reason}");
    let body = json!({"version":1,"session":session,"turn_id":turn_id,"reason":reason,"at_ms":now});
    if rollover::atomic(&root.join(LANE_FAILURE_PATH), &body).is_err() {
        eprintln!("research_lane_failure_unrecorded reason={reason}");
    }
}

pub(super) fn consider_lane(
    c: &Config,
    cfg: &PlannerConfig,
    now: i64,
    session_end_ms: i64,
    book_runnable: bool,
    last_fill_ms: Option<i64>,
) -> Result<Option<Worker>> {
    let lane = lane_queue::lane_settings(cfg)
        .ok_or("research_lane_settings_missing")?
        .clone();
    let session = c.session.to_string();
    let log = lane_log(&c.directory, &session);
    if let Some(pending) = log.pending() {
        // A lane turn left pending by a crash is closed without replay; its
        // claimed request fails as interrupted on the next settle.
        recover(&c.directory, &pending, now)?;
        settle_lane(&c.directory, &session, now)?;
        return Ok(None);
    }
    lane_queue::expire(&mut lane_queue::open(&c.directory)?, &session, now)?;
    let view = lane_view(c, cfg, now, session_end_ms, book_runnable, last_fill_ms)?;
    let schedule = owner_tasks::project_research(&view.input);
    if !schedule.research_due {
        return Ok(None);
    }
    let cutoff = view.input.cutoff_ms;
    let seq = log.last_seq().saturating_add(1);
    let turn_id = format!("research-{session}-{seq}");
    let mut requests = view.requests;
    if requests.is_empty()
        && view.projection.evidence.is_empty()
        && schedule.research_reason == Some(owner_tasks::IDLE_TRIGGER_REASON)
    {
        requests.push(idle_request(c, cfg, seq, now)?);
    }
    let (mut evidence, request) = match requests.into_iter().next() {
        Some(request) => (request_evidence(&session, &request)?, Some(request)),
        None => match view.projection.evidence.into_iter().next() {
            Some(evidence) => (evidence, None),
            None => return Ok(None),
        },
    };
    evidence.validate()?;
    let pinned = pinned_workspace(c, cfg, &mut evidence)?;
    let evidence_id = evidence.evidence_id()?;
    let identity = ResearchIdentity {
        version: 1,
        session: session.clone(),
        evidence_id: evidence_id.clone(),
        policy_sha256: digest(&planner::policy_bytes(cfg)?),
        prompt_version: digest(owner_tasks::RESEARCH_PROMPT.as_bytes()),
    };
    let task = OwnerTask::Research {
        identity: identity.clone(),
        evidence,
    };
    let (mut role, path, _, selection) = role_and_model(c, cfg)?;
    role.max_turns = Some(lane.max_tool_turns as usize);
    role.daily_call_budget = lane.max_calls_per_session;
    role.budget_unit = None;
    if let Some(model) = lane.model_id.as_ref() {
        role.model_id = model.clone();
    }
    planner::prepare_review_usage(c)?;
    let mut facts = json!({
        "session":session,
        "lane":"research",
        "trigger":"research",
        "now_ms":now,
        "session_end_ms":session_end_ms,
        "research_history":lane_history(&log),
        "active_directives":directives::active(&c.directory, now, "any"),
        "owner_task":task,
        "research_id":identity.id()?,
        "evidence_id":evidence_id,
        "evidence":serde_json::to_value(task.research_evidence())?,
        "workspace_refs":pinned,
        "turn_id":turn_id,
        "cutoff_ms":cutoff,
        "model_id":role.model_id,
    });
    if let Some(request) = request.as_ref() {
        facts["research_request"] = json!({"message_id":request.id,"body":request.body});
    }
    session::add_seed_fact(&c.directory, &session, &mut facts)?;
    let selection_binding = selection_binding(&selection);
    if let Some(selection) = selection {
        facts["model_selection"] = selection;
    }
    facts = super::transcript::redact(&facts);
    let prompt = format!("{}\n{}", owner_tasks::RESEARCH_PROMPT, facts);
    let Ok((_, _, bounds)) = settings_for(cfg) else {
        return Err("supervisor_settings_invalid".into());
    };
    if !request_fits(escaped_prompt_bytes(&prompt), &bounds) {
        return Err("supervisor_context_limit".into());
    }
    // Claim before the durable turn: a crash in between leaves a running
    // request with no turn, which the next settle fails as interrupted.
    if let Some(request) = request.as_ref() {
        let mut db = lane_queue::open(&c.directory)?;
        if !lane_queue::transition(
            &mut db,
            &request.id,
            MessageState::Queued,
            MessageState::Running,
            TransitionReason::Claimed,
            Some(&turn_id),
            now,
        )? {
            return Ok(None);
        }
    }
    let turn = session::Turn {
        id: turn_id.clone(),
        session: session.clone(),
        trigger: "research".into(),
        prompt_version: identity.prompt_version.clone(),
        notes_version: 0,
        fills_at_start: 0,
        started_at_ms: now,
        request: facts.clone(),
    };
    if let Err(error) = log.append(now, session::Body::Turn { turn }) {
        if let Some(request) = request.as_ref() {
            let mut db = lane_queue::open(&c.directory)?;
            lane_queue::transition(
                &mut db,
                &request.id,
                MessageState::Running,
                MessageState::Failed,
                TransitionReason::TurnFailed,
                None,
                now,
            )?;
        }
        return Err(error);
    }
    let timeout_ms = lane
        .timeout_ms
        .min(cutoff.saturating_sub(now).max(1) as u64);
    // The run budget is the lane's, not the trading orchestrator's
    // `run_timeout_ms` (#243). Each request keeps the role's per-call timeout.
    role.run_timeout_ms = Some(timeout_ms.max(role.timeout_ms));
    Ok(Some(spawn(
        c,
        cfg,
        role,
        path,
        prompt,
        json!({"trigger":{"kind":"research","reason":"research"},"summary":facts}),
        "research",
        now,
        timeout_ms,
        AppliedHashes::default(),
        now,
        TurnRef {
            session,
            id: turn_id,
            notes_version: 0,
            prompt_version: identity.prompt_version.clone(),
            decision: None,
        },
        RunKind::Research,
        None,
        bounds,
        selection_binding,
    )))
}

fn monotonic_elapsed_ms(worker: &Worker) -> i64 {
    worker.started.elapsed().as_millis().min(i64::MAX as u128) as i64
}

fn research_clock(worker: &Worker, turn: &session::Turn, now: i64) -> ResearchClockV1 {
    ResearchClockV1 {
        started_wall_ms: turn.started_at_ms,
        observed_wall_ms: now,
        monotonic_elapsed_ms: monotonic_elapsed_ms(worker),
    }
}

pub(super) fn finish_reply(root: &Path, worker: &Worker, now: i64, raw: &str) -> bool {
    let result = (|| -> Result<()> {
        let (task, turn) = task_for(root, worker)?;
        if now < turn.started_at_ms {
            return Err("owner_research_clock_invalid".into());
        }
        if turn.request["cutoff_ms"]
            .as_i64()
            .is_some_and(|cutoff| now >= cutoff)
        {
            return Err("owner_research_expired".into());
        }
        let evidence = task
            .research_evidence()
            .ok_or("owner_research_binding_invalid")?;
        let pinned = evidence
            .workspace_refs
            .iter()
            .map(|reference| reference.path.clone())
            .collect::<BTreeSet<_>>();
        let output = ResearchOutputV1::validate(raw, &task, &pinned)?;
        let receipt = ResearchReceipt {
            version: 1,
            evidence_id: task
                .research_identity()
                .ok_or("owner_research_binding_invalid")?
                .evidence_id
                .clone(),
            output_sha256: digest(&serde_json::to_vec(&output)?),
            model_id: turn.request["model_id"]
                .as_str()
                .ok_or("owner_research_binding_invalid")?
                .into(),
            completed_ms: now,
            status: "completed".into(),
        };
        let completed = ResearchCompletedV1 {
            version: RESEARCH_COMPLETED_VERSION,
            task: task.clone(),
            receipt,
            output: Some(output),
            failure: None,
            evidence: worker.evidence(),
            clock: research_clock(worker, &turn, now),
        };
        persist(root, &task, &completed)?;
        complete_log(root, &turn, &completed, now, "ok")?;
        Ok(())
    })();
    match result {
        Ok(()) => true,
        Err(error) => {
            let reason = super::super::failure_code(error.as_ref());
            finish_failed(root, worker, now, &reason)
        }
    }
}

pub(super) fn finish_failed(root: &Path, worker: &Worker, now: i64, reason: &str) -> bool {
    let mut settled = false;
    let result = (|| -> Result<()> {
        let (task, turn) = task_for(root, worker)?;
        if source_present(root, &task)? {
            recover(root, &turn, now)?;
            settled = true;
            return Ok(());
        }
        let bounded = if reason.is_empty() || reason.len() > 160 {
            "owner_research_failed"
        } else {
            reason
        };
        let completed = ResearchCompletedV1 {
            version: RESEARCH_COMPLETED_VERSION,
            task: task.clone(),
            receipt: ResearchReceipt {
                version: 1,
                evidence_id: task
                    .research_identity()
                    .ok_or("owner_research_binding_invalid")?
                    .evidence_id
                    .clone(),
                output_sha256: String::new(),
                model_id: turn.request["model_id"]
                    .as_str()
                    .ok_or("owner_research_binding_invalid")?
                    .into(),
                completed_ms: now,
                status: "failed".into(),
            },
            output: None,
            failure: Some(bounded.into()),
            evidence: worker.evidence(),
            clock: research_clock(worker, &turn, now),
        };
        match persist(root, &task, &completed) {
            Ok(()) => complete_log(root, &turn, &completed, now, "failed"),
            Err(error) => {
                if source_present(root, &task)? {
                    recover(root, &turn, now)?;
                    settled = true;
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    })();
    // Lane failures stay in the lane's own record. They never touch the
    // trading supervisor state, its failure streak or its backoff (#238).
    if result.is_err() {
        record_lane_failure(
            root,
            &worker.session,
            &worker.turn_id,
            now,
            "owner_research_reconciliation_required",
        );
    } else if !settled {
        record_lane_failure(root, &worker.session, &worker.turn_id, now, reason);
    }
    settled
}

/// Read-only schedule projection: the same view the lane admits from, so the
/// dashboard never reports a due turn that admission would refuse.
pub(super) fn schedule(
    c: &Config,
    cfg: &PlannerConfig,
    now: i64,
    session_end_ms: i64,
    book_runnable: bool,
    last_fill_ms: Option<i64>,
) -> Result<owner_tasks::ResearchSchedule> {
    #[cfg(test)]
    if let Some(schedule) = super::test_research_schedule_override() {
        return Ok(schedule);
    }
    let view = lane_view(c, cfg, now, session_end_ms, book_runnable, last_fill_ms)?;
    // The SAME selected evidence and pinned snapshot that feed the research
    // frame also yield two public scalars: the bounded ref count and the
    // canonical hash of the pinned snapshot bytes. No path, ref array or file
    // text is retained. This runs once at the producer completion sample; the
    // dashboard GET only copies the captured schedule.
    let mut workspace_ref_count = None;
    let mut workspace_snapshot_sha256 = None;
    let selected = match view.requests.first() {
        Some(request) => Some(request_evidence(&c.session.to_string(), request)?),
        None => view.projection.evidence.first().cloned(),
    };
    if let Some(mut evidence) = selected {
        let pinned = pinned_workspace(c, cfg, &mut evidence)?;
        let count = evidence.workspace_refs.len();
        if count <= 16 {
            workspace_ref_count = Some(count);
            workspace_snapshot_sha256 = Some(digest(&canonical_bytes(&Value::Array(pinned))?));
        }
    }
    let mut schedule = owner_tasks::project_research(&view.input);
    schedule.workspace_ref_count = workspace_ref_count;
    schedule.workspace_snapshot_sha256 = workspace_snapshot_sha256;
    Ok(schedule)
}
