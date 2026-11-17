use super::*;

/// Canonical content hash for one cross-session continuation body. Read-time
/// and processing clocks are outside the immutable evidence, so a repeat tick
/// re-reads the same identity instead of minting a new receipt.
pub(crate) fn continuation_content_sha256(value: &Value) -> Result<String> {
    let mut stable = value.clone();
    let object = stable
        .as_object_mut()
        .ok_or("candidate_continuation_invalid")?;
    object.insert("processed_at_ms".into(), json!(0));
    object.insert("recorded_at_ms".into(), json!(0));
    object.insert("measurement_sha256".into(), json!(""));
    Ok(sadr_core::replay::digest(
        serde_json::to_string(&stable)?.as_bytes(),
    ))
}

pub(crate) fn validate_continuation_body(value: &Value) -> Result<()> {
    // Closed top-level schema. An extra key is a different body, never a
    // forward-compatible extension of this version.
    if !value.as_object().is_some_and(|map| {
        map.keys()
            .all(|key| CONTINUATION_KEYS.contains(&key.as_str()))
    }) {
        return Err("candidate_continuation_invalid".into());
    }
    // The nested source runtime is a closed five-hash projection.
    if !value["source_runtime"].as_object().is_some_and(|map| {
        map.len() == SOURCE_RUNTIME_KEYS.len()
            && map
                .keys()
                .all(|key| SOURCE_RUNTIME_KEYS.contains(&key.as_str()))
    }) {
        return Err("candidate_continuation_invalid".into());
    }
    let id = value["continuation_id"]
        .as_str()
        .ok_or("candidate_continuation_invalid")?;
    let measurement = value["measurement_sha256"]
        .as_str()
        .ok_or("candidate_continuation_invalid")?;
    let scope = value["current_scope"]
        .as_object()
        .ok_or("candidate_continuation_invalid")?;
    let scope_hash_ok = |key: &str| value["current_scope"][key].as_str().is_some_and(valid_hash);
    // The source-result status is an EXPLICIT closed enum. A verified status
    // requires the exact hash; an unavailable status requires the KEY to be
    // PRESENT with an explicit NULL value. A missing key is a different schema,
    // never an implicit explicit-null, and a missing/unknown status is invalid.
    let source_result_key_present = value
        .as_object()
        .is_some_and(|map| map.contains_key("source_result_sha256"));
    let source_result_ok = match (
        value["source_result_status"].as_str(),
        value["source_result_sha256"].as_str(),
    ) {
        (Some("verified"), Some(hash)) => valid_hash(hash),
        (Some("unavailable"), None) => {
            source_result_key_present && value["source_result_sha256"].is_null()
        }
        _ => false,
    };
    if !valid_hash(id)
        || !valid_hash(measurement)
        || !matches!(value["state"].as_str(), Some("evaluated" | "unknown"))
        || value["source_scope"] != "continuation_original_episode"
        || value["capital_assumption"] != true
        || value["edge_qualified"] == true
        || value["activation_authorized"] == true
        || value["source_session"]
            .as_str()
            .is_none_or(|session| session.is_empty() || session.len() > 32)
        || value["source_lifecycle_candidate_id"]
            .as_str()
            .is_none_or(|id| !valid_hash(id))
        || value["source_task_id"]
            .as_str()
            .is_none_or(|task| task.is_empty() || task.len() > 256)
        || value["source_revision"]
            .as_u64()
            .is_none_or(|revision| revision == 0)
        || value["exit_due_ms"].as_i64().is_none()
        || value["exit_deadline_ms"].as_i64().is_none()
        || value["current_scope"]["current_session"]
            .as_str()
            .is_none_or(|session| session.is_empty() || session.len() > 32)
        || value["current_scope"]["version"].as_u64() != Some(1)
        || value["current_scope"]["account"]
            .as_str()
            .is_none_or(|account| account.is_empty() || account.len() > 128)
        || !scope_hash_ok("cohort_key")
        || !scope_hash_ok("runtime_hash")
        || !scope_hash_ok("source_ledger_spec_sha256")
        || !source_result_ok
        || scope.len() != 6
    {
        return Err("candidate_continuation_invalid".into());
    }
    if continuation_content_sha256(value)? != measurement {
        return Err("candidate_continuation_hash_mismatch".into());
    }
    // The self-hash is not identity: recompute the canonical identity digest
    // from the original source facts and the CURRENT recording scope.
    if crate::runtime::harness::research::continuation_identity_sha256(value)? != id {
        return Err("candidate_continuation_identity_mismatch".into());
    }
    Ok(())
}

fn ledger_session(directory: &Path) -> Option<&str> {
    directory
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.parse::<chrono::NaiveDate>().is_ok())
}

/// A continuation body that the canonical source validator has already checked.
/// The production writer accepts only this sealed token, never raw JSON.
pub(crate) trait ContinuationBody: sealed::Sealed {
    fn body(&self) -> &Value;
}

impl sealed::Sealed for crate::runtime::harness::research::ValidatedContinuation {}

/// Append one CURRENT continuation receipt to the existing per-session
/// lifecycle ledger. The original predecessor ledger is never written. The same
/// identity is idempotent; a different body for that identity is refused.
pub(crate) fn record_continuation<T: ContinuationBody>(
    directory: &Path,
    token: &T,
    now: i64,
) -> Result<Value> {
    let continuation = token.body();
    if now < 0 {
        return Err("lifecycle_clock_invalid".into());
    }
    validate_continuation_body(continuation)?;
    // The recording scope must name the ledger's OWN session, not a later
    // successor's session. A body copied into the wrong ledger is refused.
    if let Some(session) = ledger_session(directory)
        && continuation["current_scope"]["current_session"].as_str() != Some(session)
    {
        return Err("candidate_continuation_identity_mismatch".into());
    }
    let id = continuation["continuation_id"]
        .as_str()
        .ok_or("candidate_continuation_invalid")?;
    let measurement = continuation["measurement_sha256"]
        .as_str()
        .ok_or("candidate_continuation_invalid")?;
    // The ledger row is the authoritative recording clock. Normalize the
    // body's read-time fields to that clock so a later reader can bind body
    // metadata to the actual row timestamp; the content hash deliberately
    // excludes both fields, so identity is unchanged.
    let mut recorded = continuation.clone();
    recorded["recorded_at_ms"] = json!(now);
    recorded["processed_at_ms"] = json!(now);
    let body = serde_json::to_string(&recorded)?;
    let mut db = super::super::open_directory(directory)?;
    // Serialize the read/insert decision in one IMMEDIATE transaction. A
    // concurrent stale inserter either sees the first committed row or gets an
    // explicit busy/deferred result; it never overwrites a different receipt.
    let tx = match db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) {
        Ok(tx) => tx,
        Err(error) if is_busy(&error) => return Err("candidate_continuation_busy".into()),
        Err(_) => return Err("candidate_continuation_unavailable".into()),
    };
    let inserted = match tx.execute(
        "INSERT OR IGNORE INTO experiment_continuations(continuation_id,body,recorded_at_ms) VALUES(?,?,?)",
        params![id, body, now],
    ) {
        Ok(inserted) => inserted,
        Err(error) if is_busy(&error) => return Err("candidate_continuation_busy".into()),
        Err(_) => return Err("candidate_continuation_unavailable".into()),
    };
    if inserted == 0 {
        let existing: String = tx
            .query_row(
                "SELECT body FROM experiment_continuations WHERE continuation_id=?",
                [id],
                |row| row.get(0),
            )
            .map_err(|_| "candidate_continuation_unavailable")?;
        let stored: Value = serde_json::from_str(&existing)?;
        if continuation_content_sha256(&stored)? != continuation_content_sha256(continuation)? {
            return Err("continuation_identity_changed".into());
        }
        tx.commit()?;
        return Ok(stored);
    }
    tx.execute(
        "INSERT INTO experiment_lifecycle_events(candidate_id,from_state,to_state,event,evidence_sha256,at_ms) VALUES(?,?,?,?,?,?)",
        params![id, Option::<String>::None, "continued", "observation_continuation", measurement, now],
    )?;
    tx.commit()?;
    Ok(recorded)
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

/// Read every CURRENT continuation receipt in this session's existing lifecycle
/// ledger. A missing ledger or an older ledger without the continuation table is
/// empty; a corrupt ledger or an invalid body is unavailable. A sqlite_master
/// read failure is never silently treated as "no table".
pub(crate) fn continuations(directory: &Path) -> Result<Vec<Value>> {
    Ok(continuations_with_recorded(directory, false)?
        .into_iter()
        .map(|(_, value)| value)
        .collect())
}

/// Immutable read of a CLOSED predecessor continuation table. A non-empty WAL
/// is an explicit refusal and no sidecar is created.
pub(crate) fn continuations_immutable(directory: &Path) -> Result<Vec<Value>> {
    Ok(continuations_with_recorded(directory, true)?
        .into_iter()
        .map(|(_, value)| value)
        .collect())
}

/// Read continuations together with the AUTHORITATIVE ledger row timestamp.
/// Selection must use the row clock, not a body field that a forged body can
/// rewrite.
pub(crate) fn continuations_with_recorded(
    directory: &Path,
    immutable: bool,
) -> Result<Vec<(i64, Value)>> {
    let file = super::super::file_path(directory);
    if !file.is_file() {
        return Ok(Vec::new());
    }
    let db = if immutable {
        super::super::super::open_ledger_immutable(&file)?
    } else {
        rusqlite::Connection::open_with_flags(
            &file,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?
    };
    db.busy_timeout(std::time::Duration::ZERO)?;
    if !table_present(&db, "experiment_continuations")? {
        return Ok(Vec::new());
    }
    let mut query = db
        .prepare(
            "SELECT continuation_id,body,recorded_at_ms FROM experiment_continuations ORDER BY continuation_id",
        )
        .map_err(|_| "candidate_continuation_unavailable")?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|_| "candidate_continuation_unavailable")?;
    let mut values = Vec::new();
    for row in rows {
        let (row_id, body, recorded_at_ms) =
            row.map_err(|_| "candidate_continuation_unavailable")?;
        let value: Value =
            serde_json::from_str(&body).map_err(|_| "candidate_continuation_invalid")?;
        validate_continuation_body(&value)?;
        if value["continuation_id"].as_str() != Some(row_id.as_str())
            || recorded_at_ms < 0
            || value["recorded_at_ms"].as_i64() != Some(recorded_at_ms)
            || value["processed_at_ms"]
                .as_i64()
                .is_none_or(|processed| processed > recorded_at_ms)
        {
            return Err("candidate_continuation_identity_mismatch".into());
        }
        if let Some(session) = ledger_session(directory)
            && value["current_scope"]["current_session"].as_str() != Some(session)
        {
            return Err("candidate_continuation_identity_mismatch".into());
        }
        values.push((recorded_at_ms, value));
    }
    Ok(values)
}
