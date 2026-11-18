use super::*;

/// Bounded, role-prefixed failure code for the supervisor archive. The
/// underlying planner/model/harness code stays visible instead of becoming the
/// generic `supervisor_call_failed`; unknown text is still replaced.
#[cfg(test)]
pub(super) fn supervisor_failure_reason(reason: &str) -> String {
    let safe = !reason.is_empty()
        && reason.len() <= 64
        && reason
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    match reason {
        "supervisor_cancelled" => "supervisor_cancelled".into(),
        "planner_budget_exhausted" => "supervisor_budget_exhausted".into(),
        "planner_usage_busy_no_retry" | "planner_usage_lock_timeout" => {
            "supervisor_usage_contended".into()
        }
        "planner_call_timeout" | "model_timeout" => "supervisor_call_timeout".into(),
        code if model_http_code(code) => format!(
            "supervisor_{}",
            code.split_ascii_whitespace()
                .next()
                .unwrap_or("model_http_unknown")
        ),
        code if safe
            && (code.starts_with("session_")
                || code.starts_with("planner_")
                || code.starts_with("model_")
                || code.starts_with("harness_")) =>
        {
            format!("supervisor_{code}")
        }
        code if safe && code.starts_with("supervisor_") => code.to_owned(),
        _ => "supervisor_call_failed".into(),
    }
}

#[cfg(test)]
fn model_http_code(reason: &str) -> bool {
    let Some(status) = reason
        .split_ascii_whitespace()
        .next()
        .and_then(|code| code.strip_prefix("model_http_"))
    else {
        return false;
    };
    status.len() == 3
        && status.bytes().all(|byte| byte.is_ascii_digit())
        && (100..=599).contains(&status.parse::<u16>().unwrap_or_default())
}

#[cfg(test)]
pub(super) fn record_compaction_failure(
    root: &std::path::Path,
    session: &str,
    now: i64,
    reason: &str,
) {
    let result = with_state(root, session, |state| {
        state.compaction_failed = Some(CompactionFailure {
            reason: reason.to_owned(),
            at_ms: now,
        });
    });
    if let Err(error) = result {
        eprintln!("supervisor_compaction_failure_unrecorded reason={error}");
        record_state_error(root, session, error);
    }
}

#[cfg(test)]
pub(super) fn clear_compaction_failure(root: &std::path::Path, session: &str) {
    let result = with_state(root, session, |state| state.compaction_failed = None);
    if let Err(error) = result {
        eprintln!("supervisor_compaction_clear_unrecorded reason={error}");
        record_state_error(root, session, error);
    }
}

/// Persist the measured provider-request envelope from this turn's backend
/// provenance: the bytes beyond the JSON-escaped prompt (instructions, tools
/// and envelope). The envelope bounds the whole request, not only the prompt,
/// on later renders (#224). Tool round trips grow the input, so only the first
/// turn measures it.
#[cfg(test)]
pub(super) fn record_request_evidence(root: &std::path::Path, worker: &Worker) {
    let evidence = worker.evidence();
    if evidence["input_items"]
        .as_u64()
        .is_some_and(|items| items != 1)
    {
        return;
    }
    let (Some(request_bytes), Some(escaped)) = (
        evidence["request_bytes"].as_u64(),
        evidence["escaped_prompt_bytes"].as_u64(),
    ) else {
        return;
    };
    if escaped == 0 || request_bytes < escaped {
        return;
    }
    record_request_envelope(root, &worker.session, request_bytes - escaped);
}

/// A provider-incomplete response on a request at or over the configured cap
/// means the same oversized history will fail again until it is compacted. A
/// `content_filter` cause is retained in evidence but is not a size signal.
#[cfg(test)]
fn context_exhaustion(worker: &Worker, reason: &str, now: i64) -> Option<ContextExhaustion> {
    // Supervisor turns pass the role-prefixed code; decision turns the raw one.
    let code = reason.strip_prefix("supervisor_").unwrap_or(reason);
    if code != "model_response_incomplete" {
        return None;
    }
    let evidence = worker.evidence();
    if evidence["incomplete_reason"].as_str() == Some("content_filter") {
        return None;
    }
    let request_bytes = evidence["request_bytes"].as_u64();
    let measured = request_bytes.unwrap_or_else(|| {
        full_request_bytes(worker.escaped_prompt_bytes, &worker.bounds)
            .saturating_add(worker.bounds.reserve_bytes)
    });
    if measured.saturating_mul(100)
        < worker
            .bounds
            .max_request_bytes
            .saturating_mul(worker.bounds.near_cap_percent)
    {
        return None;
    }
    Some(ContextExhaustion {
        reason: reason.to_owned(),
        incomplete_reason: evidence["incomplete_reason"].as_str().map(str::to_owned),
        request_bytes,
        max_request_bytes: worker.bounds.max_request_bytes,
        at_ms: now,
        retries: 0,
    })
}

/// Record context exhaustion for the bounded compaction retry. The retry count
/// is preserved across failures so the loop cannot restart every cycle.
#[cfg(test)]
fn record_context_exhaustion_if_oversize(
    root: &std::path::Path,
    worker: &Worker,
    now: i64,
    reason: &str,
) {
    let Some(exhaustion) = context_exhaustion(worker, reason, now) else {
        return;
    };
    eprintln!(
        "supervisor_context_exhausted reason={} incomplete_reason={:?} request_bytes={:?} max_request_bytes={}",
        exhaustion.reason,
        exhaustion.incomplete_reason,
        exhaustion.request_bytes,
        exhaustion.max_request_bytes
    );
    record_context_exhaustion(root, &worker.session, exhaustion);
}

/// Record the failed result and count the failed turn. The turn stays pending
/// only when the log append itself failed; every other path clears it.
#[cfg(test)]
pub(super) fn fail_reply(
    root: &std::path::Path,
    worker: &Worker,
    now: i64,
    reason: &str,
    alerts: Option<&Alerts>,
) -> bool {
    if worker.kind == RunKind::Review {
        return review::finish_failed(root, worker, now, reason, alerts);
    }
    eprintln!("supervisor_failed reason={reason}");
    let actions = if worker.kind == RunKind::Handoff {
        match persist_lifecycle_artifact(root, worker, now, None, Some(reason)) {
            Ok(body) => vec![lifecycle_action(&worker.turn_id, &body)],
            Err(code) => {
                emit_failure(alerts, &worker.session, now, code);
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let logged = record_result(root, worker, now, "failed", Some(reason), "", actions);
    if !logged {
        emit_failure(
            alerts,
            &worker.session,
            now,
            "supervisor_session_log_failed",
        );
    }
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
            false,
            &worker.hashes,
            worker.selection_binding.as_deref(),
        );
    }
    record_failure_with_evidence(root, &worker.session, now, reason, &worker.evidence());
    record_context_exhaustion_if_oversize(root, worker, now, reason);
    if let Err(error) = with_state(root, &worker.session, |state| {
        state.block_tool_failure(reason, now, &worker.hashes);
    }) {
        record_state_error(root, &worker.session, error);
    }
    if worker.kind == RunKind::Compaction && (!logged || worker.lifecycle.is_none()) {
        record_compaction_failure(root, &worker.session, now, reason);
    }
    emit_failure(alerts, &worker.session, now, reason);
    false
}

/// Finish one supervisor reply. Only a committed reply counts as an ok run; a
/// failed instruction store, a stale note set or an invalid answer is a failed
/// run that alerts and retries on the next wake (#143, #155).
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) fn handle_reply(
    root: &std::path::Path,
    worker: &Worker,
    now: i64,
    raw: &str,
    allowlist: &[String],
    limits: &Limits,
    session_end_ms: i64,
    alerts: Option<&Alerts>,
) -> bool {
    // The harness returns final text; tolerate a raw provider envelope
    // from older fixtures without ever treating an empty answer as success.
    let text = reply_text(raw).unwrap_or_else(|| raw.to_owned());
    if text.trim().is_empty() {
        return fail_reply(root, worker, now, "supervisor_output_invalid", alerts);
    }
    if worker.kind == RunKind::Review {
        return review::finish_reply(root, worker, now, &text, alerts);
    }
    if worker.kind != RunKind::Supervisor {
        if session::check_notes_version(root, &worker.session, worker.notes_version).is_err() {
            return fail_reply(root, worker, now, "session_notes_changed", alerts);
        }
        return finish_lifecycle(root, worker, now, &text, alerts);
    }
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return fail_reply(root, worker, now, "supervisor_output_invalid", alerts);
    };
    if let Err(error) = session::check_notes_version(root, &worker.session, worker.notes_version) {
        eprintln!("supervisor_notes_changed reason={error}");
        let logged = record_result(
            root,
            worker,
            now,
            "stale_notes",
            Some("session_notes_changed"),
            "",
            Vec::new(),
        );
        if !logged {
            emit_failure(
                alerts,
                &worker.session,
                now,
                "supervisor_session_log_failed",
            );
        }
        finish(
            root,
            &worker.session,
            worker.kind,
            worker.reason,
            now,
            false,
            &worker.hashes,
            worker.selection_binding.as_deref(),
        );
        record_failure(root, &worker.session, now, "session_notes_changed");
        let reruns = with_state(root, &worker.session, |state| {
            state.note_reruns = state.note_reruns.saturating_add(1);
            state.note_reruns
        })
        .unwrap_or(MAX_NOTE_RERUNS);
        if reruns <= MAX_NOTE_RERUNS {
            // Clear the cooldown so the next tick rebuilds the turn from the new
            // note set instead of waiting for the idle interval.
            let _ = with_state(root, &worker.session, |state| {
                state.last_run_ms = None;
            });
        } else {
            eprintln!("session_note_rerun_cap dropped turn={}", worker.turn_id);
            emit_failure(alerts, &worker.session, now, "session_note_rerun_cap");
        }
        emit_failure(alerts, &worker.session, now, "session_notes_changed");
        return false;
    }
    let (accepted, rejected, wake) =
        directives::parse_output(&text, allowlist, limits, now, session_end_ms);
    for reason in rejected {
        eprintln!("directive_rejected reason={reason}");
    }
    let mut committed = Vec::new();
    for directive in &accepted {
        if let Some(previous) = &directive.supersedes
            && let Err(error) = directives::supersede(root, previous, directive.issued_at_ms)
        {
            eprintln!("supervisor_instruction_store_failed reason={error}");
            return fail_reply(
                root,
                worker,
                now,
                "supervisor_instruction_store_failed",
                alerts,
            );
        }
        if let Err(error) = directives::issue_once(root, directive) {
            eprintln!("supervisor_instruction_store_failed reason={error}");
            return fail_reply(
                root,
                worker,
                now,
                "supervisor_instruction_store_failed",
                alerts,
            );
        }
        let action = session::action(
            "directive",
            &directive.id,
            &serde_json::to_value(directive).unwrap_or_default(),
        );
        if let Err(error) = session::commit_action(root, &worker.session, &worker.turn_id, &action)
            && error.to_string() != "session_action_already_committed"
        {
            eprintln!("supervisor_action_receipt_failed reason={error}");
            return fail_reply(
                root,
                worker,
                now,
                "supervisor_action_receipt_failed",
                alerts,
            );
        }
        committed.push(action);
    }
    let (done, superseded) = match session::note_marks(&value) {
        Ok(marks) => marks,
        Err(_) => return fail_reply(root, worker, now, "session_note_marks_invalid", alerts),
    };
    let notes = session::Notes::new(root, &worker.session);
    for mark in done {
        if let Err(error) = notes.mark(&mark.id, mark.revision, session::NoteState::Done, now) {
            eprintln!("supervisor_note_mark_failed reason={error}");
            return fail_reply(root, worker, now, "session_note_mark_failed", alerts);
        }
    }
    for mark in superseded {
        if let Err(error) = notes.mark(&mark.id, mark.revision, session::NoteState::Superseded, now)
        {
            eprintln!("supervisor_note_mark_failed reason={error}");
            return fail_reply(root, worker, now, "session_note_mark_failed", alerts);
        }
    }
    let logged = record_result(root, worker, now, "ok", None, &text, committed);
    finish(
        root,
        &worker.session,
        worker.kind,
        worker.reason,
        now,
        logged,
        &worker.hashes,
        worker.selection_binding.as_deref(),
    );
    if !logged {
        record_failure(root, &worker.session, now, "supervisor_session_log_failed");
        emit_failure(
            alerts,
            &worker.session,
            now,
            "supervisor_session_log_failed",
        );
        return false;
    }
    if wake {
        let _ = enqueue_wake(root, &worker.session, now);
    }
    true
}

#[cfg(test)]
pub(super) fn record_result(
    root: &std::path::Path,
    worker: &Worker,
    now: i64,
    outcome: &str,
    reason: Option<&str>,
    reply: &str,
    actions: Vec<session::Action>,
) -> bool {
    let log = session::SessionLog::new(root, &worker.session);
    let result = session::TurnResult {
        turn_id: worker.turn_id.clone(),
        finished_at_ms: now,
        outcome: outcome.to_owned(),
        reason: reason.map(str::to_owned),
        reply: session::bound_reply(reply),
        actions,
    };
    let logged = log.append(now, session::Body::Result { result }).is_ok();
    // A completed turn is the recovery evidence for earlier context
    // exhaustion: the request fit and the provider answered (#224).
    if logged && outcome == "ok" {
        clear_context_exhaustion(root, &worker.session);
    }
    logged
}

#[cfg(test)]
pub(super) fn emit_failure(alerts: Option<&Alerts>, session: &str, now: i64, reason: &str) {
    if let Some(alerts) = alerts {
        let day = chrono::NaiveDate::parse_from_str(session, "%Y-%m-%d")
            .unwrap_or_else(|_| local(now).date_naive());
        alerts.emit(
            Event::SupervisorFailure,
            day,
            json!({"reason":reason,"session":session}),
        );
    }
}

/// The backend returns the Responses envelope; directives live in its final text.
#[cfg(test)]
pub(super) fn reply_text(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    super::super::harness::final_text(value["output"].as_array()?).ok()
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) fn apply_finish(
    state: &mut State,
    kind: RunKind,
    reason: &str,
    now: i64,
    ok: bool,
    hashes: &AppliedHashes,
    failure_binding: Option<&str>,
) {
    state.last_run_ms = Some(now);
    state.runs = state.runs.saturating_add(1);
    if ok {
        // A review or host lifecycle success is not evidence that decisions
        // recovered.
        if matches!(kind, RunKind::Supervisor | RunKind::Decision) {
            state.failures = 0;
        }
        if matches!(kind, RunKind::Supervisor | RunKind::Decision) {
            // Only a success under the same recorded selection clears the
            // owner-failure streak. A different profile never inherits it.
            if state.last_failure_binding.as_deref() == failure_binding {
                state.owner_failure_streak = None;
                state.last_failure_ms = None;
                state.last_failure_binding = None;
            }
        }
        if !matches!(kind, RunKind::Review | RunKind::Research) {
            state.note_reruns = 0;
            // The applied tuple advances only after a successful decision or
            // supervisor run, so a failed run is retried through the failure
            // streak rather than hidden. Research carries no applied decision
            // hashes and must preserve the previous valid tuple.
            state.applied = Some(hashes.clone());
        }
        if matches!(reason, "session_open" | "resume") {
            state.open_fired = true;
        }
        if reason == "change" {
            state.change_fired = Some(hashes.clone());
            state.change_last_ms = Some(now);
        }
        if reason == "pre_close" {
            state.pre_close_fired = true;
        }
    } else {
        state.failures = state.failures.saturating_add(1);
        // Only an actual owner decision/supervisor failure arms the optional
        // backoff. Review and host lifecycle failures keep their own semantics
        // and never advance the gating counter. The streak is scoped to the
        // recorded profile/revision, so a different binding starts over.
        // Research lane failures never touch the trading streak (#238).
        if matches!(kind, RunKind::Supervisor | RunKind::Decision) {
            let same_binding = state.last_failure_binding.as_deref() == failure_binding;
            let streak = if same_binding {
                state.owner_failure_streak.unwrap_or(0)
            } else {
                0
            };
            state.owner_failure_streak = Some(streak.saturating_add(1));
            state.last_failure_ms = Some(now);
            state.last_failure_binding = failure_binding.map(str::to_owned);
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) fn finish(
    root: &std::path::Path,
    session: &str,
    kind: RunKind,
    reason: &str,
    now: i64,
    ok: bool,
    hashes: &AppliedHashes,
    failure_binding: Option<&str>,
) {
    let result = with_state(root, session, |state| {
        apply_finish(state, kind, reason, now, ok, hashes, failure_binding);
    });
    if let Err(error) = result {
        eprintln!("supervisor_finish_unrecorded reason={error}");
        record_state_error(root, session, error);
    }
}

#[cfg(test)]
fn enqueue_wake(root: &std::path::Path, session: &str, now: i64) -> Result<()> {
    let path = root.join("model/supervisor-wake.json");
    rollover::atomic(
        &path,
        &json!({"session":session,"at_ms":now,"reason":"supervisor_wake"}),
    )?;
    Ok(())
}

pub(crate) fn take_wake(root: &std::path::Path, session: &str, now: i64, floor_ms: i64) -> bool {
    let path = root.join("model/supervisor-wake.json");
    let Ok(raw) = fs::read(&path) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
        // A corrupt wake is stale context, not an instruction: drop it with a
        // trace instead of replaying an unreadable request (#156).
        eprintln!("supervisor_wake_dropped reason=unreadable");
        let _ = fs::remove_file(path);
        return false;
    };
    let wake_session = value["session"].as_str().unwrap_or_default();
    let wake_at = value["at_ms"].as_i64();
    let fresh = wake_session == session
        && wake_at.is_some_and(|at| now.saturating_sub(at) <= floor_ms.max(1));
    if !fresh {
        let age_ms = wake_at.map(|at| now.saturating_sub(at));
        eprintln!(
            "supervisor_wake_dropped reason=stale wake_session={wake_session} session={session} age_ms={age_ms:?}"
        );
    }
    let _ = fs::remove_file(path);
    fresh
}
