use super::*;

pub(crate) fn observation_status(c: &Config, cfg: &PlannerConfig, now: i64) -> Value {
    let Some(settings) = cfg.research_tools.as_ref() else {
        return json!({"status":"disabled","reason":"research_disabled"});
    };
    let Some(observation) = settings
        .candidate_compute
        .as_ref()
        .and_then(|candidate| candidate.observation.as_ref())
        .filter(|observation| observation.enabled)
    else {
        return json!({"status":"disabled","reason":"candidate_observation_disabled"});
    };
    // A fresh successor may not have created its own queue yet. A missing
    // current queue is an empty current projection; an existing unreadable
    // queue is still unavailable.
    let mut db = if settings.queue.is_file() {
        match rusqlite::Connection::open_with_flags(
            &settings.queue,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(db) => Some(db),
            Err(_) => {
                return json!({"status":"unavailable","reason":"candidate_task_state_unavailable"});
            }
        }
    } else {
        None
    };
    // Page the original queue instead of silently truncating at 100 rows.
    let session = c.session.to_string();
    let mut rows = Vec::new();
    let mut queue_truncated = false;
    if let Some(db) = db.as_mut() {
        let mut cursor = None;
        for _ in 0..10 {
            let Ok(page) = jobs::list_candidates_page(db, &session, None, cursor.as_ref(), 100)
            else {
                return json!({"status":"unavailable","reason":"candidate_task_state_unavailable"});
            };
            if page.is_empty() {
                break;
            }
            let full = page.len() == 100;
            cursor = page.last().map(|row| jobs::TaskCursor {
                created_at_ms: row.spec.created_at_ms,
                task_id: row.spec.task_id.clone(),
                revision: row.spec.revision,
            });
            rows.extend(page);
            if !full {
                break;
            }
            if rows.len() >= 1_000 {
                // Exact 1000 is not truncated unless an actual extra row exists.
                queue_truncated =
                    jobs::list_candidates_page(db, &session, None, cursor.as_ref(), 1)
                        .map(|page| !page.is_empty())
                        .unwrap_or(true);
                break;
            }
        }
    }
    let observations = rows
        .iter()
        .filter(|row| row.spec.purpose.is_some())
        .collect::<Vec<_>>();
    let pending = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::Pending)
        .count();
    let running = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::Running)
        .count();
    let unknown = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::Unknown)
        .count();
    let cancel_requested = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::CancelRequested)
        .count();
    let succeeded = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::Succeeded)
        .count();
    let oldest = observations.iter().map(|row| row.spec.created_at_ms).min();
    let deadline = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::Pending)
        .map(|row| row.spec.expires_at_ms)
        .min();
    // Fold every verified predecessor lifecycle ledger, not only the nearest
    // one. A broken chain or unreadable ledger is unavailable, never zero.
    let chain = match super::super::carry::predecessor_chain_verified_immutable(c) {
        Ok(chain) => chain,
        Err(reason) => {
            return json!({
                "status":"unavailable",
                "reason":reason.to_string(),
                "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
                "lifecycle":{"status":"unavailable","recorded":null,"matured":null,"milestones":[]},
                "trials":super::carry::trial_fold(c, cfg),
                "activation_authorized":false,
                "financial_profit_claim":false,
                "as_of_ms":now
            });
        }
    };
    if current_lifecycle_legacy_ambiguous(c, cfg).unwrap_or(true) {
        return json!({
            "status":"unavailable",
            "reason":"candidate_predecessor_lifecycle_legacy_ambiguous",
            "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
            "lifecycle":{"status":"unavailable","recorded":null,"matured":null,"milestones":[]},
            "trials":super::carry::trial_fold(c, cfg),
            "activation_authorized":false,
            "financial_profit_claim":false,
            "as_of_ms":now
        });
    }
    let mut lifecycle_truncated = false;
    let mut items = match experiment_ledger::lifecycle::library_optional(&c.directory, &session) {
        Ok(library) => {
            lifecycle_truncated |= library["truncated"].as_bool().unwrap_or(false);
            library["items"].as_array().cloned().unwrap_or_default()
        }
        Err(_) => {
            return json!({
                "status":"unavailable",
                "reason":"candidate_lifecycle_unavailable",
                "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
                "lifecycle":{"status":"unavailable","recorded":null,"matured":null,"milestones":[]},
                "trials":super::carry::trial_fold(c, cfg),
                "activation_authorized":false,
                "financial_profit_claim":false,
                "as_of_ms":now
            });
        }
    };
    let mut seen = items
        .iter()
        .filter_map(|item| item["candidate_id"].as_str().map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>();
    for (source_session, directory, _) in &chain {
        if source_session == &session {
            continue;
        }
        let Ok(previous) = experiment_ledger::lifecycle::library_checked_immutable(directory, "")
        else {
            return json!({
                "status":"unavailable",
                "reason":"candidate_predecessor_lifecycle_unavailable",
                "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
                "lifecycle":{"status":"unavailable","recorded":null,"matured":null,"milestones":[]},
                "trials":super::carry::trial_fold(c, cfg),
                "activation_authorized":false,
                "financial_profit_claim":false,
                "as_of_ms":now
            });
        };
        lifecycle_truncated |= previous["truncated"].as_bool().unwrap_or(false);
        for mut item in previous["items"].as_array().cloned().unwrap_or_default() {
            let Some(id) = item["candidate_id"].as_str().map(str::to_owned) else {
                continue;
            };
            if !seen.insert(id) {
                continue;
            }
            item["continued"] = json!(true);
            item["origin_session"] = json!(source_session);
            items.push(item);
        }
    }
    // The CURRENT continuation is the latest verified state of the ORIGINAL
    // episode. It is attached by original identity, never added as a second
    // candidate or counted as a second trial.
    // The canonical verified lineage resolver: a continuation recorded by ANY
    // earlier verified successor is the latest state of the ORIGINAL episode.
    // Its recording scope is preserved; a later successor never re-records it.
    let continuations = match super::super::verified_continuations(c, cfg) {
        Ok(continuations) => continuations,
        Err(reason) => {
            return json!({
                "status":"unavailable",
                "reason":reason.to_string(),
                "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
                "lifecycle":{"status":"unavailable","recorded":null,"matured":null,"milestones":[]},
                "trials":super::carry::trial_fold(c, cfg),
                "activation_authorized":false,
                "financial_profit_claim":false,
                "as_of_ms":now
            });
        }
    };
    for item in &mut items {
        let Some(id) = item["candidate_id"].as_str() else {
            continue;
        };
        if let Some(continuation) = continuations
            .iter()
            .find(|continuation| continuation["source_lifecycle_candidate_id"] == id)
        {
            item["continuation"] = continuation.clone();
            item["continuation_recorded_in_session"] =
                continuation["current_scope"]["current_session"].clone();
            item["continued"] = json!(true);
            if item["observation_measurement"].is_null() {
                item["observation_measurement"] = continuation.clone();
            }
        }
    }
    let observation_items = items
        .iter()
        .filter(|item| {
            item["feature_abi"] == nivra_cli::candidate_execution::OBSERVATION_ABI
                || item["strategy_abi"] == nivra_cli::candidate_execution::OBSERVATION_ABI
        })
        .collect::<Vec<_>>();
    // Publish ONLY canonically checked measurements. A forged/invalid stored
    // body is explicit unavailable with no PnL, no exit status and no matured
    // count; it is never passed through raw from the lifecycle projection.
    let mut milestones = Vec::new();
    let mut matured = 0usize;
    for item in &observation_items {
        let checked = check_library_observation(c, cfg, &chain, item, now);
        if checked.matured {
            matured = matured.saturating_add(1);
        }
        let measurement = checked.measurement.clone().unwrap_or(Value::Null);
        // ONLY the canonical reader may mint a verified source ref. A raw
        // observation_score + job hash is not proof that the result artifact
        // re-verifies; a corrupt artifact publishes an unavailable ref status,
        // never a fabricated verified ref.
        let source_ref = checked.source_ref.clone();
        let source_ref_status = if source_ref.is_some() {
            "verified"
        } else if checked.status == "not_matured" {
            "not_matured"
        } else {
            "unavailable"
        };
        let source_ref_reason = if source_ref.is_some() {
            Value::Null
        } else if checked.status == "not_matured" {
            json!("candidate_observation_not_matured")
        } else {
            json!("candidate_observation_source_ref_unverified")
        };
        milestones.push(json!({
            "candidate_id":item["candidate_id"],
            "state":item["state"],
            "original_recorded_state":item["state"],
            "effective_state":if item["continuation"].is_object() { item["continuation"]["state"].clone() } else { item["state"].clone() },
            // The effective lifecycle state is separate from the recorded
            // scientific vocabulary. A continued `evaluated` episode is
            // `insufficient_evidence`, never an invented
            // `continued_evaluated` qualification.
            "effective_scientific_state":if item["continuation"].is_object() {
                experiment_ledger::lifecycle::scientific_state(
                    item["continuation"]["state"].as_str().unwrap_or("unknown"),
                    !item["effective_observation_measurement"].is_null(),
                )
            } else {
                item["scientific_state"].as_str().unwrap_or("unknown")
            },
            "origin_session":item["origin_session"],
            "continued":item["continued"],
            "observation_scope":item["observation_score"]["observation_scope"],
            "status":item["observation_score"]["status"],
            "gaps":item["observation_score"]["gaps"],
            "measurement_status":checked.status,
            "measurement_reason":checked.reason,
            "measurement_scope":measurement["source_scope"],
            "exit_status":measurement["exit_status"],
            "measurement_gaps":measurement["gaps"],
            "measurement_sha256":measurement["measurement_sha256"],
            // The ORIGINAL recorded body is preserved separately from the
            // effective scientific/lifecycle state. A successor never
            // relabels the predecessor's history as its own receipt.
            "original_measurement_sha256":checked.original_measurement.as_ref().map(|body| body["measurement_sha256"].clone()).unwrap_or(Value::Null),
            "original_exit_status":checked.original_measurement.as_ref().map(|body| body["exit_status"].clone()).unwrap_or(Value::Null),
            "original_measurement_status":checked.original_status,
            "original_measurement_reason":checked.original_reason,
            "continuation_status":checked.continuation_status,
            "continuation_reason":checked.continuation_reason,
            "continuation_state":item["continuation"]["state"],
            "continuation_recorded_in_session":item["continuation_recorded_in_session"],
            "processed_at_ms":item["continuation"]["processed_at_ms"],
            "exit_at_ms":item["continuation"]["exit_at_ms"],
            "source_observation_ref":source_ref,
            "source_observation_ref_status":source_ref_status,
            "source_observation_ref_reason":source_ref_reason
        }));
    }
    let trials = super::super::carry::trial_fold(c, cfg);
    let capture = json!({"enabled":false,"coverage":"disabled"});
    let (
        observation_refs,
        observation_refs_status,
        observation_refs_truncated,
        observation_refs_partial,
        observation_refs_scanned,
        observation_refs_limit,
        observation_refs_reason,
        observation_refs_source_error_reason,
    ) = match observation_refs_from_chain(c, cfg, &chain, 16) {
        Ok(refs) => (
            refs.items,
            refs.status,
            refs.truncated,
            refs.partial,
            refs.scanned,
            refs.limit,
            refs.reason,
            refs.source_error_reason,
        ),
        Err(_) => (
            Vec::new(),
            "unavailable",
            false,
            false,
            0usize,
            16usize,
            Some("observation_refs_unavailable"),
            Some("observation_refs_unavailable"),
        ),
    };
    json!({
        "status":"available",
        "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
        "lifecycle":{
            "status":"available",
            "recorded":observation_items.len(),
            "matured":matured,
            "truncated":lifecycle_truncated,
            "milestones":milestones
        },
        "cohort":{
            "account":c.account,
            "cohort_key":super::carry::current_cohort_key(c,cfg).ok(),
            "feed":"wide_quotes",
            "dataset_abi":"scorer_replay_v1"
        },
        "trials":trials,
        "observation_refs":observation_refs,
        "observation_refs_status":observation_refs_status,
        "observation_refs_truncated":observation_refs_truncated,
        "observation_refs_partial":observation_refs_partial,
        "observation_refs_scanned":observation_refs_scanned,
        "observation_refs_limit":observation_refs_limit,
        "observation_refs_reason":observation_refs_reason,
        "observation_refs_source_error_reason":observation_refs_source_error_reason,
        "abi":observation.abi,
        "measurement":{
            "evaluation_scope":"independent_episodes",
            "strategy_portfolio_claimed":false,
            "portfolio_return":null,
            "aggregate_net_pnl":null
        },
        "capture":capture,
        "financial_profit_claim":false,
        "activation_authorized":false,
        "as_of_ms":now,
        "source":"original_queue_and_lifecycle_read_only"
    })
}

#[allow(clippy::too_many_arguments)]
fn queue_projection(
    pending: usize,
    running: usize,
    unknown: usize,
    cancel_requested: usize,
    succeeded: usize,
    total: usize,
    oldest: Option<i64>,
    deadline: Option<i64>,
    now: i64,
    truncated: bool,
) -> Value {
    json!({
        "pending":pending,
        "running":running,
        "unknown":unknown,
        "cancel_requested":cancel_requested,
        "succeeded":succeeded,
        "total":total,
        "truncated":truncated,
        "oldest_created_ms":oldest,
        "oldest_age_ms":oldest.map(|created| now.saturating_sub(created)),
        "next_deadline_ms":deadline,
        "deadline_remaining_ms":deadline.map(|deadline| deadline.saturating_sub(now))
    })
}

pub(crate) fn mature_candidates(c: &Config, cfg: &PlannerConfig, now: i64) -> Result<Value> {
    let Some(settings) = cfg.research_tools.as_ref() else {
        return Ok(json!({"status":"disabled","items":[],"activation_authorized":false}));
    };
    if !settings
        .candidate_compute
        .as_ref()
        .is_some_and(|candidate| candidate.enabled)
    {
        return Ok(json!({"status":"disabled","items":[],"activation_authorized":false}));
    }
    // A fresh successor may have no current lifecycle ledger yet. A missing
    // ledger is an empty current projection; an existing corrupt ledger is
    // still unavailable. The predecessor continuation path below records into
    // the CURRENT ledger only.
    if current_lifecycle_legacy_ambiguous(c, cfg).unwrap_or(true) {
        return Ok(json!({
            "status":"unavailable",
            "reason":"candidate_predecessor_lifecycle_legacy_ambiguous",
            "items":[],
            "activation_authorized":false
        }));
    }
    let library = match experiment_ledger::lifecycle::library_optional(
        &c.directory,
        &c.session.to_string(),
    ) {
        Ok(library) => library,
        Err(_) => {
            return Ok(json!({
                "status":"unavailable",
                "reason":"candidate_lifecycle_unavailable",
                "items":[],
                "activation_authorized":false
            }));
        }
    };
    let Some(items) = library["items"].as_array().cloned() else {
        return Ok(json!({
            "status":"unavailable",
            "reason":"candidate_lifecycle_invalid",
            "items":[],
            "activation_authorized":false
        }));
    };
    // A fresh successor may not have created its own queue yet. The original
    // queues are opened separately, read-only, through the verified locator.
    let mut queue = if settings.queue.is_file() {
        Some(
            rusqlite::Connection::open_with_flags(
                &settings.queue,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|_| "candidate_task_state_unavailable")?,
        )
    } else {
        None
    };
    // Page the declared queue bound instead of silently skipping observations
    // after the first 100 rows. A scan cap is an explicit deferral, never a
    // permanent "replayed but unseen" state.
    let session = c.session.to_string();
    let mut candidates = Vec::new();
    let mut scan_truncated = false;
    if let Some(queue) = queue.as_mut() {
        let mut cursor = None;
        loop {
            let page = jobs::list_candidates_page(queue, &session, None, cursor.as_ref(), 100)
                .map_err(|_| "candidate_task_state_unavailable")?;
            if page.is_empty() {
                break;
            }
            let full = page.len() == 100;
            cursor = page.last().map(|row| jobs::TaskCursor {
                created_at_ms: row.spec.created_at_ms,
                task_id: row.spec.task_id.clone(),
                revision: row.spec.revision,
            });
            candidates.extend(page);
            if !full {
                break;
            }
            if candidates.len() >= 10_000 {
                // Exact 10k is not truncation unless an actual extra row
                // exists. Never claim a truncation the queue does not have.
                scan_truncated =
                    jobs::list_candidates_page(queue, &session, None, cursor.as_ref(), 1)
                        .map(|page| !page.is_empty())
                        .unwrap_or(true);
                break;
            }
        }
    }
    let by_lifecycle = candidates
        .iter()
        .filter(|task| task.spec.purpose.is_some())
        .map(|task| (task.spec.lifecycle_candidate_id.clone(), task))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut seen_lifecycles = std::collections::BTreeSet::new();
    let mut matured = Vec::new();
    for item in items {
        let Some(id) = item["candidate_id"].as_str() else {
            continue;
        };
        seen_lifecycles.insert(id.to_owned());
        let state = item["state"].as_str().unwrap_or_default();
        if (item["feature_abi"] == nivra_cli::candidate_execution::OBSERVATION_ABI
            || item["strategy_abi"] == nivra_cli::candidate_execution::OBSERVATION_ABI)
            && state == "replayed"
        {
            let Some(task) = by_lifecycle
                .get(id)
                .filter(|task| task.status == jobs::TaskState::Succeeded)
            else {
                continue;
            };
            let result = match queue.as_mut().and_then(|queue| {
                read_verified_candidate_result(queue, &task.spec.task_id, task.spec.revision)
                    .ok()
                    .flatten()
            }) {
                Some(result) => result,
                _ => {
                    matured.push(json!({
                        "candidate_id":id,
                        "state":"unavailable",
                        "reason":"candidate_observation_result_unverified",
                        "activation_authorized":false,
                        "edge_qualified":false
                    }));
                    continue;
                }
            };
            match queue
                .as_mut()
                .map(|queue| mature_observation(c, cfg, queue, task, &result, now))
                .unwrap_or_else(|| Err("candidate_task_state_unavailable".into()))
            {
                Ok(MaturationDecision::Waiting { due_ms }) => matured.push(json!({
                    "candidate_id":id,
                    "state":"waiting",
                    "due_ms":due_ms,
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Ok(MaturationDecision::PendingCapture {
                    due_ms,
                    deadline_ms,
                    gap,
                }) => matured.push(json!({
                    "candidate_id":id,
                    "state":"pending_capture",
                    "due_ms":due_ms,
                    "deadline_ms":deadline_ms,
                    "gap":gap,
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Ok(MaturationDecision::Matured(measurement)) => matured.push(json!({
                    "candidate_id":id,
                    "state":"evaluated",
                    "observation":measurement,
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Ok(MaturationDecision::Unknown(measurement)) => matured.push(json!({
                    "candidate_id":id,
                    "state":"unknown",
                    "observation":measurement,
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Err(error) => matured.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_observation_maturation_unavailable",
                    "detail":error.to_string().chars().take(256).collect::<String>(),
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
            }
            continue;
        }
        if state == "evaluated" {
            match experiment_ledger::lifecycle::conclude_if_ready(&c.directory, id, now) {
                Ok(Some(receipt)) => matured.push(json!({
                    "candidate_id":id,
                    "state":receipt["state"],
                    "marks":receipt["marks"],
                    "checks":receipt["checks"],
                    "observation":receipt["observation_measurement"],
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Ok(None) => {}
                Err(error) => matured.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_conclusion_unavailable",
                    "detail":error.to_string().chars().take(256).collect::<String>(),
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
            }
            continue;
        }
        if state != "forward_shadow" {
            continue;
        }
        let marks = experiment_ledger::lifecycle::pending_marks(&c.directory, id)?;
        if marks.is_empty() {
            match experiment_ledger::lifecycle::conclude_if_ready(&c.directory, id, now) {
                Ok(Some(receipt)) => matured.push(json!({
                    "candidate_id":id,
                    "state":receipt["state"],
                    "marks":receipt["marks"],
                    "checks":receipt["checks"],
                    "observation":receipt["observation_measurement"],
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Ok(None) => {}
                Err(error) => matured.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_conclusion_unavailable",
                    "detail":error.to_string().chars().take(256).collect::<String>(),
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
            }
            continue;
        }
        let mut keys = BTreeSet::new();
        let mut min_due = i64::MAX;
        let mut max_due = i64::MIN;
        for mark in &marks {
            if let (Some(key), Some(due)) = (mark["key"].as_str(), mark["due_ms"].as_i64()) {
                keys.insert(key.to_owned());
                min_due = min_due.min(due);
                max_due = max_due.max(due);
            }
        }
        if keys.is_empty() || min_due == i64::MAX {
            continue;
        }
        let declared_exit_delay = item["max_exit_delay_ms"]
            .as_i64()
            .unwrap_or(CANDIDATE_MAX_EXIT_DELAY_MS);
        let quotes = match super::super::super::super::paper_replay::candidate_quotes(
            c.quote_source(),
            c.session,
            &keys,
            min_due,
            max_due.saturating_add(declared_exit_delay),
            10_000,
            now,
        ) {
            Ok(quotes) => quotes,
            Err(_) => {
                matured.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_quote_evidence_unavailable",
                    "activation_authorized":false,
                    "edge_qualified":false
                }));
                continue;
            }
        };
        match experiment_ledger::lifecycle::mature(&c.directory, id, &quotes, now) {
            Ok(receipt) => matured.push(json!({
                "candidate_id":id,
                "state":receipt["state"],
                "marks":receipt["marks"],
                "checks":receipt["checks"],
                "coverage":quotes["coverage"],
                "activation_authorized":false,
                "edge_qualified":false
            })),
            Err(error) => matured.push(json!({
                "candidate_id":id,
                "state":"unavailable",
                "reason":"candidate_maturation_unavailable",
                "detail":error.to_string().chars().take(256).collect::<String>(),
                "activation_authorized":false,
                "edge_qualified":false
            })),
        }
    }
    // A replayed observation whose library row fell outside the bounded
    // library page is still eligible by its own queue row + lifecycle state.
    for task in candidates.iter().filter(|task| {
        task.spec.purpose.is_some()
            && task.status == jobs::TaskState::Succeeded
            && !seen_lifecycles.contains(&task.spec.lifecycle_candidate_id)
    }) {
        let id = task.spec.lifecycle_candidate_id.clone();
        if experiment_ledger::lifecycle::state(&c.directory, &id)
            .ok()
            .flatten()
            .as_deref()
            != Some("replayed")
        {
            continue;
        }
        let result = match queue.as_mut().and_then(|queue| {
            read_verified_candidate_result(queue, &task.spec.task_id, task.spec.revision)
                .ok()
                .flatten()
        }) {
            Some(result) => result,
            _ => {
                matured.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_observation_result_unverified",
                    "activation_authorized":false,
                    "edge_qualified":false
                }));
                continue;
            }
        };
        match queue
            .as_mut()
            .map(|queue| mature_observation(c, cfg, queue, task, &result, now))
            .unwrap_or_else(|| Err("candidate_task_state_unavailable".into()))
        {
            Ok(MaturationDecision::Waiting { due_ms }) => matured.push(json!({
                "candidate_id":id,
                "state":"waiting",
                "due_ms":due_ms,
                "activation_authorized":false,
                "edge_qualified":false
            })),
            Ok(MaturationDecision::PendingCapture {
                due_ms,
                deadline_ms,
                gap,
            }) => matured.push(json!({
                "candidate_id":id,
                "state":"pending_capture",
                "due_ms":due_ms,
                "deadline_ms":deadline_ms,
                "gap":gap,
                "activation_authorized":false,
                "edge_qualified":false
            })),
            Ok(MaturationDecision::Matured(measurement)) => matured.push(json!({
                "candidate_id":id,
                "state":"evaluated",
                "observation":measurement,
                "activation_authorized":false,
                "edge_qualified":false
            })),
            Ok(MaturationDecision::Unknown(measurement)) => matured.push(json!({
                "candidate_id":id,
                "state":"unknown",
                "observation":measurement,
                "activation_authorized":false,
                "edge_qualified":false
            })),
            Err(error) => matured.push(json!({
                "candidate_id":id,
                "state":"unavailable",
                "reason":"candidate_observation_maturation_unavailable",
                "detail":error.to_string().chars().take(256).collect::<String>(),
                "activation_authorized":false,
                "edge_qualified":false
            })),
        }
    }
    // Cross-session continuation: the CURRENT successor matures an ORIGINAL
    // open observation episode from the verified predecessor chain and appends
    // its own continuation receipt. It never writes the predecessor.
    match mature_predecessors(c, cfg, now) {
        Ok(items) => matured.extend(items),
        Err(error) => matured.push(json!({
            "candidate_id":Value::Null,
            "state":"unavailable",
            "reason":"candidate_predecessor_continuation_unavailable",
            "detail":error.to_string().chars().take(256).collect::<String>(),
            "activation_authorized":false,
            "edge_qualified":false
        })),
    }
    Ok(json!({
        "status":if matured.is_empty(){"idle"}else{"matured"},
        "items":matured,
        "scanned":candidates.len(),
        "truncated":scan_truncated,
        "deferred":if scan_truncated {json!(candidates.len())} else {Value::Null},
        "activation_authorized":false
    }))
}
