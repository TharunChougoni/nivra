use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn(
    c: &Config,
    cfg: &PlannerConfig,
    role: planner::ModelRole,
    path: std::path::PathBuf,
    prompt: String,
    bundle: Value,
    reason: &'static str,
    now: i64,
    timeout_ms: u64,
    hashes: AppliedHashes,
    hold_until_ms: i64,
    turn: TurnRef,
    kind: RunKind,
    lifecycle: Option<Lifecycle>,
    bounds: Compaction,
    selection_binding: Option<String>,
) -> Worker {
    let c = c.clone();
    let cfg = cfg.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_thread = Arc::clone(&cancel);
    let (sender, receiver) = mpsc::sync_channel(1);
    let invocation_id = matches!(
        kind,
        RunKind::Decision | RunKind::Review | RunKind::Research
    )
    .then(|| turn.id.clone());
    let evidence = Arc::new(Mutex::new(None));
    let evidence_thread = Arc::clone(&evidence);
    let escaped_prompt_bytes = escaped_prompt_bytes(&prompt);
    let started = std::time::Instant::now();
    let elapsed_started = started;
    thread::spawn(move || {
        if cancel_thread.load(Ordering::Acquire) {
            let _ = sender.send(Err("supervisor_cancelled".into()));
            return;
        }
        // The observation wall is the actual current wall. Duration admission
        // uses a separate monotonic reading so a frozen wall cannot freeze a
        // deadline and a wall backstep cannot fabricate a later UTC time.
        let clock = || millis();
        let elapsed = || elapsed_started.elapsed().as_millis().min(i64::MAX as u128) as i64;
        let guard = planner::CallGuard {
            cancel: Some(cancel_thread.as_ref()),
            deadline: Some(now.saturating_add(timeout_ms as i64)),
            not_before: Some(now),
            configuration: Some((&c, &cfg)),
            journal_required: std::cell::Cell::new(false),
            minimum_prior_calls: 0,
            invocation_id: invocation_id.as_deref(),
            context_hash: None,
            clock: &clock,
            duration: Some(&elapsed),
            started_wall_ms: Some(now),
        };
        let mut backend = Subscription::new(&path);
        let result = super::super::harness::run(
            &c,
            &cfg,
            "supervisor",
            &role,
            &prompt,
            &bundle,
            &mut backend,
            &guard,
        )
        .map_err(|error| error.to_string());
        // The backend retains the bounded provider provenance after the call:
        // request/token sizes and the allowlisted incomplete cause (#224).
        *evidence_thread
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) =
            Some(planner::Backend::provenance(&backend));
        let _ = sender.send(result);
    });
    Worker {
        cancel,
        receiver,
        reason,
        hashes,
        run_first: matches!(reason, "session_open" | "resume" | "change"),
        hold_until_ms,
        session: turn.session,
        turn_id: turn.id,
        notes_version: turn.notes_version,
        prompt_version: turn.prompt_version,
        kind,
        decision: turn.decision,
        lifecycle,
        bounds,
        escaped_prompt_bytes,
        evidence,
        started,
        selection_binding,
    }
}

pub(super) struct TurnRef {
    pub(super) session: String,
    pub(super) id: String,
    pub(super) notes_version: u64,
    pub(super) prompt_version: String,
    pub(super) decision: Option<super::super::decision_owner::Claim>,
}

/// Begin one persistent-session turn. The turn record is durable before the
/// model call starts, so a crash leaves a pending turn the next start can
/// recover or replay (once) instead of silently duplicating work (#141).
#[allow(clippy::too_many_arguments)]
pub(super) fn start(
    c: &Config,
    cfg: &PlannerConfig,
    reason: &'static str,
    now: i64,
    summary: &Value,
    hashes: AppliedHashes,
    hold_until_ms: i64,
    bounds: &Compaction,
) -> std::result::Result<Worker, &'static str> {
    let (role, path, timeout_ms, selection) = role_and_model(c, cfg)?;
    let session = c.session.to_string();
    let trigger = session_trigger(reason);
    let (_system, template, prompt_version) = session_prompt(cfg, trigger)?;
    let binding = resume_binding(cfg, &role, &prompt_version)?;
    let log = session::SessionLog::new(&c.directory, &session);
    let turn_id = format!("turn-{}-{}", session, log.last_seq().saturating_add(1));
    let notes = session::Notes::new(&c.directory, &session);
    let notes_version = notes
        .pin_waiting(&turn_id, now)
        .map_err(|_| "supervisor_notes_unavailable")?;
    let mut facts = summary.clone();
    facts["notes"] = notes.snapshot().prompt_block();
    facts["summary"] = json!(
        log.summary()
            .map(|summary| summary.text)
            .unwrap_or_default()
    );
    facts["recent_turns"] = json!(
        log.recent_turns(6)
            .into_iter()
            .map(|turn| json!({"id":turn.id,"trigger":turn.trigger}))
            .collect::<Vec<_>>()
    );
    session::add_seed_fact(&c.directory, &session, &mut facts)?;
    facts["session_context"] = log.conversation();
    facts["resume_binding_sha256"] = json!(binding);
    let selection_binding = selection_binding(&selection);
    if let Some(selection) = selection {
        facts["model_selection"] = selection;
    }
    facts = super::super::transcript::redact(&facts);
    let prompt = render_session_turn(&template, &facts, bounds)?;
    let turn = session::Turn {
        id: turn_id.clone(),
        session: session.clone(),
        trigger: reason.to_owned(),
        prompt_version: prompt_version.clone(),
        notes_version,
        fills_at_start: summary["fills_today"].as_u64().unwrap_or(0) as usize,
        started_at_ms: now,
        request: facts.clone(),
    };
    log.append(now, session::Body::Turn { turn })
        .map_err(|_| "supervisor_session_log_failed")?;
    let bundle = json!({"trigger":{"kind":reason,"reason":reason},"summary":facts});
    Ok(spawn(
        c,
        cfg,
        role,
        path,
        prompt,
        bundle,
        reason,
        now,
        timeout_ms,
        hashes,
        hold_until_ms,
        TurnRef {
            session,
            id: turn_id,
            notes_version,
            prompt_version,
            decision: None,
        },
        RunKind::Supervisor,
        None,
        *bounds,
        selection_binding,
    ))
}

/// Replay the last turn that never wrote a result. Its action receipts are
/// checked first by the caller; this path re-runs the model with the same turn
/// id, so a recovered action can never be committed twice (#141).
pub(super) fn resume(
    c: &Config,
    cfg: &PlannerConfig,
    turn: &session::Turn,
    now: i64,
    hashes: AppliedHashes,
    hold_until_ms: i64,
    bounds: &Compaction,
) -> std::result::Result<Worker, &'static str> {
    let lifecycle = lifecycle_for(turn)?;
    let decision = decision_for(turn)?;
    let kind = if decision.is_some() {
        RunKind::Decision
    } else {
        lifecycle
            .as_ref()
            .map_or(RunKind::Supervisor, Lifecycle::kind)
    };
    let (role, path, timeout_ms, selection) = role_and_model(c, cfg)?;
    let trigger = session_trigger(&turn.trigger);
    let (_system, template, prompt_version) = session_prompt(cfg, trigger)?;
    let binding = resume_binding(cfg, &role, &prompt_version)?;
    let recorded = turn
        .request
        .get("model_selection")
        .and_then(|value| value["revision"].as_u64())
        .unwrap_or(0);
    let current = selection
        .as_ref()
        .and_then(|value| value["revision"].as_u64())
        .unwrap_or(0);
    if recorded != current {
        // A resumed turn must not silently use a different model revision. Keep
        // the pending turn explicit for the operator instead of replaying it.
        return Err("supervisor_model_revision_changed");
    }
    check_resume_revisions(turn, &prompt_version, &binding)?;
    if let Some(lifecycle) = &lifecycle {
        persist_handoff_seed(&c.directory, lifecycle)?;
    }
    let prompt = render_session_turn(&template, &turn.request, bounds)?;
    let bundle =
        json!({"trigger":{"kind":turn.trigger,"reason":turn.trigger},"summary":turn.request});
    Ok(spawn(
        c,
        cfg,
        role,
        path,
        prompt,
        bundle,
        reason_static(&turn.trigger),
        now,
        timeout_ms,
        hashes,
        hold_until_ms,
        TurnRef {
            session: turn.session.clone(),
            id: turn.id.clone(),
            notes_version: turn.notes_version,
            prompt_version,
            decision,
        },
        kind,
        lifecycle,
        *bounds,
        selection_binding(&selection),
    ))
}

/// Start the host lifecycle turns (compaction summary or end-of-day handoff).
/// They share the session role but never commit directives or orders.
#[allow(clippy::too_many_arguments)]
pub(super) fn start_lifecycle(
    c: &Config,
    cfg: &PlannerConfig,
    kind: RunKind,
    reason: &'static str,
    now: i64,
    summary: &Value,
    hashes: AppliedHashes,
    hold_until_ms: i64,
    bounds: &Compaction,
) -> std::result::Result<Worker, &'static str> {
    let (role, path, timeout_ms, selection) = role_and_model(c, cfg)?;
    let session = c.session.to_string();
    let trigger = if kind == RunKind::Handoff {
        SessionTrigger::Handoff
    } else {
        SessionTrigger::Compaction
    };
    let (_system, template, prompt_version) = session_prompt(cfg, trigger)?;
    let binding = resume_binding(cfg, &role, &prompt_version)?;
    let log = session::SessionLog::new(&c.directory, &session);
    let turn_id = format!("turn-{}-{}", session, log.last_seq().saturating_add(1));
    let notes = session::Notes::new(&c.directory, &session);
    let notes_version = notes
        .pin_waiting(&turn_id, now)
        .map_err(|_| "supervisor_notes_unavailable")?;
    let mut facts = summary.clone();
    facts["notes"] = notes.snapshot().prompt_block();
    facts["summary"] = json!(
        log.summary()
            .map(|summary| summary.text)
            .unwrap_or_default()
    );
    facts["recent_turns"] = json!(
        log.recent_turns(12)
            .into_iter()
            .map(|turn| json!({"id":turn.id,"trigger":turn.trigger}))
            .collect::<Vec<_>>()
    );
    session::add_seed_fact(&c.directory, &session, &mut facts)?;
    facts["session_context"] = log.conversation();
    facts["resume_binding_sha256"] = json!(binding);
    let selection_binding = selection_binding(&selection);
    if let Some(selection) = selection {
        facts["model_selection"] = selection;
    }
    let lifecycle = Lifecycle {
        version: 1,
        hashes: hashes.clone(),
        task: if kind == RunKind::Handoff {
            let target = next_trading_day(c)?;
            let handoff = handoff_lifecycle(
                c,
                cfg,
                HandoffBuildContext {
                    session: session.clone(),
                    target,
                    turn_id: turn_id.clone(),
                    standing_instructions: _system.clone(),
                    facts: facts.clone(),
                },
                &hashes,
                now,
            );
            handoff.task
        } else {
            LifecycleTask::Compaction {
                up_to_seq: log.last_seq().saturating_add(1),
            }
        },
    };
    facts["host_lifecycle"] =
        serde_json::to_value(&lifecycle).map_err(|_| "supervisor_lifecycle_invalid")?;
    facts = super::super::transcript::redact(&facts);
    let prompt = render_lifecycle_turn(&template, &mut facts, bounds)?;
    let turn = session::Turn {
        id: turn_id.clone(),
        session: session.clone(),
        trigger: reason.to_owned(),
        prompt_version: prompt_version.clone(),
        notes_version,
        fills_at_start: summary["fills_today"].as_u64().unwrap_or(0) as usize,
        started_at_ms: now,
        request: facts.clone(),
    };
    log.append(now, session::Body::Turn { turn })
        .map_err(|_| "supervisor_session_log_failed")?;
    let lifecycle: Lifecycle = serde_json::from_value(facts["host_lifecycle"].clone())
        .map_err(|_| "supervisor_lifecycle_invalid")?;
    persist_handoff_seed(&c.directory, &lifecycle)?;
    let bundle = json!({"trigger":{"kind":reason,"reason":reason},"summary":facts});
    Ok(spawn(
        c,
        cfg,
        role,
        path,
        prompt,
        bundle,
        reason,
        now,
        timeout_ms,
        hashes,
        hold_until_ms,
        TurnRef {
            session,
            id: turn_id,
            notes_version,
            prompt_version,
            decision: None,
        },
        kind,
        Some(lifecycle),
        *bounds,
        selection_binding,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn poll(
    root: &std::path::Path,
    worker: &Worker,
    allowlist: &[String],
    limits: &Limits,
    now: i64,
    session_end_ms: i64,
    budget_at_park: u32,
    alerts: Option<&Alerts>,
) -> Option<bool> {
    match worker.receiver.try_recv() {
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => Some(fail_reply(
            root,
            worker,
            now,
            "supervisor_worker_disconnected",
            alerts,
        )),
        Ok(result) => {
            // Retain the measured request envelope before any failure
            // classification so the next render bounds the whole request (#224).
            record_request_evidence(root, worker);
            match result {
                Err(reason) => {
                    // A provider failure is a failed turn, not an abandoned one.
                    // The result clears the pending turn so resume cannot replay
                    // it. Keep the bounded underlying code instead of collapsing
                    // every cause to `supervisor_call_failed` (#162/A0).
                    let code = supervisor_failure_reason(&reason);
                    if reason == "planner_budget_exhausted" {
                        park(root, &worker.session, now, &code, budget_at_park);
                    }
                    Some(fail_reply(root, worker, now, &code, alerts))
                }
                Ok(raw) => Some(handle_reply(
                    root,
                    worker,
                    now,
                    &raw,
                    allowlist,
                    limits,
                    session_end_ms,
                    alerts,
                )),
            }
        }
    }
}

/// Finish a compaction or handoff turn. The host writes the seed and expires
/// non-keep notes; a failed handoff summary never loses keep notes (#144).
fn find_lifecycle_artifact(
    log: &session::SessionLog,
    lifecycle: &Lifecycle,
) -> std::result::Result<Option<session::Body>, &'static str> {
    for record in log.records() {
        match (&lifecycle.task, &record.body) {
            (LifecycleTask::Compaction { up_to_seq }, session::Body::Summary { summary })
                if summary.up_to_seq == *up_to_seq =>
            {
                return Ok(Some(record.body));
            }
            (
                LifecycleTask::Handoff {
                    target_session,
                    seed,
                },
                session::Body::Handoff { handoff },
            ) => {
                if handoff.target_session != *target_session || handoff.seed != *seed {
                    return Err("supervisor_lifecycle_receipt_conflict");
                }
                return Ok(Some(record.body));
            }
            _ => {}
        }
    }
    Ok(None)
}

fn lifecycle_artifact_result(body: &session::Body) -> (String, Option<String>) {
    match body {
        session::Body::Summary { summary } => (summary.text.clone(), None),
        session::Body::Handoff { handoff } => (
            handoff.text.clone().unwrap_or_default(),
            handoff.failure.clone(),
        ),
        _ => (String::new(), Some("supervisor_lifecycle_invalid".into())),
    }
}

pub(super) fn lifecycle_action(turn_id: &str, body: &session::Body) -> session::Action {
    let kind = if matches!(body, session::Body::Summary { .. }) {
        "compaction"
    } else {
        "handoff"
    };
    session::action(
        kind,
        turn_id,
        &serde_json::to_value(body).unwrap_or_default(),
    )
}

pub(super) fn persist_lifecycle_artifact(
    root: &std::path::Path,
    worker: &Worker,
    now: i64,
    text: Option<&str>,
    failure: Option<&str>,
) -> std::result::Result<session::Body, &'static str> {
    let lifecycle = worker
        .lifecycle
        .as_ref()
        .ok_or("supervisor_lifecycle_missing")?;
    if lifecycle.version != 1 || lifecycle.kind() != worker.kind {
        return Err("supervisor_lifecycle_invalid");
    }
    persist_lifecycle_artifact_for(root, &worker.session, lifecycle, now, text, failure)
}

pub(super) fn persist_lifecycle_artifact_for(
    root: &std::path::Path,
    session: &str,
    lifecycle: &Lifecycle,
    now: i64,
    text: Option<&str>,
    failure: Option<&str>,
) -> std::result::Result<session::Body, &'static str> {
    let log = session::SessionLog::new(root, session);
    if let Some(body) = find_lifecycle_artifact(&log, lifecycle)? {
        return Ok(body);
    }
    let body = match &lifecycle.task {
        LifecycleTask::Compaction { up_to_seq } => {
            if failure.is_some() || *up_to_seq == 0 || *up_to_seq > log.last_seq() {
                return Err("supervisor_lifecycle_prefix_invalid");
            }
            session::Body::Summary {
                summary: session::Summary {
                    up_to_seq: *up_to_seq,
                    text: session::bound_reply(text.unwrap_or_default()),
                    created_at_ms: now,
                },
            }
        }
        LifecycleTask::Handoff {
            target_session,
            seed,
        } => {
            let seed_failure = persist_handoff_seed(root, lifecycle).err();
            session::Body::Handoff {
                handoff: session::Handoff {
                    target_session: target_session.clone(),
                    text: text.map(session::bound_reply),
                    failure: seed_failure.or(failure).map(str::to_owned),
                    seed: seed.clone(),
                    created_at_ms: now,
                },
            }
        }
    };
    log.append(now, body.clone())
        .map_err(|_| "supervisor_session_log_failed")?;
    Ok(body)
}

/// A parked allowance prevents inference, never the host's daily seed. This
/// path records an explicit failed handoff and does not resolve credentials,
/// reset usage or run a model. Existing pending work is kept intact.
pub(super) fn host_handoff_failure(
    c: &Config,
    cfg: &PlannerConfig,
    facts: &Value,
    now: i64,
    hashes: &AppliedHashes,
    failure: &str,
) -> std::result::Result<(), &'static str> {
    let session = c.session.to_string();
    let log = session::SessionLog::new(&c.directory, &session);
    if log.handoff().is_some() {
        return Ok(());
    }
    let turn = if let Some(pending) = log.pending() {
        if pending.trigger != "handoff" {
            return Err("supervisor_pending_at_handoff");
        }
        pending
    } else {
        let target = next_trading_day(c)?;
        let (standing_instructions, _, _) = session_prompt(cfg, SessionTrigger::Handoff)?;
        let turn_id = format!("turn-{session}-{}", log.last_seq().saturating_add(1));
        let lifecycle = handoff_lifecycle(
            c,
            cfg,
            HandoffBuildContext {
                session: session.clone(),
                target,
                turn_id: turn_id.clone(),
                standing_instructions,
                facts: facts.clone(),
            },
            hashes,
            now,
        );
        let turn = session::Turn {
            id: turn_id,
            session: session.clone(),
            trigger: "handoff".into(),
            prompt_version: "host_handoff_v1".into(),
            notes_version: session::Notes::new(&c.directory, &session).version(),
            fills_at_start: facts["fills_today"].as_u64().unwrap_or(0) as usize,
            started_at_ms: now,
            request: super::super::transcript::redact(
                &json!({"host_lifecycle":lifecycle,"host_only":true}),
            ),
        };
        log.append(now, session::Body::Turn { turn: turn.clone() })
            .map_err(|_| "supervisor_session_log_failed")?;
        turn
    };
    let lifecycle = lifecycle_for(&turn)?.ok_or("supervisor_lifecycle_missing")?;
    persist_lifecycle_artifact_for(&c.directory, &session, &lifecycle, now, None, Some(failure))?;
    recover_lifecycle(&c.directory, &turn, now)?;
    Ok(())
}

/// Reconcile a lifecycle effect committed before a crash interrupted its result
/// append. The summary prefix or handoff seed is the durable receipt; never
/// run the model again or append that effect a second time.
pub(super) fn recover_lifecycle(
    root: &std::path::Path,
    turn: &session::Turn,
    now: i64,
) -> std::result::Result<bool, &'static str> {
    let Some(lifecycle) = lifecycle_for(turn)? else {
        return Ok(false);
    };
    let log = session::SessionLog::new(root, &turn.session);
    if log.result_for(&turn.id).is_some() {
        reconcile_lifecycle_results(root, &turn.session)?;
        return Ok(true);
    }
    if let LifecycleTask::Compaction { up_to_seq } = lifecycle.task
        && !log.records().iter().any(|record| record.seq == up_to_seq
            && matches!(&record.body, session::Body::Turn { turn: saved } if saved.id == turn.id)) {
        return Err("supervisor_lifecycle_prefix_invalid");
    }
    let Some(body) = find_lifecycle_artifact(&log, &lifecycle)? else {
        return Ok(false);
    };
    let (reply, reason) = lifecycle_artifact_result(&body);
    if reason.is_none() && lifecycle.kind() == RunKind::Handoff {
        persist_handoff_seed(root, &lifecycle)?;
        session::Notes::new(root, &turn.session)
            .expire_at_close(now)
            .map_err(|_| "supervisor_note_expiry_failed")?;
    }
    let result = session::TurnResult {
        turn_id: turn.id.clone(),
        finished_at_ms: now,
        outcome: "recovered_lifecycle_receipt".into(),
        reason: reason.clone(),
        reply,
        actions: vec![lifecycle_action(&turn.id, &body)],
    };
    log.append(now, session::Body::Result { result })
        .map_err(|_| "supervisor_session_log_failed")?;
    reconcile_lifecycle_results(root, &turn.session)?;
    Ok(true)
}

pub(super) fn finish_lifecycle(
    root: &std::path::Path,
    worker: &Worker,
    now: i64,
    text: &str,
    alerts: Option<&Alerts>,
) -> bool {
    let artifact = persist_lifecycle_artifact(root, worker, now, Some(text), None);
    let (ok, reason, actions) = match artifact {
        Ok(body) => {
            let (_, failure) = lifecycle_artifact_result(&body);
            (
                failure.is_none(),
                failure,
                vec![lifecycle_action(&worker.turn_id, &body)],
            )
        }
        Err(reason) => (false, Some(reason.to_owned()), Vec::new()),
    };
    if ok
        && worker.kind == RunKind::Handoff
        && session::Notes::new(root, &worker.session)
            .expire_at_close(now)
            .is_err()
    {
        return fail_reply(root, worker, now, "supervisor_note_expiry_failed", alerts);
    }
    let logged = record_result(
        root,
        worker,
        now,
        if ok { "ok" } else { "failed" },
        reason.as_deref(),
        text,
        actions,
    );
    if logged && (worker.lifecycle.is_some() || worker.decision.is_some()) {
        if let Err(error) = reconcile_lifecycle_results(root, &worker.session) {
            eprintln!("supervisor_lifecycle_reconcile_failed reason={error}");
            record_state_error(root, &worker.session, error);
        }
    } else if worker.lifecycle.is_none() && worker.decision.is_none() {
        finish(
            root,
            &worker.session,
            worker.kind,
            worker.reason,
            now,
            ok && logged,
            &worker.hashes,
            worker.selection_binding.as_deref(),
        );
    }
    if !ok || !logged {
        let code = reason.as_deref().unwrap_or("supervisor_session_log_failed");
        record_failure(root, &worker.session, now, code);
        if worker.kind == RunKind::Compaction && (!logged || worker.lifecycle.is_none()) {
            record_compaction_failure(root, &worker.session, now, code);
        }
        emit_failure(alerts, &worker.session, now, code);
    } else if worker.kind == RunKind::Compaction && worker.lifecycle.is_none() {
        // A successful summary is the recovery evidence for an earlier failed
        // compaction; the log is append-only either way.
        clear_compaction_failure(root, &worker.session);
    }
    ok && logged
}
