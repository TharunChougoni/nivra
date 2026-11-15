//! Owner admission, polling and supervisor task precedence.
use super::*;

pub(crate) struct Tick<'a> {
    pub now: i64,
    pub minute: u32,
    pub in_session: bool,
    pub fill_count: usize,
    pub allowlist: &'a [String],
    pub session_end_ms: i64,
    /// Current open positions for the handoff seed. Observations only.
    pub positions: Value,
}

#[cfg(test)]
pub(crate) fn consider(
    c: &Config,
    cfg: &PlannerConfig,
    worker: &mut Option<Worker>,
    aside: &mut Option<Worker>,
    tick: &Tick<'_>,
    alerts: Option<&Alerts>,
) {
    consider_with_owner(c, cfg, worker, aside, tick, alerts, None)
}

pub(crate) fn consider_owner(
    c: &Config,
    cfg: &PlannerConfig,
    worker: &mut Option<Worker>,
    aside: &mut Option<Worker>,
    tick: &Tick<'_>,
    alerts: Option<&Alerts>,
    context: &mut super::super::decision_owner::Context<'_>,
) {
    consider_with_owner(c, cfg, worker, aside, tick, alerts, Some(context))
}

fn consider_with_owner(
    c: &Config,
    cfg: &PlannerConfig,
    worker: &mut Option<Worker>,
    aside: &mut Option<Worker>,
    tick: &Tick<'_>,
    alerts: Option<&Alerts>,
    mut owner: Option<&mut super::super::decision_owner::Context<'_>>,
) {
    let configured = cfg
        .orchestrator
        .as_ref()
        .is_some_and(|orchestrator| orchestrator.triggers.contains_key(&Trigger::Supervisor));
    if !configured && worker.is_none() && aside.is_none() {
        return;
    }
    let session = c.session.to_string();
    match reconcile_lifecycle_results(&c.directory, &session) {
        Ok(true) => return,
        Ok(false) => {}
        Err(reason) => {
            eprintln!("supervisor_lifecycle_reconcile_failed reason={reason}");
            record_state_error(&c.directory, &session, reason);
            record_skip(&c.directory, &session, tick.now, reason);
            return;
        }
    }
    if let Some(running) = aside.as_ref()
        && poll_aside(&c.directory, running, tick.now).is_some()
    {
        *aside = None;
    }
    if let Some(running) = worker.as_ref() {
        let limits = settings_for(cfg)
            .map(|(_, limits, _)| limits)
            .unwrap_or_default();
        let finished = if running.kind == RunKind::Decision {
            owner
                .as_deref_mut()
                .and_then(|context| poll_decision(c, cfg, running, context, tick.now, alerts))
        } else {
            poll(
                &c.directory,
                running,
                tick.allowlist,
                &limits,
                tick.now,
                tick.session_end_ms,
                cfg.role(Trigger::Supervisor).daily_call_budget,
                alerts,
            )
        };
        if finished.is_some() {
            *worker = None;
        }
    }
    if worker.is_some() {
        if aside.is_none() && aside_request(&c.directory, &session).is_some() {
            record_skip(&c.directory, &session, tick.now, "aside_worker_busy");
        }
        return;
    }
    let Ok((wake, _, mut compaction)) = settings_for(cfg) else {
        record_skip(
            &c.directory,
            &session,
            tick.now,
            "supervisor_settings_invalid",
        );
        return;
    };
    let enabled = cfg.role(Trigger::Supervisor).enabled;
    if !enabled {
        record_skip(&c.directory, &session, tick.now, "supervisor_disabled");
        return;
    }
    let mut state = match load(&c.directory, &session) {
        Ok(state) => state,
        Err(reason) => {
            // Never start a fresh session over an unreadable state file: that
            // would lose the handoff counts and could duplicate work (#156).
            eprintln!("supervisor_state_refused reason={reason}");
            record_state_error(&c.directory, &session, reason);
            record_failure(&c.directory, &session, tick.now, reason);
            emit_failure(alerts, &session, tick.now, reason);
            return;
        }
    };
    // Optional owner failure backoff: only an actual owner decision/supervisor
    // failure under the same model-selection profile/revision defers new owner
    // model turns. Host lifecycle and protection continue.
    let current_selection = current_selection_binding(c, cfg);
    let backoff_until =
        super::wake::failure_backoff_until(&state, &wake, tick.now, current_selection.as_deref());
    if backoff_until.is_some() {
        record_skip(
            &c.directory,
            &session,
            tick.now,
            "supervisor_failure_backoff",
        );
    }
    // Bound the whole provider request, not only the rendered prompt, using the
    // envelope measured from the last provider call (#224).
    compaction.request_envelope_bytes = state.request_envelope_bytes;
    if tick.in_session
        && enabled
        && let Some(parked) = state.parked.clone()
        && planner::require_usage(c).is_ok()
    {
        let role = cfg.role(Trigger::Supervisor);
        let charged_usage = planner::usage(c)
            .ok()
            .and_then(|usage| usage["supervisor"].as_u64())
            .and_then(|used| usize::try_from(used).ok());
        if charged_usage.is_some_and(|used| {
            allowance_recovery_allowed(&parked, role.daily_call_budget, used, true)
        }) {
            match with_state(&c.directory, &session, |current| {
                if current.parked.as_ref() == Some(&parked) {
                    current.parked = None;
                    true
                } else {
                    false
                }
            }) {
                Ok(true) => {
                    eprintln!(
                        "supervisor_unparked reason=allowance_increased charged_usage={}",
                        charged_usage.unwrap_or_default()
                    );
                    state = match load(&c.directory, &session) {
                        Ok(state) => state,
                        Err(reason) => {
                            record_state_error(&c.directory, &session, reason);
                            return;
                        }
                    };
                }
                Ok(false) => {}
                Err(reason) => {
                    record_state_error(&c.directory, &session, reason);
                    record_skip(&c.directory, &session, tick.now, reason);
                    return;
                }
            }
        }
    }
    let input = Inputs {
        minute: tick.minute,
        in_session: tick.in_session,
        fill_count: tick.fill_count,
        enabled,
        in_flight: false,
        budget_left: state.parked.is_none(),
    };
    let hashes = AppliedHashes::current(cfg);
    if let Some(pending) = session::SessionLog::new(&c.directory, &session).pending() {
        if pending.trigger == "research" || pending.request["owner_task"]["kind"] == "research" {
            // Only a pre-lane build wrote research into the trading log. Close
            // it without replay; new research lives in its own lane (#237).
            if let Err(error) = research::recover_legacy(&c.directory, &pending, tick.now) {
                record_skip(
                    &c.directory,
                    &session,
                    tick.now,
                    &super::super::failure_code(error.as_ref()),
                );
            }
            return;
        }
        if pending.trigger == "review" || pending.request.get("owner_task").is_some() {
            if let Err(error) = review::recover(&c.directory, &pending, tick.now) {
                record_skip(
                    &c.directory,
                    &session,
                    tick.now,
                    &super::super::failure_code(error.as_ref()),
                );
            }
            return;
        }
    }
    // An exhausted charged allowance parks every model turn for this session.
    // Host protection and the trading loop continue; no repeated failed run.
    if state.parked.is_some() {
        if let Some(pending) = session::SessionLog::new(&c.directory, &session).pending() {
            match recover_lifecycle(&c.directory, &pending, tick.now) {
                Ok(true) => return,
                Ok(false) => {}
                Err(reason) => {
                    record_skip(&c.directory, &session, tick.now, reason);
                    return;
                }
            }
        }
        if tick.now >= tick.session_end_ms && tick.now < tick.session_end_ms + 43_200_000 {
            let facts = summary_for(&c.directory, &session, "handoff", tick, &state, &hashes);
            if let Err(reason) = host_handoff_failure(
                c,
                cfg,
                &facts,
                tick.now,
                &hashes,
                "supervisor_parked_budget",
            ) {
                record_skip(&c.directory, &session, tick.now, reason);
            }
        }
        if aside.is_none() && aside_request(&c.directory, &session).is_some() {
            record_skip(&c.directory, &session, tick.now, "aside_parked");
        }
        record_skip(&c.directory, &session, tick.now, "supervisor_parked_budget");
        return;
    }
    if backoff_until.is_some() && aside.is_none() && aside_request(&c.directory, &session).is_some()
    {
        record_skip(&c.directory, &session, tick.now, "aside_deferred_backoff");
    }
    if backoff_until.is_none()
        && aside.is_none()
        && let Some(request) = aside_request(&c.directory, &session)
    {
        let summary = summary_for(&c.directory, &session, "aside", tick, &state, &hashes);
        match start_aside(c, cfg, &request, tick.now, &summary) {
            Ok(started) => *aside = Some(started),
            Err(reason) => record_skip(&c.directory, &session, tick.now, reason),
        }
    }
    // A crash or deploy can leave one turn without its result. Recover a
    // committed action receipt without a second model call; otherwise replay
    // the turn with the same id (#141).
    let log = session::SessionLog::new(&c.directory, &session);
    if let Some(pending) = log.pending() {
        if pending.trigger == "decision" {
            let Some(context) = owner.as_deref_mut() else {
                record_skip(
                    &c.directory,
                    &session,
                    tick.now,
                    "owner_context_unavailable",
                );
                return;
            };
            match recover_decision(c, &pending, context, tick.now, alerts) {
                Ok(true) => return,
                Ok(false) => {}
                Err(reason) => {
                    record_skip(&c.directory, &session, tick.now, reason);
                    return;
                }
            }
            if !session::actions_for(&c.directory, &session, &pending.id).is_empty() {
                record_skip(&c.directory, &session, tick.now, "owner_receipt_conflict");
                return;
            }
            let claim = match decision_for(&pending) {
                Ok(Some(claim)) => claim,
                _ => {
                    record_skip(
                        &c.directory,
                        &session,
                        tick.now,
                        "owner_turn_binding_invalid",
                    );
                    return;
                }
            };
            let valid =
                super::super::decision_owner::validate_running(context, c, cfg, &claim, tick.now)
                    .and_then(|()| {
                        session::check_notes_version(&c.directory, &session, pending.notes_version)
                    });
            if let Err(error) = valid {
                let reason = super::super::decision_owner::reason(error.as_ref());
                if super::super::decision_owner::refuse(context.db, c, &claim, &reason).is_err() {
                    record_skip(
                        &c.directory,
                        &session,
                        tick.now,
                        "owner_reconciliation_required",
                    );
                    return;
                }
                let result = session::TurnResult {
                    turn_id: pending.id.clone(),
                    finished_at_ms: tick.now,
                    outcome: "refused".into(),
                    reason: Some(reason),
                    reply: String::new(),
                    actions: Vec::new(),
                };
                if log
                    .append(tick.now, session::Body::Result { result })
                    .is_err()
                {
                    record_skip(
                        &c.directory,
                        &session,
                        tick.now,
                        "supervisor_session_log_failed",
                    );
                }
                return;
            }
            // No in-process worker owns this valid running claim: the previous
            // controller died after sending the turn or before recording its
            // result. Never resend an ambiguous model request. Refuse the work
            // with its original evidence and record an explicit interruption.
            let reason = "owner_decision_interrupted";
            if super::super::decision_owner::refuse(context.db, c, &claim, reason).is_err() {
                record_skip(
                    &c.directory,
                    &session,
                    tick.now,
                    "owner_reconciliation_required",
                );
                return;
            }
            let result = session::TurnResult {
                turn_id: pending.id.clone(),
                finished_at_ms: tick.now,
                outcome: "interrupted".into(),
                reason: Some(reason.into()),
                reply: String::new(),
                actions: Vec::new(),
            };
            if log
                .append(tick.now, session::Body::Result { result })
                .is_err()
            {
                record_skip(
                    &c.directory,
                    &session,
                    tick.now,
                    "supervisor_session_log_failed",
                );
            }
            return;
        }
        match recover_lifecycle(&c.directory, &pending, tick.now) {
            Ok(true) => return,
            Ok(false) => {}
            Err(reason) => {
                record_skip(&c.directory, &session, tick.now, reason);
                return;
            }
        }
        let actions = session::actions_for(&c.directory, &session, &pending.id);
        if !actions.is_empty() {
            let result = session::TurnResult {
                turn_id: pending.id.clone(),
                finished_at_ms: tick.now,
                outcome: "recovered_action_receipt".into(),
                reason: None,
                reply: String::new(),
                actions,
            };
            let ok = log
                .append(tick.now, session::Body::Result { result })
                .is_ok();
            // If the append fails, the failure belongs to the selection recorded
            // on the pending turn, not to whichever profile is current now.
            let failure_binding = super::recorded_selection_binding(&pending);
            finish(
                &c.directory,
                &session,
                RunKind::Supervisor,
                "resume",
                tick.now,
                ok,
                &hashes,
                pending.decides_at_start,
                failure_binding.as_deref(),
            );
            if !ok {
                record_failure(
                    &c.directory,
                    &session,
                    tick.now,
                    "supervisor_session_log_failed",
                );
                emit_failure(alerts, &session, tick.now, "supervisor_session_log_failed");
            }
            return;
        }
        if tick.in_session
            || (pending.trigger == "handoff"
                && tick.now >= tick.session_end_ms
                && tick.now < tick.session_end_ms + 43_200_000)
        {
            match resume(
                c,
                cfg,
                &pending,
                tick.now,
                hashes.clone(),
                tick.now.saturating_add(wake.run_first.hold_luna_ms.max(0)),
                &compaction,
            ) {
                Ok(started) => {
                    *worker = Some(started);
                    return;
                }
                Err(reason) => {
                    record_skip(&c.directory, &session, tick.now, reason);
                }
            }
        }
    }
    // A turn without its result, including one whose recorded model revision
    // changed, is explicit pending work: never start or switch over it. The
    // operator reconciles it; no automatic replay with another model.
    if log.pending().is_some() {
        return;
    }
    // #224: a provider-incomplete turn on a near-cap request is context
    // exhaustion. Compact with the measured full-request envelope and retry
    // once in this cycle; after the bounded retries, back off instead of
    // repeating the same oversized request every cycle.
    if tick.in_session
        && let Some(exhaustion) = state.context_exhaustion.as_ref()
    {
        let turns = log.turns_since_summary();
        if !compacts_on_refusal(
            "supervisor_context_exhausted",
            true,
            worker.is_none(),
            turns,
        ) {
            // Nothing uncompacted remains to omit: no retry can shrink the
            // request, so the reason stays explicit instead of looping.
            record_skip(
                &c.directory,
                &session,
                tick.now,
                "supervisor_context_exhausted_no_history",
            );
        } else if context_recovery_due(exhaustion, &compaction, tick.now) {
            // One bounded attempt per tick: a failed start is counted durably
            // and the next tick backs off instead of retrying immediately.
            let _ = recover_context_exhaustion(c, cfg, tick, &state, &hashes, worker, &compaction);
            return;
        } else {
            record_skip(
                &c.directory,
                &session,
                tick.now,
                "supervisor_context_exhausted_backoff",
            );
        }
    }
    // A requested model switch takes effect only at a clean turn boundary. The
    // receipt is durable before the next turn records it; a policy-invalid
    // request is refused and the last valid active model is retained.
    let settled = owner.as_deref_mut().is_none_or(|context| {
        super::super::decision_owner::actions_settled(context.db, c, tick.now).unwrap_or(false)
    });
    match if settled {
        super::super::model_selection::promote(c, cfg, tick.now)
    } else {
        Ok(None)
    } {
        Ok(Some(receipt)) => {
            eprintln!(
                "model_selection_{} revision={} profile={}",
                receipt["status"].as_str().unwrap_or("unknown"),
                receipt["revision"].as_u64().unwrap_or_default(),
                receipt["active"]["profile"].as_str().unwrap_or("-")
            );
        }
        Ok(None) => {}
        Err(reason) => record_skip(&c.directory, &session, tick.now, reason),
    }
    // A promotion at this boundary may have changed the active selection. The
    // backoff belongs to the failed selection, so re-evaluate it before any
    // owner model turn is admitted.
    let current_selection = current_selection_binding(c, cfg);
    let backoff_until =
        super::wake::failure_backoff_until(&state, &wake, tick.now, current_selection.as_deref());
    if backoff_until.is_none()
        && aside.is_none()
        && let Some(context) = owner.as_deref()
    {
        match review::consider(c, cfg, context, tick, &state, &hashes, &compaction) {
            Ok(Some(started)) => {
                *worker = Some(started);
                return;
            }
            Ok(None) => {}
            Err(error) => record_skip(
                &c.directory,
                &session,
                tick.now,
                &super::super::failure_code(error.as_ref()),
            ),
        }
    }
    // Host lifecycle turns. Compaction keeps the log append-only; handoff runs
    // once per close and writes tomorrow's seed before non-keep notes expire.
    let cooled = state
        .last_run_ms
        .is_none_or(|last| tick.now.saturating_sub(last) >= wake.min_interval_ms);
    if cooled
        && tick.now >= tick.session_end_ms
        && tick.now < tick.session_end_ms + 43_200_000
        && log.handoff().is_none()
    {
        let summary = summary_for(&c.directory, &session, "handoff", tick, &state, &hashes);
        match start_lifecycle(
            c,
            cfg,
            RunKind::Handoff,
            "handoff",
            tick.now,
            &summary,
            hashes.clone(),
            state.decides,
            tick.now,
            &compaction,
        ) {
            Ok(started) => {
                *worker = Some(started);
                return;
            }
            Err(reason) => {
                record_skip(&c.directory, &session, tick.now, reason);
                if let Err(host_reason) =
                    host_handoff_failure(c, cfg, &summary, tick.now, &hashes, reason)
                {
                    record_skip(&c.directory, &session, tick.now, host_reason);
                }
            }
        }
    }
    // The legacy turn count remains the default; a configured measured context
    // ceiling can request compaction earlier. The log is never rewritten. The
    // measured ceiling counts the committed conversation only, so current
    // decision facts can still push a request over the cap (#219, #221).
    let turns = log.turns_since_summary();
    let compact_now = compaction_due(turns, log.context_bytes(), &compaction);
    let mut compacted = false;
    if cooled && tick.in_session && compact_now {
        if start_compaction(c, cfg, tick, &state, &hashes, worker, &compaction) {
            return;
        }
        compacted = true;
    }
    // Local tool preflight errors cannot heal through another identical model
    // call. Lifecycle and host protection continue; retain the original signal.
    if state.tool_contract_blocked(&hashes) {
        record_skip(
            &c.directory,
            &session,
            tick.now,
            "supervisor_tool_contract_blocked",
        );
        return;
    }
    if let Some(context) = owner {
        if backoff_until.is_some() {
            return;
        }
        match super::super::decision_owner::queued(context, c, cfg, tick.now) {
            Ok(Some(work)) => {
                let summary =
                    summary_for(&c.directory, &session, "decision", tick, &state, &hashes);
                match start_decision(
                    c,
                    cfg,
                    context,
                    work,
                    tick.now,
                    &summary,
                    hashes.clone(),
                    state.decides,
                    &compaction,
                ) {
                    Ok(started) => *worker = Some(started),
                    Err(reason) => {
                        record_skip(&c.directory, &session, tick.now, reason);
                        // A history compaction this cycle already went over the
                        // cap. Replaying history with omitted exchanges would
                        // record a different turn than the one that was
                        // rendered, so the decision waits for the next cycle.
                        let reason = if compacted && reason == "supervisor_context_limit" {
                            "supervisor_context_limit_compacted"
                        } else {
                            reason
                        };
                        compact_after_context_limit(
                            c,
                            cfg,
                            tick,
                            &state,
                            &hashes,
                            worker,
                            reason,
                            &compaction,
                        );
                    }
                }
            }
            Ok(None) => {
                // No queued decision: the configured session agenda may run
                // its pre-open or pre-close owner turn (#229). A queued
                // decision always goes first.
                let agenda = owner_tasks::project_agenda(&owner_tasks::AgendaInput {
                    settings: cfg.owner_tasks.as_ref().and_then(|tasks| tasks.agenda),
                    in_session: tick.in_session,
                    book_runnable: !context.portfolio.halted && !context.portfolio.stopped,
                    minute: tick.minute,
                    open_minute: wake.session_open_minute,
                    pre_close_minute: wake.pre_close_minute,
                    open_done: state.open_fired,
                    pre_close_done: state.pre_close_fired,
                    cooled,
                    budget_left: input.budget_left,
                });
                if let owner_tasks::AgendaState::Due(turn) = agenda {
                    let reason = turn.reason();
                    let summary =
                        summary_for(&c.directory, &session, reason, tick, &state, &hashes);
                    match start(
                        c,
                        cfg,
                        reason,
                        tick.now,
                        &summary,
                        hashes.clone(),
                        state.decides,
                        tick.now,
                        &compaction,
                    ) {
                        Ok(started) => *worker = Some(started),
                        Err(reason) => {
                            record_skip(&c.directory, &session, tick.now, reason);
                            compact_after_context_limit(
                                c,
                                cfg,
                                tick,
                                &state,
                                &hashes,
                                worker,
                                reason,
                                &compaction,
                            );
                        }
                    }
                }
            }
            Err(error) => record_skip(
                &c.directory,
                &session,
                tick.now,
                &super::super::decision_owner::reason(error.as_ref()),
            ),
        }
        return;
    }
    if backoff_until.is_some() {
        return;
    }
    let Some(reason) = due(&state, &wake, tick.now, &input, &hashes) else {
        return;
    };
    let summary = summary_for(&c.directory, &session, reason, tick, &state, &hashes);
    let run_first = matches!(reason, "session_open" | "resume" | "change");
    let hold_until_ms = if run_first {
        tick.now.saturating_add(wake.run_first.hold_luna_ms.max(0))
    } else {
        tick.now
    };
    let decides_at_start = state.decides;
    match start(
        c,
        cfg,
        reason,
        tick.now,
        &summary,
        hashes.clone(),
        decides_at_start,
        hold_until_ms,
        &compaction,
    ) {
        Ok(started) => {
            if reason == "post_fill" {
                let _ = with_state(&c.directory, &session, |state| {
                    state.last_fill_count = tick.fill_count;
                });
            }
            if reason == "change" {
                // Debounce from the attempt, so a burst coalesces even when the
                // first change run fails and the failure streak retries it.
                let _ = with_state(&c.directory, &session, |state| {
                    state.change_last_ms = Some(tick.now);
                });
            }
            *worker = Some(started);
        }
        Err(reason) => {
            record_skip(&c.directory, &session, tick.now, reason);
            compact_after_context_limit(c, cfg, tick, &state, &hashes, worker, reason, &compaction);
        }
    }
}

/// Start a host compaction turn; a refusal is recorded as a compaction failure.
/// While the context-exhaustion backoff is active, another compaction would
/// repeat the same oversized request, so it is skipped with a clear reason.
fn start_compaction(
    c: &Config,
    cfg: &PlannerConfig,
    tick: &Tick<'_>,
    state: &State,
    hashes: &AppliedHashes,
    worker: &mut Option<Worker>,
    bounds: &Compaction,
) -> bool {
    if context_backoff_active(state, bounds, tick.now) {
        record_skip(
            &c.directory,
            &c.session.to_string(),
            tick.now,
            "supervisor_context_exhausted_backoff",
        );
        return false;
    }
    start_compaction_turn(c, cfg, tick, state, hashes, worker, bounds)
}

fn start_compaction_turn(
    c: &Config,
    cfg: &PlannerConfig,
    tick: &Tick<'_>,
    state: &State,
    hashes: &AppliedHashes,
    worker: &mut Option<Worker>,
    bounds: &Compaction,
) -> bool {
    let session = c.session.to_string();
    let summary = summary_for(&c.directory, &session, "compaction", tick, state, hashes);
    match start_lifecycle(
        c,
        cfg,
        RunKind::Compaction,
        "compaction",
        tick.now,
        &summary,
        hashes.clone(),
        state.decides,
        tick.now,
        bounds,
    ) {
        Ok(started) => {
            *worker = Some(started);
            true
        }
        Err(reason) => {
            record_skip(&c.directory, &session, tick.now, reason);
            record_compaction_failure(&c.directory, &session, tick.now, reason);
            false
        }
    }
}

/// One bounded in-cycle compaction retry after a context-exhaustion failure.
/// The attempt is counted durably before the call so a crash or a second tick
/// cannot repeat it indefinitely (#224).
fn recover_context_exhaustion(
    c: &Config,
    cfg: &PlannerConfig,
    tick: &Tick<'_>,
    state: &State,
    hashes: &AppliedHashes,
    worker: &mut Option<Worker>,
    bounds: &Compaction,
) -> bool {
    let session = c.session.to_string();
    let counted = with_state(&c.directory, &session, |state| {
        let Some(current) = state.context_exhaustion.as_mut() else {
            return false;
        };
        if tick.now.saturating_sub(current.at_ms) >= bounds.context_backoff_ms {
            // A new bounded window after the backoff expired.
            current.retries = 0;
        }
        current.retries = current.retries.saturating_add(1);
        current.at_ms = tick.now;
        true
    })
    .unwrap_or(false);
    if !counted {
        return false;
    }
    if let Some(exhaustion) = state.context_exhaustion.as_ref() {
        eprintln!(
            "supervisor_context_recovery reason={} retries={} request_bytes={:?}",
            exhaustion.reason,
            exhaustion.retries.saturating_add(1),
            exhaustion.request_bytes
        );
    }
    start_compaction_turn(c, cfg, tick, state, hashes, worker, bounds)
}

/// One in-cycle retry while the count is under the limit, or a fresh attempt
/// once the bounded backoff has expired.
pub(super) fn context_recovery_due(
    exhaustion: &ContextExhaustion,
    bounds: &Compaction,
    now: i64,
) -> bool {
    exhaustion.retries < bounds.context_retry_limit
        || now.saturating_sub(exhaustion.at_ms) >= bounds.context_backoff_ms
}

/// After the bounded retries, no compaction is started until the backoff
/// expires; the skip reason stays visible in state.
pub(super) fn context_backoff_active(state: &State, bounds: &Compaction, now: i64) -> bool {
    state.context_exhaustion.as_ref().is_some_and(|exhaustion| {
        exhaustion.retries >= bounds.context_retry_limit
            && now.saturating_sub(exhaustion.at_ms) < bounds.context_backoff_ms
    })
}

/// The compaction trigger measures only the committed conversation, so current
/// decision facts can push a request over the cap while compaction is not yet
/// due. Compact the history then, rather than skipping every decision until the
/// close (#219). Without uncompacted history there is nothing to recover.
#[allow(clippy::too_many_arguments)]
fn compact_after_context_limit(
    c: &Config,
    cfg: &PlannerConfig,
    tick: &Tick<'_>,
    state: &State,
    hashes: &AppliedHashes,
    worker: &mut Option<Worker>,
    reason: &str,
    bounds: &Compaction,
) {
    let turns =
        session::SessionLog::new(&c.directory, &c.session.to_string()).turns_since_summary();
    if compacts_on_refusal(reason, tick.in_session, worker.is_none(), turns) {
        start_compaction(c, cfg, tick, state, hashes, worker, bounds);
    }
}

pub(super) fn compacts_on_refusal(
    reason: &str,
    in_session: bool,
    idle: bool,
    turns: usize,
) -> bool {
    matches!(
        reason,
        "supervisor_context_limit"
            | "supervisor_context_limit_compacted"
            | "supervisor_context_exhausted"
    ) && in_session
        && idle
        && turns > 0
}
