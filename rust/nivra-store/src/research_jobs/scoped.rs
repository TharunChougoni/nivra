//! Scoped research tasks (registered analyses and archive bar jobs).
use super::queue::{enqueue_tx, event, immediate, read};
use super::*;

pub(super) fn ensure_scoped_tables(tx: &Transaction<'_>) -> Result<()> {
    let core: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('scripts','jobs','leases','job_events')",
        [],
        |row| row.get(0),
    ).map_err(|_| Error::Storage)?;
    if core != 4 {
        return Err(Error::Storage);
    }
    let version: i64 = tx
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| Error::Storage)?;
    let tables: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('scoped_tasks','scoped_task_events')",
        [],
        |r| r.get(0),
    ).map_err(|_| Error::Storage)?;
    match (version, tables) {
        (0, 0) => {}
        (1, 2) => return Ok(()),
        _ => return Err(Error::Storage),
    }
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS scoped_tasks(task_id TEXT NOT NULL,revision INTEGER NOT NULL,job_id TEXT NOT NULL UNIQUE,session TEXT NOT NULL,parent_turn_id TEXT NOT NULL,body TEXT NOT NULL,created_ms INTEGER NOT NULL,cancel_receipt TEXT,refusal_receipt TEXT,PRIMARY KEY(task_id,revision));\
         CREATE TABLE IF NOT EXISTS scoped_task_events(seq INTEGER PRIMARY KEY,task_id TEXT NOT NULL,revision INTEGER NOT NULL,event TEXT NOT NULL,at_ms INTEGER NOT NULL);",
    ).map_err(|_| Error::Storage)?;
    tx.pragma_update(None, "user_version", 1_i64)
        .map_err(|_| Error::Storage)?;
    Ok(())
}
pub(super) fn scoped_tables_exist(tx: &Transaction<'_>) -> Result<bool> {
    let version: i64 = tx
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| Error::Storage)?;
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('scoped_tasks','scoped_task_events')",
        [],
        |r| r.get(0),
    ).map_err(|_| Error::Storage)?;
    match (version, count) {
        (0, 0) => Ok(false),
        (1, 2) => Ok(true),
        _ => Err(Error::Storage),
    }
}

pub(super) fn task_state(state: State) -> TaskState {
    match state {
        State::Pending => TaskState::Pending,
        State::Running => TaskState::Running,
        State::Unknown => TaskState::Unknown,
        State::CancelRequested => TaskState::CancelRequested,
        State::Terminated => TaskState::Terminated,
        State::Succeeded => TaskState::Succeeded,
        State::Cancelled => TaskState::Cancelled,
        State::Refused => TaskState::Refused,
    }
}

fn scoped_tx(tx: &Transaction<'_>, task_id: &str, revision: u32) -> Result<Option<TaskRecord>> {
    if !valid_hash(task_id) || revision == 0 {
        return Err(Error::Invalid);
    }
    type ScopedRow = (String, Option<String>, Option<String>, String, String, i64);
    let row: Option<ScopedRow> = tx.query_row(
        "SELECT body,cancel_receipt,refusal_receipt,session,parent_turn_id,created_ms FROM scoped_tasks WHERE task_id=? AND revision=? AND length(body)<=16384",
        params![task_id, revision],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ).optional().map_err(|_| Error::Storage)?;
    let Some((body, cancel, refusal, session, parent_turn_id, created_ms)) = row else {
        return Ok(None);
    };
    let spec: ScopedTaskSpec = serde_json::from_str(&body).map_err(|_| Error::Invalid)?;
    if spec.task_id != task_id || spec.revision != revision {
        return Err(Error::Invalid);
    }
    let job = read(tx, &spec.job_id)?.ok_or(Error::Missing)?;
    spec.validate(&job.spec, created_ms)
        .map_err(|_| Error::Invalid)?;
    if spec.session != session || spec.parent_turn_id != parent_turn_id {
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
            "SELECT max(at_ms) FROM (SELECT at_ms FROM job_events WHERE job=? AND (event='completed' OR event LIKE 'terminated:%') UNION ALL SELECT at_ms FROM scoped_task_events WHERE task_id=? AND revision=? AND event IN ('cancelled','refused'))",
            params![spec.job_id, task_id, revision],
            |r| r.get(0),
        ).optional().map_err(|_| Error::Storage)?.flatten()
    } else {
        None
    };
    Ok(Some(TaskRecord {
        spec,
        status,
        job,
        cancel_receipt,
        refusal_receipt,
        terminal_at_ms,
    }))
}
fn scoped_event(
    tx: &Transaction<'_>,
    task_id: &str,
    revision: u32,
    name: &str,
    now: i64,
) -> Result<()> {
    let last: Option<i64> = tx
        .query_row(
            "SELECT max(at_ms) FROM scoped_task_events WHERE task_id=? AND revision=?",
            params![task_id, revision],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if last.is_some_and(|last| now < last) {
        return Err(Error::Invalid);
    }
    tx.execute(
        "INSERT INTO scoped_task_events(task_id,revision,event,at_ms) VALUES(?,?,?,?)",
        params![task_id, revision, name, now],
    )
    .map_err(|_| Error::Storage)?;
    Ok(())
}

/// Atomically create a scoped task and its existing compute job in the
/// caller-validated, already initialized queue. This performs only additive
/// schema creation; it never initializes, replaces, or resets the queue.
pub fn enqueue_scoped(
    db: &mut Connection,
    job_spec: &Spec,
    task_spec: &ScopedTaskSpec,
    now: i64,
) -> Result<TaskRecord> {
    match enqueue_scoped_bounded(db, job_spec, task_spec, now, None)? {
        Admission::Admitted(record) => Ok(*record),
        Admission::Deferred { .. } => Err(Error::Capacity),
    }
}
/// Check compute limits and enqueue in the same immediate transaction. Aliases,
/// task keys, goals and owner turns cannot evade a content-bound input allowance.
pub fn enqueue_scoped_bounded(
    db: &mut Connection,
    job_spec: &Spec,
    task_spec: &ScopedTaskSpec,
    now: i64,
    limits: Option<&AdmissionLimits>,
) -> Result<Admission> {
    if let Some(limits) = limits {
        limits.validate()?;
    }
    task_spec.validate(job_spec, task_spec.created_at_ms)?;
    if now < task_spec.created_at_ms {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_scoped_tables(&tx)?;
    let body = serde_json::to_string(task_spec).map_err(|_| Error::Invalid)?;
    if let Some(old) = scoped_tx(&tx, &task_spec.task_id, task_spec.revision)? {
        if old.spec == *task_spec && old.job.spec == *job_spec {
            return Ok(Admission::Admitted(Box::new(old)));
        }
        return Err(Error::Collision);
    }
    if task_spec.created_at_ms != now {
        return Err(Error::Invalid);
    }
    let latest: Option<u32> = tx
        .query_row(
            "SELECT max(revision) FROM scoped_tasks WHERE task_id=?",
            [&task_spec.task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?
        .flatten();
    match latest {
        None if task_spec.revision != 1 => return Err(Error::State),
        Some(revision) => {
            let previous = scoped_tx(&tx, &task_spec.task_id, revision)?.ok_or(Error::Missing)?;
            if previous.spec.task_key != task_spec.task_key
                || previous.spec.session != task_spec.session
                || previous.spec.book_binding != task_spec.book_binding
            {
                return Err(Error::Collision);
            }
            if !matches!(
                previous.status,
                TaskState::Succeeded
                    | TaskState::Terminated
                    | TaskState::Cancelled
                    | TaskState::Refused
            ) || task_spec.revision != revision.saturating_add(1)
            {
                return Err(Error::State);
            }
        }
        None => {}
    }
    if let Some(limits) = limits {
        tx.execute_batch("CREATE INDEX IF NOT EXISTS scoped_tasks_session ON scoped_tasks(session); CREATE TABLE IF NOT EXISTS scoped_admission_deferrals(task_id TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(task_id,revision));").map_err(|_| Error::Storage)?;
        let (registered_total, registered_same): (u32, u32) = tx.query_row(
            "SELECT count(*),coalesce(sum(json_extract(body,'$.registered_analysis.code_sha256')=?3 AND json_extract(body,'$.registered_analysis.runtime_sha256')=?4 AND json_extract(body,'$.registered_analysis.input_sha256')=?5),0) FROM scoped_tasks WHERE session=?1 AND json_extract(body,'$.book_binding')=?2",
            params![task_spec.session, task_spec.book_binding, task_spec.registered_analysis.code_sha256, task_spec.registered_analysis.runtime_sha256, task_spec.registered_analysis.input_sha256],
            |r| Ok((r.get(0)?,r.get(1)?))).map_err(|_| Error::Storage)?;
        let (total, same) = (registered_total, registered_same);
        let deferred = if total >= limits.max_session_trials {
            Some(("research_session_compute_budget_exhausted", total))
        } else if same >= limits.max_trials_per_input {
            Some(("research_unchanged_input_budget_exhausted", same))
        } else {
            None
        };
        if let Some((reason, trials)) = deferred {
            let body = serde_json::to_string(&serde_json::json!({"reason":reason,"trials":trials,"requested":task_spec,"observed_ms":now,"next_condition":if total >= limits.max_session_trials {"next_authorized_session"}else{"new_registered_content_or_reviewed_compute_policy"}})).map_err(|_| Error::Invalid)?;
            tx.execute("INSERT OR IGNORE INTO scoped_admission_deferrals(task_id,revision,body) VALUES(?,?,?)",params![task_spec.task_id,task_spec.revision,body]).map_err(|_| Error::Storage)?;
            tx.commit().map_err(|_| Error::Storage)?;
            return Ok(Admission::Deferred { reason, trials });
        }
    }
    if read(&tx, &job_spec.id)?.is_some() {
        return Err(Error::Collision);
    }
    let job = enqueue_tx(&tx, job_spec, now)?;
    tx.execute(
        "INSERT INTO scoped_tasks(task_id,revision,job_id,session,parent_turn_id,body,created_ms) VALUES(?,?,?,?,?,?,?)",
        params![task_spec.task_id, task_spec.revision, task_spec.job_id, task_spec.session, task_spec.parent_turn_id, body, now],
    ).map_err(|_| Error::Collision)?;
    scoped_event(&tx, &task_spec.task_id, task_spec.revision, "enqueued", now)?;
    let record = scoped_tx(&tx, &task_spec.task_id, task_spec.revision)?.ok_or(Error::Missing)?;
    if record.job != job {
        return Err(Error::Storage);
    }
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(Admission::Admitted(Box::new(record)))
}

/// Load one task revision. Missing or malformed state is never treated as a
/// new request; callers must surface the refusal.
pub fn load_scoped(db: &mut Connection, task_id: &str, revision: u32) -> Result<TaskRecord> {
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !scoped_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Err(Error::Missing);
    }
    let result = scoped_tx(&tx, task_id, revision)?.ok_or(Error::Missing);
    tx.commit().map_err(|_| Error::Storage)?;
    result
}

pub fn latest_scoped(db: &mut Connection, task_id: &str) -> Result<Option<TaskRecord>> {
    if !valid_hash(task_id) {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !scoped_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let revision: Option<u32> = tx
        .query_row(
            "SELECT max(revision) FROM scoped_tasks WHERE task_id=?",
            [task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?
        .flatten();
    let result = revision
        .map(|revision| scoped_tx(&tx, task_id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

pub fn scoped_for_job(db: &mut Connection, job_id: &str) -> Result<Option<TaskRecord>> {
    if !valid_hash(job_id) {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !scoped_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let key: Option<(String, u32)> = tx
        .query_row(
            "SELECT task_id,revision FROM scoped_tasks WHERE job_id=?",
            [job_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    let result = key
        .map(|(id, revision)| scoped_tx(&tx, &id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

pub fn list_scoped(
    db: &mut Connection,
    session: &str,
    parent_turn_id: Option<&str>,
    limit: usize,
) -> Result<Vec<TaskRecord>> {
    list_scoped_page(db, session, parent_turn_id, None, limit)
}

pub fn list_scoped_page(
    db: &mut Connection,
    session: &str,
    parent_turn_id: Option<&str>,
    after: Option<&TaskCursor>,
    limit: usize,
) -> Result<Vec<TaskRecord>> {
    if chrono::NaiveDate::parse_from_str(session, "%Y-%m-%d").is_err()
        || !(1..=100).contains(&limit)
    {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !scoped_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(Vec::new());
    }
    let mut statement = tx.prepare(
        "SELECT task_id,revision FROM scoped_tasks WHERE session=? AND (? IS NULL OR parent_turn_id=?) AND (? IS NULL OR created_ms>? OR (created_ms=? AND (task_id>? OR (task_id=? AND revision>?)))) ORDER BY created_ms,task_id,revision LIMIT ?",
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
        .map(|(id, revision)| scoped_tx(&tx, &id, revision)?.ok_or(Error::Missing))
        .collect::<Result<Vec<_>>>()?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(records)
}

pub fn active_scoped(db: &mut Connection) -> Result<Option<TaskRecord>> {
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !scoped_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let key: Option<(String, u32)> = tx.query_row(
        "SELECT t.task_id,t.revision FROM scoped_tasks t JOIN jobs j ON j.id=t.job_id WHERE t.refusal_receipt IS NULL AND j.state IN ('Pending','Running','Unknown','CancelRequested') ORDER BY CASE j.state WHEN 'Running' THEN 0 WHEN 'Unknown' THEN 1 WHEN 'CancelRequested' THEN 2 ELSE 3 END,t.created_ms,t.task_id,t.revision LIMIT 1",
        [],
        |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional().map_err(|_| Error::Storage)?;
    let record = key
        .map(|(id, revision)| scoped_tx(&tx, &id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(record)
}

pub fn cancel_scoped(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
    expected_revision: u32,
    now: i64,
) -> Result<TaskRecord> {
    if revision != expected_revision || now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_scoped_tables(&tx)?;
    let mut old = scoped_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
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
        "UPDATE scoped_tasks SET cancel_receipt=? WHERE task_id=? AND revision=?",
        params![
            serde_json::to_string(&receipt).map_err(|_| Error::Invalid)?,
            task_id,
            revision
        ],
    )
    .map_err(|_| Error::Storage)?;
    scoped_event(&tx, task_id, revision, "cancel_requested", now)?;
    if next == "Cancelled" {
        scoped_event(&tx, task_id, revision, "cancelled", now)?;
    }
    old = scoped_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(old)
}

pub fn refuse_unstarted(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
    reason: RefusalReason,
    now: i64,
) -> Result<TaskRecord> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_scoped_tables(&tx)?;
    let old = scoped_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
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
        "UPDATE scoped_tasks SET refusal_receipt=? WHERE task_id=? AND revision=?",
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
    scoped_event(&tx, task_id, revision, "refused", now)?;
    let result = scoped_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}
