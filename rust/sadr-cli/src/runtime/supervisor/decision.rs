//! Persistent owner decision start, completion and interruption recovery.
use super::*;

pub(super) fn next_trading_day(c: &Config) -> std::result::Result<String, &'static str> {
    let after = c
        .session
        .succ_opt()
        .ok_or("supervisor_successor_calendar_unavailable")?;
    c.next_sessions(after)
        .map(|(date, _)| date.to_string())
        .map_err(|_| "supervisor_successor_calendar_unavailable")
}

pub(super) fn decision_for(
    turn: &session::Turn,
) -> std::result::Result<Option<super::super::decision_owner::Claim>, &'static str> {
    if turn.trigger != "decision" {
        return if turn.request.get("host_decision").is_some() {
            Err("owner_turn_binding_invalid")
        } else {
            Ok(None)
        };
    }
    let claim: super::super::decision_owner::Claim =
        serde_json::from_value(turn.request["host_decision"].clone())
            .map_err(|_| "owner_turn_metadata_missing")?;
    if claim.version != 1 || claim.turn_id != turn.id || claim.model_id.is_empty() {
        return Err("owner_turn_binding_invalid");
    }
    Ok(Some(claim))
}

#[allow(clippy::too_many_arguments)] // Current financial authority and durable evidence are independent inputs.
pub(super) fn start_decision(
    c: &Config,
    cfg: &PlannerConfig,
    context: &mut super::super::decision_owner::Context<'_>,
    mut work: super::super::decision_owner::Work,
    now: i64,
    summary: &Value,
    hashes: AppliedHashes,
    decides_at_start: u32,
    bounds: &Compaction,
) -> std::result::Result<Worker, &'static str> {
    let (role, path, timeout_ms, selection) = role_and_model(c, cfg)?;
    let (_, template, prompt_version) = session_prompt(cfg, SessionTrigger::Decision)?;
    let log = session::SessionLog::new(&c.directory, &c.session.to_string());
    let turn_id = format!("turn-{}-{}", c.session, log.last_seq().saturating_add(1));
    work.claim.turn_id = turn_id.clone();
    work.claim.model_id = role.model_id.clone();
    work.claim.started_ms = now;
    let notes = session::Notes::new(&c.directory, &c.session.to_string());
    let notes_version = notes
        .pin_waiting(&turn_id, now)
        .map_err(|_| "supervisor_notes_unavailable")?;
    let selection_binding = super::selection_binding(&selection);
    let facts = decision_facts(
        c,
        cfg,
        &work,
        summary,
        &hashes,
        &role,
        selection,
        &prompt_version,
    )?;
    let prompt = render_session_turn(&template, &facts, bounds)?;
    let turn = session::Turn {
        id: turn_id.clone(),
        session: c.session.to_string(),
        trigger: "decision".into(),
        prompt_version: prompt_version.clone(),
        notes_version,
        decides_at_start,
        fills_at_start: context.portfolio.fills.len(),
        started_at_ms: now,
        request: facts.clone(),
    };
    log.append(now, session::Body::Turn { turn })
        .map_err(|_| "supervisor_session_log_failed")?;
    // The turn that rendered these research updates is durable, so they are
    // delivered. A crash before this re-delivers them once.
    let delivered = facts["research_updates"]
        .as_array()
        .map(|updates| {
            updates
                .iter()
                .filter_map(|update| update["message_id"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Err(error) = super::super::lane_queue::mark_delivered(c, &delivered, &turn_id, now) {
        eprintln!("research_updates_delivery_unrecorded reason={error}");
    }
    if let Err(error) = super::super::decision_owner::bind(context, c, cfg, &mut work.claim, now) {
        let reason = super::super::decision_owner::reason(error.as_ref());
        super::super::decision_owner::refuse(context.db, c, &work.claim, &reason)
            .map_err(|_| "owner_refusal_store_failed")?;
        log.append(
            now,
            session::Body::Result {
                result: session::TurnResult {
                    turn_id,
                    finished_at_ms: now,
                    outcome: "refused".into(),
                    reason: Some(reason),
                    reply: String::new(),
                    actions: Vec::new(),
                },
            },
        )
        .map_err(|_| "supervisor_session_log_failed")?;
        return Err("owner_turn_not_started");
    }
    work.bundle["trigger"] = json!({"kind":"decision","reason":"eligible_digest"});
    work.bundle["summary"] = facts;
    Ok(spawn(
        c,
        cfg,
        role,
        path,
        prompt,
        work.bundle,
        "decision",
        now,
        timeout_ms,
        hashes,
        decides_at_start,
        now,
        TurnRef {
            session: c.session.to_string(),
            id: turn_id,
            notes_version,
            prompt_version,
            decision: Some(work.claim),
        },
        RunKind::Decision,
        None,
        *bounds,
        selection_binding,
    ))
}

pub(super) fn complete_decision(
    root: &std::path::Path,
    worker: &Worker,
    plan: &Value,
    now: i64,
    alerts: Option<&Alerts>,
) -> bool {
    let Some(id) = plan["decision_id"].as_str() else {
        return false;
    };
    let action = session::action("paper_plan", id, plan);
    if let Err(error) = session::commit_action(root, &worker.session, &worker.turn_id, &action)
        && error.to_string() != "session_action_already_committed"
    {
        record_failure(
            root,
            &worker.session,
            now,
            "owner_receipt_projection_failed",
        );
        emit_failure(
            alerts,
            &worker.session,
            now,
            "owner_receipt_projection_failed",
        );
        // SQLite already committed. Leave the turn pending for reconciliation;
        // never convert a known plan into a model replay on a file-write error.
        return false;
    }
    let logged = record_result(
        root,
        worker,
        now,
        "ok",
        None,
        plan["raw_output"].as_str().unwrap_or_default(),
        vec![action],
    );
    if logged && reconcile_lifecycle_results(root, &worker.session).is_err() {
        record_failure(
            root,
            &worker.session,
            now,
            "owner_state_reconciliation_failed",
        );
        return false;
    }
    logged
}

pub(super) fn recover_decision(
    c: &Config,
    turn: &session::Turn,
    context: &mut super::super::decision_owner::Context<'_>,
    now: i64,
    alerts: Option<&Alerts>,
) -> std::result::Result<bool, &'static str> {
    let Some(claim) = decision_for(turn)? else {
        return Ok(false);
    };
    let Some(plan) = super::super::decision_owner::committed(context.db, c, &claim)
        .map_err(|_| "owner_receipt_invalid")?
    else {
        return Ok(false);
    };
    let action = session::action(
        "paper_plan",
        plan["decision_id"]
            .as_str()
            .ok_or("owner_receipt_invalid")?,
        &plan,
    );
    if let Err(error) = session::commit_action(&c.directory, &turn.session, &turn.id, &action)
        && error.to_string() != "session_action_already_committed"
    {
        emit_failure(
            alerts,
            &turn.session,
            now,
            "owner_receipt_projection_failed",
        );
        return Err("owner_receipt_projection_failed");
    }
    let log = session::SessionLog::new(&c.directory, &turn.session);
    if log.result_for(&turn.id).is_none() {
        log.append(
            now,
            session::Body::Result {
                result: session::TurnResult {
                    turn_id: turn.id.clone(),
                    finished_at_ms: now,
                    outcome: "recovered_decision_receipt".into(),
                    reason: None,
                    reply: session::bound_reply(plan["raw_output"].as_str().unwrap_or_default()),
                    actions: vec![action],
                },
            },
        )
        .map_err(|_| "supervisor_session_log_failed")?;
        reconcile_lifecycle_results(&c.directory, &turn.session)?;
    }
    Ok(true)
}

pub(super) fn poll_decision(
    c: &Config,
    cfg: &PlannerConfig,
    worker: &Worker,
    context: &mut super::super::decision_owner::Context<'_>,
    now: i64,
    alerts: Option<&Alerts>,
) -> Option<bool> {
    let result = match worker.receiver.try_recv() {
        Err(mpsc::TryRecvError::Empty) => return None,
        Err(mpsc::TryRecvError::Disconnected) => Err("owner_worker_disconnected".to_owned()),
        Ok(result) => {
            // Retain the measured request envelope before any failure
            // classification so the next render bounds the whole request (#224).
            record_request_evidence(&c.directory, worker);
            result
        }
    };
    let claim = worker.decision.as_ref()?;
    let outcome = (|| -> Result<Value> {
        let raw = result.map_err(|reason| -> Box<dyn std::error::Error> { reason.into() })?;
        let turn = session::SessionLog::new(&c.directory, &worker.session)
            .turns()
            .into_iter()
            .find(|t| t.id == worker.turn_id)
            .ok_or("owner_turn_missing")?;
        session::check_notes_version(&c.directory, &worker.session, worker.notes_version)?;
        let current = c
            .planner_config
            .as_deref()
            .ok_or("owner_configuration_unavailable")
            .and_then(|path| {
                PlannerConfig::load_for_session(path, c.session)
                    .map_err(|_| "owner_configuration_unavailable")
            })?;
        let (role, _, _, _) = role_and_model(c, &current)?;
        check_resume_revisions(
            &turn,
            &worker.prompt_version,
            &resume_binding(&current, &role, &worker.prompt_version)?,
        )?;
        if role.model_id != claim.model_id {
            return Err("owner_model_revision_changed".into());
        }
        super::super::decision_owner::commit(context, c, cfg, claim, &raw, now)
    })();
    Some(match outcome {
        Ok(plan) => complete_decision(&c.directory, worker, &plan, now, alerts),
        Err(error) => {
            let code = super::super::decision_owner::reason(error.as_ref());
            if code == "planner_budget_exhausted" {
                park(
                    &c.directory,
                    &worker.session,
                    now,
                    "supervisor_budget_exhausted",
                    cfg.role(Trigger::Supervisor).daily_call_budget,
                );
            }
            if super::super::decision_owner::refuse(context.db, c, claim, &code).is_err() {
                record_failure(
                    &c.directory,
                    &worker.session,
                    now,
                    "owner_refusal_store_failed",
                );
                false
            } else {
                fail_reply(&c.directory, worker, now, &code, alerts)
            }
        }
    })
}
