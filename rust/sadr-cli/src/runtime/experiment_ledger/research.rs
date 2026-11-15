//! Immutable registered-analysis versions and their actual terminal receipts.
//! These are research candidates, not executable strategies or accepted edges.
use super::*;
use sadr_store::research_jobs as jobs;
use serde_json::json;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS research_candidates(
 candidate_id TEXT PRIMARY KEY, body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS research_candidate_outcomes(
 candidate_id TEXT PRIMARY KEY, body TEXT NOT NULL);";

pub(crate) fn candidate_id(spec: &jobs::ScopedTaskSpec) -> String {
    trial_id(&[
        "registered-analysis-v1",
        &spec.book_binding,
        &spec.session,
        &spec.task_id,
        &spec.revision.to_string(),
    ])
}

fn immutable(db: &Connection, table: &str, id: &str, body: &Value) -> Result<()> {
    // Table names are compile-time constants supplied only by this module.
    let encoded = serde_json::to_string(body)?;
    let previous: Option<String> = db
        .query_row(
            &format!("SELECT body FROM {table} WHERE candidate_id=?"),
            [id],
            |row| row.get(0),
        )
        .optional()?;
    match previous {
        Some(previous) if previous != encoded => Err("research_candidate_identity_changed".into()),
        Some(_) => Ok(()),
        None => {
            db.execute(
                &format!("INSERT INTO {table}(candidate_id,body) VALUES(?,?)"),
                params![id, encoded],
            )?;
            Ok(())
        }
    }
}

/// Called after queue admission and during reconciliation. The original queue
/// remains authoritative; a crash between the two writes is repaired on scan.
pub(crate) fn reconcile(c: &Config, queue: &mut Connection, task: &jobs::TaskRecord) -> Result<()> {
    reconcile_page(c, queue, std::iter::once(task))
}

pub(crate) fn reconcile_page<'a>(
    c: &Config,
    queue: &mut Connection,
    tasks: impl Iterator<Item = &'a jobs::TaskRecord>,
) -> Result<()> {
    let mut tasks = tasks.peekable();
    if tasks.peek().is_none() {
        return Ok(());
    }
    let mut db = open(c)?;
    db.execute_batch(SCHEMA)?;
    let tx = db.transaction()?;
    for task in tasks {
        reconcile_row(c, &tx, queue, task)?;
    }
    tx.commit()?;
    Ok(())
}

fn reconcile_row(
    c: &Config,
    tx: &Connection,
    queue: &mut Connection,
    task: &jobs::TaskRecord,
) -> Result<()> {
    if task.spec.session != c.session.to_string() {
        return Err("research_candidate_session_mismatch".into());
    }
    let id = candidate_id(&task.spec);
    let parent = (task.spec.revision > 1).then(|| {
        let mut parent = task.spec.clone();
        parent.revision -= 1;
        candidate_id(&parent)
    });
    let candidate = json!({
        "version":1,"candidate_id":id,"parent_candidate_id":parent,
        "kind":"registered_analysis","admission":"proposed",
        "task":task.spec,
        // Registration binds executable/input/runtime bytes. It does not
        // declare a strategy ABI, parameters, observation window or horizon.
        "strategy_abi":null,"strategy_parameters":null,
        "training_window":null,"holding_horizon":null,
        "hypothesis":task.spec.goal,
        "scientific_state":"insufficient_evidence",
        "next_condition":"strategy_specification_replay_and_forward_evidence_required"
    });
    immutable(tx, "research_candidates", &id, &candidate)?;
    let terminal = match task.status {
        jobs::TaskState::Succeeded => "analysis_succeeded",
        jobs::TaskState::Terminated => "analysis_terminated",
        jobs::TaskState::Cancelled => "analysis_cancelled",
        jobs::TaskState::Refused => "analysis_refused",
        _ => return Ok(()),
    };
    let at = task
        .terminal_at_ms
        .ok_or("research_terminal_time_missing")?;
    let outcome = json!({
        "candidate_id":id,"event":terminal,"at_ms":at,
        "result_sha256":task.job.result_hash,
        "scientific_state":"insufficient_evidence","edge_qualified":false,
        "next_condition":"strategy_specification_replay_and_forward_evidence_required"
    });
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM research_candidate_outcomes WHERE candidate_id=?)",
        [&id],
        |row| row.get(0),
    )?;
    if !exists && task.status == jobs::TaskState::Succeeded {
        // Process success is recorded only after the existing worker validates
        // launch/completion receipts and the original result bytes/hash.
        sadr_research::worker::read_verified_result(queue, &task.spec.task_id, task.spec.revision)
            .map_err(|_| "research_candidate_result_unverified")?
            .ok_or("research_candidate_result_unverified")?;
    }
    immutable(tx, "research_candidate_outcomes", &id, &outcome)?;
    Ok(())
}

/// Read the original ingest ledger without creating, migrating or repairing it.
/// The caller may report unavailable metadata; it must never halt the paper book.
pub(crate) fn library(c: &Config, cfg: &super::super::planner::PlannerConfig) -> Result<Value> {
    let Some(settings) = cfg
        .research_tools
        .as_ref()
        .filter(|_| cfg.orchestrator_owns_decisions())
    else {
        return Ok(json!({"status":"disabled","items":[],"activation_authorized":false}));
    };
    settings.validate()?;
    let file = path(c);
    if !file.exists() {
        return Ok(json!({"status":"not_recorded","items":[],"activation_authorized":false}));
    }
    let db = Connection::open_with_flags(
        &file,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(std::time::Duration::ZERO)?;
    let max = settings.library_max_candidates.unwrap_or(32);
    let byte_limit = settings.library_max_bytes.unwrap_or(262_144);
    // Primary-key order + LIMIT bounds work without scanning/filtering JSON.
    // SQL checks length before materializing any unbounded stored JSON string.
    let mut query = db.prepare("SELECT CASE WHEN length(CAST(c.body AS BLOB))<=?1 THEN c.body END,CASE WHEN length(CAST(o.body AS BLOB))<=?1 THEN o.body END,o.body IS NOT NULL FROM research_candidates c LEFT JOIN research_candidate_outcomes o USING(candidate_id) ORDER BY c.candidate_id LIMIT ?2")?;
    let rows = query.query_map(params![byte_limit as i64, (max + 1) as i64], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, bool>(2)?,
        ))
    })?;
    let binding = super::super::book_binding(c)?;
    let mut items = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    for row in rows {
        if items.len() == max {
            truncated = true;
            break;
        }
        let (body, outcome, has_outcome) = row?;
        if has_outcome && outcome.is_none() {
            return Err("research_library_record_limit".into());
        }
        let body = body.ok_or("research_library_record_limit")?;
        bytes = bytes
            .checked_add(body.len())
            .and_then(|n| n.checked_add(outcome.as_ref().map_or(0, String::len)))
            .ok_or("research_library_record_limit")?;
        if bytes > byte_limit {
            truncated = true;
            break;
        }
        let candidate: Value = serde_json::from_str(&body)?;
        if candidate["task"]["book_binding"] != binding
            || candidate["task"]["session"] != c.session.to_string()
        {
            return Err("research_library_binding_invalid".into());
        }
        let outcome: Value = outcome
            .map(|v| serde_json::from_str(&v))
            .transpose()?
            .unwrap_or(Value::Null);
        let task = &candidate["task"];
        if !candidate["candidate_id"].as_str().is_some_and(valid_hash)
            || (!outcome.is_null() && outcome["candidate_id"] != candidate["candidate_id"])
            || !task["registered_analysis"]["code_sha256"]
                .as_str()
                .is_some_and(valid_hash)
            || !task["registered_analysis"]["input_sha256"]
                .as_str()
                .is_some_and(valid_hash)
        {
            return Err("research_library_record_invalid".into());
        }

        let value = json!({"candidate_id":candidate["candidate_id"],"parent_candidate_id":candidate["parent_candidate_id"],
            "session":task["session"],"owner_turn_id":task["parent_turn_id"],"policy_sha256":task["policy_sha256"],
            "source_sha256":task["registered_analysis"]["code_sha256"],"input_sha256":task["registered_analysis"]["input_sha256"],
            "runtime_sha256":task["registered_analysis"]["runtime_sha256"],"hypothesis":candidate["hypothesis"],
            "strategy_abi":candidate["strategy_abi"],"holding_horizon":candidate["holding_horizon"],
            "state":outcome["event"].as_str().unwrap_or("proposed"),"terminal_at_ms":outcome["at_ms"],
            "result_sha256":outcome["result_sha256"],"scientific_state":"insufficient_evidence"});
        items.push(super::super::transcript::redact(&value));
    }
    Ok(json!({"status":"ready","items":items,"truncated":truncated,
        "source":"original_research_candidate_ledger","activation_authorized":false}))
}

#[cfg(test)]
pub(crate) fn snapshot(c: &Config) -> Result<(Vec<Value>, Vec<Value>)> {
    let db = open(c)?;
    let read = |table: &str| -> Result<Vec<Value>> {
        let mut query = db.prepare(&format!("SELECT body FROM {table} ORDER BY candidate_id"))?;
        let rows = query.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    };
    Ok((
        read("research_candidates")?,
        read("research_candidate_outcomes")?,
    ))
}
