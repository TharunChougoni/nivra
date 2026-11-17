//! A sealed review is a one-attempt owner turn with no financial capabilities.
use super::super::owner_tasks::{
    self, OwnerReviewOutput, OwnerTask, ReviewIdentity, ReviewReceipt,
};
use super::*;
use nivra_core::postclose::SealedEvidence;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(super) use tests::*;

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
