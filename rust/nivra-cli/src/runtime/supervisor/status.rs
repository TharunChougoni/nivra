use super::*;

enum StatusStateError {
    OtherSession,
    Malformed,
    Unreadable,
}

fn read_status_state(
    root: &std::path::Path,
    session: &str,
) -> std::result::Result<Option<State>, StatusStateError> {
    let path = root.join(STATE_PATH);
    let bytes = match crate::read_bounded(&path, MAX_STATUS_STATE_BYTES) {
        Ok(bytes) => bytes,
        Err(()) => {
            return match path.try_exists() {
                Ok(false) => Ok(None),
                _ => Err(StatusStateError::Unreadable),
            };
        }
    };
    let state: State = serde_json::from_slice(&bytes).map_err(|_| StatusStateError::Malformed)?;
    if state.session == session {
        Ok(Some(state))
    } else {
        Err(StatusStateError::OtherSession)
    }
}

const SUPERVISOR_REASONS: &[&str] = &[
    "model_response_incomplete",
    "supervisor_budget_exhausted",
    "supervisor_call_failed",
    "supervisor_call_timeout",
    "supervisor_cancelled",
    "supervisor_compaction_invalid",
    "supervisor_config_revision_changed",
    "supervisor_context_exhausted",
    "supervisor_context_exhausted_backoff",
    "supervisor_context_exhausted_no_history",
    "supervisor_disabled",
    "supervisor_failure_backoff",
    "supervisor_handoff_pending",
    "supervisor_lifecycle_receipt_conflict",
    "supervisor_model_response_incomplete",
    "supervisor_notes_unavailable",
    "supervisor_parked_budget",
    "supervisor_pending_at_handoff",
    "supervisor_prompt_revision_changed",
    "supervisor_session_log_failed",
    "supervisor_settings_invalid",
    "supervisor_state_corrupt",
    "supervisor_state_unreadable",
    "supervisor_state_unwritable",
    "supervisor_usage_contended",
    "supervisor_worker_disconnected",
];

/// Compaction failures reuse the bounded failure codes written by the turn
/// paths, including non-`supervisor_` codes such as `session_notes_changed`.
const COMPACTION_FAILURE_REASONS: &[&str] = &[
    "model_response_incomplete",
    "session_notes_changed",
    "supervisor_action_receipt_failed",
    "supervisor_budget_exhausted",
    "supervisor_call_failed",
    "supervisor_call_timeout",
    "supervisor_cancelled",
    "supervisor_compaction_invalid",
    "supervisor_harness_citation_ids_invalid",
    "supervisor_harness_final_tools_invalid",
    "supervisor_harness_owner_tools_invalid",
    "supervisor_harness_request_invalid",
    "supervisor_harness_tool_definitions_changed",
    "supervisor_harness_tool_invalid",
    "supervisor_instruction_store_failed",
    "supervisor_lifecycle_failed",
    "supervisor_model_response_incomplete",
    "supervisor_note_expiry_failed",
    "supervisor_output_invalid",
    "supervisor_session_log_failed",
    "supervisor_session_note_mark_failed",
    "supervisor_session_note_marks_invalid",
    "supervisor_usage_contended",
    "supervisor_worker_disconnected",
];

fn known_reason(reason: &str) -> Option<&'static str> {
    SUPERVISOR_REASONS
        .iter()
        .copied()
        .find(|known| *known == reason)
}

fn known_compaction_reason(reason: &str) -> Option<&'static str> {
    COMPACTION_FAILURE_REASONS
        .iter()
        .copied()
        .find(|known| *known == reason)
        .or_else(|| known_reason(reason))
}

const INCOMPLETE_REASONS: &[&str] = &["max_output_tokens", "content_filter", "other"];

fn known_incomplete_reason(reason: &str) -> Option<&'static str> {
    INCOMPLETE_REASONS
        .iter()
        .copied()
        .find(|known| *known == reason)
}

const PENDING_KINDS: &[&str] = &[
    "session_open",
    "resume",
    "change",
    "pre_close",
    "post_fill",
    "failure_streak",
    "decision",
    "review",
    "research",
    "compaction",
    "handoff",
    "aside",
    "idle",
];

fn known_pending_kind(trigger: &str) -> &'static str {
    PENDING_KINDS
        .iter()
        .copied()
        .find(|known| *known == trigger)
        .unwrap_or("unknown")
}

/// Explicit planner-policy availability for the read-only projection. A
/// configured policy that failed to load, or whose usage/authority projection
/// failed, is `Unavailable`, never `NotConfigured`.
pub(crate) enum SupervisorConfig<'a> {
    Configured(&'a PlannerConfig),
    Unavailable {
        config: Option<&'a PlannerConfig>,
        reason: &'static str,
    },
    NotConfigured,
}

/// Build the explicit availability input from the controller's current policy
/// and the last failure reason. `reason` is a static code, never raw text.
pub(crate) fn supervisor_config<'a>(
    config: Option<&'a PlannerConfig>,
    fallback: Option<&'a PlannerConfig>,
    reason: Option<&'static str>,
) -> SupervisorConfig<'a> {
    match (config, reason) {
        (Some(config), _) => SupervisorConfig::Configured(config),
        (None, Some(reason)) => SupervisorConfig::Unavailable {
            config: fallback,
            reason,
        },
        (None, None) => SupervisorConfig::NotConfigured,
    }
}

const SUPERVISOR_CONFIG_FAILURE_REASONS: &[&str] = &[
    "planner_config_unavailable",
    "planner_usage_unavailable",
    "planner_usage_unavailable_recovery_required",
    "planner_authority_invalid",
    "planner_event_configuration_changed",
    "planner_integrity_failure",
];

pub(crate) fn known_config_failure_reason(reason: &str) -> Option<&'static str> {
    SUPERVISOR_CONFIG_FAILURE_REASONS
        .iter()
        .copied()
        .find(|known| *known == reason)
}

pub(super) fn pending_source(source: session::PendingSource) -> SupervisorPendingSource {
    match source {
        session::PendingSource::Missing => SupervisorPendingSource::Missing,
        session::PendingSource::NoPending => SupervisorPendingSource::NoPending,
        session::PendingSource::Pending => SupervisorPendingSource::Pending,
        session::PendingSource::Corrupt => SupervisorPendingSource::Corrupt,
        session::PendingSource::Incomplete => SupervisorPendingSource::Incomplete,
        session::PendingSource::Unreadable => SupervisorPendingSource::Unreadable,
        session::PendingSource::Oversize => SupervisorPendingSource::Oversize,
        session::PendingSource::NonRegular => SupervisorPendingSource::NonRegular,
        session::PendingSource::Symlink => SupervisorPendingSource::Symlink,
    }
}

/// Captured research projection supplied by the controller. It is optional so
/// every existing operational-status caller keeps its exact behavior.
pub(crate) struct ResearchOperational {
    pub(crate) config: SupervisorConfigSource,
    pub(crate) config_reason: Option<&'static str>,
    pub(crate) enabled: Option<bool>,
    pub(crate) schedule: Option<owner_tasks::ResearchSchedule>,
    pub(crate) pending: Option<SupervisorPending>,
    pub(crate) pending_source: SupervisorPendingSource,
    pub(crate) as_of_ms: i64,
}

fn supervisor_research(research: ResearchOperational) -> SupervisorResearch {
    SupervisorResearch {
        config: research.config,
        config_reason: research.config_reason,
        enabled: research.enabled,
        due: research
            .schedule
            .as_ref()
            .map(|schedule| schedule.research_due),
        state: research
            .schedule
            .as_ref()
            .map(|schedule| schedule.research_state.as_str()),
        reason: research
            .schedule
            .as_ref()
            .and_then(|schedule| schedule.research_reason),
        evidence_id: research
            .schedule
            .as_ref()
            .and_then(|schedule| schedule.evidence_id.clone()),
        evidence_count: research
            .schedule
            .as_ref()
            .map(|schedule| schedule.evidence_count),
        cutoff_ms: research
            .schedule
            .as_ref()
            .and_then(|schedule| schedule.cutoff_ms),
        deferred_until_ms: research
            .schedule
            .as_ref()
            .and_then(|schedule| schedule.deferred_until_ms),
        pending_source: research.pending_source,
        pending: research.pending,
        as_of_ms: research.as_of_ms,
    }
}

/// Read-only projection of the durable supervisor state and the pending turn.
/// Unknown, missing and malformed sources stay distinct from an idle zero.
pub(crate) fn operational_status_with_research(
    root: &std::path::Path,
    session: &str,
    now: i64,
    config: SupervisorConfig<'_>,
    charged_calls: Option<u64>,
    research: Option<ResearchOperational>,
    current_selection: Option<&str>,
) -> SupervisorOperationalStatus {
    let (cfg, config_source, config_reason) = match config {
        SupervisorConfig::Configured(cfg) => (Some(cfg), SupervisorConfigSource::Configured, None),
        SupervisorConfig::Unavailable { config, reason } => {
            let reason =
                known_config_failure_reason(reason).unwrap_or("planner_config_unavailable");
            (config, SupervisorConfigSource::Unavailable, Some(reason))
        }
        SupervisorConfig::NotConfigured => {
            let mut status = SupervisorOperationalStatus::disabled();
            // A genuinely absent policy still has a disabled research capture;
            // keep its bounded as-of sample instead of dropping the block.
            status.research = research.map(supervisor_research);
            return status;
        }
    };
    let role = cfg.map(|cfg| cfg.role(Trigger::Supervisor));
    let backoff_settings = cfg.and_then(|cfg| {
        settings_for(cfg)
            .ok()
            .map(|(_, _, compaction)| BackoffSettings {
                configured_ms: compaction.context_backoff_ms,
                retry_limit: compaction.context_retry_limit,
            })
    });
    let (state, source) = match read_status_state(root, session) {
        Ok(Some(state)) => (Some(state), SupervisorStateSource::Present),
        Ok(None) => (None, SupervisorStateSource::Missing),
        Err(StatusStateError::OtherSession) => (None, SupervisorStateSource::OtherSession),
        Err(StatusStateError::Malformed) => (None, SupervisorStateSource::Malformed),
        Err(StatusStateError::Unreadable) => (None, SupervisorStateSource::Unreadable),
    };
    let state_present = matches!(source, SupervisorStateSource::Present);
    let parked = state
        .as_ref()
        .and_then(|state| state.parked.as_ref())
        .map(|parked| {
            let reason = known_reason(&parked.reason);
            SupervisorParked {
                reason,
                reason_known: reason.is_some(),
                at_ms: parked.at_ms,
                budget_at_park: parked.budget_at_park,
            }
        });
    let context = state
        .as_ref()
        .and_then(|state| state.context_exhaustion.as_ref())
        .map(|context| {
            let reason = known_reason(&context.reason);
            SupervisorContext {
                reason,
                reason_known: reason.is_some(),
                incomplete_reason: context
                    .incomplete_reason
                    .as_deref()
                    .and_then(known_incomplete_reason),
                request_bytes: context.request_bytes,
                max_request_bytes: context.max_request_bytes,
                at_ms: context.at_ms,
                retries: context.retries,
            }
        });
    let compaction = state
        .as_ref()
        .and_then(|state| state.compaction_failed.as_ref())
        .map(|failure| {
            let reason = known_compaction_reason(&failure.reason);
            SupervisorCompaction {
                reason,
                reason_known: reason.is_some(),
                at_ms: failure.at_ms,
            }
        });
    let last_completed_run = state.as_ref().and_then(|state| {
        state.last_run_ms.map(|at_ms| SupervisorLastRun {
            at_ms,
            age_ms: now.checked_sub(at_ms).filter(|age| *age >= 0),
            runs: state.runs,
        })
    });
    let backoff = match (
        backoff_settings,
        state
            .as_ref()
            .and_then(|state| state.context_exhaustion.as_ref()),
    ) {
        (Some(settings), Some(exhaustion)) => {
            let active = exhaustion.retries >= settings.retry_limit
                && now.saturating_sub(exhaustion.at_ms) < settings.configured_ms;
            let earliest_retry_at_ms = if active {
                exhaustion.at_ms.checked_add(settings.configured_ms)
            } else {
                None
            };
            SupervisorBackoff {
                configured_ms: Some(settings.configured_ms),
                retry_limit: Some(settings.retry_limit),
                enforced: Some(true),
                enforced_scope: "current_session_context_exhaustion_only",
                active: Some(active),
                active_scope: "authoritative_current_session_state_only",
                earliest_retry_at_ms,
            }
        }
        (Some(settings), None) => SupervisorBackoff {
            configured_ms: Some(settings.configured_ms),
            retry_limit: Some(settings.retry_limit),
            enforced: Some(true),
            enforced_scope: "current_session_context_exhaustion_only",
            active: state_present.then_some(false),
            active_scope: "authoritative_current_session_state_only",
            earliest_retry_at_ms: None,
        },
        (None, _) => SupervisorBackoff {
            configured_ms: None,
            retry_limit: None,
            enforced: None,
            enforced_scope: "current_session_context_exhaustion_only",
            active: None,
            active_scope: "authoritative_current_session_state_only",
            earliest_retry_at_ms: None,
        },
    };
    let exhausted = if state_present {
        match parked.as_ref() {
            None => Some(false),
            Some(parked) if parked.reason_known => {
                Some(parked.reason == Some("supervisor_budget_exhausted"))
            }
            Some(_) => None,
        }
    } else {
        None
    };
    let budget = SupervisorBudget {
        configured_daily_calls: role.as_ref().map(|role| role.daily_call_budget),
        charged_calls,
        charged_calls_scope: "provider_usage_projection_not_turn_runs",
        parked_at_calls: parked.as_ref().and_then(|parked| parked.budget_at_park),
        exhausted,
        exhausted_scope: "authoritative_current_session_parked_state_only",
    };
    let pending_projection = session::SessionLog::new(root, session).pending_checked();
    let pending = pending_projection
        .turn
        .as_ref()
        .map(|turn| SupervisorPending {
            kind: known_pending_kind(&turn.trigger),
            started_at_ms: turn.started_at_ms,
            age_ms: now.checked_sub(turn.started_at_ms).filter(|age| *age >= 0),
        });
    // A configured-but-unavailable policy, a non-present state source or an
    // unavailable pending source is unknown. A real checked pending turn is
    // still exposed separately as evidence; it is never allowed to turn an
    // unknown source into an idle or pending claim.
    let status = if !matches!(config_source, SupervisorConfigSource::Configured)
        || !matches!(source, SupervisorStateSource::Present)
        || !pending_projection.is_available()
    {
        SupervisorStatus::Unknown
    } else if parked.is_some() {
        SupervisorStatus::Parked
    } else if pending.is_some() {
        SupervisorStatus::Pending
    } else {
        SupervisorStatus::Idle
    };
    // Reuse the exact admission binding and method. A foreign or unknown
    // current selection is not deferred, a missing/malformed state source is
    // unknown rather than an active or false backoff, and the legacy
    // None==None branch remains valid without inventing a profile.
    let state_present = matches!(source, SupervisorStateSource::Present);
    let owner_failure_backoff = cfg
        .and_then(|cfg| settings_for(cfg).ok())
        .map(|(wake, _, _)| {
            let configured_ms = wake.failure_backoff_ms;
            let until = if state_present {
                state.as_ref().and_then(|state| {
                    self::wake::failure_backoff_until(state, &wake, now, current_selection)
                })
            } else {
                None
            };
            let active = match (configured_ms, state_present) {
                (Some(_), true) => Some(until.is_some()),
                (Some(_), false) => None,
                (None, _) => None,
            };
            let failures = if state_present {
                state.as_ref().and_then(|state| state.owner_failure_streak)
            } else {
                None
            };
            let last_failure_ms = if state_present {
                state.as_ref().and_then(|state| state.last_failure_ms)
            } else {
                None
            };
            SupervisorOwnerFailureBackoff {
                configured_ms,
                enforced_scope: "owner_model_turns_only",
                active,
                earliest_retry_at_ms: until,
                owner_failures: failures,
                last_owner_failure_ms: last_failure_ms,
            }
        });
    let backoff_until_for_ops = owner_failure_backoff
        .as_ref()
        .and_then(|backoff| backoff.earliest_retry_at_ms);
    let aside = aside_projection(root, session, now, state.as_ref(), backoff_until_for_ops);
    let research = research.map(supervisor_research);
    SupervisorOperationalStatus {
        status,
        config: config_source,
        config_reason,
        config_scope: "loaded_policy_or_static_failure_code",
        state: source,
        enabled: role.as_ref().map(|role| role.enabled),
        parked,
        budget,
        context,
        compaction,
        backoff,
        last_completed_run,
        pending_source: pending_source(pending_projection.source),
        pending_scope: "checked_bounded_session_log",
        pending,
        research,
        owner_failure_backoff,
        aside,
    }
}

fn aside_projection(
    root: &std::path::Path,
    session: &str,
    now: i64,
    state: Option<&State>,
    backoff_until_ms: Option<i64>,
) -> Option<SupervisorAside> {
    let request = aside_request(root, session)?;
    let pending = session::SessionLog::new(root, session).pending();
    let (reason, until) = if state.is_some_and(|state| state.parked.is_some()) {
        (Some("aside_parked"), None)
    } else if let Some(until) = backoff_until_ms.filter(|until| now < *until) {
        (Some("aside_deferred_backoff"), Some(until))
    } else if pending.is_some() {
        (Some("aside_owner_turn_pending"), None)
    } else {
        (None, None)
    };
    Some(SupervisorAside {
        requested: true,
        id: Some(request.id),
        deferred_reason: reason,
        deferred_until_ms: until,
    })
}

pub(super) fn settings_for(
    cfg: &PlannerConfig,
) -> std::result::Result<(Wake, Limits, Compaction), &'static str> {
    let trigger = cfg
        .orchestrator
        .as_ref()
        .and_then(|orchestrator| orchestrator.triggers.get(&Trigger::Supervisor));
    let wake = trigger.map(Wake::from_trigger).unwrap_or_default();
    let profile = cfg.profile_value("supervisor");
    let wake = if let Some(raw) = profile {
        wake.overlay(raw)?
    } else {
        wake
    };
    wake.validate()?;
    let compaction = Compaction::from_profile(profile)?;
    let mut limits = Limits::default();
    if let Some(raw) = cfg.profile_value("directives") {
        limits = limits.tighten(raw)?;
    }
    Ok((wake, limits, compaction))
}
