use super::*;

/// Path-free hash references to original review artifacts, for the successor seed.
#[cfg(test)]
pub(crate) fn review_state(root: &std::path::Path, session: &str) -> Value {
    review::state(root, session)
}

/// Bounded read-only projection of owner review work. It never starts or writes work.
#[cfg(test)]
pub(crate) fn review_status(
    c: &Config,
    cfg: &PlannerConfig,
    p: &nivra_core::execution::Portfolio,
    now: i64,
    session_end_ms: i64,
) -> Value {
    if cfg
        .owner_tasks
        .as_ref()
        .is_none_or(|settings| !settings.review.enabled)
    {
        return json!({"version":1,"status":"disabled"});
    }
    // Match admission: invalid supervisor settings refuse the turn instead of
    // silently falling back to defaults and reporting a due/unparked schedule.
    let wake = match settings_for(cfg) {
        Ok((wake, _, _)) => wake,
        Err(_) => {
            return json!({"version":1,"status":"invalid","reason":"supervisor_settings_invalid"});
        }
    };
    let cutoff = match review::settings_cutoff(c, cfg, session_end_ms) {
        Ok(cutoff) => cutoff,
        Err(error) => {
            let reason = if error.to_string() == "owner_review_cutoff_invalid" {
                "owner_review_cutoff_invalid"
            } else {
                "owner_tasks_settings_invalid"
            };
            return json!({"version":1,"status":"invalid","reason":reason});
        }
    };
    match review::open_reviews(&c.directory, false) {
        Ok(_) => {}
        Err(error) if error.to_string() == "owner_review_source_missing" => {}
        Err(_) => {
            return json!({"version":1,"status":"invalid","reason":"owner_review_path_invalid"});
        }
    }
    let session = c.session.to_string();
    let log = session::SessionLog::new(&c.directory, &session);
    let state = state::load(&c.directory, &session).ok();
    let backoff_until = state.as_ref().and_then(|state| {
        let current = current_selection_binding(c, cfg);
        self::wake::failure_backoff_until(state, &wake, now, current.as_deref())
    });
    let input = review::schedule_input(
        c,
        &log,
        state.as_ref(),
        p.closed,
        now,
        session_end_ms,
        cutoff,
        backoff_until,
    );
    let schedule = owner_tasks::project(&input);
    let mut value = json!({
        "version":1,
        "status":schedule.review_state,
        "review_due":schedule.review_due,
        "as_of_ms":now,
        "cutoff_ms":cutoff,
        "attempted":input.attempted,
        "state_available":state.is_some(),
    });
    if let Some(reason) = schedule.review_reason {
        value["reason"] = json!(reason);
    }
    if let Some(until) = schedule.deferred_until_ms {
        value["deferred_until_ms"] = json!(until);
    }
    value
}

const RESEARCH_REASONS: &[&str] = &[
    "owner_research_artifact_bound",
    "owner_research_binding_invalid",
    "owner_research_clock_invalid",
    "owner_research_cutoff_invalid",
    "owner_research_expired",
    "owner_research_failed",
    "owner_research_interrupted",
    "owner_research_no_eligible_evidence",
    "owner_research_output_bound",
    "owner_research_output_citation_invalid",
    "owner_research_output_financial_refused",
    "owner_research_output_invalid",
    "owner_research_path_invalid",
    "owner_research_quote_capture_invalid",
    "owner_research_receipt_conflict",
    "owner_research_reconciliation_required",
    "owner_research_requires_decision_owner",
    "owner_research_settings_invalid",
    "owner_research_source_changed",
    "owner_research_source_invalid",
    "owner_research_source_missing",
    "owner_research_source_unverified",
    "owner_research_source_unsynced",
    "owner_research_turn_missing",
    "owner_research_workspace_changed",
    "owner_research_workspace_disabled",
    "owner_research_workspace_invalid",
    "owner_research_workspace_missing",
    "owner_research_workspace_not_file",
    "owner_research_workspace_not_text",
    "owner_research_workspace_permissions",
    "owner_research_workspace_source_invalid",
    "owner_research_workspace_source_missing",
    "owner_research_workspace_symlink",
    "owner_research_workspace_too_large",
    "research_lane_book_not_runnable",
    "research_lane_call_budget_exhausted",
    "research_lane_market_hours",
    "research_lane_log_unavailable",
    "research_lane_min_interval",
    "research_lane_required",
    "research_lane_settings_invalid",
    "research_lane_settings_missing",
    "research_lane_token_budget_exhausted",
    "research_lane_turn_budget_exhausted",
    "research_lifecycle_unavailable",
    "research_queue_unavailable",
    "research_source_changed",
    "research_source_invalid",
    "research_source_missing",
    "research_source_unverified",
    "supervisor_settings_invalid",
];

fn known_research_reason(reason: &str) -> Option<&'static str> {
    RESEARCH_REASONS
        .iter()
        .copied()
        .find(|known| *known == reason)
}

/// Normalize an internal workspace/source error at the read-only boundary to a
/// bounded static research code. Raw paths, prose and provider text never reach
/// the dashboard or the operational projection.
pub(in super::super) fn research_boundary_reason(reason: &str) -> &'static str {
    match reason {
        "workspace_ref_changed" => "owner_research_workspace_changed",
        "workspace_ref_missing" => "owner_research_workspace_missing",
        "workspace_ref_too_large" => "owner_research_workspace_too_large",
        "workspace_ref_not_text" => "owner_research_workspace_not_text",
        "workspace_ref_permissions" => "owner_research_workspace_permissions",
        "workspace_ref_symlink" | "workspace_symlink" => "owner_research_workspace_symlink",
        "workspace_ref_not_file" => "owner_research_workspace_not_file",
        "workspace_ref_invalid"
        | "workspace_path_invalid"
        | "workspace_path_absolute"
        | "workspace_path_escape"
        | "workspace_disabled"
        | "workspace_settings_invalid"
        | "workspace_root_invalid" => "owner_research_workspace_invalid",
        "research_source_changed" | "candidate_result_unverified" => {
            "owner_research_source_changed"
        }
        "research_source_invalid" => "owner_research_source_invalid",
        "research_source_missing" => "owner_research_source_missing",
        "research_source_unverified" => "owner_research_source_unverified",
        // A historical turn whose evidence or owner-task binding cannot be
        // verified is a source problem, never a settings-invalid mislabel.
        "owner_research_evidence_invalid" => "owner_research_source_invalid",
        "owner_task_binding_invalid" => "owner_research_source_unverified",
        other if other.starts_with("workspace_") || other.starts_with("session_seed_") => {
            "owner_research_workspace_invalid"
        }
        other
            if other.starts_with("prior_config_")
                || other.starts_with("paper_cost_")
                || other.starts_with("book_identity") =>
        {
            "owner_research_workspace_source_invalid"
        }
        // Unknown internal errors stay a bounded static code; raw text, paths
        // and provider prose never reach the dashboard or operational block.
        other => known_research_reason(other).unwrap_or("owner_research_source_unverified"),
    }
}

/// One read-only capture of the research policy availability and schedule at
/// the controller completion sample. The dashboard `owner_research` view and
/// the operational-health research block are both derived from this value, so
/// they cannot disagree or read the queue, state or pending log twice.
struct ResearchCapture {
    config: SupervisorConfigSource,
    config_reason: Option<&'static str>,
    enabled: Option<bool>,
    status: ResearchCaptureStatus,
    schedule: Option<owner_tasks::ResearchSchedule>,
    pending: Option<SupervisorPending>,
    pending_source: SupervisorPendingSource,
    state_available: bool,
    as_of_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResearchCaptureStatus {
    Disabled,
    Unavailable,
    Invalid,
    Configured,
}

impl ResearchCapture {
    fn disabled(
        enabled: Option<bool>,
        pending: Option<SupervisorPending>,
        pending_source: SupervisorPendingSource,
        as_of_ms: i64,
    ) -> Self {
        Self {
            config: SupervisorConfigSource::NotConfigured,
            config_reason: None,
            enabled,
            status: ResearchCaptureStatus::Disabled,
            schedule: None,
            pending,
            pending_source,
            state_available: false,
            as_of_ms,
        }
    }

    fn unavailable(
        reason: &'static str,
        enabled: Option<bool>,
        pending: Option<SupervisorPending>,
        pending_source: SupervisorPendingSource,
        as_of_ms: i64,
    ) -> Self {
        Self {
            config: SupervisorConfigSource::Unavailable,
            config_reason: Some(reason),
            enabled,
            status: ResearchCaptureStatus::Unavailable,
            schedule: None,
            pending,
            pending_source,
            state_available: false,
            as_of_ms,
        }
    }

    fn invalid(
        reason: &'static str,
        enabled: Option<bool>,
        pending: Option<SupervisorPending>,
        pending_source: SupervisorPendingSource,
        as_of_ms: i64,
    ) -> Self {
        Self {
            config: SupervisorConfigSource::Unavailable,
            config_reason: Some(reason),
            enabled,
            status: ResearchCaptureStatus::Invalid,
            schedule: None,
            pending,
            pending_source,
            state_available: false,
            as_of_ms,
        }
    }

    fn configured(
        schedule: owner_tasks::ResearchSchedule,
        pending: Option<SupervisorPending>,
        pending_source: SupervisorPendingSource,
        state_available: bool,
        as_of_ms: i64,
    ) -> Self {
        Self {
            config: SupervisorConfigSource::Configured,
            config_reason: None,
            enabled: Some(true),
            status: ResearchCaptureStatus::Configured,
            schedule: Some(schedule),
            pending,
            pending_source,
            state_available,
            as_of_ms,
        }
    }

    /// The dashboard view. Every branch carries the same captured sample.
    fn view(&self) -> Value {
        match self.status {
            ResearchCaptureStatus::Disabled => json!({
                "version":1,
                "status":"disabled",
                "as_of_ms":self.as_of_ms,
            }),
            ResearchCaptureStatus::Unavailable => json!({
                "version":1,
                "status":"unavailable",
                "reason":self.config_reason.unwrap_or("planner_config_unavailable"),
                "activation_authorized":false,
                "as_of_ms":self.as_of_ms,
            }),
            ResearchCaptureStatus::Invalid => json!({
                "version":1,
                "status":"invalid",
                "reason":self.config_reason.unwrap_or("owner_research_settings_invalid"),
                "as_of_ms":self.as_of_ms,
            }),
            ResearchCaptureStatus::Configured => {
                let schedule = self
                    .schedule
                    .as_ref()
                    .expect("configured research capture has a schedule");
                let mut value = json!({
                    "version":1,
                    "status":schedule.research_state,
                    "research_due":schedule.research_due,
                    "as_of_ms":self.as_of_ms,
                    "cutoff_ms":schedule.cutoff_ms,
                    "evidence_count":schedule.evidence_count,
                    "state_available":self.state_available,
                });
                if let Some(reason) = schedule.research_reason {
                    value["reason"] = json!(reason);
                }
                if let Some(id) = &schedule.evidence_id {
                    value["evidence_id"] = json!(id);
                }
                if let Some(until) = schedule.deferred_until_ms {
                    value["deferred_until_ms"] = json!(until);
                }
                // Public scalars only, and only when the pinned source was
                // captured and verified at this completion sample. A captured
                // empty workspace is count 0 with a valid empty-array hash;
                // an uncaptured source stays absent (Not reported), never zero.
                if let Some(count) = schedule.workspace_ref_count {
                    value["workspace_ref_count"] = json!(count);
                }
                if let Some(hash) = &schedule.workspace_snapshot_sha256 {
                    value["workspace_snapshot_sha256"] = json!(hash);
                }
                value
            }
        }
    }

    fn operational(self) -> ResearchOperational {
        ResearchOperational {
            config: self.config,
            config_reason: self.config_reason,
            enabled: self.enabled,
            schedule: self.schedule,
            pending: self.pending,
            pending_source: self.pending_source,
            as_of_ms: self.as_of_ms,
        }
    }
}

/// Read the research policy availability and schedule once at the supplied
/// completion sample. It never creates a queue, turn or provider call and
/// never renews `as_of_ms`.
fn capture_research(
    c: &Config,
    cfg: Option<&PlannerConfig>,
    config_reason: Option<&'static str>,
    p: &nivra_core::execution::Portfolio,
    as_of_ms: i64,
    session_end_ms: Option<i64>,
) -> ResearchCapture {
    let Some(cfg) = cfg else {
        return match config_reason {
            Some(reason) => ResearchCapture::unavailable(
                known_config_failure_reason(reason).unwrap_or("planner_config_unavailable"),
                None,
                None,
                SupervisorPendingSource::NotChecked,
                as_of_ms,
            ),
            None => {
                ResearchCapture::disabled(None, None, SupervisorPendingSource::NotChecked, as_of_ms)
            }
        };
    };
    let Some(session_end_ms) = session_end_ms else {
        return ResearchCapture::unavailable(
            "controller_session_end_unavailable",
            None,
            None,
            SupervisorPendingSource::NotChecked,
            as_of_ms,
        );
    };
    let session = c.session.to_string();
    let mut pending_projection = research::lane_log(&c.directory, &session).pending_checked();
    // No lane log yet provably means no research turn ever started.
    if matches!(pending_projection.source, session::PendingSource::Missing) {
        pending_projection.source = session::PendingSource::NoPending;
    }
    let pending = pending_projection
        .turn
        .as_ref()
        .map(|turn| SupervisorPending {
            kind: "research",
            started_at_ms: turn.started_at_ms,
            age_ms: as_of_ms
                .checked_sub(turn.started_at_ms)
                .filter(|age| *age >= 0),
        });
    let pending_source = pending_source(pending_projection.source);
    let Some(settings) = cfg
        .owner_tasks
        .as_ref()
        .and_then(|settings| settings.research.as_ref())
    else {
        return ResearchCapture::disabled(Some(false), pending, pending_source, as_of_ms);
    };
    if !settings.enabled {
        return ResearchCapture::disabled(Some(false), pending, pending_source, as_of_ms);
    }
    if settings_for(cfg).is_err() {
        return ResearchCapture::invalid(
            "supervisor_settings_invalid",
            Some(true),
            pending,
            pending_source,
            as_of_ms,
        );
    }
    let state_present = state::load(&c.directory, &session).is_ok();
    // The halted/stopped book parks the lane; an open position does not.
    let book_runnable = !p.halted && !p.stopped;
    let last_fill_ms = p.fills.iter().map(|fill| fill.at).max();
    match research::schedule(
        c,
        cfg,
        as_of_ms,
        session_end_ms,
        book_runnable,
        last_fill_ms,
    ) {
        Ok(schedule) => {
            ResearchCapture::configured(schedule, pending, pending_source, state_present, as_of_ms)
        }
        Err(error) => ResearchCapture::invalid(
            research_boundary_reason(&error.to_string()),
            Some(true),
            pending,
            pending_source,
            as_of_ms,
        ),
    }
}

/// One controller projection: the dashboard `owner_research` view and the
/// operational-health research block. The controller samples one completion
/// time and calls this once so both outputs share the same read-only capture.
pub(crate) fn research_projection(
    c: &Config,
    cfg: Option<&PlannerConfig>,
    config_reason: Option<&'static str>,
    p: &nivra_core::execution::Portfolio,
    as_of_ms: i64,
    session_end_ms: Option<i64>,
) -> (Value, ResearchOperational) {
    let capture = capture_research(c, cfg, config_reason, p, as_of_ms, session_end_ms);
    let view = capture.view();
    (view, capture.operational())
}

/// Host-owned lifecycle authority, retained in the original turn independently
/// of model summaries. Version and trigger must agree before recovery.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg(test)]
pub(super) struct Lifecycle {
    pub(super) version: u32,
    pub(super) hashes: AppliedHashes,
    pub(super) task: LifecycleTask,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg(test)]
pub(super) enum LifecycleTask {
    Compaction { up_to_seq: u64 },
    Handoff { target_session: String, seed: Value },
}

#[cfg(test)]
impl Lifecycle {
    pub(super) fn kind(&self) -> RunKind {
        match self.task {
            LifecycleTask::Compaction { .. } => RunKind::Compaction,
            LifecycleTask::Handoff { .. } => RunKind::Handoff,
        }
    }
}

#[cfg(test)]
pub(super) fn persist_handoff_seed(
    root: &std::path::Path,
    lifecycle: &Lifecycle,
) -> std::result::Result<(), &'static str> {
    if let LifecycleTask::Handoff {
        target_session,
        seed,
    } = &lifecycle.task
    {
        session::write_seed(root, target_session, seed).map_err(|_| "session_seed_failed")?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn lifecycle_for(
    turn: &session::Turn,
) -> std::result::Result<Option<Lifecycle>, &'static str> {
    let expected = match turn.trigger.as_str() {
        "compaction" => Some(RunKind::Compaction),
        "handoff" => Some(RunKind::Handoff),
        _ => None,
    };
    let Some(raw) = turn.request.get("host_lifecycle") else {
        return if expected.is_some() {
            Err("supervisor_legacy_lifecycle_pending")
        } else {
            Ok(None)
        };
    };
    let lifecycle: Lifecycle =
        serde_json::from_value(raw.clone()).map_err(|_| "supervisor_lifecycle_invalid")?;
    if lifecycle.version != 1 || expected != Some(lifecycle.kind()) {
        return Err("supervisor_lifecycle_invalid");
    }
    match &lifecycle.task {
        LifecycleTask::Compaction { up_to_seq } if *up_to_seq == 0 => {
            return Err("supervisor_lifecycle_invalid");
        }
        LifecycleTask::Handoff {
            target_session,
            seed,
        } => {
            let source = NaiveDate::parse_from_str(&turn.session, "%Y-%m-%d")
                .map_err(|_| "supervisor_lifecycle_invalid")?;
            let target = NaiveDate::parse_from_str(target_session, "%Y-%m-%d")
                .map_err(|_| "supervisor_lifecycle_invalid")?;
            if target <= source
                || target_session != &target.to_string()
                || seed["source_session"] != turn.session
                || seed["target_session"] != *target_session
                || (seed["schema_version"] == 2 && seed["source_turn_id"] != turn.id)
                || session::validate_seed(seed, target_session).is_err()
            {
                return Err("supervisor_lifecycle_invalid");
            }
        }
        _ => {}
    }
    Ok(Some(lifecycle))
}
