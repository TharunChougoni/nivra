//! Bounded analysis output and the verified result a succeeded task stores.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    pub name: String,
    pub value: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisResult {
    pub identity: ExecutionIdentity,
    pub script_hash: String,
    pub runtime_hash: String,
    pub input_hash: String,
    pub metrics: Vec<Metric>,
    pub observations: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedTaskResult {
    pub task_id: String,
    pub revision: u32,
    pub job_id: String,
    pub lease: String,
    pub result_hash: String,
    pub terminal_at_ms: Option<i64>,
    pub result: AnalysisResult,
}
fn number(s: &str) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    let unsigned = s.strip_prefix('-').unwrap_or(s);
    let mut parts = unsigned.split('.');
    let whole = parts.next().unwrap_or_default();
    !whole.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && parts
            .next()
            .is_none_or(|f| !f.is_empty() && f.len() <= 18 && f.bytes().all(|b| b.is_ascii_digit()))
        && parts.next().is_none()
}
pub(super) fn output(raw: &[u8], launch: &Launch) -> Result<AnalysisResult> {
    if raw.len() > launch.spec.output_bytes as usize || raw.len() > 65536 {
        return Err(Error::Result);
    }
    // Every object has a deny_unknown_fields typed schema; derive rejects repeated
    // fields, including nested identities/metrics. No opaque JSON escape hatch.
    let result: AnalysisResult = serde_json::from_slice(raw).map_err(|_| Error::Result)?;
    if result.identity != launch.identity
        || result.script_hash != launch.spec.script_hash
        || result.runtime_hash != launch.spec.runtime_hash
        || result.input_hash != launch.spec.input_hash
        || result.metrics.len() > 64
        || result.observations.len() > 32
        || result
            .metrics
            .iter()
            .any(|m| !alias(&m.name) || !number(&m.value))
        || result
            .metrics
            .iter()
            .map(|m| &m.name)
            .collect::<BTreeSet<_>>()
            .len()
            != result.metrics.len()
        || result
            .observations
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 1024 || s.chars().any(char::is_control))
    {
        return Err(Error::Result);
    }
    Ok(result)
}

/// Read the immutable worker receipt only after the existing job row is
/// succeeded and its lease/result hash match. No pending, cancelled,
/// ambiguous, or merely present result artifact is exposed.
pub fn read_verified_result(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
) -> Result<Option<VerifiedTaskResult>> {
    let task = jobs::load_scoped(db, task_id, revision).map_err(|_| Error::Queue)?;
    if task.status != jobs::TaskState::Succeeded {
        return Ok(None);
    }
    let receipts_ready: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='registered_worker_receipts')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| Error::Queue)?;
    if !receipts_ready {
        return Err(Error::Receipt);
    }
    let terminal_at_ms = task.terminal_at_ms.ok_or(Error::State)?;
    if terminal_at_ms < task.spec.created_at_ms || terminal_at_ms > task.spec.expires_at_ms {
        return Err(Error::State);
    }
    let lease = task.job.lease.as_deref().ok_or(Error::State)?;
    let result_hash = task.job.result_hash.as_deref().ok_or(Error::State)?;
    if !is_hash(lease) || !is_hash(result_hash) {
        return Err(Error::Receipt);
    }
    let identity = ExecutionIdentity {
        job_id: task.spec.job_id.clone(),
        lease: lease.to_owned(),
        registration_sha256: String::new(),
    };
    let launch_bytes = saved(db, &identity, "launch")?.ok_or(Error::Receipt)?;
    let launch: Launch = serde_json::from_slice(&launch_bytes).map_err(|_| Error::Receipt)?;
    if launch.identity.job_id != task.spec.job_id
        || launch.identity.lease != lease
        || canonical(&launch)? != launch_bytes
        || !is_hash(&launch.identity.registration_sha256)
        || launch.spec != task.job.spec
        || launch.spec.script_hash != task.spec.registered_analysis.code_sha256
        || launch.spec.runtime_hash != task.spec.registered_analysis.runtime_sha256
        || launch.spec.input_hash != task.spec.registered_analysis.input_sha256
        || launch.spec.memory_bytes != task.spec.resources.memory_bytes
        || launch.spec.output_bytes != task.spec.resources.output_bytes
        || launch.spec.wall_millis != task.spec.resources.wall_millis
        || launch.limits.cpu_millis != task.spec.resources.cpu_millis
        || launch.limits.tasks != task.spec.resources.tasks
        || launch.limits.workspace_bytes != task.spec.resources.workspace_bytes
        || launch.limits.file_bytes != task.spec.resources.file_bytes
        || launch.started_ms < task.spec.created_at_ms
        || launch.started_ms >= task.spec.expires_at_ms
        || launch.deadline_ms
            != launch
                .started_ms
                .saturating_add(i64::from(launch.spec.wall_millis))
        || digest(&(&launch.spec, &launch.split, &launch.limits))?
            != launch.identity.registration_sha256
    {
        return Err(Error::Receipt);
    }
    let stopped_bytes = saved(db, &launch.identity, "stopped")?.ok_or(Error::Receipt)?;
    let stopped: StoppedReceipt =
        serde_json::from_slice(&stopped_bytes).map_err(|_| Error::Receipt)?;
    if stopped.identity != launch.identity
        || !is_hash(&stopped.evidence_sha256)
        || canonical(&stopped)? != stopped_bytes
    {
        return Err(Error::Receipt);
    }
    let raw = saved(db, &launch.identity, "result")?.ok_or(Error::Receipt)?;
    if raw.len() > launch.spec.output_bytes as usize
        || raw.len() > 65_536
        || hash(&raw) != result_hash
    {
        return Err(Error::Result);
    }
    let result = output(&raw, &launch)?;
    if result.identity != launch.identity
        || result.script_hash != task.spec.registered_analysis.code_sha256
        || result.runtime_hash != task.spec.registered_analysis.runtime_sha256
        || result.input_hash != task.spec.registered_analysis.input_sha256
    {
        return Err(Error::Result);
    }
    Ok(Some(VerifiedTaskResult {
        task_id: task.spec.task_id,
        revision: task.spec.revision,
        job_id: task.spec.job_id,
        lease: lease.to_owned(),
        result_hash: result_hash.to_owned(),
        terminal_at_ms: Some(terminal_at_ms),
        result,
    }))
}
