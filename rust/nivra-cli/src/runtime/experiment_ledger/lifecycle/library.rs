use super::*;

/// Presence-aware measurement read from the RAW stored projection. An absent
/// key (or an SQL NULL column) is `None`, the authoritative "not matured yet".
/// A PRESENT value is `Some`, including explicit JSON null, the string "null",
/// a number, array or bool, so the canonical reader can mark it unavailable
/// instead of silently treating a corrupt body as not matured.
fn canonical_measurement(raw_projection: &str) -> Option<Value> {
    serde_json::from_str::<Value>(raw_projection)
        .ok()
        .and_then(|value| value.get("observation_measurement").cloned())
}

fn library_item(
    id: &str,
    spec: &ExperimentSpec,
    state: &str,
    projection: &Projection,
    measurement: Option<Value>,
) -> Value {
    let measurement_present = measurement.is_some();
    json!({
        "candidate_id": id,
        "parent_candidate_id": spec.parent_candidate_id,
        "session": spec.session,
        "source_sha256": spec.source_sha256,
        "input_sha256": spec.params_sha256,
        "hypothesis": spec.hypothesis,
        "strategy_abi": spec.feature_abi,
        "feature_abi": spec.feature_abi,
        "origin_session": spec.session,
        "continued": false,
        "report_kind": if spec.feature_abi == "cash_quote_action_tape_v1" {
            "executable_replay"
        } else if spec.feature_abi == "observation_single_entry_v1" {
            "forward_observation_score"
        } else {
            "ranking_diagnostic"
        },
        "holding_horizon": spec.holding_ms.to_string(),
        "state": state,
        "result_sha256": projection.replay_report_sha256,
        "observation_score": projection.observation_score,
        "observation_measurement": measurement,
        "observation_measurement_present": measurement_present,
        "scientific_state": scientific_state(state, measurement_present),
        "activation_authorized": false,
        "source": "experiment_lifecycle"
    })
}

/// Direct bounded lookup of ONE lifecycle item by candidate id. This avoids the
/// 10k library page entirely, so a target outside the page is an authoritative
/// `None` instead of a false `not_matured`.
pub(crate) fn observation_item(directory: &Path, id: &str) -> Result<Option<Value>> {
    let file = super::super::file_path(directory);
    if !file.is_file() {
        return Ok(None);
    }
    let db = rusqlite::Connection::open_with_flags(
        &file,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(std::time::Duration::ZERO)?;
    if !table_present(&db, "experiment_lifecycle_specs")?
        || !table_present(&db, "experiment_lifecycle_state")?
    {
        return Ok(None);
    }
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT s.body,st.state,st.projection FROM experiment_lifecycle_specs s JOIN experiment_lifecycle_state st USING(candidate_id) WHERE s.candidate_id=?",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|_| "candidate_lifecycle_unavailable")?;
    let Some((body, state, projection)) = row else {
        return Ok(None);
    };
    let spec: ExperimentSpec = serde_json::from_str(&body)?;
    let measurement = canonical_measurement(&projection);
    let projection: Projection = serde_json::from_str(&projection)?;
    Ok(Some(library_item(
        id,
        &spec,
        &state,
        &projection,
        measurement,
    )))
}

const LIBRARY_PAGE: usize = 256;
const LIBRARY_MAX: usize = 10_000;

pub(super) fn table_present(db: &Connection, name: &str) -> Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
        [name],
        |row| row.get::<_, bool>(0),
    )
    .map_err(|_| "candidate_lifecycle_unavailable".into())
}

/// Whether this ledger carries the lifecycle tables. A legacy ledger without
/// them is an EMPTY library only when the original queue independently proves
/// no candidate admissions; otherwise the projection is ambiguous.
pub(crate) fn lifecycle_tables_present(directory: &Path, immutable: bool) -> Result<bool> {
    let file = super::super::file_path(directory);
    if !file.is_file() {
        return Ok(false);
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
    Ok(table_present(&db, "experiment_lifecycle_specs")?
        && table_present(&db, "experiment_lifecycle_state")?)
}

fn empty_library() -> Value {
    json!({"status":"not_recorded","items":[],"truncated":false,"activation_authorized":false,"source":"experiment_lifecycle"})
}

/// Read the lifecycle library with bounded keyset paging. The previous reader
/// stopped at 256 rows and silently dropped later observations; this reader
/// walks the full bounded range and reports truncation truthfully. A ledger
/// without the lifecycle tables is a valid legacy ledger with no lifecycle
/// records; a table that exists but cannot be read is unavailable.
pub(crate) fn library(directory: &Path, session: &str) -> Result<Value> {
    library_bounded(directory, session, LIBRARY_MAX)
}

pub(crate) fn library_bounded(directory: &Path, session: &str, max: usize) -> Result<Value> {
    library_bounded_with(directory, session, max, false)
}

/// Immutable read of a CLOSED predecessor lifecycle ledger. A non-empty WAL is
/// an explicit refusal and no `-wal`/`-shm` sidecar is created.
pub(crate) fn library_bounded_immutable(
    directory: &Path,
    session: &str,
    max: usize,
) -> Result<Value> {
    library_bounded_with(directory, session, max, true)
}

fn library_bounded_with(
    directory: &Path,
    session: &str,
    max: usize,
    immutable: bool,
) -> Result<Value> {
    let file = super::super::file_path(directory);
    if !file.is_file() {
        return Ok(empty_library());
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
    if !table_present(&db, "experiment_lifecycle_specs")?
        || !table_present(&db, "experiment_lifecycle_state")?
    {
        return Ok(empty_library());
    }
    let mut items = Vec::new();
    let mut truncated = false;
    let mut after: Option<String> = None;
    // Filter the requested session BEFORE the bounded page limit. Filtering in
    // Rust after the limit made a page of other-session rows look like a
    // truncated target-session library.
    let session_pattern = (!session.is_empty()).then(|| format!("\"session\":\"{session}\""));
    loop {
        let mut query = db.prepare(
            "SELECT s.candidate_id,s.body,st.state,st.projection FROM experiment_lifecycle_specs s JOIN experiment_lifecycle_state st USING(candidate_id) WHERE (?1 IS NULL OR s.candidate_id>?1) AND (?3 IS NULL OR instr(s.body,?3)>0) ORDER BY s.candidate_id LIMIT ?2",
        ).map_err(|_| "candidate_lifecycle_unavailable")?;
        let rows = query
            .query_map(
                params![after, (LIBRARY_PAGE + 1) as i64, session_pattern],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(|_| "candidate_lifecycle_unavailable")?;
        let mut page = 0usize;
        for row in rows {
            let (id, body, state, projection) =
                row.map_err(|_| "candidate_lifecycle_unavailable")?;
            page += 1;
            after = Some(id.clone());
            if items.len() >= max {
                truncated = true;
                break;
            }
            let spec: ExperimentSpec = serde_json::from_str(&body)?;
            if !session.is_empty() && spec.session != session {
                continue;
            }
            let measurement = canonical_measurement(&projection);
            let projection: Projection = serde_json::from_str(&projection)?;
            items.push(library_item(&id, &spec, &state, &projection, measurement));
        }
        if page < LIBRARY_PAGE + 1 || items.len() >= max {
            break;
        }
    }
    if !truncated && items.len() >= max {
        // Exact `max` is not truncation unless an actual extra row exists.
        let extra: Option<String> = db
            .query_row(
                "SELECT candidate_id FROM experiment_lifecycle_specs WHERE candidate_id>? AND (?2 IS NULL OR instr(body,?2)>0) ORDER BY candidate_id LIMIT 1",
                params![after.unwrap_or_default(), session_pattern],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "candidate_lifecycle_unavailable")?;
        truncated = extra.is_some();
    }
    Ok(json!({
        "status": if items.is_empty() { "not_recorded" } else { "ready" },
        "items": items,
        "truncated": truncated,
        "activation_authorized": false,
        "source": "experiment_lifecycle"
    }))
}

/// Authority read of a CLOSED predecessor ledger: a missing ledger is
/// unavailable, and a non-empty WAL is an explicit refusal.
pub(crate) fn library_checked_immutable(directory: &Path, session: &str) -> Result<Value> {
    if !super::super::file_path(directory).is_file() {
        return Err("candidate_lifecycle_unavailable".into());
    }
    library_bounded_immutable(directory, session, LIBRARY_MAX)
}

/// Read the current lifecycle library, treating a missing ledger as an empty
/// projection. An existing but corrupt ledger is still unavailable.
pub(crate) fn library_optional(directory: &Path, session: &str) -> Result<Value> {
    if !super::super::file_path(directory).is_file() {
        return Ok(
            json!({"status":"not_recorded","items":[],"activation_authorized":false,"source":"experiment_lifecycle"}),
        );
    }
    library(directory, session)
}

pub(crate) fn merge_into_library(research: Option<Value>, lifecycle: &Value) -> Option<Value> {
    let extra = match lifecycle["items"].as_array() {
        Some(items) if !items.is_empty() => items.clone(),
        _ => return research,
    };
    let mut body = match research {
        None | Some(Value::Null) => {
            json!({"status":"ready","items":[],"activation_authorized":false,"source":"experiment_lifecycle"})
        }
        Some(value) => value,
    };
    if let Some(items) = body["items"].as_array_mut() {
        items.extend(extra);
    } else {
        body["items"] = json!(extra);
    }
    body["activation_authorized"] = json!(false);
    if body["status"] == "disabled" || body["status"] == "not_recorded" {
        body["status"] = json!("ready");
    }
    Some(body)
}
