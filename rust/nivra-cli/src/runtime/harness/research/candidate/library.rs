use super::*;

/// A legacy CURRENT lifecycle ledger without the lifecycle tables is a known
/// empty library ONLY when the original queue independently proves no candidate
/// admissions. With real admissions it is ambiguous and must not be projected
/// as a known zero.
pub(super) fn current_lifecycle_legacy_ambiguous(c: &Config, cfg: &PlannerConfig) -> Result<bool> {
    let Some(settings) = cfg.research_tools.as_ref() else {
        return Ok(false);
    };
    let file = experiment_ledger::file_path(&c.directory);
    if !file.is_file() {
        return Ok(false);
    }
    // A corrupt ledger is UNAVAILABLE, not a legacy-ambiguous empty library.
    let tables = match experiment_ledger::lifecycle::lifecycle_tables_present(&c.directory, false) {
        Ok(tables) => tables,
        Err(_) => return Ok(false),
    };
    if tables {
        return Ok(false);
    }
    if !settings.queue.is_file() {
        // A missing queue is not proof of zero admissions.
        return Ok(true);
    }
    let queue = rusqlite::Connection::open_with_flags(
        &settings.queue,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| "candidate_task_state_unavailable")?;
    super::super::carry::queue_has_candidate_rows(&queue)
}

/// Structured, honest source-observation enumeration. `items` is the bounded
/// list; `status` distinguishes a complete list from a truncated or partial
/// scan; `scanned`/`limit`/`reason` make the bound explicit so an empty or short
/// list is never published as an authoritative complete result.
pub(crate) struct ObservationRefsProjection {
    pub items: Vec<Value>,
    /// `available` (complete), `truncated` (resource bound) or `partial`
    /// (a source/library error was seen; may also be truncated).
    pub status: &'static str,
    pub truncated: bool,
    pub partial: bool,
    pub scanned: usize,
    pub limit: usize,
    /// The TRUNCATION reason (a resource bound), never a source error.
    pub reason: Option<&'static str>,
    /// The first SOURCE error. Kept separate so a partial scan is never
    /// relabelled as a clean `observation_ref_limit` truncation.
    pub source_error_reason: Option<&'static str>,
}

/// Host-built, read-only source-observation refs from the FULL verified
/// lineage. The model can discover the EXACT original observation episode O
/// without an arbitrary path or a private helper, then resolve it through the
/// native `candidate_result(observation_ref)` selector.
///
/// The display is bounded at `limit`, but the scan looks one row past it: an
/// exact-`limit` list is COMPLETE, and only an actual extra verified ref marks
/// truncation. A source error after `limit` refs is PARTIAL and is never
/// masked by the display bound.
pub(crate) fn observation_refs_from_chain(
    c: &Config,
    cfg: &PlannerConfig,
    chain: &[(String, std::path::PathBuf, Value)],
    limit: usize,
) -> Result<ObservationRefsProjection> {
    verify_lineage_prefix(c, cfg, chain)?;
    let current_session = c.session.to_string();
    let mut refs = Vec::new();
    let mut scanned = 0usize;
    let mut truncated = false;
    let mut partial = false;
    let mut reason: Option<&'static str> = None;
    let mut source_error_reason: Option<&'static str> = None;
    let mut note_source_error = |code: &'static str| {
        partial = true;
        if source_error_reason.is_none() {
            source_error_reason = Some(code);
        }
    };
    'chain: for (session, directory, _) in chain {
        let library = if session == &current_session {
            match experiment_ledger::lifecycle::library(directory, session) {
                Ok(library) => library,
                Err(_) => {
                    note_source_error("candidate_observation_library_unavailable");
                    continue;
                }
            }
        } else {
            match experiment_ledger::lifecycle::library_bounded_immutable(directory, "", 10_000) {
                Ok(library) => library,
                Err(_) => {
                    note_source_error("candidate_observation_library_unavailable");
                    continue;
                }
            }
        };
        if library["truncated"].as_bool().unwrap_or(false) {
            truncated = true;
            reason = Some("source_library_truncated");
        }
        let observation_items = library["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| super::super::continuation::is_observation_item(item))
            .collect::<Vec<_>>();
        if observation_items.is_empty() {
            continue;
        }
        // Open the source queue only when this hop actually has an observation
        // item. A successor that recorded only a continuation legitimately has
        // no current queue.
        let mut queue = if session == &current_session {
            let settings = match cfg.research_tools.as_ref() {
                Some(settings) => settings,
                None => {
                    note_source_error("candidate_observation_queue_unavailable");
                    continue;
                }
            };
            match rusqlite::Connection::open_with_flags(
                &settings.queue,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            ) {
                Ok(queue) => queue,
                Err(_) => {
                    note_source_error("candidate_observation_queue_unavailable");
                    continue;
                }
            }
        } else {
            match open_original_queue_read_only_strict(c, cfg, session) {
                Ok(queue) => queue,
                Err(_) => {
                    note_source_error("candidate_observation_queue_unavailable");
                    continue;
                }
            }
        };
        for item in observation_items {
            scanned = scanned.saturating_add(1);
            let Some(id) = item["candidate_id"].as_str() else {
                note_source_error("candidate_observation_identity_missing");
                continue;
            };
            let expected_binding = match expected_source_book_binding(c, chain, session) {
                Ok(binding) => binding,
                Err(_) => {
                    note_source_error("candidate_observation_source_unverified");
                    continue;
                }
            };
            let task = match find_observation_task(&mut queue, session, id, &expected_binding) {
                Ok(task) => task,
                Err(error) => {
                    let code = error.to_string();
                    if code == "candidate_observation_task_truncated" {
                        truncated = true;
                        reason = Some("candidate_observation_task_truncated");
                    } else if code == "candidate_observation_task_unavailable" {
                        note_source_error("candidate_observation_task_unavailable");
                    } else if code == "candidate_observation_book_binding_mismatch" {
                        note_source_error("candidate_observation_book_binding_mismatch");
                    } else {
                        note_source_error("candidate_observation_task_missing");
                    }
                    continue;
                }
            };
            if task.status != jobs::TaskState::Succeeded {
                note_source_error("candidate_observation_task_not_terminal");
                continue;
            }
            let result = match read_verified_candidate_result(
                &mut queue,
                &task.spec.task_id,
                task.spec.revision,
            ) {
                Ok(Some(result)) => result,
                _ => {
                    note_source_error("candidate_observation_result_unverified");
                    continue;
                }
            };
            let Some(score) = result.observation.as_ref() else {
                note_source_error("candidate_observation_score_missing");
                continue;
            };
            let reference = match source_observation_ref_from_task(&task, score) {
                Ok(reference) => reference,
                Err(_) => {
                    note_source_error("candidate_observation_ref_unavailable");
                    continue;
                }
            };
            refs.push(json!({
                "source_session":session,
                "candidate_id":id,
                "observation_task_id":task.spec.task_id,
                "observation_revision":task.spec.revision,
                "source_observation_ref":reference,
                "read_only":true,
                "activation_authorized":false
            }));
            // Look ONE row past the display bound: an exact-limit list is
            // complete, and only an actual extra verified ref is truncation.
            if refs.len() > limit {
                refs.truncate(limit);
                truncated = true;
                if reason.is_none() {
                    reason = Some("observation_ref_limit");
                }
                break 'chain;
            }
        }
    }
    // A source error is never hidden behind a resource-bound truncation.
    let status = if partial {
        "partial"
    } else if truncated {
        "truncated"
    } else {
        "available"
    };
    Ok(ObservationRefsProjection {
        items: refs,
        status,
        truncated,
        partial,
        scanned,
        limit,
        reason,
        source_error_reason,
    })
}

/// Sanitized lifecycle library for the controller/UI projection. Every
/// observation measurement is checked by the canonical reader before it is
/// published; an invalid record is explicit unavailable with no PnL, no exit
/// status and no matured count. Legacy non-observation rows are untouched.
pub(crate) fn checked_lifecycle_library(
    c: &Config,
    cfg: &PlannerConfig,
    now: i64,
) -> Result<Value> {
    let session = c.session.to_string();
    let mut library = experiment_ledger::lifecycle::library(&c.directory, &session)?;
    let chain = predecessor_chain_verified_immutable(c)
        .unwrap_or_else(|_| vec![(session.clone(), c.directory.clone(), Value::Null)]);
    let mut matured = 0usize;
    if let Some(items) = library["items"].as_array_mut() {
        for item in items.iter_mut() {
            if !super::super::continuation::is_observation_item(item) {
                continue;
            }
            let checked = check_library_observation(c, cfg, &chain, item, now);
            if checked.matured {
                matured = matured.saturating_add(1);
            }
            item["measurement_status"] = json!(checked.status);
            item["measurement_reason"] = json!(checked.reason);
            // The EFFECTIVE verified measurement is published; the ORIGINAL
            // stored body is preserved separately so history truth is never
            // relabelled as the successor's effective state.
            item["original_observation_measurement"] =
                checked.original_measurement.unwrap_or(Value::Null);
            item["original_measurement_status"] = json!(checked.original_status);
            item["original_measurement_reason"] = json!(checked.original_reason);
            item["continuation_status"] = json!(checked.continuation_status);
            item["continuation_reason"] = json!(checked.continuation_reason);
            item["observation_measurement"] = checked.measurement.unwrap_or(Value::Null);
            if let Some(reference) = checked.source_ref {
                item["source_observation_ref"] = reference;
            }
        }
    }
    library["matured"] = json!(matured);
    library["measurement_projection"] = json!("canonical_checked_observation_only");
    Ok(library)
}

/// A bounded library-item projection whose stored measurement has been checked
/// by the canonical reader. An invalid record is explicit unavailable, never a
/// raw financial body or a fake zero.
pub(crate) struct CheckedLibraryObservation {
    /// The EFFECTIVE verified measurement (the verified continuation when one
    /// exists for the item's own episode, otherwise the checked current body).
    pub measurement: Option<Value>,
    pub source_ref: Option<Value>,
    pub status: &'static str,
    pub reason: Option<&'static str>,
    /// True ONLY when the canonical reader verified a complete flat executed
    /// measurement. A verified-but-unmatured unknown is never counted.
    pub matured: bool,
    /// The ORIGINAL stored body before continuation precedence, kept separately
    /// so history truth is never relabelled as the successor's effective state.
    pub original_measurement: Option<Value>,
    pub original_status: &'static str,
    pub original_reason: Option<&'static str>,
    /// Whether a verified continuation for THIS item's episode was found.
    pub continuation_status: &'static str,
    pub continuation_reason: Option<&'static str>,
}

fn find_observation_task(
    db: &mut Connection,
    session: &str,
    lifecycle_id: &str,
    expected_binding: &str,
) -> Result<jobs::CandidateTaskRecord> {
    let mut cursor = None;
    let mut scanned = 0usize;
    let mut saw_foreign = false;
    loop {
        let page = jobs::list_candidates_page(db, session, None, cursor.as_ref(), 100)
            .map_err(|_| "candidate_observation_task_unavailable")?;
        if page.is_empty() {
            break;
        }
        let full = page.len() == 100;
        scanned = scanned.saturating_add(page.len());
        cursor = page.last().map(|row| jobs::TaskCursor {
            created_at_ms: row.spec.created_at_ms,
            task_id: row.spec.task_id.clone(),
            revision: row.spec.revision,
        });
        for row in page {
            if row.spec.lifecycle_candidate_id != lifecycle_id {
                continue;
            }
            // A shared queue may hold a foreign book/account row with the same
            // lifecycle id. It is never the verified episode; keep scanning for
            // the row whose own binding is the verified session binding.
            if row.spec.book_binding != expected_binding {
                saw_foreign = true;
                continue;
            }
            return Ok(row);
        }
        if !full {
            break;
        }
        if scanned >= 10_000 {
            let extra = jobs::list_candidates_page(db, session, None, cursor.as_ref(), 1)
                .map_err(|_| "candidate_observation_task_unavailable")?;
            if extra.is_empty() {
                break;
            }
            return Err("candidate_observation_task_truncated".into());
        }
    }
    if saw_foreign {
        return Err("candidate_observation_book_binding_mismatch".into());
    }
    Err("candidate_observation_task_missing".into())
}

fn read_observation_spec(
    queue: &Connection,
    target: &jobs::ObservationTargetV1,
) -> Result<nivra_cli::candidate_execution::ObservationExecutionSpecV1> {
    let bytes =
        jobs::read_candidate_artifact(queue, &target.spec_sha256, "observation_spec", 65_536)
            .map_err(|_| "candidate_observation_spec_missing")?;
    if digest(&bytes) != target.spec_sha256 {
        return Err("candidate_observation_spec_changed".into());
    }
    let spec: nivra_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&bytes).map_err(|_| "candidate_observation_spec_invalid")?;
    nivra_cli::candidate_execution::validate_observation_spec(&spec)
        .map_err(|_| "candidate_observation_spec_invalid")?;
    Ok(spec)
}

/// The VERIFIED book binding for one session. The CURRENT session uses the
/// verified current book; a predecessor uses the `source_binding` recorded by
/// the verified carry hop that points at it. A shared queue row from a foreign
/// book/account is never accepted just because its ids match.
pub(crate) fn expected_source_book_binding(
    c: &Config,
    chain: &[(String, std::path::PathBuf, Value)],
    session: &str,
) -> Result<String> {
    if session == c.session.to_string() {
        return book_binding(c);
    }
    chain
        .iter()
        .find(|(_, _, carry)| carry["source_session"] == session)
        .and_then(|(_, _, carry)| carry["source_binding"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "candidate_observation_source_unverified".into())
}

/// Bind one queue task's recorded book to the VERIFIED session binding. The
/// check runs BEFORE any source artifact/result is read.
pub(crate) fn verify_task_book_binding(
    c: &Config,
    chain: &[(String, std::path::PathBuf, Value)],
    task: &jobs::CandidateTaskRecord,
) -> Result<()> {
    let expected = expected_source_book_binding(c, chain, &task.spec.session)?;
    if task.spec.book_binding != expected {
        return Err("candidate_observation_book_binding_mismatch".into());
    }
    Ok(())
}

/// Re-derive one CURRENT stored body against the verified task/score/spec.
#[allow(clippy::too_many_arguments)]
fn verify_current_library_body(
    c: &Config,
    queue: &Connection,
    directory: &Path,
    task: &jobs::CandidateTaskRecord,
    target: &jobs::ObservationTargetV1,
    score: &nivra_cli::candidate_execution::ObservationScoreV1,
    measurement: &Value,
    session: &str,
    immutable: bool,
    now: i64,
) -> Result<VerifiedStoredObservation> {
    let spec = read_observation_spec(queue, target)?;
    let previous_session = if immutable {
        session
            .parse::<chrono::NaiveDate>()
            .ok()
            .and_then(|day| day.pred_opt())
    } else {
        candidate_previous_trading_session(c)
    };
    let context = StoredObservationContext {
        source: directory,
        session: session
            .parse()
            .map_err(|_| "candidate_observation_session_invalid")?,
        previous_session,
        task,
        score,
        spec: &spec,
        target,
        kind: StoredObservationKind::CurrentMaturation,
        immutable,
        recorded_at_ms: None,
        source_runtime_hash: None,
        source_cohort_key: None,
        current_runtime_hash: None,
        current_cohort_key: None,
        now,
    };
    verify_stored_observation(&context, measurement)
}

fn continuation_matured(value: &Value) -> bool {
    value["state"] == "evaluated"
        && value["execution"].is_object()
        && value["flat"] == true
        && value["accounting_complete"] == true
}

/// The stored continuation body must bind to the ACTUAL ledger item identity:
/// the item's own session/lifecycle id and its verified task/score/entry. A
/// genuine X body copied into a Y projection never matches Y.
fn continuation_body_matches_item(
    body: &Value,
    item_score: &nivra_cli::candidate_execution::ObservationScoreV1,
    item_session: &str,
    item_lifecycle_id: &str,
) -> bool {
    let score_sha = serde_json::to_value(item_score)
        .ok()
        .map(|value| digest(&serde_json::to_vec(&value).unwrap_or_default()));
    let entry_sha = item_score
        .entry
        .as_ref()
        .and_then(|entry| serde_json::to_value(entry).ok())
        .map(|value| digest(&serde_json::to_vec(&value).unwrap_or_default()));
    let entry = item_score.entry.as_ref();
    body["source_lifecycle_candidate_id"].as_str() == Some(item_lifecycle_id)
        && body["source_session"].as_str() == Some(item_session)
        && body["source_task_id"].as_str() == Some(item_score.task_id.as_str())
        && body["source_revision"].as_u64() == Some(u64::from(item_score.revision))
        && body["source_candidate_id"].as_str() == Some(item_score.candidate_id.as_str())
        && body["source_observation_sha256"].as_str()
            == Some(item_score.observation_sha256.as_str())
        && body["source_score_sha256"].as_str() == score_sha.as_deref()
        && body["source_entry_sha256"].as_str() == entry_sha.as_deref()
        && entry.is_some_and(|entry| {
            body["entry"] == serde_json::to_value(entry).unwrap_or(Value::Null)
                && body["entry_quote"] == serde_json::to_value(&entry.quote).unwrap_or(Value::Null)
                && body["key"].as_str() == Some(entry.key.as_str())
        })
}

/// Verify ONE lifecycle library item's stored observation measurement against
/// the ORIGINAL verified source. The context is derived from the ACTUAL ledger
/// ITEM identity (session, lifecycle candidate, task, revision, score SHA)
/// first; the stored body may only echo it. A verified continuation for the
/// SAME episode is the effective state even when an older CURRENT record
/// exists; the original record is preserved separately. The source bytes are
/// never repaired.
pub(crate) fn check_library_observation(
    c: &Config,
    cfg: &PlannerConfig,
    chain: &[(String, std::path::PathBuf, Value)],
    item: &Value,
    now: i64,
) -> CheckedLibraryObservation {
    let stored = item["observation_measurement"].clone();
    // Storage presence is authoritative. An absent/NULL measurement is the
    // genuine "not matured yet"; a PRESENT non-object body (string, number,
    // array, bool or explicit null) is corrupt evidence and must be
    // UNAVAILABLE, never a false not_matured.
    let present = item["observation_measurement_present"]
        .as_bool()
        .unwrap_or(stored.is_object());
    let stored_is_object = stored.is_object();
    let not_matured = !present && stored.is_null();
    let unavailable = |reason: &'static str| CheckedLibraryObservation {
        measurement: None,
        source_ref: None,
        status: "unavailable",
        reason: Some(reason),
        matured: false,
        original_measurement: stored_is_object.then(|| stored.clone()),
        original_status: if stored_is_object {
            "unavailable"
        } else {
            "not_matured"
        },
        original_reason: Some(reason),
        continuation_status: "not_recorded",
        continuation_reason: None,
    };
    if !not_matured && !stored_is_object {
        return unavailable("candidate_observation_measurement_unverified");
    }
    let session = match item["session"].as_str() {
        Some(session) => session,
        None => return unavailable("candidate_observation_session_missing"),
    };
    let lifecycle_id = match item["candidate_id"].as_str() {
        Some(id) => id,
        None => return unavailable("candidate_observation_identity_missing"),
    };
    // The ITEM's verified score is the identity authority, never the body.
    let item_score: nivra_cli::candidate_execution::ObservationScoreV1 =
        match serde_json::from_value(item["observation_score"].clone()) {
            Ok(score) => score,
            Err(_) => return unavailable("candidate_observation_score_missing"),
        };
    if nivra_cli::candidate_execution::validate_observation_score(&item_score).is_err() {
        return unavailable("candidate_observation_score_missing");
    }
    let (directory, immutable) = if session == c.session.to_string() {
        (c.directory.clone(), false)
    } else {
        match chain.iter().find(|(hop, _, _)| hop == session) {
            Some((_, directory, _)) => (directory.clone(), true),
            None => return unavailable("candidate_observation_source_unverified"),
        }
    };
    let mut queue = if immutable {
        match open_original_queue_read_only_strict(c, cfg, session) {
            Ok(queue) => queue,
            Err(_) => return unavailable("candidate_observation_queue_unavailable"),
        }
    } else {
        let Some(settings) = cfg.research_tools.as_ref() else {
            return unavailable("candidate_observation_queue_unavailable");
        };
        match rusqlite::Connection::open_with_flags(
            &settings.queue,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        ) {
            Ok(queue) => queue,
            Err(_) => return unavailable("candidate_observation_queue_unavailable"),
        }
    };
    let expected_binding = match expected_source_book_binding(c, chain, session) {
        Ok(binding) => binding,
        Err(_) => return unavailable("candidate_observation_source_unverified"),
    };
    let task = match find_observation_task(&mut queue, session, lifecycle_id, &expected_binding) {
        Ok(task) => task,
        Err(error) => {
            let code = error.to_string();
            let reason = match code.as_str() {
                "candidate_observation_task_truncated" => "candidate_observation_task_truncated",
                "candidate_observation_task_unavailable" => {
                    "candidate_observation_task_unavailable"
                }
                "candidate_observation_book_binding_mismatch" => {
                    "candidate_observation_book_binding_mismatch"
                }
                _ => "candidate_observation_task_missing",
            };
            return unavailable(reason);
        }
    };
    // The queue row must reproduce the ITEM's own identity.
    if task.spec.session != session
        || task.spec.lifecycle_candidate_id != lifecycle_id
        || task.spec.candidate_id != item_score.candidate_id
        || task.spec.task_id != item_score.task_id
        || task.spec.revision != item_score.revision
        || task.spec.book_binding != expected_binding
    {
        return unavailable("candidate_observation_identity_mismatch");
    }
    let target = match &task.spec.purpose {
        Some(jobs::CandidatePurposeV1::Observe { target }) if target.validate() => {
            target.as_ref().clone()
        }
        _ => return unavailable("candidate_observation_target_invalid"),
    };
    if task.status != jobs::TaskState::Succeeded {
        return unavailable("candidate_observation_result_unverified");
    }
    let result =
        match read_verified_candidate_result(&mut queue, &task.spec.task_id, task.spec.revision) {
            Ok(Some(result)) => result,
            _ => return unavailable("candidate_observation_result_unverified"),
        };
    let Some(score) = result.observation.as_ref() else {
        return unavailable("candidate_observation_score_missing");
    };
    if serde_json::to_value(score).ok() != Some(item["observation_score"].clone()) {
        return unavailable("candidate_observation_score_mismatch");
    }
    let source_ref = source_observation_ref_from_task(&task, score).ok();
    if not_matured {
        return CheckedLibraryObservation {
            measurement: None,
            source_ref,
            status: "not_matured",
            reason: None,
            matured: false,
            original_measurement: None,
            original_status: "not_matured",
            original_reason: None,
            continuation_status: "not_recorded",
            continuation_reason: None,
        };
    }
    // A verified continuation for THIS item's episode is the effective state.
    // A broken lineage is an explicit unavailable continuation, never a silent
    // absence and never a reason to trust a stored body on its own.
    let (continuations, continuation_error) = match verified_continuations_from_chain(c, cfg, chain)
    {
        Ok(values) => (values, None),
        Err(error) => (Vec::new(), Some(bounded_context_reason(error.as_ref()))),
    };
    let own_continuation = continuations.iter().find(|value| {
        value["source_lifecycle_candidate_id"] == lifecycle_id && value["source_session"] == session
    });
    let continuation_status = if own_continuation.is_some() {
        "verified"
    } else if continuation_error.is_some() {
        "unavailable"
    } else {
        "not_recorded"
    };
    // The ORIGINAL stored body's own status, computed separately from the
    // effective state so history truth is never relabelled.
    let (original_status, original_reason) = if stored_is_object {
        if stored["continuation_id"].is_string() {
            if continuation_body_matches_item(&stored, &item_score, session, lifecycle_id) {
                let found = own_continuation.is_some_and(|value| {
                    value["measurement_sha256"] == stored["measurement_sha256"]
                });
                if found {
                    ("verified", None)
                } else {
                    (
                        "unavailable",
                        Some("candidate_observation_continuation_unverified"),
                    )
                }
            } else {
                (
                    "unavailable",
                    Some("candidate_observation_identity_mismatch"),
                )
            }
        } else {
            match verify_current_library_body(
                c, &queue, &directory, &task, &target, score, &stored, session, immutable, now,
            ) {
                Ok(_) => ("verified", None),
                Err(_) => (
                    "unavailable",
                    Some("candidate_observation_measurement_unverified"),
                ),
            }
        }
    } else {
        ("not_matured", None)
    };
    if let Some(continuation) = own_continuation {
        if continuation["source_task_id"].as_str() == Some(item_score.task_id.as_str())
            && continuation["source_revision"].as_u64() == Some(u64::from(item_score.revision))
            && continuation["source_observation_sha256"].as_str()
                == Some(item_score.observation_sha256.as_str())
            && continuation["source_candidate_id"].as_str()
                == Some(item_score.candidate_id.as_str())
        {
            return CheckedLibraryObservation {
                measurement: Some(continuation.clone()),
                source_ref,
                status: "verified",
                reason: None,
                matured: continuation_matured(continuation),
                original_measurement: stored_is_object.then(|| stored.clone()),
                original_status,
                original_reason,
                continuation_status: "verified",
                continuation_reason: None,
            };
        }
        return unavailable("candidate_observation_identity_mismatch");
    }
    if stored["continuation_id"].is_string() {
        // No verified continuation for this episode: the stored body is never
        // trusted on its own, and a copied body is an explicit identity error.
        let reason = if continuation_body_matches_item(&stored, &item_score, session, lifecycle_id)
        {
            "candidate_observation_continuation_unverified"
        } else {
            "candidate_observation_identity_mismatch"
        };
        return CheckedLibraryObservation {
            measurement: None,
            source_ref,
            status: "unavailable",
            reason: Some(reason),
            matured: false,
            original_measurement: Some(stored.clone()),
            original_status,
            original_reason,
            continuation_status,
            continuation_reason: continuation_error,
        };
    }
    match verify_current_library_body(
        c, &queue, &directory, &task, &target, score, &stored, session, immutable, now,
    ) {
        Ok(verified) => CheckedLibraryObservation {
            measurement: Some(stored.clone()),
            source_ref,
            status: "verified",
            reason: None,
            matured: verified.matured,
            original_measurement: Some(stored.clone()),
            original_status: "verified",
            original_reason: None,
            continuation_status,
            continuation_reason: continuation_error,
        },
        Err(_) => CheckedLibraryObservation {
            measurement: None,
            source_ref,
            status: "unavailable",
            reason: Some("candidate_observation_measurement_unverified"),
            matured: false,
            original_measurement: Some(stored.clone()),
            original_status,
            original_reason,
            continuation_status,
            continuation_reason: continuation_error,
        },
    }
}
