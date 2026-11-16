//! A sealed review is a one-attempt owner turn with no financial capabilities.
use super::super::owner_tasks::{
    self, OwnerReviewOutput, OwnerTask, ReviewIdentity, ReviewReceipt,
};
use super::*;
use sadr_core::postclose::SealedEvidence;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

/// The original artifact is small: one bounded review output plus receipt.
const MAX_ARTIFACT_BYTES: u64 = 1_000_000;
const MAX_ARTIFACT_WRITE_BYTES: usize = 262_144;
const MAX_REVIEW_REFS: usize = 8;

/// Frozen legacy wire type. Its field order and serialization are part of the
/// on-disk contract and must never gain or lose fields.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Completed {
    task: OwnerTask,
    receipt: ReviewReceipt,
    output: Option<OwnerReviewOutput>,
    failure: Option<String>,
    evidence: Value,
}

const REVIEW_COMPLETED_VERSION: u32 = 1;

/// Explicit clock evidence for a review artifact. Wall values are observations,
/// never `started + elapsed`; the monotonic reading is separate evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewClockAnomalyV1 {
    reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewClockV1 {
    started_wall_ms: i64,
    observed_wall_ms: i64,
    monotonic_elapsed_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clock_anomaly: Option<ReviewClockAnomalyV1>,
}

/// Versioned review artifact. It carries the observed wall and monotonic clock
/// evidence explicitly; old readers reject it and new readers still accept the
/// frozen legacy shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewCompletedV1 {
    version: u32,
    task: OwnerTask,
    receipt: ReviewReceipt,
    output: Option<OwnerReviewOutput>,
    failure: Option<String>,
    evidence: Value,
    clock: ReviewClockV1,
}

#[derive(Debug)]
enum CompletedArtifact {
    Legacy(Completed),
    V1(ReviewCompletedV1),
}

impl CompletedArtifact {
    fn task(&self) -> &OwnerTask {
        match self {
            Self::Legacy(completed) => &completed.task,
            Self::V1(completed) => &completed.task,
        }
    }
    fn receipt(&self) -> &ReviewReceipt {
        match self {
            Self::Legacy(completed) => &completed.receipt,
            Self::V1(completed) => &completed.receipt,
        }
    }
    fn output(&self) -> Option<&OwnerReviewOutput> {
        match self {
            Self::Legacy(completed) => completed.output.as_ref(),
            Self::V1(completed) => completed.output.as_ref(),
        }
    }
    fn failure(&self) -> Option<&str> {
        match self {
            Self::Legacy(completed) => completed.failure.as_deref(),
            Self::V1(completed) => completed.failure.as_deref(),
        }
    }
    fn output_json(&self) -> Result<String> {
        Ok(self
            .output()
            .map(serde_json::to_string)
            .transpose()?
            .unwrap_or_default())
    }
}

fn task_for(root: &Path, worker: &Worker) -> Result<(OwnerTask, session::Turn, SealedEvidence)> {
    let turn = session::SessionLog::new(root, &worker.session)
        .records()
        .into_iter()
        .find_map(|r| {
            if let session::Body::Turn { turn } = r.body {
                (turn.id == worker.turn_id).then_some(turn)
            } else {
                None
            }
        })
        .ok_or("owner_review_turn_missing")?;
    let task = OwnerTask::from_turn(&turn)?.ok_or("owner_review_binding_invalid")?;
    if task.review_identity().is_none() {
        return Err("owner_review_binding_invalid".into());
    }
    let sealed = serde_json::from_value(turn.request["sealed_evidence"].clone())
        .map_err(|_| "owner_review_binding_invalid")?;
    Ok((task, turn, sealed))
}
#[cfg(test)]
fn artifact_path(root: &Path, task: &OwnerTask) -> Result<std::path::PathBuf> {
    Ok(root.join("model/reviews").join(format!(
        "{}.json",
        task.review_identity().expect("review task").id()?
    )))
}

fn current_uid() -> u32 {
    fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .unwrap_or(u32::MAX)
}

fn open_root(root: &Path) -> Result<fs::File> {
    let flags =
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC;
    rustix::fs::open(root, flags, rustix::fs::Mode::empty())
        .map(fs::File::from)
        .map_err(|_| "owner_review_path_invalid".into())
}

fn open_directory_at(parent: &fs::File, name: &str, create: bool) -> Result<(fs::File, bool)> {
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let name = std::ffi::OsStr::new(name);
    match rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty()) {
        Ok(fd) => Ok((fs::File::from(fd), false)),
        Err(error) if error == rustix::io::Errno::NOENT && create => {
            let created = match rustix::fs::mkdirat(
                parent,
                name,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
            ) {
                Ok(()) => true,
                Err(error) if error == rustix::io::Errno::EXIST => false,
                Err(_) => return Err("owner_review_path_invalid".into()),
            };
            rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty())
                .map(|fd| (fs::File::from(fd), created))
                .map_err(|_| "owner_review_path_invalid".into())
        }
        Err(error) if error == rustix::io::Errno::NOENT => {
            Err("owner_review_source_missing".into())
        }
        Err(_) => Err("owner_review_path_invalid".into()),
    }
}

/// Open `model/reviews` from the data directory without following a symlinked
/// ancestor. An existing directory must already be 0700; only a directory this
/// call created is chmodded.
pub(super) fn open_reviews(root: &Path, create: bool) -> Result<fs::File> {
    let root_directory = open_root(root)?;
    let (model, _) = open_directory_at(&root_directory, "model", create)?;
    let (reviews, created) = open_directory_at(&model, "reviews", create)?;
    if created {
        reviews
            .set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(|_| "owner_review_path_invalid")?;
    }
    let metadata = reviews
        .metadata()
        .map_err(|_| "owner_review_path_invalid")?;
    if !metadata.is_dir() || metadata.mode() & 0o777 != 0o700 || metadata.uid() != current_uid() {
        return Err("owner_review_path_invalid".into());
    }
    Ok(reviews)
}

struct Source {
    file: fs::File,
    directory: fs::File,
    bytes: Vec<u8>,
}

fn read_source(root: &Path, task: &OwnerTask) -> Result<Source> {
    let directory = open_reviews(root, false)?;
    let name = format!(
        "{}.json",
        task.review_identity().expect("review task").id()?
    );
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC;
    let fd = match rustix::fs::openat(
        &directory,
        std::ffi::OsStr::new(&name),
        flags,
        rustix::fs::Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(error) if error == rustix::io::Errno::NOENT => {
            return Err("owner_review_source_missing".into());
        }
        Err(_) => return Err("owner_review_source_invalid".into()),
    };
    let file = fs::File::from(fd);
    let metadata = file.metadata().map_err(|_| "owner_review_source_invalid")?;
    if !metadata.is_file()
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != current_uid()
        || metadata.nlink() != 1
        || metadata.len() > MAX_ARTIFACT_BYTES
    {
        return Err("owner_review_source_invalid".into());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "owner_review_source_invalid")?;
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err("owner_review_source_invalid".into());
    }
    Ok(Source {
        file,
        directory,
        bytes,
    })
}

fn sync_file_and_directory(file: &fs::File, directory: &fs::File) -> Result<()> {
    #[cfg(test)]
    if source_sync_fails() {
        return Err("owner_review_source_unsynced".into());
    }
    file.sync_all()
        .map_err(|_| "owner_review_source_unsynced")?;
    directory
        .sync_all()
        .map_err(|_| "owner_review_source_unsynced")?;
    Ok(())
}

fn source_present(root: &Path, task: &OwnerTask) -> Result<bool> {
    match read_source(root, task) {
        Ok(_) => Ok(true),
        Err(error) if error.to_string() == "owner_review_source_missing" => Ok(false),
        Err(error) => Err(error),
    }
}

/// Recursively sort every object key. `serde_json` may be built with
/// `preserve_order`, where `Value` keeps insertion order, so canonical bytes
/// cannot rely on the map implementation. Values, including floats, are kept
/// exactly as parsed; only object key order changes.
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

fn canonical_bytes(value: &Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&canonical_value(value.clone()))?)
}

/// Host-written artifacts are canonical JSON. Any byte change, including
/// trailing whitespace or reordered keys, is tampering rather than a valid
/// receipt. Legacy bytes are checked against the frozen legacy type; V1 bytes
/// are checked against the recursively sorted Value so no input order is
/// accepted. Unknown fields still refuse for this V1 shape.
fn parse_completed(bytes: &[u8]) -> Result<CompletedArtifact> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "owner_review_source_invalid")?;
    match value.get("version") {
        Some(version) => {
            if version.as_u64() != Some(u64::from(REVIEW_COMPLETED_VERSION)) {
                return Err("owner_review_artifact_version_unsupported".into());
            }
            let completed: ReviewCompletedV1 =
                serde_json::from_value(value.clone()).map_err(|_| "owner_review_source_invalid")?;
            let canonical = canonical_bytes(&value).map_err(|_| "owner_review_source_invalid")?;
            if canonical != bytes {
                return Err("owner_review_source_invalid".into());
            }
            Ok(CompletedArtifact::V1(completed))
        }
        None => {
            let completed: Completed =
                serde_json::from_slice(bytes).map_err(|_| "owner_review_source_invalid")?;
            if serde_json::to_vec(&completed).map_err(|_| "owner_review_source_invalid")? != bytes {
                return Err("owner_review_source_invalid".into());
            }
            Ok(CompletedArtifact::Legacy(completed))
        }
    }
}

fn validate_completed(
    completed: &CompletedArtifact,
    task: &OwnerTask,
    sealed: &SealedEvidence,
) -> Result<()> {
    let receipt = completed.receipt();
    if completed.task() != task
        || receipt.version != 1
        || receipt.review_id != task.review_identity().expect("review task").id()?
        || receipt.sealed_sha256 != sealed.sha256
        || receipt.model_id.is_empty()
        || receipt.completed_ms <= 0
    {
        return Err("owner_review_receipt_conflict".into());
    }
    if let CompletedArtifact::V1(completed) = completed
        && (completed.version != REVIEW_COMPLETED_VERSION
            || completed.clock.observed_wall_ms != receipt.completed_ms
            || completed.clock.started_wall_ms <= 0
            || !clock_elapsed_valid(&completed.clock)
            || !clock_anomaly_valid(&completed.clock))
    {
        return Err("owner_review_receipt_conflict".into());
    }
    match (
        completed.output(),
        completed.failure(),
        receipt.status.as_str(),
    ) {
        (Some(output), None, "completed") => {
            let raw = serde_json::to_string(output)?;
            OwnerReviewOutput::validate(&raw, task, sealed)?;
            if receipt.output_sha256 != digest(raw.as_bytes()) {
                return Err("owner_review_receipt_conflict".into());
            }
        }
        (None, Some(reason), "failed") if !reason.is_empty() && reason.len() <= 160 => {}
        _ => return Err("owner_review_receipt_conflict".into()),
    }
    Ok(())
}

fn clock_anomaly_valid(clock: &ReviewClockV1) -> bool {
    clock.clock_anomaly.as_ref().is_none_or(|anomaly| {
        matches!(
            anomaly.reason.as_str(),
            "wall_backstep" | "monotonic_elapsed_invalid"
        )
    })
}

fn clock_elapsed_valid(clock: &ReviewClockV1) -> bool {
    clock.monotonic_elapsed_ms >= 0
        || clock
            .clock_anomaly
            .as_ref()
            .is_some_and(|anomaly| anomaly.reason == "monotonic_elapsed_invalid")
}

/// A failed turn may observe a wall earlier than its start only when the
/// artifact carries honest clock evidence. The legacy old-writer bug is
/// recognized only for its exact fully-bound failed clock-invalid shape.
fn validate_artifact_clock(completed: &CompletedArtifact, turn: &session::Turn) -> Result<()> {
    match completed {
        CompletedArtifact::Legacy(completed) => {
            if completed.receipt.completed_ms < turn.started_at_ms
                && (completed.receipt.status != "failed"
                    || completed.failure.as_deref() != Some("owner_review_clock_invalid")
                    || completed.output.is_some())
            {
                return Err("owner_review_receipt_conflict".into());
            }
        }
        CompletedArtifact::V1(completed) => {
            let clock = &completed.clock;
            if clock.started_wall_ms != turn.started_at_ms
                || clock.observed_wall_ms != completed.receipt.completed_ms
                || !clock_elapsed_valid(clock)
            {
                return Err("owner_review_receipt_conflict".into());
            }
            if completed.receipt.completed_ms < turn.started_at_ms {
                let anomaly = clock
                    .clock_anomaly
                    .as_ref()
                    .ok_or("owner_review_receipt_conflict")?;
                if completed.receipt.status != "failed"
                    || completed.failure.is_none()
                    || anomaly.reason != "wall_backstep"
                {
                    return Err("owner_review_receipt_conflict".into());
                }
            }
        }
    }
    Ok(())
}

trait ArtifactWire {
    fn encode(&self) -> Result<Vec<u8>>;
}

impl ArtifactWire for Completed {
    fn encode(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
}

impl ArtifactWire for ReviewCompletedV1 {
    fn encode(&self) -> Result<Vec<u8>> {
        // Canonical sorted-key JSON: the bytes do not depend on whether
        // `serde_json` was built with `preserve_order`. Unknown fields still
        // refuse on read for this V1 shape.
        canonical_bytes(&serde_json::to_value(self)?)
    }
}

#[cfg(test)]
fn encode_v1(completed: &ReviewCompletedV1) -> Result<Vec<u8>> {
    completed.encode()
}

fn persist<T: ArtifactWire>(root: &Path, task: &OwnerTask, completed: &T) -> Result<()> {
    let bytes = completed.encode()?;
    if bytes.len() > MAX_ARTIFACT_WRITE_BYTES {
        return Err("owner_review_artifact_bound".into());
    }
    let directory = open_reviews(root, true)?;
    let name = format!(
        "{}.json",
        task.review_identity().expect("review task").id()?
    );
    let flags = rustix::fs::OFlags::WRONLY
        | rustix::fs::OFlags::CREATE
        | rustix::fs::OFlags::EXCL
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let fd = match rustix::fs::openat(
        &directory,
        std::ffi::OsStr::new(&name),
        flags,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    ) {
        Ok(fd) => fd,
        Err(error) if error == rustix::io::Errno::EXIST || error == rustix::io::Errno::LOOP => {
            return Err("owner_review_source_invalid".into());
        }
        Err(_) => return Err("owner_review_path_invalid".into()),
    };
    let mut file = fs::File::from(fd);
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| "owner_review_path_invalid")?;
    file.write_all(&bytes)?;
    sync_file_and_directory(&file, &directory)?;
    Ok(())
}

fn review_action(turn: &session::Turn, receipt: &ReviewReceipt) -> session::Action {
    session::action(
        "owner_review",
        &turn.id,
        &serde_json::to_value(receipt).unwrap_or_default(),
    )
}
fn expected_result(
    turn: &session::Turn,
    completed: &CompletedArtifact,
    outcome: &str,
) -> Result<session::TurnResult> {
    Ok(session::TurnResult {
        turn_id: turn.id.clone(),
        finished_at_ms: completed.receipt().completed_ms,
        outcome: outcome.into(),
        reason: completed.failure().map(str::to_owned),
        reply: completed.output_json()?,
        actions: vec![review_action(turn, completed.receipt())],
    })
}
fn reconcile_review(root: &Path, session: &str) {
    if let Err(error) = super::reconcile_lifecycle_results(root, session) {
        eprintln!("supervisor_lifecycle_reconcile_failed reason={error}");
        super::record_state_error(root, session, error);
    }
}

fn complete_log(
    root: &Path,
    turn: &session::Turn,
    completed: &CompletedArtifact,
    now: i64,
    outcome: &str,
) -> Result<()> {
    let action = review_action(turn, completed.receipt());
    let actions = session::checked_actions_for(root, &turn.session, &turn.id)?;
    if actions.is_empty() {
        session::commit_action(root, &turn.session, &turn.id, &action)?;
    } else if actions != vec![action] {
        return Err("owner_review_receipt_conflict".into());
    }
    let log = session::SessionLog::new(root, &turn.session);
    let result = expected_result(turn, completed, outcome)?;
    if let Some(existing) = log.result_for(&turn.id) {
        let same_outcome = matches!(
            (existing.outcome.as_str(), result.outcome.as_str()),
            (
                "ok" | "recovered_review_receipt",
                "ok" | "recovered_review_receipt"
            ) | ("failed", "failed")
                | ("interrupted", "interrupted")
        );
        let same = same_outcome
            && existing.turn_id == result.turn_id
            && existing.finished_at_ms == result.finished_at_ms
            && existing.reason == result.reason
            && existing.reply == result.reply
            && existing.actions == result.actions;
        if !same {
            return Err("owner_review_receipt_conflict".into());
        }
        reconcile_review(root, &turn.session);
        return Ok(());
    }
    log.append(now, session::Body::Result { result })?;
    reconcile_review(root, &turn.session);
    Ok(())
}
pub(super) fn recover(root: &Path, turn: &session::Turn, now: i64) -> Result<bool> {
    let Some(task) = OwnerTask::from_turn(turn)? else {
        return Ok(false);
    };
    if task.review_identity().is_none() {
        return Ok(false);
    }
    let sealed: SealedEvidence = serde_json::from_value(turn.request["sealed_evidence"].clone())
        .map_err(|_| "owner_review_binding_invalid")?;
    match read_source(root, &task) {
        Ok(source) => {
            let completed = parse_completed(&source.bytes)?;
            validate_completed(&completed, &task, &sealed)?;
            if completed.receipt().model_id != turn.request["model_id"].as_str().unwrap_or_default()
            {
                return Err("owner_review_receipt_conflict".into());
            }
            validate_artifact_clock(&completed, turn)?;
            // Revalidate durability on the same pinned descriptors before the
            // source effect becomes a committed session action/result.
            sync_file_and_directory(&source.file, &source.directory)?;
            let outcome = if completed.failure().is_some() {
                "failed"
            } else {
                "recovered_review_receipt"
            };
            complete_log(root, turn, &completed, now, outcome)?;
        }
        Err(error) if error.to_string() == "owner_review_source_missing" => {
            let actions = session::checked_actions_for(root, &turn.session, &turn.id)?;
            if !actions.is_empty() {
                return Err("owner_review_source_missing".into());
            }
            let log = session::SessionLog::new(root, &turn.session);
            if log.result_for(&turn.id).is_some() {
                reconcile_review(root, &turn.session);
                return Ok(true);
            }
            // The request may have reached the provider. Original evidence remains
            // on the turn and the one sealed attempt is never replayed.
            log.append(
                now,
                session::Body::Result {
                    result: session::TurnResult {
                        turn_id: turn.id.clone(),
                        finished_at_ms: now,
                        outcome: "interrupted".into(),
                        reason: Some("owner_review_interrupted".into()),
                        reply: String::new(),
                        actions: Vec::new(),
                    },
                },
            )?;
            reconcile_review(root, &turn.session);
        }
        Err(error) => return Err(error),
    }
    Ok(true)
}
/// Bounded, path-free references to the original review artifacts for the seed.
pub(super) fn state(root: &Path, session: &str) -> Value {
    let log = session::SessionLog::new(root, session);
    let mut refs = Vec::new();
    for record in log.records() {
        let session::Body::Turn { turn } = record.body else {
            continue;
        };
        if turn.session != session {
            return json!({"available":false,"reason":"owner_review_binding_invalid"});
        }
        let task = match OwnerTask::from_turn(&turn) {
            Ok(Some(task)) => task,
            Ok(None) => continue,
            Err(_) => return json!({"available":false,"reason":"owner_review_binding_invalid"}),
        };
        if task.review_identity().is_none() {
            continue;
        }
        let sealed: SealedEvidence =
            match serde_json::from_value(turn.request["sealed_evidence"].clone()) {
                Ok(sealed) => sealed,
                Err(_) => return json!({"available":false,"reason":"owner_review_binding_invalid"}),
            };
        let source = match read_source(root, &task) {
            Ok(source) => source,
            Err(error) => {
                let reason = match error.to_string().as_str() {
                    "owner_review_source_missing" => "owner_review_artifact_unavailable",
                    "owner_review_path_invalid" => "owner_review_path_invalid",
                    _ => "owner_review_artifact_invalid",
                };
                return json!({"available":false,"reason":reason});
            }
        };
        let completed = match parse_completed(&source.bytes) {
            Ok(completed) => completed,
            Err(_) => return json!({"available":false,"reason":"owner_review_artifact_invalid"}),
        };
        if validate_completed(&completed, &task, &sealed).is_err()
            || completed.receipt().model_id != turn.request["model_id"].as_str().unwrap_or_default()
            || validate_artifact_clock(&completed, &turn).is_err()
        {
            return json!({"available":false,"reason":"owner_review_artifact_invalid"});
        }
        let review_id = match task.review_identity().expect("review task").id() {
            Ok(id) => id,
            Err(_) => return json!({"available":false,"reason":"owner_review_binding_invalid"}),
        };
        refs.push(json!({
            "review_id": review_id,
            "sealed_sha256": task.review_identity().expect("review task").sealed_sha256,
            "artifact_sha256": digest(&source.bytes),
            "status": completed.receipt().status,
            "completed_ms": completed.receipt().completed_ms,
        }));
        if refs.len() > MAX_REVIEW_REFS {
            return json!({"available":false,"reason":"owner_review_refs_exceeded"});
        }
    }
    if refs.is_empty() {
        json!({"available":false,"reason":"owner_review_not_attempted"})
    } else {
        json!({"available":true,"refs":refs})
    }
}
pub(super) fn settings_cutoff(c: &Config, cfg: &PlannerConfig, session_end_ms: i64) -> Result<i64> {
    let settings = cfg
        .owner_tasks
        .as_ref()
        .filter(|settings| settings.review.enabled)
        .ok_or("owner_review_disabled")?;
    settings.validate(cfg)?;
    if settings.review.cutoff_minute <= c.execution.session_end_minute {
        return Err("owner_review_cutoff_invalid".into());
    }
    Ok(session_end_ms.saturating_add(
        (i64::from(settings.review.cutoff_minute) - i64::from(c.execution.session_end_minute))
            .saturating_mul(60_000),
    ))
}

/// One pure schedule input for both admission and the read-only projection.
#[allow(clippy::too_many_arguments)]
pub(super) fn schedule_input(
    c: &Config,
    log: &session::SessionLog,
    state: Option<&State>,
    closed: bool,
    now: i64,
    session_end_ms: i64,
    cutoff_ms: i64,
    backoff_until_ms: Option<i64>,
) -> owner_tasks::ScheduleInput {
    let session = c.session.to_string();
    let mut attempted = false;
    let mut attempt_invalid = false;
    let mut attempt_conflict = false;
    let mut recorded_report = None;
    for record in log.records() {
        let session::Body::Turn { turn } = record.body else {
            continue;
        };
        if turn.trigger != "review" && turn.request.get("owner_task").is_none() {
            continue;
        }
        if turn.session != session {
            attempted = true;
            attempt_invalid = true;
            continue;
        }
        match OwnerTask::from_turn(&turn) {
            Ok(Some(task)) if task.review_identity().is_some() => {}
            Ok(Some(_)) => continue,
            _ => {
                attempted = true;
                attempt_invalid = true;
                continue;
            }
        }
        attempted = true;
        let sealed: SealedEvidence =
            match serde_json::from_value(turn.request["sealed_evidence"].clone()) {
                Ok(sealed) => sealed,
                Err(_) => {
                    attempt_invalid = true;
                    continue;
                }
            };
        match &recorded_report {
            None => recorded_report = Some(sealed.input.report_sha256),
            Some(prior) if prior != &sealed.input.report_sha256 => attempt_conflict = true,
            _ => {}
        }
    }
    let report_path = c.directory.join("report.json");
    let report_present = report_path.is_file();
    if attempted && report_present {
        let current = crate::read_bounded(&report_path, 16_000_000)
            .ok()
            .map(|bytes| digest(&bytes));
        if let (Some(recorded), Some(current)) = (&recorded_report, &current)
            && recorded != current
        {
            attempt_conflict = true;
        }
    }
    owner_tasks::ScheduleInput {
        enabled: true,
        closed,
        report_present,
        parked: state.is_some_and(|state| state.parked.is_some()),
        // A corrupt or unreadable log is never mistaken for idle (#233).
        busy: !log.pending_checked().idle() || log.handoff().is_some(),
        attempted,
        attempt_conflict,
        attempt_invalid,
        session_day: local(now).date_naive() == c.session,
        now_ms: now,
        close_ms: session_end_ms,
        cutoff_ms,
        backoff_until_ms,
    }
}

pub(super) fn consider(
    c: &Config,
    cfg: &PlannerConfig,
    context: &super::super::decision_owner::Context<'_>,
    tick: &Tick<'_>,
    state: &State,
    hashes: &AppliedHashes,
    bounds: &Compaction,
) -> Result<Option<Worker>> {
    if cfg
        .owner_tasks
        .as_ref()
        .is_none_or(|settings| !settings.review.enabled)
    {
        return Ok(None);
    }
    let cutoff = settings_cutoff(c, cfg, tick.session_end_ms)?;
    let log = session::SessionLog::new(&c.directory, &c.session.to_string());
    let input = schedule_input(
        c,
        &log,
        Some(state),
        context.portfolio.closed,
        tick.now,
        tick.session_end_ms,
        cutoff,
        None,
    );
    let schedule = owner_tasks::project(&input);
    if schedule.review_state == owner_tasks::ReviewState::Invalid {
        return Err(schedule
            .review_reason
            .unwrap_or("owner_review_binding_invalid")
            .into());
    }
    if !schedule.review_due {
        return Ok(None);
    }
    // Refuse an existing unsafe path before spending a provider call.
    match open_reviews(&c.directory, false) {
        Ok(_) => {}
        Err(error) if error.to_string() == "owner_review_source_missing" => {}
        Err(error) => return Err(error),
    }
    let sealed = super::super::postclose::seal_evidence(context.db, c, context.portfolio)?;
    let identity = ReviewIdentity {
        version: 1,
        session: c.session.to_string(),
        sealed_sha256: sealed.sha256.clone(),
        policy_sha256: digest(&planner::policy_bytes(cfg)?),
        prompt_version: digest(owner_tasks::REVIEW_PROMPT.as_bytes()),
    };
    let task = OwnerTask::Review {
        identity,
        cutoff_ms: cutoff,
    };
    let (role, path, timeout_ms, selection) = role_and_model(c, cfg).map_err(|s| s.to_string())?;
    planner::prepare_review_usage(c)?;
    let turn_id = format!("turn-{}-{}", c.session, log.last_seq().saturating_add(1));
    let selection_binding = super::selection_binding(&selection);
    let facts = json!({"owner_task":task,"sealed_evidence":sealed,"review_id":task.review_identity().expect("review task").id()?,
        "turn_id":turn_id,"model_id":role.model_id,"model_selection":selection});
    let prompt = format!("{}\n{}", owner_tasks::REVIEW_PROMPT, facts);
    if !request_fits(escaped_prompt_bytes(&prompt), bounds) {
        return Err("supervisor_context_limit".into());
    }
    let turn = session::Turn {
        id: turn_id.clone(),
        session: c.session.to_string(),
        trigger: "review".into(),
        prompt_version: task
            .review_identity()
            .expect("review task")
            .prompt_version
            .clone(),
        notes_version: 0,
        fills_at_start: tick.fill_count,
        started_at_ms: tick.now,
        request: facts.clone(),
    };
    log.append(tick.now, session::Body::Turn { turn })?;
    Ok(Some(spawn(
        c,
        cfg,
        role,
        path,
        prompt,
        json!({"trigger":{"kind":"review","reason":"review"},"summary":facts}),
        "review",
        tick.now,
        timeout_ms.min(cutoff.saturating_sub(tick.now).max(1) as u64),
        hashes.clone(),
        tick.now,
        TurnRef {
            session: c.session.to_string(),
            id: turn_id,
            notes_version: 0,
            prompt_version: task
                .review_identity()
                .expect("review task")
                .prompt_version
                .clone(),
            decision: None,
        },
        RunKind::Review,
        None,
        *bounds,
        selection_binding,
    )))
}

fn monotonic_elapsed_ms(worker: &Worker) -> i64 {
    #[cfg(test)]
    if let Some(value) = TEST_ELAPSED_MS.with(|cell| cell.get()) {
        return value;
    }
    worker.started.elapsed().as_millis().min(i64::MAX as u128) as i64
}

fn review_clock(worker: &Worker, turn: &session::Turn, now: i64) -> ReviewClockV1 {
    let monotonic_elapsed_ms = monotonic_elapsed_ms(worker);
    let clock_anomaly = if now < turn.started_at_ms {
        Some(ReviewClockAnomalyV1 {
            reason: "wall_backstep".into(),
        })
    } else if monotonic_elapsed_ms < 0 {
        Some(ReviewClockAnomalyV1 {
            reason: "monotonic_elapsed_invalid".into(),
        })
    } else {
        None
    };
    ReviewClockV1 {
        started_wall_ms: turn.started_at_ms,
        observed_wall_ms: now,
        monotonic_elapsed_ms,
        clock_anomaly,
    }
}

pub(super) fn finish_reply(
    root: &Path,
    worker: &Worker,
    now: i64,
    raw: &str,
    alerts: Option<&Alerts>,
) -> bool {
    let result = (|| -> Result<()> {
        let (task, turn, sealed) = task_for(root, worker)?;
        let cutoff_ms = match &task {
            OwnerTask::Review { cutoff_ms, .. } => *cutoff_ms,
            OwnerTask::Research { .. } => return Err("owner_review_binding_invalid".into()),
        };
        if now < turn.started_at_ms {
            return Err("owner_review_clock_invalid".into());
        }
        if now >= cutoff_ms {
            return Err("owner_review_expired".into());
        }
        let output = OwnerReviewOutput::validate(raw, &task, &sealed)?;
        let receipt = ReviewReceipt {
            version: 1,
            review_id: task.review_identity().expect("review task").id()?,
            sealed_sha256: sealed.sha256,
            output_sha256: digest(&serde_json::to_vec(&output)?),
            model_id: turn.request["model_id"]
                .as_str()
                .ok_or("owner_review_binding_invalid")?
                .into(),
            completed_ms: now,
            status: "completed".into(),
        };
        let completed = ReviewCompletedV1 {
            version: REVIEW_COMPLETED_VERSION,
            task: task.clone(),
            receipt,
            output: Some(output),
            failure: None,
            evidence: worker.evidence(),
            clock: review_clock(worker, &turn, now),
        };
        persist(root, &task, &completed)?;
        complete_log(root, &turn, &CompletedArtifact::V1(completed), now, "ok")?;
        Ok(())
    })();
    match result {
        Ok(()) => true,
        Err(error) => {
            let reason = super::super::failure_code(error.as_ref());
            finish_failed(root, worker, now, &reason, alerts)
        }
    }
}
pub(super) fn finish_failed(
    root: &Path,
    worker: &Worker,
    now: i64,
    reason: &str,
    alerts: Option<&Alerts>,
) -> bool {
    let mut settled = false;
    let result = (|| -> Result<()> {
        let (task, turn, sealed) = task_for(root, worker)?;
        // A committed source wins over a later session-log failure.
        if source_present(root, &task)? {
            recover(root, &turn, now)?;
            settled = true;
            return Ok(());
        }
        let completed = ReviewCompletedV1 {
            version: REVIEW_COMPLETED_VERSION,
            task: task.clone(),
            receipt: ReviewReceipt {
                version: 1,
                review_id: task.review_identity().expect("review task").id()?,
                sealed_sha256: sealed.sha256,
                output_sha256: String::new(),
                model_id: turn.request["model_id"]
                    .as_str()
                    .ok_or("owner_review_binding_invalid")?
                    .into(),
                completed_ms: now,
                status: "failed".into(),
            },
            output: None,
            failure: Some(reason.into()),
            evidence: worker.evidence(),
            clock: review_clock(worker, &turn, now),
        };
        match persist(root, &task, &completed) {
            Ok(()) => complete_log(
                root,
                &turn,
                &CompletedArtifact::V1(completed),
                now,
                "failed",
            ),
            Err(error) => {
                // A concurrent or earlier writer may have committed the one
                // terminal artifact. Recover it; never replay the provider.
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
    if result.is_err() {
        record_skip(
            root,
            &worker.session,
            now,
            "owner_review_reconciliation_required",
        );
    }
    if !settled {
        record_failure_with_evidence(root, &worker.session, now, reason, &worker.evidence());
        emit_failure(alerts, &worker.session, now, reason);
    }
    settled
}

#[cfg(test)]
fn review_fixture(
    dir: &Path,
) -> (
    Config,
    PlannerConfig,
    rusqlite::Connection,
    sadr_core::execution::Portfolio,
    i64,
    i64,
) {
    let (c, mut cfg) = super::super::decision_owner::owner_fixture_config(dir);
    cfg.owner_tasks = Some(owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: owner_tasks::ReviewSettings {
            enabled: true,
            cutoff_minute: 1080,
        },
        research: None,
    });
    cfg.orchestrator
        .as_mut()
        .unwrap()
        .triggers
        .get_mut(&super::super::orchestrator_model::Trigger::Supervisor)
        .unwrap()
        .daily_call_budget = Some(8);
    let model = dir.join("review-model.json");
    fs::write(
        &model,
        json!({"grant_file": dir.join("absent-grant.json")}).to_string(),
    )
    .unwrap();
    cfg.model_config = Some(model);
    fs::write(
        c.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&cfg).unwrap(),
    )
    .unwrap();
    let db = super::super::open_book(&c).unwrap();
    let mut p = super::super::tests::test_portfolio();
    p.model_led = true;
    p.closed = true;
    p.health = "closed".into();
    db.execute(
        "INSERT INTO state VALUES(1,?,?)",
        rusqlite::params![
            super::super::book_binding(&c).unwrap(),
            serde_json::to_string(&p).unwrap()
        ],
    )
    .unwrap();
    let report = json!({"paper_only":true,"live_enabled":false,"broker":"upstox","account":c.account,"session":c.session,"portfolio":p.clone()});
    fs::write(
        c.directory.join("report.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    let end = c
        .session
        .and_time(chrono::NaiveTime::from_hms_opt(15, 30, 0).unwrap())
        .and_local_timezone(chrono::FixedOffset::east_opt(19800).unwrap())
        .single()
        .unwrap()
        .timestamp_millis();
    (c, cfg, db, p, end, end + 60_000)
}

#[cfg(test)]
fn try_start_review(
    c: &Config,
    cfg: &PlannerConfig,
    db: &mut rusqlite::Connection,
    p: &sadr_core::execution::Portfolio,
    end: i64,
    now: i64,
) -> Option<Worker> {
    let tick = Tick {
        now,
        minute: 931,
        in_session: false,
        fill_count: 0,
        allowlist: &[],
        session_end_ms: end,
        positions: json!({}),
    };
    let state = super::state::State::fresh(&c.session.to_string());
    let hashes = AppliedHashes::current(cfg);
    let context = super::super::decision_owner::Context {
        db: &mut *db,
        portfolio: p,
        instruments: &[],
    };
    consider(
        c,
        cfg,
        &context,
        &tick,
        &state,
        &hashes,
        &Compaction::default(),
    )
    .unwrap()
}

#[cfg(test)]
fn start_review(
    c: &Config,
    cfg: &PlannerConfig,
    db: &mut rusqlite::Connection,
    p: &sadr_core::execution::Portfolio,
    end: i64,
    now: i64,
) -> (Option<Worker>, session::Turn) {
    let worker = try_start_review(c, cfg, db, p, end, now);
    let turn = session::SessionLog::new(&c.directory, &c.session.to_string())
        .pending()
        .expect("review turn");
    (worker, turn)
}

#[cfg(test)]
fn valid_review_output(turn: &session::Turn) -> String {
    let sealed: SealedEvidence =
        serde_json::from_value(turn.request["sealed_evidence"].clone()).unwrap();
    let task = OwnerTask::from_turn(turn).unwrap().unwrap();
    let evidence_id = sealed.input.evidence[0].id.clone();
    json!({
        "version":1,
        "review_id":task.review_identity().expect("review task").id().unwrap(),
        "sealed_sha256":sealed.sha256,
        "review":{
            "observations":[{"text":"closed flat paper book","evidence_ids":[evidence_id.clone()]}],
            "hypotheses":[],
            "uncertainties":["single session sample"],
            "memory":[{"kind":"observation","text":"closed flat paper book","evidence_ids":[evidence_id],"uncertainty":"single session"}]
        }
    })
    .to_string()
}

#[cfg(test)]
fn completed_for(turn: &session::Turn, now: i64) -> (OwnerTask, Completed) {
    let task = OwnerTask::from_turn(turn).unwrap().unwrap();
    let sealed: SealedEvidence =
        serde_json::from_value(turn.request["sealed_evidence"].clone()).unwrap();
    let output = OwnerReviewOutput::validate(&valid_review_output(turn), &task, &sealed).unwrap();
    let receipt = ReviewReceipt {
        version: 1,
        review_id: task.review_identity().expect("review task").id().unwrap(),
        sealed_sha256: sealed.sha256.clone(),
        output_sha256: digest(&serde_json::to_vec(&output).unwrap()),
        model_id: turn.request["model_id"].as_str().unwrap().into(),
        completed_ms: now,
        status: "completed".into(),
    };
    (
        task.clone(),
        Completed {
            task,
            receipt,
            output: Some(output),
            failure: None,
            evidence: json!({}),
        },
    )
}

#[cfg(test)]
fn failed_for(turn: &session::Turn, now: i64, reason: &str) -> (OwnerTask, Completed) {
    let task = OwnerTask::from_turn(turn).unwrap().unwrap();
    let sealed: SealedEvidence =
        serde_json::from_value(turn.request["sealed_evidence"].clone()).unwrap();
    (
        task.clone(),
        Completed {
            task: task.clone(),
            receipt: ReviewReceipt {
                version: 1,
                review_id: task.review_identity().expect("review task").id().unwrap(),
                sealed_sha256: sealed.sha256,
                output_sha256: String::new(),
                model_id: turn.request["model_id"].as_str().unwrap().into(),
                completed_ms: now,
                status: "failed".into(),
            },
            output: None,
            failure: Some(reason.into()),
            evidence: json!({}),
        },
    )
}

#[cfg(test)]
fn consider_result(
    c: &Config,
    cfg: &PlannerConfig,
    db: &mut rusqlite::Connection,
    p: &sadr_core::execution::Portfolio,
    end: i64,
    now: i64,
) -> Result<Option<Worker>> {
    let tick = Tick {
        now,
        minute: 931,
        in_session: false,
        fill_count: 0,
        allowlist: &[],
        session_end_ms: end,
        positions: json!({}),
    };
    let state = super::state::State::fresh(&c.session.to_string());
    let hashes = AppliedHashes::current(cfg);
    let context = super::super::decision_owner::Context {
        db: &mut *db,
        portfolio: p,
        instruments: &[],
    };
    consider(
        c,
        cfg,
        &context,
        &tick,
        &state,
        &hashes,
        &Compaction::default(),
    )
}

#[cfg(test)]
fn state_runs(c: &Config) -> (u32, u32, Option<i64>) {
    let state = super::state::load(&c.directory, &c.session.to_string()).unwrap();
    (state.runs, state.failures, state.last_run_ms)
}

#[cfg(test)]
fn fail_source_sync(count: u32) {
    SOURCE_SYNC_FAILURES.with(|failures| failures.set(count));
}

#[cfg(test)]
#[allow(dead_code)]
fn source_sync_fails() -> bool {
    SOURCE_SYNC_FAILURES.with(|failures| {
        let remaining = failures.get();
        if remaining == 0 {
            false
        } else {
            failures.set(remaining - 1);
            true
        }
    })
}

#[cfg(test)]
thread_local! {
    static SOURCE_SYNC_FAILURES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    static TEST_ELAPSED_MS: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn set_test_elapsed_ms(value: Option<i64>) {
    TEST_ELAPSED_MS.with(|cell| cell.set(value));
}

#[cfg(test)]
const LEGACY_GOLDEN_JSON: &str = r#"{"task":{"kind":"review","identity":{"version":1,"session":"2026-01-02","sealed_sha256":"1111111111111111111111111111111111111111111111111111111111111111","policy_sha256":"2222222222222222222222222222222222222222222222222222222222222222","prompt_version":"3333333333333333333333333333333333333333333333333333333333333333"},"cutoff_ms":1700000000000},"receipt":{"version":1,"review_id":"4444444444444444444444444444444444444444444444444444444444444444","sealed_sha256":"1111111111111111111111111111111111111111111111111111111111111111","output_sha256":"5555555555555555555555555555555555555555555555555555555555555555","model_id":"fixture-model","completed_ms":1700000000100,"status":"completed"},"output":{"version":1,"review_id":"4444444444444444444444444444444444444444444444444444444444444444","sealed_sha256":"1111111111111111111111111111111111111111111111111111111111111111","review":{"observations":[],"hypotheses":[],"uncertainties":[],"memory":[]}},"failure":null,"evidence":{}}"#;
#[cfg(test)]
const LEGACY_GOLDEN_SHA256: &str =
    "78613e129af8e295404ec7456fc2650253777e52f8c402abd314cff953287817";
#[cfg(test)]
const V1_GOLDEN_JSON: &str = r#"{"clock":{"clock_anomaly":{"reason":"wall_backstep"},"monotonic_elapsed_ms":100,"observed_wall_ms":1699999999900,"started_wall_ms":1700000000000},"evidence":{},"failure":"owner_review_clock_invalid","output":null,"receipt":{"completed_ms":1699999999900,"model_id":"fixture-model","output_sha256":"","review_id":"4444444444444444444444444444444444444444444444444444444444444444","sealed_sha256":"1111111111111111111111111111111111111111111111111111111111111111","status":"failed","version":1},"task":{"cutoff_ms":1700000000000,"identity":{"policy_sha256":"2222222222222222222222222222222222222222222222222222222222222222","prompt_version":"3333333333333333333333333333333333333333333333333333333333333333","sealed_sha256":"1111111111111111111111111111111111111111111111111111111111111111","session":"2026-01-02","version":1},"kind":"review"},"version":1}"#;
#[cfg(test)]
const V1_GOLDEN_SHA256: &str = "228f693f8bd521b07a6296c77ee960c8e676d65ef5fddb1f9b2664c807b7ecd3";

#[cfg(test)]
fn golden_legacy_completed() -> Completed {
    let identity = ReviewIdentity {
        version: 1,
        session: "2026-01-02".into(),
        sealed_sha256: "1".repeat(64),
        policy_sha256: "2".repeat(64),
        prompt_version: "3".repeat(64),
    };
    let task = OwnerTask::Review {
        identity,
        cutoff_ms: 1_700_000_000_000,
    };
    let receipt = ReviewReceipt {
        version: 1,
        review_id: "4".repeat(64),
        sealed_sha256: "1".repeat(64),
        output_sha256: "5".repeat(64),
        model_id: "fixture-model".into(),
        completed_ms: 1_700_000_000_100,
        status: "completed".into(),
    };
    let output = OwnerReviewOutput {
        version: 1,
        review_id: "4".repeat(64),
        sealed_sha256: "1".repeat(64),
        review: sadr_core::postclose::ReviewOutput {
            observations: Vec::new(),
            hypotheses: Vec::new(),
            uncertainties: Vec::new(),
            memory: Vec::new(),
        },
    };
    Completed {
        task,
        receipt,
        output: Some(output),
        failure: None,
        evidence: json!({}),
    }
}

#[cfg(test)]
fn golden_v1_completed() -> ReviewCompletedV1 {
    let identity = ReviewIdentity {
        version: 1,
        session: "2026-01-02".into(),
        sealed_sha256: "1".repeat(64),
        policy_sha256: "2".repeat(64),
        prompt_version: "3".repeat(64),
    };
    let task = OwnerTask::Review {
        identity,
        cutoff_ms: 1_700_000_000_000,
    };
    ReviewCompletedV1 {
        version: 1,
        task,
        receipt: ReviewReceipt {
            version: 1,
            review_id: "4".repeat(64),
            sealed_sha256: "1".repeat(64),
            output_sha256: String::new(),
            model_id: "fixture-model".into(),
            completed_ms: 1_699_999_999_900,
            status: "failed".into(),
        },
        output: None,
        failure: Some("owner_review_clock_invalid".into()),
        evidence: json!({}),
        clock: ReviewClockV1 {
            started_wall_ms: 1_700_000_000_000,
            observed_wall_ms: 1_699_999_999_900,
            monotonic_elapsed_ms: 100,
            clock_anomaly: Some(ReviewClockAnomalyV1 {
                reason: "wall_backstep".into(),
            }),
        },
    }
}

/// The sealed post-close review contract as one chain. Its scenarios already
/// use separate fixtures; it stays one function only because each split costs
/// a `#[test]` against the hard 400 budget (#231). It runs as its own named
/// chain in `supervisor::run_named`, so a failure here no longer hides the
/// other supervisor chains, and the failing assert's line names the scenario.
#[cfg(test)]
pub(super) fn exercise() {
    use crate::runtime::planner::{Backend, CallGuard, ModelRole};
    use std::cell::Cell;
    use std::os::unix::fs::DirBuilderExt;

    // The legacy wire type is frozen byte-for-byte, while V1 is canonical JSON
    // with explicit artifact version, observed wall times and clock evidence.
    let legacy_golden = golden_legacy_completed();
    let legacy_bytes = serde_json::to_vec(&legacy_golden).unwrap();
    assert_eq!(
        String::from_utf8(legacy_bytes.clone()).unwrap(),
        LEGACY_GOLDEN_JSON
    );
    assert_eq!(digest(&legacy_bytes), LEGACY_GOLDEN_SHA256);
    let v1_golden = golden_v1_completed();
    let v1_bytes = encode_v1(&v1_golden).unwrap();
    assert_eq!(String::from_utf8(v1_bytes.clone()).unwrap(), V1_GOLDEN_JSON);
    assert_eq!(digest(&v1_bytes), V1_GOLDEN_SHA256);
    match parse_completed(&v1_bytes).unwrap() {
        CompletedArtifact::V1(parsed) => assert_eq!(parsed, v1_golden),
        CompletedArtifact::Legacy(_) => panic!("V1 artifact parsed as legacy"),
    }
    match parse_completed(&legacy_bytes).unwrap() {
        CompletedArtifact::Legacy(parsed) => assert_eq!(parsed, legacy_golden),
        CompletedArtifact::V1(_) => panic!("legacy artifact parsed as V1"),
    }
    // Canonicalization must not narrow valid payload numbers to integers.
    let mut float_golden = v1_golden.clone();
    float_golden.evidence = json!({"score":1.5,"nested":{"ratio":0.25}});
    let float_bytes = encode_v1(&float_golden).unwrap();
    match parse_completed(&float_bytes).unwrap() {
        CompletedArtifact::V1(parsed) => assert_eq!(parsed, float_golden),
        CompletedArtifact::Legacy(_) => panic!("V1 float artifact parsed as legacy"),
    }
    // Unknown versions, trailing bytes and unknown fields still refuse.
    let mut unsupported: Value = serde_json::from_slice(&v1_bytes).unwrap();
    unsupported["version"] = json!(2);
    let unsupported = serde_json::to_vec(&unsupported).unwrap();
    assert_eq!(
        parse_completed(&unsupported).unwrap_err().to_string(),
        "owner_review_artifact_version_unsupported"
    );
    let mut trailing = v1_bytes.clone();
    trailing.push(b' ');
    assert_eq!(
        parse_completed(&trailing).unwrap_err().to_string(),
        "owner_review_source_invalid"
    );
    let mut unknown: Value = serde_json::from_slice(&v1_bytes).unwrap();
    unknown["future_field"] = json!(true);
    let unknown = serde_json::to_vec(&unknown).unwrap();
    assert_eq!(
        parse_completed(&unknown).unwrap_err().to_string(),
        "owner_review_source_invalid"
    );
    // The derived struct order is not canonical: the actual writer must recurse
    // through every object, not just rely on the map implementation.
    let struct_order = serde_json::to_vec(&v1_golden).unwrap();
    assert_ne!(struct_order, v1_bytes);
    assert_eq!(
        parse_completed(&struct_order).unwrap_err().to_string(),
        "owner_review_source_invalid"
    );
    // Top-level and nested key reordering are tampering, not acceptable input.
    let body = V1_GOLDEN_JSON
        .strip_prefix('{')
        .unwrap()
        .strip_suffix(",\"version\":1}")
        .unwrap();
    let top_reordered = format!("{{\"version\":1,{body}}}");
    assert_ne!(top_reordered, V1_GOLDEN_JSON);
    assert_eq!(
        parse_completed(top_reordered.as_bytes())
            .unwrap_err()
            .to_string(),
        "owner_review_source_invalid"
    );
    let nested_reordered = top_reordered.replace(
        r#""clock":{"clock_anomaly":{"reason":"wall_backstep"},"monotonic_elapsed_ms":100,"observed_wall_ms":1699999999900,"started_wall_ms":1700000000000}"#,
        r#""clock":{"started_wall_ms":1700000000000,"observed_wall_ms":1699999999900,"monotonic_elapsed_ms":100,"clock_anomaly":{"reason":"wall_backstep"}}"#,
    );
    assert_ne!(nested_reordered, top_reordered);
    assert_eq!(
        parse_completed(nested_reordered.as_bytes())
            .unwrap_err()
            .to_string(),
        "owner_review_source_invalid"
    );
    // Whitespace, even between otherwise canonical tokens, is not canonical.
    let spaced = V1_GOLDEN_JSON.replace("\":", "\": ");
    assert_ne!(spaced, V1_GOLDEN_JSON);
    assert_eq!(
        parse_completed(spaced.as_bytes()).unwrap_err().to_string(),
        "owner_review_source_invalid"
    );

    let base = owner_tasks::ScheduleInput {
        enabled: true,
        closed: true,
        report_present: true,
        parked: false,
        busy: false,
        attempted: false,
        attempt_conflict: false,
        attempt_invalid: false,
        session_day: true,
        now_ms: 100,
        close_ms: 100,
        cutoff_ms: 200,
        backoff_until_ms: None,
    };
    assert_eq!(owner_tasks::project(&base).review_state, "due");
    assert!(owner_tasks::project(&base).review_due);
    for (mutate, expected) in [
        (json!({"enabled":false}), "disabled"),
        (json!({"attempt_invalid":true}), "invalid"),
        (json!({"attempt_conflict":true}), "invalid"),
        (json!({"attempted":true}), "attempted"),
        (json!({"session_day":false}), "invalid"),
        (json!({"now_ms":201}), "expired"),
        (json!({"parked":true}), "parked"),
        (json!({"busy":true}), "busy"),
        (json!({"closed":false}), "awaiting_sealed_report"),
        (json!({"report_present":false}), "awaiting_sealed_report"),
        (json!({"now_ms":99}), "awaiting_sealed_report"),
    ] {
        let mut input = owner_tasks::ScheduleInput { ..base };
        if let Some(value) = mutate.get("enabled").and_then(Value::as_bool) {
            input.enabled = value;
        }
        if let Some(value) = mutate.get("closed").and_then(Value::as_bool) {
            input.closed = value;
        }
        if let Some(value) = mutate.get("report_present").and_then(Value::as_bool) {
            input.report_present = value;
        }
        if let Some(value) = mutate.get("parked").and_then(Value::as_bool) {
            input.parked = value;
        }
        if let Some(value) = mutate.get("busy").and_then(Value::as_bool) {
            input.busy = value;
        }
        if let Some(value) = mutate.get("attempted").and_then(Value::as_bool) {
            input.attempted = value;
        }
        if let Some(value) = mutate.get("attempt_conflict").and_then(Value::as_bool) {
            input.attempt_conflict = value;
        }
        if let Some(value) = mutate.get("attempt_invalid").and_then(Value::as_bool) {
            input.attempt_invalid = value;
        }
        if let Some(value) = mutate.get("session_day").and_then(Value::as_bool) {
            input.session_day = value;
        }
        if let Some(value) = mutate.get("now_ms").and_then(Value::as_i64) {
            input.now_ms = value;
        }
        let projected = owner_tasks::project(&input);
        assert_eq!(projected.review_state, expected, "{mutate}");
        assert_eq!(projected.review_due, expected == "due");
        assert_eq!(projected.as_of_ms, input.now_ms);
    }
    assert_eq!(
        owner_tasks::project(&owner_tasks::ScheduleInput {
            attempt_invalid: true,
            ..base
        })
        .review_reason,
        Some("owner_review_binding_invalid")
    );
    assert_eq!(
        owner_tasks::project(&owner_tasks::ScheduleInput {
            attempt_conflict: true,
            ..base
        })
        .review_reason,
        Some("owner_review_report_changed")
    );
    assert_eq!(
        owner_tasks::project(&owner_tasks::ScheduleInput {
            session_day: false,
            ..base
        })
        .review_reason,
        Some("owner_review_session_date_mismatch")
    );
    // A configured owner failure backoff defers an otherwise-due review with a
    // static reason and the real expiry; it never silently drops the work.
    let deferred = owner_tasks::project(&owner_tasks::ScheduleInput {
        backoff_until_ms: Some(150),
        ..base
    });
    assert_eq!(deferred.review_state, "deferred");
    assert_eq!(deferred.review_reason, Some("supervisor_failure_backoff"));
    assert_eq!(deferred.deferred_until_ms, Some(150));
    assert!(!deferred.review_due);
    assert_eq!(
        owner_tasks::project(&owner_tasks::ScheduleInput {
            backoff_until_ms: Some(150),
            now_ms: 150,
            ..base
        })
        .review_state,
        "due"
    );

    // Optional configuration keeps old policy bytes; enabled review binds a prompt.
    let mut plain = PlannerConfig::default();
    let plain_bytes = planner::policy_bytes(&plain).unwrap();
    assert!(!String::from_utf8_lossy(&plain_bytes).contains("owner_tasks"));
    let mut value = serde_json::to_value(&plain).unwrap();
    owner_tasks::bind_policy(&plain, &mut value);
    assert!(value.get("owner_tasks").is_none());
    plain.owner_tasks = Some(owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: owner_tasks::ReviewSettings {
            enabled: true,
            cutoff_minute: 1080,
        },
        research: None,
    });
    let mut enabled_value = serde_json::to_value(&plain).unwrap();
    owner_tasks::bind_policy(&plain, &mut enabled_value);
    assert_eq!(
        enabled_value["owner_tasks"]["review_prompt_sha256"],
        digest(owner_tasks::REVIEW_PROMPT.as_bytes())
    );
    let mut invalid = plain.clone();
    invalid.owner_tasks.as_mut().unwrap().version = 2;
    let settings = invalid.owner_tasks.clone().unwrap();
    assert_eq!(
        settings.validate(&invalid).unwrap_err().to_string(),
        "owner_tasks_settings_invalid"
    );
    invalid = plain.clone();
    invalid.owner_tasks.as_mut().unwrap().review.cutoff_minute = 930;
    let settings = invalid.owner_tasks.clone().unwrap();
    assert!(settings.validate(&invalid).is_err());
    // Review needs the same admitted owner role; the legacy decide role is not it.
    let legacy = PlannerConfig {
        owner_tasks: plain.owner_tasks.clone(),
        ..PlannerConfig::default()
    };
    assert_eq!(
        legacy
            .owner_tasks
            .as_ref()
            .unwrap()
            .validate(&legacy)
            .unwrap_err()
            .to_string(),
        "owner_review_requires_decision_owner"
    );

    let no_report_dir = tempfile::tempdir().unwrap();
    let (nc, ncfg, mut ndb, np, nend, nnow) = review_fixture(no_report_dir.path());
    fs::remove_file(nc.directory.join("report.json")).unwrap();
    assert!(try_start_review(&nc, &ncfg, &mut ndb, &np, nend, nnow).is_none());
    assert_eq!(
        session::SessionLog::new(&nc.directory, &nc.session.to_string())
            .turns()
            .len(),
        0
    );
    assert_eq!(
        super::review_status(&nc, &ncfg, &np, nnow, nend)["status"],
        "awaiting_sealed_report"
    );
    let expired_dir = tempfile::tempdir().unwrap();
    let (xc, xcfg, mut xdb, xp, xend, _) = review_fixture(expired_dir.path());
    let expired_cutoff = xend + (1080 - 930) * 60_000;
    assert!(try_start_review(&xc, &xcfg, &mut xdb, &xp, xend, expired_cutoff + 1).is_none());
    assert_eq!(
        session::SessionLog::new(&xc.directory, &xc.session.to_string())
            .turns()
            .len(),
        0
    );
    let parked_dir = tempfile::tempdir().unwrap();
    let (kc, kcfg, mut kdb, kp, kend, know) = review_fixture(parked_dir.path());
    let mut parked_state = super::state::State::fresh(&kc.session.to_string());
    parked_state.parked = Some(super::state::Parked {
        reason: "supervisor_budget_exhausted".into(),
        at_ms: know,
        budget_at_park: Some(1),
    });
    let parked_tick = Tick {
        now: know,
        minute: 931,
        in_session: false,
        fill_count: 0,
        allowlist: &[],
        session_end_ms: kend,
        positions: json!({}),
    };
    let parked_hashes = AppliedHashes::current(&kcfg);
    let parked_context = super::super::decision_owner::Context {
        db: &mut kdb,
        portfolio: &kp,
        instruments: &[],
    };
    assert!(
        consider(
            &kc,
            &kcfg,
            &parked_context,
            &parked_tick,
            &parked_state,
            &parked_hashes,
            &Compaction::default()
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        session::SessionLog::new(&kc.directory, &kc.session.to_string())
            .turns()
            .len(),
        0
    );

    let dir = tempfile::tempdir().unwrap();
    let (c, cfg, mut db, p, end, now) = review_fixture(dir.path());
    assert!(
        String::from_utf8_lossy(&planner::policy_bytes(&cfg).unwrap())
            .contains(&digest(owner_tasks::REVIEW_PROMPT.as_bytes()))
    );
    let tick = Tick {
        now,
        minute: 931,
        in_session: false,
        fill_count: 0,
        allowlist: &[],
        session_end_ms: end,
        positions: json!({}),
    };
    let state = super::state::State::fresh(&c.session.to_string());
    let hashes = AppliedHashes::current(&cfg);
    let context = super::super::decision_owner::Context {
        db: &mut db,
        portfolio: &p,
        instruments: &[],
    };
    let mut worker: Worker = consider(
        &c,
        &cfg,
        &context,
        &tick,
        &state,
        &hashes,
        &Compaction::default(),
    )
    .unwrap()
    .expect("eligible review starts");
    assert_eq!(worker.kind, RunKind::Review);
    let log = session::SessionLog::new(&c.directory, &c.session.to_string());
    let turn = log.pending().unwrap();
    assert_eq!(turn.trigger, "review");
    assert_eq!(turn.request["owner_task"]["kind"], "review");
    let sealed: SealedEvidence =
        serde_json::from_value(turn.request["sealed_evidence"].clone()).unwrap();
    let task = OwnerTask::from_turn(&turn).unwrap().unwrap();
    let review_id = task.review_identity().expect("review task").id().unwrap();
    assert_eq!(turn.request["review_id"], review_id);
    assert!(
        consider(
            &c,
            &cfg,
            &context,
            &tick,
            &state,
            &hashes,
            &Compaction::default()
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(log.turns().len(), 1);
    let status = super::review_status(&c, &cfg, &p, now, end);
    assert_eq!(status["status"], "attempted");
    assert_eq!(status["attempted"], true);
    assert_eq!(status["review_due"], false);

    let raw = valid_review_output(&turn);
    let bundle = json!({"trigger":{"kind":"review","reason":"review"},"summary":turn.request});

    let frame_dir = tempfile::tempdir().unwrap();
    let (fc, mut fcfg, fdb, fp, fend, fnow) = review_fixture(frame_dir.path());
    // A review keeps no tools even when the role, workspace, web and research
    // capabilities are configured.
    fcfg.workspace = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
    fcfg.web_tools = Some(
        serde_json::from_value(json!({
            "enabled":true,
            "news_feeds":["https://example.com/feed.xml"]
        }))
        .unwrap(),
    );
    fcfg.research_tools = Some(
        serde_json::from_value(json!({
            "queue": fc.directory.join("review-research.sqlite"),
            "stage_root": fc.directory.join("review-research-stage"),
            "registrations": [{
                "analysis_id":"review_analysis",
                "dataset_id":"review_dataset",
                "code_file": fc.directory.join("review-analysis.py"),
                "runtime_file": fc.directory.join("review-runtime"),
                "input_file": fc.directory.join("review-input.json"),
                "code_sha256": "0".repeat(64),
                "runtime_sha256": "0".repeat(64),
                "input_sha256": "0".repeat(64),
                "memory_bytes": 65536,
                "wall_millis": 1000,
                "limits": {"cpu_millis":100,"tasks":1,"workspace_bytes":65536,"file_bytes":4096}
            }]
        }))
        .unwrap(),
    );
    // The call guard reloads the on-disk policy, so the capability fixture must
    // be the persisted configuration rather than an in-memory overlay.
    fs::write(
        fc.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&fcfg).unwrap(),
    )
    .unwrap();
    let frame_sealed = super::super::postclose::seal_evidence(&fdb, &fc, &fp).unwrap();
    let frame_identity = ReviewIdentity {
        version: 1,
        session: fc.session.to_string(),
        sealed_sha256: frame_sealed.sha256.clone(),
        policy_sha256: digest(&planner::policy_bytes(&fcfg).unwrap()),
        prompt_version: digest(owner_tasks::REVIEW_PROMPT.as_bytes()),
    };
    let frame_task = OwnerTask::Review {
        identity: frame_identity,
        cutoff_ms: fend + 9_000_000,
    };
    let frame_review_id = frame_task
        .review_identity()
        .expect("review task")
        .id()
        .unwrap();
    let frame_sealed_sha256 = frame_sealed.sha256.clone();
    let frame_turn_id = "turn-review-frame";
    let (mut frame_role, _, _, _) = role_and_model(&fc, &fcfg).unwrap();
    frame_role.tools = vec!["web_search".into(), "x_search".into()];
    let frame_facts = json!({
        "owner_task":frame_task,
        "sealed_evidence":frame_sealed,
        "review_id":frame_review_id.clone(),
        "turn_id":frame_turn_id,
        "model_id":frame_role.model_id.clone(),
        "model_selection":null
    });
    let frame_bundle = json!({"trigger":{"kind":"review","reason":"review"},"summary":frame_facts});
    let frame_prompt = format!("{}\n{}", owner_tasks::REVIEW_PROMPT, frame_facts);
    let frame_raw = json!({
        "version":1,
        "review_id":frame_review_id,
        "sealed_sha256":frame_sealed_sha256,
        "review":{"observations":[],"hypotheses":[],"uncertainties":["no evidence reviewed"],"memory":[]}
    })
    .to_string();
    let response = json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":frame_raw}]}]});
    struct Fake {
        requests: Vec<Value>,
        response: Value,
    }
    impl Backend for Fake {
        fn call(&mut self, _: &ModelRole, prompt: &str) -> Result<String> {
            self.requests.push(serde_json::from_str(prompt)?);
            Ok(self.response.to_string())
        }
    }
    let clock = || fnow;
    let guard = CallGuard {
        cancel: None,
        deadline: None,
        not_before: None,
        configuration: Some((&fc, &fcfg)),
        journal_required: Cell::new(false),
        minimum_prior_calls: 0,
        invocation_id: Some(frame_turn_id),
        context_hash: None,
        clock: &clock,
        duration: None,
        started_wall_ms: None,
    };
    let mut fake = Fake {
        requests: Vec::new(),
        response,
    };
    let collected = super::super::harness::run_collect(
        &fc,
        &fcfg,
        "supervisor",
        &frame_role,
        &frame_prompt,
        &frame_bundle,
        &mut fake,
        &guard,
        &mut Vec::new(),
        true,
    )
    .unwrap();
    assert_eq!(collected, frame_raw);
    let frame = &fake.requests[0];
    assert_eq!(frame["role"], "supervisor");
    assert_eq!(frame["tools"], json!([]));
    assert_eq!(frame["final_answer_only"], true);
    assert_eq!(frame["review_only"], true);
    assert!(frame.get("owner_tools").is_none());
    let instructions = frame["instructions"].as_str().unwrap();
    assert!(instructions.contains(owner_tasks::REVIEW_PROMPT));
    assert!(instructions.contains("maximize the continuing paper account"));
    assert!(!instructions.contains("TradePlan"));
    assert!(!instructions.contains("intraday supervisor"));
    let content = frame["input"][0]["content"].as_str().unwrap();
    assert!(content.contains("\"sealed_evidence\""));
    assert!(content.contains("\"owner_task\""));
    assert!(!content.contains("registered analysis catalog"));
    let parsed: crate::runtime::harness::Frame = serde_json::from_value(frame.clone()).unwrap();
    assert!(crate::model::frame_preflight(&parsed, &frame_role).is_ok());
    let mut tooled: crate::runtime::harness::Frame = serde_json::from_value(frame.clone()).unwrap();
    tooled.tools = vec![json!({"type":"function","name":"get_portfolio"})];
    assert_eq!(
        crate::model::frame_preflight(&tooled, &frame_role)
            .unwrap_err()
            .to_string(),
        "harness_final_tools_invalid"
    );
    let mut unmarked: crate::runtime::harness::Frame =
        serde_json::from_value(frame.clone()).unwrap();
    unmarked.review_only = false;
    assert_eq!(
        crate::model::frame_preflight(&unmarked, &frame_role)
            .unwrap_err()
            .to_string(),
        "harness_final_tools_invalid"
    );
    let mut not_final: crate::runtime::harness::Frame =
        serde_json::from_value(frame.clone()).unwrap();
    not_final.final_answer_only = false;
    assert_eq!(
        crate::model::frame_preflight(&not_final, &frame_role)
            .unwrap_err()
            .to_string(),
        "harness_final_tools_invalid"
    );
    // Only a sealed supervisor review may be final-only. Digest, normal
    // supervisor and the retired decide role are refused without that marker.
    let mut digest_final: crate::runtime::harness::Frame =
        serde_json::from_value(frame.clone()).unwrap();
    digest_final.role = "digest".into();
    digest_final.review_only = false;
    assert_eq!(
        crate::model::frame_preflight(&digest_final, &frame_role)
            .unwrap_err()
            .to_string(),
        "harness_final_tools_invalid"
    );
    let mut decide_final: crate::runtime::harness::Frame =
        serde_json::from_value(frame.clone()).unwrap();
    decide_final.role = "decide".into();
    decide_final.review_only = false;
    assert_eq!(
        crate::model::frame_preflight(&decide_final, &frame_role)
            .unwrap_err()
            .to_string(),
        "harness_final_tools_invalid"
    );
    // An exhausted allowance refuses before the backend call.
    let mut budget_role = frame_role.clone();
    budget_role.daily_call_budget = 0;
    let mut budget_fake = Fake {
        requests: Vec::new(),
        response: json!({"output":[]}),
    };
    assert_eq!(
        super::super::harness::run_collect(
            &fc,
            &fcfg,
            "supervisor",
            &budget_role,
            &frame_prompt,
            &frame_bundle,
            &mut budget_fake,
            &guard,
            &mut Vec::new(),
            true
        )
        .unwrap_err()
        .to_string(),
        "planner_budget_exhausted"
    );
    assert!(budget_fake.requests.is_empty());
    // Unadvertised local and provider tools are refused, never executed.
    let mut tool_fake = Fake {
        requests: Vec::new(),
        response: json!({"output":[{"type":"function_call","call_id":"call-1","name":"get_portfolio","arguments":"{}"}]}),
    };
    assert_eq!(
        super::super::harness::run_collect(
            &fc,
            &fcfg,
            "supervisor",
            &frame_role,
            &frame_prompt,
            &frame_bundle,
            &mut tool_fake,
            &guard,
            &mut Vec::new(),
            true
        )
        .unwrap_err()
        .to_string(),
        "harness_tool_not_allowed"
    );
    let mut provider_fake = Fake {
        requests: Vec::new(),
        response: json!({"output":[{"type":"web_search_call"}]}),
    };
    assert_eq!(
        super::super::harness::run_collect(
            &fc,
            &fcfg,
            "supervisor",
            &frame_role,
            &frame_prompt,
            &frame_bundle,
            &mut provider_fake,
            &guard,
            &mut Vec::new(),
            true
        )
        .unwrap_err()
        .to_string(),
        "harness_output_role_or_tool_invalid"
    );

    let valid = OwnerReviewOutput::validate(&raw, &task, &sealed).unwrap();
    let valid_value = serde_json::to_value(&valid).unwrap();
    let mut foreign = valid_value.clone();
    foreign["review"]["observations"][0]["evidence_ids"] = json!(["foreign"]);
    assert_eq!(
        OwnerReviewOutput::validate(&foreign.to_string(), &task, &sealed)
            .unwrap_err()
            .to_string(),
        "postclose_review_invalid"
    );
    let mut paraphrased = valid_value.clone();
    paraphrased["review"]["memory"][0]["text"] = json!("paraphrased");
    assert_eq!(
        OwnerReviewOutput::validate(&paraphrased.to_string(), &task, &sealed)
            .unwrap_err()
            .to_string(),
        "postclose_memory_not_reviewed"
    );
    let mut wrong_id = valid_value.clone();
    wrong_id["review_id"] = json!("a".repeat(64));
    assert_eq!(
        OwnerReviewOutput::validate(&wrong_id.to_string(), &task, &sealed)
            .unwrap_err()
            .to_string(),
        "owner_review_output_invalid"
    );
    assert_eq!(
        OwnerReviewOutput::validate(
            &json!({"actions":[],"stance":"hold","notes":""}).to_string(),
            &task,
            &sealed
        )
        .unwrap_err()
        .to_string(),
        "owner_review_output_invalid"
    );
    assert_eq!(
        OwnerReviewOutput::validate(&"x".repeat(65_537), &task, &sealed)
            .unwrap_err()
            .to_string(),
        "owner_review_output_bound"
    );
    let mut wrong = turn.clone();
    wrong.trigger = "decision".into();
    assert_eq!(
        OwnerTask::from_turn(&wrong).unwrap_err().to_string(),
        "owner_review_binding_invalid"
    );
    wrong = turn.clone();
    wrong.request.as_object_mut().unwrap().remove("owner_task");
    assert_eq!(
        OwnerTask::from_turn(&wrong).unwrap_err().to_string(),
        "owner_review_binding_invalid"
    );
    wrong = turn.clone();
    wrong.request["owner_task"]["identity"]["sealed_sha256"] = json!("a".repeat(64));
    assert_eq!(
        OwnerTask::from_turn(&wrong).unwrap_err().to_string(),
        "owner_review_binding_invalid"
    );
    let mut wrong_bundle = bundle.clone();
    wrong_bundle["trigger"]["kind"] = json!("decision");
    assert_eq!(
        owner_tasks::validate_review_bundle(&c, &cfg, &wrong_bundle, Some(&turn.id))
            .unwrap_err()
            .to_string(),
        "owner_review_binding_invalid"
    );
    assert_eq!(
        owner_tasks::validate_review_bundle(&c, &cfg, &bundle, Some("other-turn"))
            .unwrap_err()
            .to_string(),
        "owner_review_binding_invalid"
    );
    let mut no_review = cfg.clone();
    no_review.owner_tasks.as_mut().unwrap().review.enabled = false;
    assert_eq!(
        owner_tasks::validate_review_bundle(&c, &no_review, &bundle, Some(&turn.id))
            .unwrap_err()
            .to_string(),
        "owner_review_binding_invalid"
    );
    // A new provider invocation must use the recorded model revision and a
    // concrete model identity.
    let mut stale_selection = bundle.clone();
    stale_selection["summary"]["model_selection"] = json!({"revision":1});
    assert_eq!(
        owner_tasks::validate_review_bundle(&c, &cfg, &stale_selection, Some(&turn.id))
            .unwrap_err()
            .to_string(),
        "owner_review_binding_invalid"
    );
    let mut missing_model = bundle.clone();
    missing_model["summary"]["model_id"] = json!("");
    assert_eq!(
        owner_tasks::validate_review_bundle(&c, &cfg, &missing_model, Some(&turn.id))
            .unwrap_err()
            .to_string(),
        "owner_review_binding_invalid"
    );

    let (sender, receiver) = mpsc::sync_channel(1);
    worker.receiver = receiver;
    sender.send(Ok(raw)).unwrap();
    assert_eq!(
        poll(
            &c.directory,
            &worker,
            &[],
            &Limits::default(),
            now + 1,
            end,
            8,
            None
        ),
        Some(true)
    );
    let result = log.result_for(&turn.id).unwrap();
    assert_eq!(result.outcome, "ok");
    assert_eq!(result.actions.len(), 1);
    assert_eq!(result.actions[0].kind, "owner_review");
    assert_eq!(
        session::actions_for(&c.directory, &c.session.to_string(), &turn.id).len(),
        1
    );
    let artifact = artifact_path(&c.directory, &task).unwrap();
    assert!(artifact.is_file());
    assert_eq!(
        fs::metadata(&artifact).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(artifact.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let refs = super::review_state(&c.directory, &c.session.to_string());
    assert_eq!(refs["available"], true);
    assert_eq!(refs["refs"][0]["review_id"], review_id);
    assert_eq!(
        refs["refs"][0]["artifact_sha256"],
        digest(&fs::read(&artifact).unwrap())
    );
    assert!(!refs.to_string().contains(c.directory.to_str().unwrap()));
    let records = log.records().len();
    assert!(recover(&c.directory, &turn, now + 2).unwrap());
    assert_eq!(log.records().len(), records);
    assert!(
        consider(
            &c,
            &cfg,
            &context,
            &tick,
            &state,
            &hashes,
            &Compaction::default()
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(log.turns().len(), 1);
    let lifecycle = handoff_lifecycle(
        &c,
        &cfg,
        HandoffBuildContext {
            session: c.session.to_string(),
            target: "2026-10-17".into(),
            turn_id: "turn-handoff-review".into(),
            standing_instructions: "paper only".into(),
            facts: json!({"positions":[],"active_directives":[],"alerts":[]}),
        },
        &hashes,
        now + 3,
    );
    let LifecycleTask::Handoff { seed, .. } = lifecycle.task else {
        panic!("handoff lifecycle expected")
    };
    assert_eq!(seed["review_state"]["available"], true);
    assert_eq!(seed["review_state"]["refs"][0]["review_id"], review_id);
    session::validate_seed(&seed, "2026-10-17").unwrap();
    let mut unreviewed = cfg.clone();
    unreviewed.owner_tasks = None;
    let lifecycle = handoff_lifecycle(
        &c,
        &unreviewed,
        HandoffBuildContext {
            session: c.session.to_string(),
            target: "2026-10-17".into(),
            turn_id: "turn-handoff-review-off".into(),
            standing_instructions: "paper only".into(),
            facts: json!({"positions":[],"active_directives":[],"alerts":[]}),
        },
        &hashes,
        now + 4,
    );
    let LifecycleTask::Handoff { seed, .. } = lifecycle.task else {
        panic!("handoff lifecycle expected")
    };
    assert!(seed.get("review_state").is_none());

    // A tampered source is not carried into the successor seed.
    let artifact = artifact_path(&c.directory, &task).unwrap();
    let mut tampered = fs::read(&artifact).unwrap();
    tampered.push(b' ');
    fs::write(&artifact, &tampered).unwrap();
    let tampered_state = super::review_state(&c.directory, &c.session.to_string());
    assert_eq!(tampered_state["available"], false);
    assert_eq!(tampered_state["reason"], "owner_review_artifact_invalid");
    let lifecycle = handoff_lifecycle(
        &c,
        &cfg,
        HandoffBuildContext {
            session: c.session.to_string(),
            target: "2026-10-17".into(),
            turn_id: "turn-handoff-review-tampered".into(),
            standing_instructions: "paper only".into(),
            facts: json!({"positions":[],"active_directives":[],"alerts":[]}),
        },
        &hashes,
        now + 5,
    );
    let LifecycleTask::Handoff { seed, .. } = lifecycle.task else {
        panic!("handoff lifecycle expected")
    };
    assert_eq!(seed["review_state"]["available"], false);
    assert_eq!(
        seed["review_state"]["reason"],
        "owner_review_artifact_invalid"
    );
    session::validate_seed(&seed, "2026-10-17").unwrap();

    let fail_dir = tempfile::tempdir().unwrap();
    let (fc, fcfg, mut fdb, fp, fend, fnow) = review_fixture(fail_dir.path());
    let (fail_worker, fail_turn) = start_review(&fc, &fcfg, &mut fdb, &fp, fend, fnow);
    let mut fail_worker = fail_worker.unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    fail_worker.receiver = receiver;
    sender
        .send(Ok(
            json!({"actions":[],"stance":"hold","notes":""}).to_string()
        ))
        .unwrap();
    assert_eq!(
        poll(
            &fc.directory,
            &fail_worker,
            &[],
            &Limits::default(),
            fnow + 1,
            fend,
            8,
            None
        ),
        Some(false)
    );
    let fail_task = OwnerTask::from_turn(&fail_turn).unwrap().unwrap();
    let failed: ReviewCompletedV1 = serde_json::from_slice(
        &fs::read(artifact_path(&fc.directory, &fail_task).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(failed.receipt.status, "failed");
    assert_eq!(
        failed.failure.as_deref(),
        Some("owner_review_output_invalid")
    );
    let fail_log = session::SessionLog::new(&fc.directory, &fc.session.to_string());
    assert_eq!(
        fail_log.result_for(&fail_turn.id).unwrap().outcome,
        "failed"
    );
    assert_eq!(
        fail_log
            .result_for(&fail_turn.id)
            .unwrap()
            .reason
            .as_deref(),
        Some("owner_review_output_invalid")
    );
    let failure: Value = serde_json::from_slice(
        &fs::read(fc.directory.join("model/supervisor-failure.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(failure["reason"], "owner_review_output_invalid");
    let refs = super::review_state(&fc.directory, &fc.session.to_string());
    assert_eq!(refs["available"], true);
    assert_eq!(refs["refs"][0]["status"], "failed");
    assert!(try_start_review(&fc, &fcfg, &mut fdb, &fp, fend, fnow + 2).is_none());
    assert_eq!(fail_log.turns().len(), 1);

    // A failed review whose observed wall stepped behind the turn start is
    // terminalized once with honest monotonic elapsed and anomaly evidence.
    let backstep_dir = tempfile::tempdir().unwrap();
    let (bc, bcfg, mut bdb, bp, bend, bnow) = review_fixture(backstep_dir.path());
    let (backstep_worker, backstep_turn) = start_review(&bc, &bcfg, &mut bdb, &bp, bend, bnow);
    let mut backstep_worker = backstep_worker.unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    backstep_worker.receiver = receiver;
    sender
        .send(Ok(valid_review_output(&backstep_turn)))
        .unwrap();
    set_test_elapsed_ms(Some(17));
    assert_eq!(
        poll(
            &bc.directory,
            &backstep_worker,
            &[],
            &Limits::default(),
            bnow - 1,
            bend,
            8,
            None
        ),
        Some(false)
    );
    set_test_elapsed_ms(None);
    let backstep_task = OwnerTask::from_turn(&backstep_turn).unwrap().unwrap();
    let backstep_path = artifact_path(&bc.directory, &backstep_task).unwrap();
    let backstep_bytes = fs::read(&backstep_path).unwrap();
    let backstep: ReviewCompletedV1 = serde_json::from_slice(&backstep_bytes).unwrap();
    assert_eq!(backstep.receipt.status, "failed");
    assert_eq!(
        backstep.failure.as_deref(),
        Some("owner_review_clock_invalid")
    );
    assert_eq!(backstep.receipt.completed_ms, bnow - 1);
    assert!(backstep.receipt.completed_ms < backstep_turn.started_at_ms);
    assert_eq!(backstep.clock.started_wall_ms, backstep_turn.started_at_ms);
    assert_eq!(backstep.clock.observed_wall_ms, bnow - 1);
    assert_eq!(backstep.clock.monotonic_elapsed_ms, 17);
    assert_eq!(
        backstep.clock.clock_anomaly.as_ref().unwrap().reason,
        "wall_backstep"
    );
    let backstep_log = session::SessionLog::new(&bc.directory, &bc.session.to_string());
    let backstep_result = backstep_log.result_for(&backstep_turn.id).unwrap();
    assert_eq!(backstep_result.outcome, "failed");
    assert_eq!(backstep_result.finished_at_ms, bnow - 1);
    assert_eq!(
        backstep_result.reason.as_deref(),
        Some("owner_review_clock_invalid")
    );
    let backstep_records = backstep_log.records().len();
    assert!(recover(&bc.directory, &backstep_turn, bnow + 1).unwrap());
    assert_eq!(backstep_log.records().len(), backstep_records);
    assert_eq!(fs::read(&backstep_path).unwrap(), backstep_bytes);
    assert_eq!(state_runs(&bc), (1, 1, Some(bnow - 1)));

    // The old writer's fully bound failed clock-invalid artifact is recognized
    // without rewriting its bytes; original times are recovered and the turn is
    // terminal, not permanently pending.
    let legacy_dir = tempfile::tempdir().unwrap();
    let (lc, lcfg, mut ldb, lp, lend, lnow) = review_fixture(legacy_dir.path());
    let (legacy_worker, legacy_turn) = start_review(&lc, &lcfg, &mut ldb, &lp, lend, lnow);
    drop(legacy_worker);
    let (legacy_task, legacy_completed) =
        failed_for(&legacy_turn, lnow - 1, "owner_review_clock_invalid");
    persist(&lc.directory, &legacy_task, &legacy_completed).unwrap();
    let legacy_path = artifact_path(&lc.directory, &legacy_task).unwrap();
    let legacy_bytes = fs::read(&legacy_path).unwrap();
    assert!(recover(&lc.directory, &legacy_turn, lnow + 1).unwrap());
    let legacy_log = session::SessionLog::new(&lc.directory, &lc.session.to_string());
    let legacy_result = legacy_log.result_for(&legacy_turn.id).unwrap();
    assert_eq!(legacy_result.outcome, "failed");
    assert_eq!(legacy_result.finished_at_ms, lnow - 1);
    assert_eq!(
        legacy_result.reason.as_deref(),
        Some("owner_review_clock_invalid")
    );
    assert_eq!(fs::read(&legacy_path).unwrap(), legacy_bytes);
    let legacy_records = legacy_log.records().len();
    assert!(recover(&lc.directory, &legacy_turn, lnow + 2).unwrap());
    assert_eq!(legacy_log.records().len(), legacy_records);
    assert_eq!(state_runs(&lc), (1, 1, Some(lnow - 1)));
    let legacy_state = super::review_state(&lc.directory, &lc.session.to_string());
    assert_eq!(legacy_state["available"], true);
    assert_eq!(legacy_state["refs"][0]["completed_ms"], lnow - 1);

    let expire_dir = tempfile::tempdir().unwrap();
    let (ec, ecfg, mut edb, ep, eend, enow) = review_fixture(expire_dir.path());
    let (expire_worker, expire_turn) = start_review(&ec, &ecfg, &mut edb, &ep, eend, enow);
    let mut expire_worker = expire_worker.unwrap();
    let cutoff = eend + (1080 - 930) * 60_000;
    let (sender, receiver) = mpsc::sync_channel(1);
    expire_worker.receiver = receiver;
    sender.send(Ok(valid_review_output(&expire_turn))).unwrap();
    assert_eq!(
        poll(
            &ec.directory,
            &expire_worker,
            &[],
            &Limits::default(),
            cutoff + 1,
            eend,
            8,
            None
        ),
        Some(false)
    );
    let expire_task = OwnerTask::from_turn(&expire_turn).unwrap().unwrap();
    let expired: ReviewCompletedV1 = serde_json::from_slice(
        &fs::read(artifact_path(&ec.directory, &expire_task).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(expired.failure.as_deref(), Some("owner_review_expired"));

    let interrupt_dir = tempfile::tempdir().unwrap();
    let (ic, icfg, mut idb, ip, iend, inow) = review_fixture(interrupt_dir.path());
    let (interrupt_worker, interrupt_turn) = start_review(&ic, &icfg, &mut idb, &ip, iend, inow);
    drop(interrupt_worker);
    assert!(recover(&ic.directory, &interrupt_turn, inow + 1).unwrap());
    let interrupt_log = session::SessionLog::new(&ic.directory, &ic.session.to_string());
    let interrupted = interrupt_log.result_for(&interrupt_turn.id).unwrap();
    assert_eq!(interrupted.outcome, "interrupted");
    assert_eq!(
        interrupted.reason.as_deref(),
        Some("owner_review_interrupted")
    );
    assert!(
        !artifact_path(
            &ic.directory,
            &OwnerTask::from_turn(&interrupt_turn).unwrap().unwrap()
        )
        .unwrap()
        .exists()
    );
    assert!(try_start_review(&ic, &icfg, &mut idb, &ip, iend, inow + 2).is_none());

    let partial_dir = tempfile::tempdir().unwrap();
    let (pc, pcfg, mut pdb, pp, pend, pnow) = review_fixture(partial_dir.path());
    let (partial_worker, partial_turn) = start_review(&pc, &pcfg, &mut pdb, &pp, pend, pnow);
    drop(partial_worker);
    let partial_task = OwnerTask::from_turn(&partial_turn).unwrap().unwrap();
    let partial_sealed: SealedEvidence =
        serde_json::from_value(partial_turn.request["sealed_evidence"].clone()).unwrap();
    let partial_output = OwnerReviewOutput::validate(
        &valid_review_output(&partial_turn),
        &partial_task,
        &partial_sealed,
    )
    .unwrap();
    let partial_receipt = ReviewReceipt {
        version: 1,
        review_id: partial_task
            .review_identity()
            .expect("review task")
            .id()
            .unwrap(),
        sealed_sha256: partial_sealed.sha256.clone(),
        output_sha256: digest(&serde_json::to_vec(&partial_output).unwrap()),
        model_id: partial_turn.request["model_id"].as_str().unwrap().into(),
        completed_ms: pnow + 1,
        status: "completed".into(),
    };
    let partial = Completed {
        task: partial_task.clone(),
        receipt: partial_receipt,
        output: Some(partial_output),
        failure: None,
        evidence: json!({}),
    };
    persist(&pc.directory, &partial_task, &partial).unwrap();
    assert!(recover(&pc.directory, &partial_turn, pnow + 2).unwrap());
    let partial_log = session::SessionLog::new(&pc.directory, &pc.session.to_string());
    assert_eq!(
        partial_log.result_for(&partial_turn.id).unwrap().outcome,
        "recovered_review_receipt"
    );
    assert_eq!(
        session::actions_for(&pc.directory, &pc.session.to_string(), &partial_turn.id).len(),
        1
    );
    let records = partial_log.records().len();
    assert!(recover(&pc.directory, &partial_turn, pnow + 3).unwrap());
    assert_eq!(partial_log.records().len(), records);
    assert_eq!(
        session::actions_for(&pc.directory, &pc.session.to_string(), &partial_turn.id).len(),
        1
    );

    let guard_dir = tempfile::tempdir().unwrap();
    let (gc, gcfg, mut gdb, gp, gend, gnow) = review_fixture(guard_dir.path());
    let (guard_worker, guard_turn) = start_review(&gc, &gcfg, &mut gdb, &gp, gend, gnow);
    drop(guard_worker);
    let guard_task = OwnerTask::from_turn(&guard_turn).unwrap().unwrap();
    let guard_path = artifact_path(&gc.directory, &guard_task).unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(guard_path.parent().unwrap())
        .unwrap();
    let target = gc.directory.join("review-target.json");
    fs::write(&target, b"original").unwrap();
    std::os::unix::fs::symlink(&target, &guard_path).unwrap();
    assert_eq!(
        recover(&gc.directory, &guard_turn, gnow + 1)
            .unwrap_err()
            .to_string(),
        "owner_review_source_invalid"
    );
    assert_eq!(fs::read(&target).unwrap(), b"original");
    assert!(
        fs::symlink_metadata(&guard_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(&guard_path).unwrap();
    fs::write(&guard_path, b"{\"task\":").unwrap();
    assert_eq!(
        recover(&gc.directory, &guard_turn, gnow + 2)
            .unwrap_err()
            .to_string(),
        "owner_review_source_invalid"
    );
    assert_eq!(fs::read(&guard_path).unwrap(), b"{\"task\":");
    fs::write(&guard_path, vec![b'x'; 1_000_001]).unwrap();
    assert_eq!(
        recover(&gc.directory, &guard_turn, gnow + 3)
            .unwrap_err()
            .to_string(),
        "owner_review_source_invalid"
    );
    let outside = tempfile::tempdir().unwrap();
    fs::remove_dir_all(guard_path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(outside.path(), guard_path.parent().unwrap()).unwrap();
    let guard_sealed: SealedEvidence =
        serde_json::from_value(guard_turn.request["sealed_evidence"].clone()).unwrap();
    let guard_output = OwnerReviewOutput::validate(
        &valid_review_output(&guard_turn),
        &guard_task,
        &guard_sealed,
    )
    .unwrap();
    let guard_receipt = ReviewReceipt {
        version: 1,
        review_id: guard_task
            .review_identity()
            .expect("review task")
            .id()
            .unwrap(),
        sealed_sha256: guard_sealed.sha256.clone(),
        output_sha256: digest(&serde_json::to_vec(&guard_output).unwrap()),
        model_id: guard_turn.request["model_id"].as_str().unwrap().into(),
        completed_ms: gnow + 1,
        status: "completed".into(),
    };
    let guard_completed = Completed {
        task: guard_task.clone(),
        receipt: guard_receipt,
        output: Some(guard_output),
        failure: None,
        evidence: json!({}),
    };
    assert_eq!(
        persist(&gc.directory, &guard_task, &guard_completed)
            .unwrap_err()
            .to_string(),
        "owner_review_path_invalid"
    );
    assert!(fs::read_dir(outside.path()).unwrap().next().is_none());

    let status_dir = tempfile::tempdir().unwrap();
    let (sc, scfg, _sdb, sp, send, snow) = review_fixture(status_dir.path());
    let mut unreviewed = scfg.clone();
    unreviewed.owner_tasks = None;
    assert_eq!(
        super::review_status(&sc, &unreviewed, &sp, snow, send)["status"],
        "disabled"
    );
    assert_eq!(
        super::review_status(&sc, &scfg, &sp, snow, send)["status"],
        "due"
    );
    assert_eq!(
        super::review_status(&sc, &scfg, &sp, send + (1080 - 930) * 60_000 + 1, send)["status"],
        "expired"
    );
    super::state::park(
        &sc.directory,
        &sc.session.to_string(),
        snow,
        "supervisor_budget_exhausted",
        1,
    );
    let parked_status = super::review_status(&sc, &scfg, &sp, snow, send);
    assert_eq!(parked_status["status"], "parked");
    assert_eq!(parked_status["state_available"], true);
    let invalid_cfg = PlannerConfig {
        owner_tasks: scfg.owner_tasks.clone(),
        ..PlannerConfig::default()
    };
    let invalid_status = super::review_status(&sc, &invalid_cfg, &sp, snow, send);
    assert_eq!(invalid_status["status"], "invalid");
    assert_eq!(invalid_status["reason"], "owner_tasks_settings_invalid");
    let mut late = sc.clone();
    late.execution.session_end_minute = 1200;
    let late_status = super::review_status(&late, &scfg, &sp, snow, send);
    assert_eq!(late_status["status"], "invalid");
    assert_eq!(late_status["reason"], "owner_review_cutoff_invalid");

    // Invalid supervisor settings are refused by the read-only projection too,
    // not hidden behind a default/zero fallback that reports a due schedule.
    let mut invalid_wake_cfg = scfg.clone();
    invalid_wake_cfg
        .orchestrator
        .as_mut()
        .unwrap()
        .triggers
        .get_mut(&super::super::orchestrator_model::Trigger::Supervisor)
        .unwrap()
        .failure_backoff_ms = Some(1);
    let invalid_wake_status = super::review_status(&sc, &invalid_wake_cfg, &sp, snow, send);
    assert_eq!(invalid_wake_status["status"], "invalid");
    assert_eq!(invalid_wake_status["reason"], "supervisor_settings_invalid");
    assert!(invalid_wake_status.get("deferred_until_ms").is_none());

    // A configured owner failure backoff defers the read-only review status with
    // a static reason and the real expiry; expiry restores the normal schedule.
    let defer_dir = tempfile::tempdir().unwrap();
    let (dc, dcfg, _ddb, dp, dend, dnow) = review_fixture(defer_dir.path());
    let mut backoff_cfg = dcfg.clone();
    backoff_cfg
        .orchestrator
        .as_mut()
        .unwrap()
        .triggers
        .get_mut(&super::super::orchestrator_model::Trigger::Supervisor)
        .unwrap()
        .failure_backoff_ms = Some(60_000);
    super::state::with_state(&dc.directory, &dc.session.to_string(), |state| {
        state.failures = 3;
        state.owner_failure_streak = Some(3);
        state.last_failure_ms = Some(dnow);
        state.last_failure_binding = None;
    })
    .unwrap();
    let deferred_status = super::review_status(&dc, &backoff_cfg, &dp, dnow + 1, dend);
    assert_eq!(deferred_status["status"], "deferred");
    assert_eq!(deferred_status["reason"], "supervisor_failure_backoff");
    assert_eq!(deferred_status["deferred_until_ms"], dnow + 60_000);
    assert_eq!(deferred_status["review_due"], false);
    let expired_status = super::review_status(&dc, &backoff_cfg, &dp, dnow + 60_000, dend);
    assert_eq!(expired_status["status"], "due");

    // A recorded review remains valid evidence after the binary prompt changes.
    let old_dir = tempfile::tempdir().unwrap();
    let (oc, ocfg, odb, op, oend, onow) = review_fixture(old_dir.path());
    let old_sealed = super::super::postclose::seal_evidence(&odb, &oc, &op).unwrap();
    let old_prompt = digest(b"review prompt from an older binary");
    let old_identity = ReviewIdentity {
        version: 1,
        session: oc.session.to_string(),
        sealed_sha256: old_sealed.sha256.clone(),
        policy_sha256: digest(&planner::policy_bytes(&ocfg).unwrap()),
        prompt_version: old_prompt.clone(),
    };
    let old_task = OwnerTask::Review {
        identity: old_identity,
        cutoff_ms: oend + 9_000_000,
    };
    let old_id = old_task
        .review_identity()
        .expect("review task")
        .id()
        .unwrap();
    let old_turn_id = "turn-review-old-prompt";
    let old_turn = session::Turn {
        id: old_turn_id.into(),
        session: oc.session.to_string(),
        trigger: "review".into(),
        prompt_version: old_prompt,
        notes_version: 0,
        fills_at_start: 0,
        started_at_ms: onow,
        request: json!({
            "owner_task": old_task.clone(),
            "sealed_evidence": old_sealed.clone(),
            "review_id": old_id.clone(),
            "turn_id": old_turn_id,
            "model_id": "recorded-review-model",
            "model_selection": null
        }),
    };
    let old_log = session::SessionLog::new(&oc.directory, &oc.session.to_string());
    old_log
        .append(
            onow,
            session::Body::Turn {
                turn: old_turn.clone(),
            },
        )
        .unwrap();
    let old_output = OwnerReviewOutput::validate(
        &json!({
            "version":1,
            "review_id":old_id.clone(),
            "sealed_sha256":old_sealed.sha256.clone(),
            "review":{"observations":[],"hypotheses":[],"uncertainties":["no cited claim"],"memory":[]}
        })
        .to_string(),
        &old_task,
        &old_sealed,
    )
    .unwrap();
    let old_completed = Completed {
        task: old_task.clone(),
        receipt: ReviewReceipt {
            version: 1,
            review_id: old_id.clone(),
            sealed_sha256: old_sealed.sha256.clone(),
            output_sha256: digest(&serde_json::to_vec(&old_output).unwrap()),
            model_id: "recorded-review-model".into(),
            completed_ms: onow + 1,
            status: "completed".into(),
        },
        output: Some(old_output),
        failure: None,
        evidence: json!({}),
    };
    persist(&oc.directory, &old_task, &old_completed).unwrap();
    assert!(OwnerTask::from_turn(&old_turn).unwrap().is_some());
    assert!(recover(&oc.directory, &old_turn, onow + 2).unwrap());
    let old_result = old_log.result_for(old_turn_id).unwrap();
    assert_eq!(old_result.outcome, "recovered_review_receipt");
    assert_eq!(old_result.finished_at_ms, onow + 1);
    assert_eq!(old_log.records().len(), 2);
    let old_refs = super::review_state(&oc.directory, &oc.session.to_string());
    assert_eq!(old_refs["available"], true);
    assert_eq!(old_refs["refs"][0]["review_id"], old_id);
    let old_records = old_log.records().len();
    assert!(recover(&oc.directory, &old_turn, onow + 3).unwrap());
    assert_eq!(old_log.records().len(), old_records);
    assert_eq!(
        owner_tasks::validate_review_bundle(
            &oc,
            &ocfg,
            &json!({"trigger":{"kind":"review","reason":"review"},"summary":old_turn.request.clone()}),
            Some(old_turn_id)
        )
        .unwrap_err()
        .to_string(),
        "owner_review_binding_invalid"
    );

    // Artifact paths are pinned without following a symlinked ancestor, and an
    // existing review directory must already be private.
    use std::os::unix::fs::PermissionsExt;
    let safe_dir = tempfile::tempdir().unwrap();
    let (sc, scfg, mut sdb, sp, send, snow) = review_fixture(safe_dir.path());
    let (safe_worker, safe_turn) = start_review(&sc, &scfg, &mut sdb, &sp, send, snow);
    drop(safe_worker);
    let (safe_task, safe_completed) = completed_for(&safe_turn, snow + 1);
    let safe_bytes = serde_json::to_vec(&safe_completed).unwrap();
    let reviews = sc.directory.join("model/reviews");
    fs::create_dir_all(reviews.parent().unwrap()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), &reviews).unwrap();
    let outside_artifact = outside.path().join(format!(
        "{}.json",
        safe_task
            .review_identity()
            .expect("review task")
            .id()
            .unwrap()
    ));
    fs::write(&outside_artifact, &safe_bytes).unwrap();
    fs::set_permissions(&outside_artifact, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        recover(&sc.directory, &safe_turn, snow + 2)
            .unwrap_err()
            .to_string(),
        "owner_review_path_invalid"
    );
    assert_eq!(
        super::review_state(&sc.directory, &sc.session.to_string())["reason"],
        "owner_review_path_invalid"
    );
    assert_eq!(
        persist(&sc.directory, &safe_task, &safe_completed)
            .unwrap_err()
            .to_string(),
        "owner_review_path_invalid"
    );
    assert_eq!(fs::read(&outside_artifact).unwrap(), safe_bytes);
    fs::remove_file(&reviews).unwrap();
    fs::create_dir(&reviews).unwrap();
    fs::set_permissions(&reviews, fs::Permissions::from_mode(0o755)).unwrap();
    let inside_artifact = reviews.join(format!(
        "{}.json",
        safe_task
            .review_identity()
            .expect("review task")
            .id()
            .unwrap()
    ));
    fs::write(&inside_artifact, &safe_bytes).unwrap();
    fs::set_permissions(&inside_artifact, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        recover(&sc.directory, &safe_turn, snow + 3)
            .unwrap_err()
            .to_string(),
        "owner_review_path_invalid"
    );
    assert_eq!(
        super::review_state(&sc.directory, &sc.session.to_string())["reason"],
        "owner_review_path_invalid"
    );
    assert_eq!(
        persist(&sc.directory, &safe_task, &safe_completed)
            .unwrap_err()
            .to_string(),
        "owner_review_path_invalid"
    );
    assert_eq!(fs::read(&inside_artifact).unwrap(), safe_bytes);
    fs::set_permissions(&reviews, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&inside_artifact, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        recover(&sc.directory, &safe_turn, snow + 4)
            .unwrap_err()
            .to_string(),
        "owner_review_source_invalid"
    );
    assert_eq!(
        super::review_state(&sc.directory, &sc.session.to_string())["reason"],
        "owner_review_artifact_invalid"
    );
    assert_eq!(
        persist(&sc.directory, &safe_task, &safe_completed)
            .unwrap_err()
            .to_string(),
        "owner_review_source_invalid"
    );
    assert_eq!(fs::read(&inside_artifact).unwrap(), safe_bytes);
    fs::set_permissions(&inside_artifact, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(recover(&sc.directory, &safe_turn, snow + 5).unwrap());
    assert_eq!(
        super::review_state(&sc.directory, &sc.session.to_string())["available"],
        true
    );
    let target = sc.directory.join("review-target.json");
    fs::write(&target, &safe_bytes).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&inside_artifact).unwrap();
    std::os::unix::fs::symlink(&target, &inside_artifact).unwrap();
    assert_eq!(
        recover(&sc.directory, &safe_turn, snow + 6)
            .unwrap_err()
            .to_string(),
        "owner_review_source_invalid"
    );
    assert_eq!(
        super::review_state(&sc.directory, &sc.session.to_string())["reason"],
        "owner_review_artifact_invalid"
    );
    assert_eq!(fs::read(&target).unwrap(), safe_bytes);

    // A completed page-cache write is not durable until the same descriptor and
    // directory sync; recovery must not append a result before that.
    let sync_dir = tempfile::tempdir().unwrap();
    let (yc, ycfg, mut ydb, yp, yend, ynow) = review_fixture(sync_dir.path());
    let (sync_worker, sync_turn) = start_review(&yc, &ycfg, &mut ydb, &yp, yend, ynow);
    let mut sync_worker = sync_worker.unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    sync_worker.receiver = receiver;
    sender.send(Ok(valid_review_output(&sync_turn))).unwrap();
    fail_source_sync(2);
    assert_eq!(
        poll(
            &yc.directory,
            &sync_worker,
            &[],
            &Limits::default(),
            ynow + 1,
            yend,
            8,
            None
        ),
        Some(false)
    );
    let sync_log = session::SessionLog::new(&yc.directory, &yc.session.to_string());
    assert!(sync_log.result_for(&sync_turn.id).is_none());
    let sync_task = OwnerTask::from_turn(&sync_turn).unwrap().unwrap();
    let sync_artifact = artifact_path(&yc.directory, &sync_task).unwrap();
    assert!(sync_artifact.is_file());
    let sync_completed: ReviewCompletedV1 =
        serde_json::from_slice(&fs::read(&sync_artifact).unwrap()).unwrap();
    assert_eq!(sync_completed.receipt.completed_ms, ynow + 1);
    assert_eq!(state_runs(&yc), (0, 0, None));
    assert!(recover(&yc.directory, &sync_turn, ynow + 2).unwrap());
    let sync_result = sync_log.result_for(&sync_turn.id).unwrap();
    assert_eq!(sync_result.outcome, "recovered_review_receipt");
    assert_eq!(sync_result.finished_at_ms, ynow + 1);
    assert_eq!(state_runs(&yc), (1, 0, Some(ynow + 1)));
    let sync_records = sync_log.records().len();
    assert!(recover(&yc.directory, &sync_turn, ynow + 3).unwrap());
    assert_eq!(sync_log.records().len(), sync_records);
    assert_eq!(state_runs(&yc), (1, 0, Some(ynow + 1)));
    fail_source_sync(0);

    // A corrupt action receipt is an explicit refusal, not an empty receipt.
    let action_dir = tempfile::tempdir().unwrap();
    let (ac, acfg, mut adb, ap, aend, anow) = review_fixture(action_dir.path());
    let (action_worker, action_turn) = start_review(&ac, &acfg, &mut adb, &ap, aend, anow);
    drop(action_worker);
    let (action_task, action_completed) = completed_for(&action_turn, anow + 1);
    let action_log = session::SessionLog::new(&ac.directory, &ac.session.to_string());
    let action_path = ac
        .directory
        .join(session::ROOT_PREFIX)
        .join(ac.session.to_string())
        .join("actions")
        .join(format!("{}.json", action_turn.id));
    fs::create_dir_all(action_path.parent().unwrap()).unwrap();
    fs::write(&action_path, b"{").unwrap();
    assert_eq!(
        recover(&ac.directory, &action_turn, anow + 2)
            .unwrap_err()
            .to_string(),
        "session_action_receipt_invalid"
    );
    assert!(action_log.result_for(&action_turn.id).is_none());
    fs::remove_file(&action_path).unwrap();
    let expected_action = review_action(&action_turn, &action_completed.receipt);
    session::commit_action(
        &ac.directory,
        &ac.session.to_string(),
        &action_turn.id,
        &expected_action,
    )
    .unwrap();
    assert_eq!(
        recover(&ac.directory, &action_turn, anow + 3)
            .unwrap_err()
            .to_string(),
        "owner_review_source_missing"
    );
    assert!(action_log.result_for(&action_turn.id).is_none());
    persist(&ac.directory, &action_task, &action_completed).unwrap();
    assert!(recover(&ac.directory, &action_turn, anow + 4).unwrap());
    let action_result = action_log.result_for(&action_turn.id).unwrap();
    assert_eq!(action_result.outcome, "recovered_review_receipt");
    assert_eq!(action_result.actions, vec![expected_action.clone()]);
    assert_eq!(
        session::actions_for(&ac.directory, &ac.session.to_string(), &action_turn.id).len(),
        1
    );

    // One sealed report is one attempt; later evidence ticks do not authorize a
    // second call, while a changed original report is an integrity conflict.
    let schedule_dir = tempfile::tempdir().unwrap();
    let (rc, rcfg, mut rdb, rp, rend, rnow) = review_fixture(schedule_dir.path());
    let (review_worker, _review_turn) = start_review(&rc, &rcfg, &mut rdb, &rp, rend, rnow);
    drop(review_worker);
    let review_log = session::SessionLog::new(&rc.directory, &rc.session.to_string());
    assert_eq!(review_log.turns().len(), 1);
    rdb.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,NULL,'review_fixture_tick')",
        rusqlite::params![rnow + 1],
    )
    .unwrap();
    assert!(try_start_review(&rc, &rcfg, &mut rdb, &rp, rend, rnow + 2).is_none());
    assert_eq!(review_log.turns().len(), 1);
    assert_eq!(
        super::review_status(&rc, &rcfg, &rp, rnow + 2, rend)["status"],
        "attempted"
    );
    let report_path = rc.directory.join("report.json");
    let mut report_bytes = fs::read(&report_path).unwrap();
    report_bytes.push(b'\n');
    fs::write(&report_path, &report_bytes).unwrap();
    assert_eq!(
        consider_result(&rc, &rcfg, &mut rdb, &rp, rend, rnow + 3)
            .unwrap_err()
            .to_string(),
        "owner_review_report_changed"
    );
    assert_eq!(review_log.turns().len(), 1);
    let conflict_status = super::review_status(&rc, &rcfg, &rp, rnow + 3, rend);
    assert_eq!(conflict_status["status"], "invalid");
    assert_eq!(conflict_status["reason"], "owner_review_report_changed");
    assert_eq!(conflict_status["review_due"], false);

    let date_dir = tempfile::tempdir().unwrap();
    let (dc, dcfg, mut ddb, dp, dend, dnow) = review_fixture(date_dir.path());
    let shifted_end = dend + 86_400_000;
    let shifted_now = dnow + 86_400_000;
    let date_status = super::review_status(&dc, &dcfg, &dp, shifted_now, shifted_end);
    assert_eq!(date_status["status"], "invalid");
    assert_eq!(date_status["reason"], "owner_review_session_date_mismatch");
    assert_eq!(date_status["review_due"], false);
    assert_eq!(
        consider_result(&dc, &dcfg, &mut ddb, &dp, shifted_end, shifted_now)
            .unwrap_err()
            .to_string(),
        "owner_review_session_date_mismatch"
    );
    assert_eq!(
        session::SessionLog::new(&dc.directory, &dc.session.to_string())
            .turns()
            .len(),
        0
    );

    // Every terminal review path updates the same run/failure cursor once.
    let err_dir = tempfile::tempdir().unwrap();
    let (ec, ecfg, mut edb, ep, eend, enow) = review_fixture(err_dir.path());
    let (err_worker, _err_turn) = start_review(&ec, &ecfg, &mut edb, &ep, eend, enow);
    let mut err_worker = err_worker.unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    err_worker.receiver = receiver;
    sender.send(Err("model_http_500".into())).unwrap();
    assert_eq!(
        poll(
            &ec.directory,
            &err_worker,
            &[],
            &Limits::default(),
            enow + 1,
            eend,
            8,
            None
        ),
        Some(false)
    );
    assert_eq!(state_runs(&ec), (1, 1, Some(enow + 1)));

    let invalid_dir = tempfile::tempdir().unwrap();
    let (ic, icfg, mut idb, ip, iend, inow) = review_fixture(invalid_dir.path());
    let (invalid_worker, _invalid_turn) = start_review(&ic, &icfg, &mut idb, &ip, iend, inow);
    let mut invalid_worker = invalid_worker.unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    invalid_worker.receiver = receiver;
    sender
        .send(Ok(
            json!({"actions":[],"stance":"hold","notes":""}).to_string()
        ))
        .unwrap();
    assert_eq!(
        poll(
            &ic.directory,
            &invalid_worker,
            &[],
            &Limits::default(),
            inow + 1,
            iend,
            8,
            None
        ),
        Some(false)
    );
    assert_eq!(state_runs(&ic), (1, 1, Some(inow + 1)));

    let interrupt_dir = tempfile::tempdir().unwrap();
    let (nc, ncfg, mut ndb, np, nend, nnow) = review_fixture(interrupt_dir.path());
    let (interrupt_worker, interrupt_turn) = start_review(&nc, &ncfg, &mut ndb, &np, nend, nnow);
    drop(interrupt_worker);
    assert!(recover(&nc.directory, &interrupt_turn, nnow + 1).unwrap());
    assert_eq!(state_runs(&nc), (1, 1, Some(nnow + 1)));

    let recovered_dir = tempfile::tempdir().unwrap();
    let (gc, gcfg, mut gdb, gp, gend, gnow) = review_fixture(recovered_dir.path());
    let (recovered_worker, recovered_turn) = start_review(&gc, &gcfg, &mut gdb, &gp, gend, gnow);
    drop(recovered_worker);
    let (recovered_task, recovered_completed) = completed_for(&recovered_turn, gnow + 1);
    persist(&gc.directory, &recovered_task, &recovered_completed).unwrap();
    assert!(recover(&gc.directory, &recovered_turn, gnow + 2).unwrap());
    assert_eq!(state_runs(&gc), (1, 0, Some(gnow + 1)));

    let failed_dir = tempfile::tempdir().unwrap();
    let (hc, hcfg, mut hdb, hp, hend, hnow) = review_fixture(failed_dir.path());
    let (failed_worker, failed_turn) = start_review(&hc, &hcfg, &mut hdb, &hp, hend, hnow);
    drop(failed_worker);
    let (failed_task, failed_completed) =
        failed_for(&failed_turn, hnow + 1, "owner_review_output_invalid");
    persist(&hc.directory, &failed_task, &failed_completed).unwrap();
    assert!(recover(&hc.directory, &failed_turn, hnow + 2).unwrap());
    assert_eq!(state_runs(&hc), (1, 1, Some(hnow + 1)));

    let lost_dir = tempfile::tempdir().unwrap();
    let (lc, lcfg, mut ldb, lp, lend, lnow) = review_fixture(lost_dir.path());
    let (lost_worker, lost_turn) = start_review(&lc, &lcfg, &mut ldb, &lp, lend, lnow);
    drop(lost_worker);
    let (lost_task, lost_completed) = completed_for(&lost_turn, lnow + 1);
    persist(&lc.directory, &lost_task, &lost_completed).unwrap();
    assert!(recover(&lc.directory, &lost_turn, lnow + 2).unwrap());
    let lost_result = session::SessionLog::new(&lc.directory, &lc.session.to_string())
        .result_for(&lost_turn.id)
        .unwrap();
    assert_eq!(state_runs(&lc), (1, 0, Some(lnow + 1)));
    fs::remove_file(lc.directory.join("model/supervisor-state.json")).unwrap();
    assert!(super::reconcile_lifecycle_results(&lc.directory, &lc.session.to_string()).unwrap());
    assert_eq!(state_runs(&lc), (1, 0, Some(lost_result.finished_at_ms)));
    let lost_records = session::SessionLog::new(&lc.directory, &lc.session.to_string())
        .records()
        .len();
    assert!(!super::reconcile_lifecycle_results(&lc.directory, &lc.session.to_string()).unwrap());
    assert_eq!(
        session::SessionLog::new(&lc.directory, &lc.session.to_string())
            .records()
            .len(),
        lost_records
    );
    assert_eq!(state_runs(&lc), (1, 0, Some(lost_result.finished_at_ms)));

    let decision_dir = tempfile::tempdir().unwrap();
    let (jc, jcfg, mut jdb, jp, jend, jnow) = review_fixture(decision_dir.path());
    let (decision_worker, decision_turn) = start_review(&jc, &jcfg, &mut jdb, &jp, jend, jnow);
    drop(decision_worker);
    let (decision_task, decision_completed) = completed_for(&decision_turn, jnow + 1);
    persist(&jc.directory, &decision_task, &decision_completed).unwrap();
    super::state::with_state(&jc.directory, &jc.session.to_string(), |state| {
        state.failures = 2;
    })
    .unwrap();
    assert!(recover(&jc.directory, &decision_turn, jnow + 2).unwrap());
    assert_eq!(state_runs(&jc), (1, 2, Some(jnow + 1)));
}
