//! Candidate ranker tasks and their bounded artifacts.
use super::queue::{enqueue_tx, event, immediate, read};
use super::scoped::{ensure_scoped_tables, scoped_tables_exist, task_state};
use super::*;

impl CandidateTaskV1 {
    fn validate(&self, job: &Spec, now: i64) -> Result<()> {
        let valid_text = |s: &str, max: usize| {
            !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control)
        };
        let valid_slug = |s: &str, max: usize| {
            !s.is_empty()
                && s.len() <= max
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
        };
        let caps = &self.resources;
        if self.owner_evidence.as_ref().is_some_and(|e| {
            !valid_text(&e.digest_id, 256)
                || !valid_hash(&e.owner_work_id)
                || !valid_hash(&e.facts_sha256)
                || !valid_hash(&e.prompt_hashes_sha256)
                || !valid_hash(&e.owner_claim_sha256)
                || e.profile_sha256.as_deref().is_some_and(|h| !valid_hash(h))
        }) {
            return Err(Error::Invalid);
        }
        let execution_valid = self
            .execution
            .as_ref()
            .is_none_or(CandidateExecutionBinding::validate);
        let purpose_valid = match &self.purpose {
            None => true,
            Some(CandidatePurposeV1::Observe { target }) => {
                target.validate()
                    && self.execution.is_none()
                    && target.snapshot_sha256 == self.dataset_hash
                    && target.runtime.source_sha256 == self.source_sha256
                    && target.runtime.params_sha256 == self.params_sha256
                    && target.runtime.runtime_hash == self.runtime_binding.runtime_hash
                    && target.runtime.helper_sha256 == self.runtime_binding.helper_sha256
                    && target.runtime.unit_policy_sha256 == self.runtime_binding.unit_policy_sha256
                    && target.runtime.scorer_compare_sha256
                        == self.runtime_binding.scorer_compare_sha256
                    && target.runtime.sandbox_source_sha256
                        == self.runtime_binding.sandbox_source_sha256
                    && target.holding_ms == self.horizon_minutes.saturating_mul(60_000)
                    && (0..=60_000).contains(&self.max_exit_delay_ms)
                    && target.max_exit_delay_ms == self.max_exit_delay_ms
                    && target.window_from_ms == self.windows.selection_from_ms
                    && target.window_to_ms <= self.windows.selection_to_ms
                    && target.signal_ms == self.created_at_ms
                    && target.target_frozen_at_ms == self.created_at_ms
            }
        };
        let parent_valid = match (&self.parent_ref, &self.parent_task_id) {
            (None, _) => true,
            (Some(parent), None) => {
                // A cross-session parent is a versioned CONTINUATION, not a
                // claim that the new source/params are the same algorithm. The
                // original parent hashes stay in the parent_ref provenance and
                // the new source/params key the actual input debt. A
                // prospective observation still reuses the original algorithm
                // exactly, so its source/params must match the parent.
                parent.validate()
                    && self.parent_candidate_id.as_deref() == Some(parent.candidate_id.as_str())
                    && self.parent_lifecycle_candidate_id.as_deref()
                        == Some(parent.lifecycle_candidate_id.as_str())
                    && parent.runtime_hash == self.runtime_binding.runtime_hash
                    && (self.purpose.is_none()
                        || (parent.source_sha256 == self.source_sha256
                            && parent.params_sha256 == self.params_sha256))
            }
            (Some(_), Some(_)) => false,
        };
        let execution_runtime_sha256 =
            candidate_execution_runtime_hash(&self.runtime_binding, &self.sandbox);
        let observation_keys = match &self.purpose {
            Some(CandidatePurposeV1::Observe { target }) => Some((
                candidate_observation_content_key(
                    &self.source_sha256,
                    &self.params_sha256,
                    &self.dataset_hash,
                    &target.quotes_sha256,
                    &target.spec_sha256,
                    target.window_from_ms,
                    target.window_to_ms,
                    target.holding_ms,
                    target.max_exit_delay_ms,
                    &self.runtime_binding.runtime_hash,
                    &target.runtime.replay_abi,
                    &self.unit_policy.policy_sha256(),
                ),
                candidate_observation_trial_input_key(
                    &self.source_sha256,
                    &self.params_sha256,
                    &self.dataset_hash,
                    &target.quotes_sha256,
                    &target.spec_sha256,
                    &self.runtime_binding.runtime_hash,
                    &target.runtime.replay_abi,
                ),
                candidate_observation_input_hash(
                    &self.params_sha256,
                    &self.dataset_hash,
                    &target.quotes_sha256,
                    &target.spec_sha256,
                    &self.runtime_binding.runtime_hash,
                ),
            )),
            _ => None,
        };
        let (expected_content_key, expected_input_key, expected_job_input_hash) =
            match (&self.execution, observation_keys) {
                (_, Some(keys)) => keys,
                (Some(execution), None) if execution_valid => (
                    candidate_execution_content_key(
                        &self.source_sha256,
                        &self.params_sha256,
                        &self.dataset_hash,
                        &self.windows,
                        self.horizon_minutes,
                        &self.abi,
                        &self.unit_policy.policy_sha256(),
                        execution,
                        &execution_runtime_sha256,
                    ),
                    candidate_execution_trial_input_key(
                        &self.source_sha256,
                        &self.params_sha256,
                        &self.dataset_hash,
                        &self.abi,
                        execution,
                        &execution_runtime_sha256,
                    ),
                    candidate_execution_input_hash(
                        &self.params_sha256,
                        &self.dataset_hash,
                        execution,
                        &execution_runtime_sha256,
                    ),
                ),
                (Some(_), None) => (String::new(), String::new(), String::new()),
                (None, None) => (
                    candidate_content_key(
                        &self.source_sha256,
                        &self.params_sha256,
                        &self.dataset_hash,
                        &self.windows,
                        self.horizon_minutes,
                        &self.abi,
                        &self.unit_policy.policy_sha256(),
                    ),
                    candidate_trial_input_key(
                        &self.source_sha256,
                        &self.params_sha256,
                        &self.dataset_hash,
                        &self.abi,
                    ),
                    candidate_input_hash(&self.params_sha256, &self.dataset_hash),
                ),
            };
        if self.version != 1
            || !execution_valid
            || !purpose_valid
            || !parent_valid
            || !valid_hash(&self.task_id)
            || !valid_hash(&self.job_id)
            || !valid_hash(&self.candidate_id)
            || !valid_hash(&self.lifecycle_candidate_id)
            || !valid_hash(&self.content_key)
            || !valid_hash(&self.input_key)
            || self.revision != 1
            || !valid_slug(&self.task_key, 48)
            || !valid_text(&self.parent_turn_id, 256)
            || chrono::NaiveDate::parse_from_str(&self.session, "%Y-%m-%d").is_err()
            || !valid_hash(&self.book_binding)
            || !valid_hash(&self.policy_sha256)
            || !valid_text(&self.model_id, 128)
            || !valid_text(&self.hypothesis, 1024)
            || !valid_hash(&self.source_sha256)
            || !valid_hash(&self.params_sha256)
            || !valid_hash(&self.dataset_hash)
            || !valid_slug(&self.dataset_id, 64)
            || !self.windows.validate()
            || !(1..=1440).contains(&self.horizon_minutes)
            || self.abi != "scorer_replay_v1"
            || !self.runtime_binding.validate()
            || !self.sandbox.validate()
            || !self
                .unit_policy
                .validate_for(&self.sandbox, &self.resources)
            || self.runtime_binding.helper_sha256 != self.unit_policy.helper_sha256
            || self.runtime_binding.unit_policy_sha256 != self.unit_policy.policy_sha256()
            || self.task_id
                != candidate_task_id(
                    &self.book_binding,
                    &self.session,
                    &self.task_key,
                    &self.content_key,
                    self.parent_candidate_id.as_deref(),
                )
            || self.candidate_id
                != candidate_version_id(
                    &self.book_binding,
                    &self.session,
                    &self.task_key,
                    &self.content_key,
                    self.parent_candidate_id.as_deref(),
                )
            || self.job_id != candidate_job_id(&self.task_id, self.revision)
            || self.job_id != job.id
            || self.content_key != expected_content_key
            || self.input_key != expected_input_key
            || job.capability != SpecCapability::CandidateSource
            || job.script_hash != self.source_sha256
            || job.input_hash != expected_job_input_hash
            || job.runtime_hash != self.runtime_binding.runtime_hash
            || job.retry_safe_compute
            || job.max_attempts != 1
            || caps.memory_bytes != job.memory_bytes
            || caps.output_bytes != job.output_bytes
            || caps.wall_millis != job.wall_millis
            || caps.cpu_millis == 0
            || caps.cpu_millis > caps.wall_millis
            || caps.tasks != 1
            || caps.workspace_bytes != 1_048_576
            || caps.file_bytes != 65_536
            || caps.wall_millis == 0
            || caps.wall_millis > 600_000
            || caps.output_bytes == 0
            || caps.output_bytes > 65_536
            || caps.memory_bytes == 0
            || caps.memory_bytes > 4 * 1024 * 1024 * 1024
            || self.created_at_ms != now
            || self.expires_at_ms <= now
            || self.expires_at_ms > now.checked_add(86_400_000).ok_or(Error::Invalid)?
            || now < 0
            || (self.parent_candidate_id.is_some() != self.parent_lifecycle_candidate_id.is_some())
            || (self.parent_ref.is_none()
                && self.purpose.is_none()
                && (self.parent_candidate_id.is_some() != self.parent_task_id.is_some()))
            || (self.parent_ref.is_some() && self.parent_task_id.is_some())
            || self
                .parent_candidate_id
                .as_deref()
                .is_some_and(|id| !valid_hash(id))
            || self
                .parent_lifecycle_candidate_id
                .as_deref()
                .is_some_and(|id| !valid_hash(id))
            || self
                .parent_task_id
                .as_deref()
                .is_some_and(|id| !valid_hash(id))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

fn ensure_candidate_tables(tx: &Transaction<'_>) -> Result<()> {
    ensure_scoped_tables(tx)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS candidate_tasks(task_id TEXT NOT NULL,revision INTEGER NOT NULL,job_id TEXT NOT NULL UNIQUE,session TEXT NOT NULL,parent_turn_id TEXT NOT NULL,content_key TEXT NOT NULL,input_key TEXT NOT NULL,body TEXT NOT NULL,created_ms INTEGER NOT NULL,cancel_receipt TEXT,refusal_receipt TEXT,PRIMARY KEY(task_id,revision));\
         CREATE INDEX IF NOT EXISTS candidate_tasks_session ON candidate_tasks(session);\
         CREATE INDEX IF NOT EXISTS candidate_tasks_input ON candidate_tasks(session,input_key);\
         CREATE TABLE IF NOT EXISTS candidate_task_events(seq INTEGER PRIMARY KEY,task_id TEXT NOT NULL,revision INTEGER NOT NULL,event TEXT NOT NULL,at_ms INTEGER NOT NULL);\
         CREATE TABLE IF NOT EXISTS candidate_artifacts(hash TEXT PRIMARY KEY,kind TEXT NOT NULL,body BLOB NOT NULL,created_ms INTEGER NOT NULL);",
    )
    .map_err(|_| Error::Storage)?;
    Ok(())
}

pub(super) fn candidate_tables_exist(tx: &Transaction<'_>) -> Result<bool> {
    if !scoped_tables_exist(tx)? {
        return Ok(false);
    }
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('candidate_tasks','candidate_task_events','candidate_artifacts')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    Ok(count == 3)
}

fn candidate_tx(
    tx: &Transaction<'_>,
    task_id: &str,
    revision: u32,
) -> Result<Option<CandidateTaskRecord>> {
    if !valid_hash(task_id) || revision == 0 {
        return Err(Error::Invalid);
    }
    type CandidateRow = (
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        String,
        i64,
    );
    let row: Option<CandidateRow> = tx
        .query_row(
            "SELECT body,cancel_receipt,refusal_receipt,session,parent_turn_id,content_key,input_key,created_ms FROM candidate_tasks WHERE task_id=? AND revision=? AND length(body)<=16384",
            params![task_id, revision],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    let Some((body, cancel, refusal, session, parent_turn_id, content_key, input_key, created_ms)) =
        row
    else {
        return Ok(None);
    };
    let spec: CandidateTaskV1 = serde_json::from_str(&body).map_err(|_| Error::Invalid)?;
    if spec.task_id != task_id || spec.revision != revision {
        return Err(Error::Invalid);
    }
    let job = read(tx, &spec.job_id)?.ok_or(Error::Missing)?;
    spec.validate(&job.spec, created_ms)
        .map_err(|_| Error::Invalid)?;
    if spec.session != session
        || spec.parent_turn_id != parent_turn_id
        || spec.content_key != content_key
        || spec.input_key != input_key
    {
        return Err(Error::Invalid);
    }
    let cancel_receipt: Option<CancelReceipt> = cancel
        .map(|body| serde_json::from_str(&body).map_err(|_| Error::Invalid))
        .transpose()?;
    let refusal_receipt: Option<RefusalReceipt> = refusal
        .map(|body| serde_json::from_str(&body).map_err(|_| Error::Invalid))
        .transpose()?;
    if cancel_receipt.as_ref().is_some_and(|r| {
        r.task_id != task_id
            || r.revision != revision
            || r.expected_revision != revision
            || r.lease != job.lease
    }) || refusal_receipt.as_ref().is_some_and(|r| {
        r.task_id != task_id
            || r.revision != revision
            || r.job_id != spec.job_id
            || job.state != State::Refused
            || job.attempts != 0
    }) || (cancel_receipt.is_some() && refusal_receipt.is_some())
    {
        return Err(Error::Invalid);
    }
    if (job.state == State::Refused) != refusal_receipt.is_some() {
        return Err(Error::Invalid);
    }
    let status = task_state(job.state);
    let terminal_at_ms = if matches!(
        status,
        TaskState::Succeeded | TaskState::Terminated | TaskState::Cancelled | TaskState::Refused
    ) {
        tx.query_row(
            "SELECT max(at_ms) FROM (SELECT at_ms FROM job_events WHERE job=? AND (event IN ('completed','completed_reconciled') OR event LIKE 'terminated:%') UNION ALL SELECT at_ms FROM candidate_task_events WHERE task_id=? AND revision=? AND event IN ('cancelled','refused'))",
            params![spec.job_id, task_id, revision],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?
        .flatten()
    } else {
        None
    };
    Ok(Some(CandidateTaskRecord {
        spec,
        status,
        job,
        cancel_receipt,
        refusal_receipt,
        terminal_at_ms,
    }))
}

fn candidate_event(
    tx: &Transaction<'_>,
    task_id: &str,
    revision: u32,
    name: &str,
    now: i64,
) -> Result<()> {
    let last: Option<i64> = tx
        .query_row(
            "SELECT max(at_ms) FROM candidate_task_events WHERE task_id=? AND revision=?",
            params![task_id, revision],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if last.is_some_and(|last| now < last) {
        return Err(Error::Invalid);
    }
    tx.execute(
        "INSERT INTO candidate_task_events(task_id,revision,event,at_ms) VALUES(?,?,?,?)",
        params![task_id, revision, name, now],
    )
    .map_err(|_| Error::Storage)?;
    Ok(())
}

fn latest_candidate_key_tx(
    tx: &Transaction<'_>,
    session: &str,
    book_binding: &str,
    task_key: &str,
) -> Result<Option<CandidateTaskRecord>> {
    let key: Option<(String, u32)> = tx
        .query_row(
            "SELECT task_id,revision FROM candidate_tasks WHERE session=? AND json_extract(body,'$.book_binding')=? AND json_extract(body,'$.task_key')=? ORDER BY created_ms DESC,rowid DESC LIMIT 1",
            params![session, book_binding, task_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    match key {
        Some((id, revision)) => candidate_tx(tx, &id, revision),
        None => Ok(None),
    }
}

pub fn latest_candidate_for_key(
    db: &mut Connection,
    session: &str,
    book_binding: &str,
    task_key: &str,
) -> Result<Option<CandidateTaskRecord>> {
    if chrono::NaiveDate::parse_from_str(session, "%Y-%m-%d").is_err()
        || !valid_hash(book_binding)
        || task_key.is_empty()
        || task_key.len() > 48
    {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let result = latest_candidate_key_tx(&tx, session, book_binding, task_key)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

/// Atomically create a candidate task, its shared compute job and the three
/// immutable input artifacts. Admission is evaluated before any artifact is
/// written, so a deferred or colliding submission leaves no orphan blob.
pub fn enqueue_candidate_bounded(
    db: &mut Connection,
    job_spec: &Spec,
    task_spec: &CandidateTaskV1,
    artifacts: &[(&str, &[u8])],
    now: i64,
    limits: Option<&AdmissionLimits>,
) -> Result<CandidateAdmission> {
    if let Some(limits) = limits {
        limits.validate()?;
    }
    task_spec.validate(job_spec, task_spec.created_at_ms)?;
    if now < task_spec.created_at_ms {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_candidate_tables(&tx)?;
    let body = serde_json::to_string(task_spec).map_err(|_| Error::Invalid)?;
    if let Some(old) = candidate_tx(&tx, &task_spec.task_id, task_spec.revision)? {
        if old.spec == *task_spec && old.job.spec == *job_spec {
            return Ok(CandidateAdmission::Admitted(Box::new(old)));
        }
        return Err(Error::Collision);
    }
    if task_spec.created_at_ms != now {
        return Err(Error::Invalid);
    }
    if let Some(previous) = latest_candidate_key_tx(
        &tx,
        &task_spec.session,
        &task_spec.book_binding,
        &task_spec.task_key,
    )? {
        if previous.spec.content_key == task_spec.content_key {
            return Err(Error::Collision);
        }
        if matches!(
            previous.status,
            TaskState::Pending | TaskState::Unknown | TaskState::CancelRequested
        ) {
            return Err(Error::State);
        }
        if task_spec.parent_candidate_id.as_deref() != Some(previous.spec.candidate_id.as_str())
            || task_spec.parent_task_id.as_deref() != Some(previous.spec.task_id.as_str())
            || task_spec.parent_lifecycle_candidate_id.as_deref()
                != Some(previous.spec.lifecycle_candidate_id.as_str())
        {
            return Err(Error::Collision);
        }
    } else if task_spec.parent_candidate_id.is_some()
        && task_spec.parent_ref.is_none()
        && task_spec.purpose.is_none()
    {
        return Err(Error::State);
    }
    if let Some(limits) = limits {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS scoped_admission_deferrals(task_id TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(task_id,revision));").map_err(|_| Error::Storage)?;
        let (registered_total, candidate_total): (u32, u32) = tx
            .query_row(
                "SELECT (SELECT count(*) FROM scoped_tasks WHERE session=?1 AND json_extract(body,'$.book_binding')=?2),(SELECT count(*) FROM candidate_tasks WHERE session=?1 AND json_extract(body,'$.book_binding')=?2)",
                params![task_spec.session, task_spec.book_binding],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| Error::Storage)?;
        let total = registered_total.saturating_add(candidate_total);
        let same: u32 = tx
            .query_row(
                "SELECT count(*) FROM candidate_tasks WHERE session=? AND json_extract(body,'$.book_binding')=? AND input_key=?",
                params![task_spec.session, task_spec.book_binding, task_spec.input_key],
                |r| r.get(0),
            )
            .map_err(|_| Error::Storage)?;
        let deferred = if total >= limits.max_session_trials {
            Some(("research_session_compute_budget_exhausted", total))
        } else if same >= limits.max_trials_per_input {
            Some(("research_unchanged_input_budget_exhausted", same))
        } else {
            None
        };
        if let Some((reason, trials)) = deferred {
            let body = serde_json::to_string(&serde_json::json!({"reason":reason,"trials":trials,"requested":task_spec,"observed_ms":now,"next_condition":if total >= limits.max_session_trials {"next_authorized_session"} else {"new_candidate_input_or_reviewed_compute_policy"}})).map_err(|_| Error::Invalid)?;
            tx.execute(
                "INSERT OR IGNORE INTO scoped_admission_deferrals(task_id,revision,body) VALUES(?,?,?)",
                params![task_spec.task_id, task_spec.revision, body],
            )
            .map_err(|_| Error::Storage)?;
            tx.commit().map_err(|_| Error::Storage)?;
            return Ok(CandidateAdmission::Deferred { reason, trials });
        }
    }
    if read(&tx, &job_spec.id)?.is_some() {
        return Err(Error::Collision);
    }
    if task_spec.execution.is_some() {
        for kind in ["execution_spec", "execution_manifest", "execution_events"] {
            if !artifacts.iter().any(|(name, _)| *name == kind) {
                return Err(Error::Invalid);
            }
        }
    }
    if let Some(CandidatePurposeV1::Observe { target }) = &task_spec.purpose {
        for kind in ["observation_spec", "observation_quotes"] {
            if !artifacts.iter().any(|(name, _)| *name == kind) {
                return Err(Error::Invalid);
            }
        }
        let _ = target;
    }
    for &(kind, artifact) in artifacts {
        let hash = put_candidate_artifact_tx(&tx, kind, artifact, now)?;
        let expected = match kind {
            "source" => Some(&task_spec.source_sha256),
            "params" => Some(&task_spec.params_sha256),
            "dataset" => Some(&task_spec.dataset_hash),
            "execution_spec" => task_spec
                .execution
                .as_ref()
                .map(|execution| &execution.spec_sha256),
            "execution_manifest" => task_spec
                .execution
                .as_ref()
                .map(|execution| &execution.manifest_sha256),
            "execution_events" => task_spec
                .execution
                .as_ref()
                .map(|execution| &execution.events_sha256),
            "observation_spec" => match &task_spec.purpose {
                Some(CandidatePurposeV1::Observe { target }) => Some(&target.spec_sha256),
                _ => None,
            },
            "observation_quotes" => match &task_spec.purpose {
                Some(CandidatePurposeV1::Observe { target }) => Some(&target.quotes_sha256),
                _ => None,
            },
            _ => None,
        };
        if expected.is_some_and(|expected| expected != &hash) {
            return Err(Error::Invalid);
        }
        if task_spec.execution.is_none()
            && matches!(
                kind,
                "execution_spec" | "execution_manifest" | "execution_events"
            )
        {
            return Err(Error::Invalid);
        }
        if task_spec.purpose.is_none() && matches!(kind, "observation_spec" | "observation_quotes")
        {
            return Err(Error::Invalid);
        }
    }
    let job = enqueue_tx(&tx, job_spec, now)?;
    tx.execute(
        "INSERT INTO candidate_tasks(task_id,revision,job_id,session,parent_turn_id,content_key,input_key,body,created_ms) VALUES(?,?,?,?,?,?,?,?,?)",
        params![
            task_spec.task_id,
            task_spec.revision,
            task_spec.job_id,
            task_spec.session,
            task_spec.parent_turn_id,
            task_spec.content_key,
            task_spec.input_key,
            body,
            now
        ],
    )
    .map_err(|_| Error::Collision)?;
    candidate_event(&tx, &task_spec.task_id, task_spec.revision, "enqueued", now)?;
    let record =
        candidate_tx(&tx, &task_spec.task_id, task_spec.revision)?.ok_or(Error::Missing)?;
    if record.job != job {
        return Err(Error::Storage);
    }
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(CandidateAdmission::Admitted(Box::new(record)))
}

pub fn enqueue_candidate(
    db: &mut Connection,
    job_spec: &Spec,
    task_spec: &CandidateTaskV1,
    now: i64,
) -> Result<CandidateTaskRecord> {
    match enqueue_candidate_bounded(db, job_spec, task_spec, &[], now, None)? {
        CandidateAdmission::Admitted(record) => Ok(*record),
        CandidateAdmission::Deferred { .. } => Err(Error::Capacity),
    }
}

pub fn load_candidate(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
) -> Result<CandidateTaskRecord> {
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Err(Error::Missing);
    }
    let result = candidate_tx(&tx, task_id, revision)?.ok_or(Error::Missing);
    tx.commit().map_err(|_| Error::Storage)?;
    result
}

pub fn latest_candidate(db: &mut Connection, task_id: &str) -> Result<Option<CandidateTaskRecord>> {
    if !valid_hash(task_id) {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let revision: Option<u32> = tx
        .query_row(
            "SELECT max(revision) FROM candidate_tasks WHERE task_id=?",
            [task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?
        .flatten();
    let result = revision
        .map(|revision| candidate_tx(&tx, task_id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

pub fn list_candidates(
    db: &mut Connection,
    session: &str,
    parent_turn_id: Option<&str>,
    limit: usize,
) -> Result<Vec<CandidateTaskRecord>> {
    list_candidates_page(db, session, parent_turn_id, None, limit)
}

pub fn list_candidates_page(
    db: &mut Connection,
    session: &str,
    parent_turn_id: Option<&str>,
    after: Option<&TaskCursor>,
    limit: usize,
) -> Result<Vec<CandidateTaskRecord>> {
    if chrono::NaiveDate::parse_from_str(session, "%Y-%m-%d").is_err()
        || !(1..=100).contains(&limit)
    {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(Vec::new());
    }
    let mut statement = tx.prepare(
        "SELECT task_id,revision FROM candidate_tasks WHERE session=? AND (? IS NULL OR parent_turn_id=?) AND (? IS NULL OR created_ms>? OR (created_ms=? AND (task_id>? OR (task_id=? AND revision>?)))) ORDER BY created_ms,task_id,revision LIMIT ?",
    ).map_err(|_| Error::Storage)?;
    let ids: Vec<(String, u32)> = statement
        .query_map(
            params![
                session,
                parent_turn_id,
                parent_turn_id,
                after.map(|c| c.created_at_ms),
                after.map_or(0, |c| c.created_at_ms),
                after.map(|c| c.created_at_ms),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.revision),
                limit as i64
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::Storage)?
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| Error::Storage)?;
    drop(statement);
    let records = ids
        .into_iter()
        .map(|(id, revision)| candidate_tx(&tx, &id, revision)?.ok_or(Error::Missing))
        .collect::<Result<Vec<_>>>()?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(records)
}

/// Page candidate rows newest-first. The controller uses this for the true
/// newest-N projection; paging oldest-first and reversing a bounded prefix is
/// not the same thing once the queue exceeds the page bound.
pub fn list_candidates_desc_page(
    db: &mut Connection,
    session: &str,
    after: Option<&TaskCursor>,
    limit: usize,
) -> Result<Vec<CandidateTaskRecord>> {
    if chrono::NaiveDate::parse_from_str(session, "%Y-%m-%d").is_err()
        || !(1..=100).contains(&limit)
    {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(Vec::new());
    }
    let mut statement = tx.prepare(
        "SELECT task_id,revision FROM candidate_tasks WHERE session=? AND (? IS NULL OR created_ms<? OR (created_ms=? AND (task_id<? OR (task_id=? AND revision<?)))) ORDER BY created_ms DESC,task_id DESC,revision DESC LIMIT ?",
    ).map_err(|_| Error::Storage)?;
    let ids: Vec<(String, u32)> = statement
        .query_map(
            params![
                session,
                after.map(|c| c.created_at_ms),
                after.map_or(0, |c| c.created_at_ms),
                after.map(|c| c.created_at_ms),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.revision),
                limit as i64
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::Storage)?
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| Error::Storage)?;
    drop(statement);
    let records = ids
        .into_iter()
        .map(|(id, revision)| candidate_tx(&tx, &id, revision)?.ok_or(Error::Missing))
        .collect::<Result<Vec<_>>>()?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(records)
}

pub fn active_candidate(db: &mut Connection) -> Result<Option<CandidateTaskRecord>> {
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    // A live attempt occupies the original queue slot and is never skipped or
    // replaced. Only when nothing is live may a Pending row be selected.
    let key: Option<(String, u32)> = tx.query_row(
        "SELECT t.task_id,t.revision FROM candidate_tasks t JOIN jobs j ON j.id=t.job_id WHERE t.refusal_receipt IS NULL AND j.state IN ('Running','Unknown','CancelRequested') ORDER BY CASE j.state WHEN 'Running' THEN 0 WHEN 'Unknown' THEN 1 ELSE 2 END,t.created_ms,t.task_id,t.revision LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional().map_err(|_| Error::Storage)?;
    let key = match key {
        Some(key) => Some(key),
        None => tx
            .query_row(
                "SELECT t.task_id,t.revision FROM candidate_tasks t JOIN jobs j ON j.id=t.job_id WHERE t.refusal_receipt IS NULL AND j.state='Pending' ORDER BY t.created_ms,t.task_id,t.revision LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| Error::Storage)?,
    };
    let record = key
        .map(|(id, revision)| candidate_tx(&tx, &id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(record)
}

/// Page Pending candidate rows oldest-first. The controller scans this page to
/// find the oldest launchable row without mutating an ineligible predecessor's
/// original book binding, lease or task row.
pub fn pending_candidates_page(
    db: &mut Connection,
    after: Option<&TaskCursor>,
    limit: usize,
) -> Result<Vec<CandidateTaskRecord>> {
    if !(1..=100).contains(&limit) {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(Vec::new());
    }
    let mut statement = tx
        .prepare(
            "SELECT t.task_id,t.revision FROM candidate_tasks t JOIN jobs j ON j.id=t.job_id WHERE t.refusal_receipt IS NULL AND j.state='Pending' AND (? IS NULL OR t.created_ms>? OR (t.created_ms=? AND (t.task_id>? OR (t.task_id=? AND t.revision>?)))) ORDER BY t.created_ms,t.task_id,t.revision LIMIT ?",
        )
        .map_err(|_| Error::Storage)?;
    let ids: Vec<(String, u32)> = statement
        .query_map(
            params![
                after.map(|c| c.created_at_ms),
                after.map_or(0, |c| c.created_at_ms),
                after.map(|c| c.created_at_ms),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.revision),
                limit as i64
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::Storage)?
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| Error::Storage)?;
    drop(statement);
    let records = ids
        .into_iter()
        .map(|(id, revision)| candidate_tx(&tx, &id, revision)?.ok_or(Error::Missing))
        .collect::<Result<Vec<_>>>()?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(records)
}

/// A candidate row written before the owned-unit policy existed is a legacy
/// experimental schema. Callers must fail closed and require a reviewed
/// migration; they must never tell an operator to recreate the database.
pub fn candidate_requires_migration(db: &Connection) -> Result<bool> {
    let exists: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='candidate_tasks'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if exists == 0 {
        return Ok(false);
    }
    let legacy: i64 = db
        .query_row(
            "SELECT count(*) FROM candidate_tasks WHERE json_extract(body,'$.unit_policy') IS NULL",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    Ok(legacy > 0)
}

pub fn candidate_for_job(db: &mut Connection, job_id: &str) -> Result<Option<CandidateTaskRecord>> {
    if !valid_hash(job_id) {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let key: Option<(String, u32)> = tx
        .query_row(
            "SELECT task_id,revision FROM candidate_tasks WHERE job_id=?",
            [job_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    let result = key
        .map(|(id, revision)| candidate_tx(&tx, &id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

pub fn cancel_candidate(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
    expected_revision: u32,
    now: i64,
) -> Result<CandidateTaskRecord> {
    if revision != expected_revision || now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_candidate_tables(&tx)?;
    let mut old = candidate_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    if let Some(receipt) = &old.cancel_receipt {
        if receipt.expected_revision == expected_revision {
            tx.commit().map_err(|_| Error::Storage)?;
            return Ok(old);
        }
        return Err(Error::Collision);
    }
    if old.refusal_receipt.is_some()
        || matches!(
            old.status,
            TaskState::Succeeded | TaskState::Terminated | TaskState::Cancelled
        )
    {
        return Err(Error::State);
    }
    let next = match old.job.state {
        State::Pending => "Cancelled",
        State::Running | State::Unknown => "CancelRequested",
        State::CancelRequested => "CancelRequested",
        _ => return Err(Error::State),
    };
    if old.job.state != State::CancelRequested {
        tx.execute(
            "UPDATE jobs SET state=? WHERE id=?",
            params![next, old.spec.job_id],
        )
        .map_err(|_| Error::Storage)?;
        event(
            &tx,
            &old.spec.job_id,
            "cancel_requested",
            now,
            old.job.attempts,
        )?;
    }
    let receipt = CancelReceipt {
        task_id: old.spec.task_id.clone(),
        revision,
        expected_revision,
        requested_ms: now,
        lease: old.job.lease.clone(),
        job_state: next.to_owned(),
    };
    tx.execute(
        "UPDATE candidate_tasks SET cancel_receipt=? WHERE task_id=? AND revision=?",
        params![
            serde_json::to_string(&receipt).map_err(|_| Error::Invalid)?,
            task_id,
            revision
        ],
    )
    .map_err(|_| Error::Storage)?;
    candidate_event(&tx, task_id, revision, "cancel_requested", now)?;
    if next == "Cancelled" {
        candidate_event(&tx, task_id, revision, "cancelled", now)?;
    }
    old = candidate_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(old)
}

pub fn refuse_unstarted_candidate(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
    reason: RefusalReason,
    now: i64,
) -> Result<CandidateTaskRecord> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_candidate_tables(&tx)?;
    let old = candidate_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    if let Some(receipt) = &old.refusal_receipt {
        return if receipt.reason == reason {
            Ok(old)
        } else {
            Err(Error::Collision)
        };
    }
    if old.job.state != State::Pending || old.job.attempts != 0 || old.cancel_receipt.is_some() {
        return Err(Error::State);
    }
    let receipt = RefusalReceipt {
        task_id: old.spec.task_id.clone(),
        revision,
        job_id: old.spec.job_id.clone(),
        reason,
        at_ms: now,
    };
    tx.execute(
        "UPDATE candidate_tasks SET refusal_receipt=? WHERE task_id=? AND revision=?",
        params![
            serde_json::to_string(&receipt).map_err(|_| Error::Invalid)?,
            task_id,
            revision
        ],
    )
    .map_err(|_| Error::Storage)?;
    tx.execute(
        "UPDATE jobs SET state='Refused' WHERE id=? AND state='Pending' AND attempts=0",
        [&old.spec.job_id],
    )
    .map_err(|_| Error::Storage)?;
    candidate_event(&tx, task_id, revision, "refused", now)?;
    let result = candidate_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

/// Terminalize a claimed candidate attempt with host sandbox evidence. The
/// receipt must describe the existing score_source kill-and-wait contract, or
/// explicitly record that no sandbox was started. `cause` is the authoritative
/// domain reason: an operator cancel receipt always wins over a later expiry or
/// failure classification, so a recorded human cancel is never mislabelled.
pub fn record_candidate_termination(
    db: &mut Connection,
    id: &str,
    lease: &str,
    receipt: &str,
    cause: CandidateStopCause,
    now: i64,
) -> Result<Job> {
    if !valid_hash(lease)
        || !valid_hash(receipt)
        || now < 0
        || cause == CandidateStopCause::Succeeded
    {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.spec.capability != SpecCapability::CandidateSource {
        return Err(Error::Invalid);
    }
    if old.lease.as_deref() != Some(lease) {
        return Err(Error::Lease);
    }
    let operator =
        cause == CandidateStopCause::OperatorCancel || operator_cancel_recorded(&tx, id)?;
    let label = format!("terminated:{}:{receipt}", cause.as_str());
    if matches!(old.state, State::Terminated | State::Cancelled) {
        let prior: Option<String> = tx
            .query_row(
                "SELECT event FROM job_events WHERE job=? AND attempt=? AND event LIKE 'terminated:%' ORDER BY seq DESC LIMIT 1",
                params![id, old.attempts],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| Error::Storage)?;
        return if prior.as_deref() == Some(&label) {
            Ok(old)
        } else {
            Err(Error::Collision)
        };
    }
    let next = match old.state {
        State::Running | State::Unknown | State::CancelRequested => {
            if operator {
                "Cancelled"
            } else {
                "Terminated"
            }
        }
        _ => return Err(Error::State),
    };
    tx.execute("UPDATE jobs SET state=? WHERE id=?", params![next, id])
        .map_err(|_| Error::Storage)?;
    event(&tx, id, &label, now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}

pub(super) fn operator_cancel_recorded(tx: &Transaction<'_>, job_id: &str) -> Result<bool> {
    let receipt: Option<Option<String>> = tx
        .query_row(
            "SELECT cancel_receipt FROM candidate_tasks WHERE job_id=?",
            [job_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    Ok(receipt.flatten().is_some())
}

pub(super) fn termination_cause(
    tx: &Transaction<'_>,
    job_id: &str,
    attempt: u32,
) -> Result<Option<CandidateStopCause>> {
    let event: Option<String> = tx
        .query_row(
            "SELECT event FROM job_events WHERE job=? AND attempt=? AND event LIKE 'terminated:%' ORDER BY seq DESC LIMIT 1",
            params![job_id, attempt],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    Ok(event
        .as_deref()
        .and_then(|value| value.strip_prefix("terminated:"))
        .and_then(|value| value.split_once(':'))
        .and_then(|(cause, _)| CandidateStopCause::parse(cause)))
}

fn candidate_artifact_limit(kind: &str) -> Result<usize> {
    match kind {
        "source" => Ok(262_144),
        "params" => Ok(65_536),
        "dataset" => Ok(4 * 1024 * 1024),
        "execution_spec" => Ok(65_536),
        "execution_manifest" => Ok(512 * 1024),
        "execution_events" => Ok(16 * 1024 * 1024),
        "observation_spec" => Ok(65_536),
        "observation_quotes" => Ok(2_000_000),
        "result" => Ok(1_048_576),
        "stopped" => Ok(65_536),
        "launch" => Ok(65_536),
        "quotes" => Ok(1_048_576),
        _ => Err(Error::Invalid),
    }
}

const CANDIDATE_ARTIFACT_TOTAL_ROWS: i64 = 4096;
const CANDIDATE_ARTIFACT_TOTAL_BYTES: i64 = 64 * 1024 * 1024;

fn put_candidate_artifact_tx(
    tx: &Transaction<'_>,
    kind: &str,
    body: &[u8],
    now: i64,
) -> Result<String> {
    let limit = candidate_artifact_limit(kind)?;
    if body.is_empty() || body.len() > limit || now < 0 {
        return Err(Error::Invalid);
    }
    let hash = sha256_hex(body);
    let existing: Option<(String, Vec<u8>)> = tx
        .query_row(
            "SELECT kind,body FROM candidate_artifacts WHERE hash=?",
            params![&hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    if let Some((stored_kind, stored)) = existing {
        return if stored_kind == kind && stored == body {
            Ok(hash)
        } else {
            Err(Error::Collision)
        };
    }
    let (rows, bytes): (i64, i64) = tx
        .query_row(
            "SELECT count(*),coalesce(sum(length(body)),0) FROM candidate_artifacts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::Storage)?;
    if rows >= CANDIDATE_ARTIFACT_TOTAL_ROWS
        || bytes.saturating_add(body.len() as i64) > CANDIDATE_ARTIFACT_TOTAL_BYTES
    {
        return Err(Error::Capacity);
    }
    tx.execute(
        "INSERT INTO candidate_artifacts(hash,kind,body,created_ms) VALUES(?,?,?,?)",
        params![hash, kind, body, now],
    )
    .map_err(|_| Error::Storage)?;
    Ok(hash)
}

pub fn put_candidate_artifact(
    db: &mut Connection,
    kind: &str,
    body: &[u8],
    now: i64,
) -> Result<String> {
    let tx = immediate(db)?;
    ensure_candidate_tables(&tx)?;
    let hash = put_candidate_artifact_tx(&tx, kind, body, now)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(hash)
}

/// Store one artifact under a caller-supplied recorded cap. The global artifact
/// limit remains the upper bound; the task's original cap is the lower bound so
/// a later config change cannot accept or reject an existing task differently.
pub fn put_candidate_artifact_bounded(
    db: &mut Connection,
    kind: &str,
    body: &[u8],
    max: usize,
    now: i64,
) -> Result<String> {
    let limit = candidate_artifact_limit(kind)?;
    if max == 0 || max > limit || body.is_empty() || body.len() > max {
        return Err(Error::Invalid);
    }
    put_candidate_artifact(db, kind, body, now)
}

pub fn read_candidate_artifact(
    db: &Connection,
    hash: &str,
    kind: &str,
    max: usize,
) -> Result<Vec<u8>> {
    if !valid_hash(hash) || max == 0 || max > 16 * 1024 * 1024 {
        return Err(Error::Invalid);
    }
    let limit = candidate_artifact_limit(kind)?;
    if max > limit {
        return Err(Error::Invalid);
    }
    let row: Option<(String, Vec<u8>)> = db
        .query_row(
            "SELECT kind,body FROM candidate_artifacts WHERE hash=? AND length(body)<=?",
            params![hash, limit as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    let Some((stored_kind, body)) = row else {
        return Err(Error::Missing);
    };
    if stored_kind != kind || body.len() > max {
        return Err(Error::Invalid);
    }
    Ok(body)
}
