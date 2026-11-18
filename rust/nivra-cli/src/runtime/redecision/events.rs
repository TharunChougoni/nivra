use super::*;
use crate::runtime::phases::PlannerLogPhase;

/// Make one durable owner recheck for a terminal registered-analysis task.
/// The event identity is bound to the book/task revision rather than a clock,
/// so a driver restart can retry insertion without manufacturing another wake.
pub(crate) fn enqueue_owner_research_event(
    db: &Connection,
    c: &Config,
    task_id: &str,
    revision: u32,
    result_sha256: Option<&str>,
    status: &str,
    observed_ms: i64,
) -> Result<()> {
    enqueue_owner_research_event_kind(
        db,
        c,
        task_id,
        revision,
        result_sha256,
        status,
        observed_ms,
        "registered",
    )
}

/// Terminal event with an explicit task capability. The owner must use the
/// candidate tools for a candidate receipt, never the registered alias.
#[allow(clippy::too_many_arguments)]
pub(crate) fn enqueue_owner_research_event_kind(
    db: &Connection,
    c: &Config,
    task_id: &str,
    revision: u32,
    result_sha256: Option<&str>,
    status: &str,
    observed_ms: i64,
    task_kind: &str,
) -> Result<()> {
    let binding = book_binding(c)?;
    let valid_id = task_id.len() == 64
        && task_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let valid_result_hash = result_sha256.is_none_or(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    let result_matches_status = (status == "succeeded") == result_sha256.is_some();
    if revision == 0
        || !valid_id
        || !valid_result_hash
        || !result_matches_status
        || observed_ms < 0
        || !matches!(status, "succeeded" | "terminated" | "cancelled" | "refused")
        || !matches!(task_kind, "registered" | "candidate")
    {
        return Err("planner_owner_research_event_invalid".into());
    }
    if !exists(db)? {
        return Err("planner_owner_research_authority_missing".into());
    }
    let id = digest(format!("owner-research-terminal:{binding}:{task_id}:{revision}").as_bytes());
    let body = json!({
        "id":id.clone(),
        "kind":"recheck",
        "reason":"research_task_terminal",
        "observed_ms":observed_ms,
        "session_binding":binding,
        "research_task":{
            "task_id":task_id,
            "revision":revision,
            "kind":task_kind,
            "status":status,
            "result_sha256":result_sha256,
        }
    })
    .to_string();
    db.execute(
        "INSERT OR IGNORE INTO planner_event_log VALUES(?,?,?,?)",
        rusqlite::params![id, observed_ms, body, PlannerLogPhase::Pending.as_str()],
    )?;
    let existing: String = db.query_row(
        "SELECT body FROM planner_event_log WHERE id=?",
        [&id],
        |row| row.get(0),
    )?;
    if existing != body {
        return Err("planner_event_identity_changed".into());
    }
    Ok(())
}

/// A host exit review becomes a proximity event. It does not trade.
pub(crate) fn enqueue_exit_review(
    db: &Connection,
    binding: &str,
    key: &str,
    reason: &str,
    at: i64,
) -> Result<()> {
    if !exists(db)? {
        return Ok(());
    }
    let id = digest(format!("exit-review:{binding}:{key}:{reason}:{at}").as_bytes());
    let present: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM planner_event_log WHERE id=?)",
        [&id],
        |row| row.get(0),
    )?;
    if present {
        return Ok(());
    }
    let body = json!({"id":id,"kind":"proximity","reason":reason,"instrument_key":key,"observed_ms":at,"session_binding":binding});
    db.execute(
        "INSERT INTO planner_event_log VALUES(?,?,?,?)",
        rusqlite::params![id, at, body.to_string(), PlannerLogPhase::Pending.as_str()],
    )?;
    Ok(())
}
