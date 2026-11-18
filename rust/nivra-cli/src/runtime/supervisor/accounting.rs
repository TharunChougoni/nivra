use super::*;

/// Compaction trigger bounds. `max_turns` preserves the legacy count; an
/// optional measured context ceiling adds an earlier trigger. Byte counts are
/// the reconstructed summary and completed exchanges, and `reserve_bytes`
/// covers the static prompt, notes, seed, current facts and result headroom.
/// `max_request_bytes` bounds the full provider request (rendered prompt,
/// standing instructions, tool definitions and envelope), using the measured
/// request envelope when a provider call has supplied one (#224).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Compaction {
    pub(super) max_turns: usize,
    pub(super) max_context_bytes: Option<u64>,
    pub(super) max_request_bytes: u64,
    pub(super) reserve_bytes: u64,
    /// Measured bytes of the full provider request beyond the JSON-escaped
    /// rendered prompt (standing instructions, tool definitions and envelope).
    /// Runtime evidence from the last provider call, never profile input;
    /// absent keeps the historical prompt-only bound (#224).
    pub(super) request_envelope_bytes: Option<u64>,
    /// Bounded in-cycle compaction retries after context exhaustion (#224).
    pub(super) context_retry_limit: u32,
    /// Backoff after those retries fail, before another compaction attempt.
    pub(super) context_backoff_ms: i64,
    /// Request size, as a percent of `max_request_bytes`, at or above which an
    /// incomplete provider response is treated as context exhaustion.
    pub(super) near_cap_percent: u64,
}

impl Default for Compaction {
    fn default() -> Self {
        Self {
            max_turns: COMPACT_AFTER_TURNS,
            max_context_bytes: None,
            max_request_bytes: 1_048_576,
            reserve_bytes: 16_384,
            request_envelope_bytes: None,
            context_retry_limit: 1,
            context_backoff_ms: 900_000,
            near_cap_percent: 90,
        }
    }
}

impl Compaction {
    /// Read optional bounds from the experiment profile's free-form supervisor
    /// object. Invalid values refuse instead of silently falling back.
    pub(super) fn from_profile(raw: Option<&Value>) -> std::result::Result<Self, &'static str> {
        let Some(raw) = raw.and_then(|raw| raw.get("compaction")) else {
            return Ok(Self::default());
        };
        let object = raw.as_object().ok_or("supervisor_compaction_invalid")?;
        let mut next = Self::default();
        if let Some(value) = object.get("max_turns") {
            let value = value.as_u64().ok_or("supervisor_compaction_invalid")?;
            if !(1..=256).contains(&value) {
                return Err("supervisor_compaction_invalid");
            }
            next.max_turns = value as usize;
        }
        if let Some(value) = object.get("max_context_bytes") {
            let value = value.as_u64().ok_or("supervisor_compaction_invalid")?;
            if !(4_096..=8_388_608).contains(&value) {
                return Err("supervisor_compaction_invalid");
            }
            next.max_context_bytes = Some(value);
        }
        if let Some(value) = object.get("reserve_bytes") {
            let value = value.as_u64().ok_or("supervisor_compaction_invalid")?;
            if value > 1_048_576 {
                return Err("supervisor_compaction_invalid");
            }
            next.reserve_bytes = value;
        }
        if let Some(value) = object.get("max_request_bytes") {
            let value = value.as_u64().ok_or("supervisor_compaction_invalid")?;
            if !(4_096..=8_388_608).contains(&value) {
                return Err("supervisor_compaction_invalid");
            }
            next.max_request_bytes = value;
        }
        if let Some(value) = object.get("context_retry_limit") {
            let value = value.as_u64().ok_or("supervisor_compaction_invalid")?;
            if !(1..=3).contains(&value) {
                return Err("supervisor_compaction_invalid");
            }
            next.context_retry_limit = value as u32;
        }
        if let Some(value) = object.get("context_backoff_ms") {
            let value = value.as_i64().ok_or("supervisor_compaction_invalid")?;
            if !(60_000..=3_600_000).contains(&value) {
                return Err("supervisor_compaction_invalid");
            }
            next.context_backoff_ms = value;
        }
        if let Some(value) = object.get("near_cap_percent") {
            let value = value.as_u64().ok_or("supervisor_compaction_invalid")?;
            if !(50..=100).contains(&value) {
                return Err("supervisor_compaction_invalid");
            }
            next.near_cap_percent = value;
        }
        if next
            .max_context_bytes
            .is_some_and(|limit| next.reserve_bytes >= limit)
            || next.reserve_bytes >= next.max_request_bytes
        {
            return Err("supervisor_compaction_invalid");
        }
        Ok(next)
    }
}

/// The full provider request size implied by an already JSON-escaped prompt:
/// the escaped prompt plus the measured envelope. Without a measured envelope
/// this is the historical prompt-only measure.
#[cfg(test)]
pub(super) fn full_request_bytes(escaped_prompt_bytes: u64, bounds: &Compaction) -> u64 {
    escaped_prompt_bytes.saturating_add(bounds.request_envelope_bytes.unwrap_or(0))
}

/// A rendered prompt fits only when the full provider request plus the reserve
/// stays under the configured cap (#224).
#[cfg(test)]
pub(super) fn request_fits(escaped_prompt_bytes: u64, bounds: &Compaction) -> bool {
    full_request_bytes(escaped_prompt_bytes, bounds).saturating_add(bounds.reserve_bytes)
        <= bounds.max_request_bytes
}

/// The JSON-escaped length of a rendered prompt, as embedded in the provider
/// request body. A serialization failure falls back to the raw length.
#[cfg(test)]
pub(super) fn escaped_prompt_bytes(prompt: &str) -> u64 {
    serde_json::to_string(prompt)
        .map(|escaped| escaped.len() as u64)
        .unwrap_or(prompt.len() as u64)
}

#[cfg(test)]
pub(super) fn compaction_due(turns: usize, context_bytes: u64, settings: &Compaction) -> bool {
    turns >= settings.max_turns
        || context_bytes
            .saturating_add(settings.request_envelope_bytes.unwrap_or(0))
            .saturating_add(settings.reserve_bytes)
            > settings.max_request_bytes
        || settings
            .max_context_bytes
            .is_some_and(|limit| context_bytes.saturating_add(settings.reserve_bytes) > limit)
}

/// The applied inputs the supervisor has actually seen. A change wake keys on
/// these hashes, never on file mtimes or the supervisor's own writes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppliedHashes {
    pub(super) version: String,
    pub(super) profile: String,
    pub(super) prompts: String,
    pub(super) config: String,
}

#[cfg(test)]
impl AppliedHashes {
    pub(super) fn current(cfg: &PlannerConfig) -> Self {
        let prompts =
            serde_json::to_vec(&cfg.prompt_hashes().unwrap_or(Value::Null)).unwrap_or_default();
        Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            profile: cfg.profile_sha256().unwrap_or_default().to_owned(),
            prompts: super::super::digest(&prompts),
            config: planner::policy_bytes(cfg)
                .map(|bytes| super::super::digest(&bytes))
                .unwrap_or_default(),
        }
    }

    pub(super) fn changed_from(&self, prior: &Self) -> bool {
        self != prior
    }
}

#[cfg(test)]
struct LifecycleResult {
    seq: u64,
    trigger: String,
    hashes: AppliedHashes,
    kind: RunKind,
    result: session::TurnResult,
    selection_binding: Option<String>,
}

/// Apply state effects from durable terminal results exactly once. The cursor
/// and the state transition share one atomic state-file replacement, so a
/// crash after the result frame cannot double-count a run or lose cooldown.
#[cfg(test)]
pub(super) fn reconcile_lifecycle_results(
    root: &std::path::Path,
    session_id: &str,
) -> std::result::Result<bool, &'static str> {
    let state = load(root, session_id)?;
    let log = session::SessionLog::new(root, session_id);
    let records = log.records();
    let through = records.last().map_or(0, |record| record.seq);
    if through < state.lifecycle_result_seq {
        return Err("supervisor_lifecycle_cursor_invalid");
    }
    if through == state.lifecycle_result_seq {
        return Ok(false);
    }
    let mut turns = std::collections::BTreeMap::new();
    for record in &records {
        if let session::Body::Turn { turn } = &record.body
            && turns
                .insert(turn.id.clone(), (record.seq, turn.clone()))
                .is_some()
        {
            return Err("supervisor_lifecycle_turn_duplicate");
        }
    }
    let mut lifecycle_results = Vec::new();
    let mut seen_lifecycle_turns = std::collections::BTreeSet::new();
    for record in &records {
        let session::Body::Result { result } = &record.body else {
            continue;
        };
        let Some((turn_seq, turn)) = turns.get(&result.turn_id) else {
            continue;
        };
        if record.seq <= *turn_seq {
            return Err("supervisor_lifecycle_record_order_invalid");
        }
        if turn.session != session_id {
            return Err("supervisor_lifecycle_invalid");
        }
        let (hashes, kind) = if turn.trigger == "decision"
            || turn.request.get("host_decision").is_some()
        {
            decision_for(turn)?.ok_or("owner_turn_binding_invalid")?;
            let hashes = serde_json::from_value(turn.request["owner_applied_hashes"].clone())
                .map_err(|_| "owner_turn_binding_invalid")?;
            (hashes, RunKind::Decision)
        } else if turn.trigger == "review" || turn.request["owner_task"]["kind"] == "review" {
            // Review evidence is validated by the review path. Accounting
            // only needs the terminal result and its original timestamp.
            (AppliedHashes::default(), RunKind::Review)
        } else if turn.trigger == "research" || turn.request["owner_task"]["kind"] == "research" {
            // Research runs in its own lane log (#237). A legacy research turn
            // in the trading log never feeds trading accounting.
            continue;
        } else {
            let lifecycle_trigger = matches!(turn.trigger.as_str(), "compaction" | "handoff");
            let has_metadata = turn.request.get("host_lifecycle").is_some();
            if !has_metadata {
                continue;
            }
            if !lifecycle_trigger {
                return Err("supervisor_lifecycle_invalid");
            }
            let lifecycle = lifecycle_for(turn)?.ok_or("supervisor_lifecycle_invalid")?;
            (lifecycle.hashes.clone(), lifecycle.kind())
        };
        if !seen_lifecycle_turns.insert(result.turn_id.as_str()) {
            return Err("supervisor_lifecycle_result_duplicate");
        }
        if record.seq <= state.lifecycle_result_seq {
            continue;
        }
        lifecycle_results.push(LifecycleResult {
            seq: record.seq,
            trigger: turn.trigger.clone(),
            hashes,
            kind,
            result: result.clone(),
            selection_binding: recorded_selection_binding(turn),
        });
    }
    with_state(root, session_id, |state| {
        let mut applied = false;
        for entry in &lifecycle_results {
            if entry.seq <= state.lifecycle_result_seq {
                continue;
            }
            let succeeded = matches!(
                entry.result.outcome.as_str(),
                "ok" | "recovered_lifecycle_receipt"
                    | "recovered_decision_receipt"
                    | "recovered_review_receipt"
                    | "recovered_research_receipt"
                    | "recovered_action_receipt"
            ) && entry.result.reason.is_none();
            apply_finish(
                state,
                entry.kind,
                reason_static(&entry.trigger),
                entry.result.finished_at_ms,
                succeeded,
                &entry.hashes,
                entry.selection_binding.as_deref(),
            );
            if let Some(reason) = entry.result.reason.as_deref() {
                state.block_tool_failure(reason, entry.result.finished_at_ms, &entry.hashes);
            }
            if entry.kind == RunKind::Compaction {
                if succeeded {
                    state.compaction_failed = None;
                } else {
                    let reason = entry
                        .result
                        .reason
                        .as_deref()
                        .map(supervisor_failure_reason)
                        .unwrap_or_else(|| "supervisor_lifecycle_failed".into());
                    state.compaction_failed = Some(CompactionFailure {
                        reason,
                        at_ms: entry.result.finished_at_ms,
                    });
                }
            }
            state.lifecycle_result_seq = entry.seq;
            applied = true;
        }
        state.lifecycle_result_seq = state.lifecycle_result_seq.max(through);
        applied
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
pub(super) enum RunKind {
    Supervisor,
    Decision,
    Review,
    Research,
    Compaction,
    Handoff,
    Aside,
}
