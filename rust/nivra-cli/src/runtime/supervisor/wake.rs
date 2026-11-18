//! Supervisor wake configuration and pure scheduling precedence.
#[cfg(test)]
use super::AppliedHashes;
use super::state::State;
use crate::runtime::orchestrator_model::RunFirstSettings;
use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct Wake {
    pub idle_interval_ms: i64,
    pub after_fill: bool,
    pub failure_streak: u32,
    /// Optional owner failure backoff. Absent keeps the historical wake-only
    /// failure streak and leaves old config/state bytes unchanged.
    pub failure_backoff_ms: Option<i64>,
    pub session_open_minute: u32,
    pub pre_close_minute: u32,
    pub min_interval_ms: i64,
    pub wake_luna_floor_ms: i64,
    pub run_first: RunFirstSettings,
    pub change_min_gap_ms: i64,
}

impl Default for Wake {
    fn default() -> Self {
        Self {
            idle_interval_ms: 900_000,
            after_fill: true,
            failure_streak: 3,
            failure_backoff_ms: None,
            session_open_minute: 555,
            pre_close_minute: 910,
            min_interval_ms: 120_000,
            wake_luna_floor_ms: 60_000,
            run_first: RunFirstSettings::default(),
            change_min_gap_ms: 600_000,
        }
    }
}

impl Wake {
    pub(super) fn from_trigger(settings: &super::orchestrator_model::TriggerSettings) -> Self {
        let base = Self::default();
        Self {
            idle_interval_ms: settings.idle_interval_ms.unwrap_or(base.idle_interval_ms),
            after_fill: settings.after_fill.unwrap_or(base.after_fill),
            failure_streak: settings.failure_streak.unwrap_or(base.failure_streak),
            failure_backoff_ms: settings.failure_backoff_ms,
            session_open_minute: settings
                .session_open_minute
                .unwrap_or(base.session_open_minute),
            pre_close_minute: settings.pre_close_minute.unwrap_or(base.pre_close_minute),
            min_interval_ms: settings.min_interval_ms.unwrap_or(base.min_interval_ms),
            wake_luna_floor_ms: settings
                .wake_luna_floor_ms
                .unwrap_or(base.wake_luna_floor_ms),
            run_first: settings.run_first.unwrap_or(base.run_first),
            change_min_gap_ms: settings.change_min_gap_ms.unwrap_or(base.change_min_gap_ms),
        }
    }

    pub(super) fn overlay(&self, raw: &Value) -> std::result::Result<Self, &'static str> {
        let mut next = self.clone();
        let set_i64 = |field: &mut i64, key: &str, floor: i64, ceiling: i64| {
            if let Some(value) = raw.get(key) {
                let value = value.as_i64().ok_or("supervisor_setting_invalid")?;
                if !(floor..=ceiling).contains(&value) {
                    return Err("supervisor_setting_invalid");
                }
                *field = value;
            }
            Ok(())
        };
        set_i64(
            &mut next.idle_interval_ms,
            "idle_interval_ms",
            60_000,
            3_600_000,
        )?;
        set_i64(
            &mut next.min_interval_ms,
            "min_interval_ms",
            30_000,
            900_000,
        )?;
        set_i64(
            &mut next.wake_luna_floor_ms,
            "wake_luna_floor_ms",
            0,
            900_000,
        )?;
        set_i64(
            &mut next.change_min_gap_ms,
            "change_min_gap_ms",
            60_000,
            3_600_000,
        )?;
        if let Some(value) = raw.get("run_first") {
            let object = value.as_object().ok_or("supervisor_setting_invalid")?;
            for (key, field) in [
                ("on_session_start", &mut next.run_first.on_session_start),
                ("on_resume", &mut next.run_first.on_resume),
                ("on_change", &mut next.run_first.on_change),
            ] {
                if let Some(value) = object.get(key) {
                    *field = value.as_bool().ok_or("supervisor_setting_invalid")?;
                }
            }
            if let Some(value) = object.get("hold_luna_ms") {
                let value = value.as_i64().ok_or("supervisor_setting_invalid")?;
                if !(0..=900_000).contains(&value) {
                    return Err("supervisor_setting_invalid");
                }
                next.run_first.hold_luna_ms = value;
            }
        }
        if let Some(value) = raw.get("failure_streak").and_then(Value::as_u64) {
            if !(1..=20).contains(&value) {
                return Err("supervisor_setting_invalid");
            }
            next.failure_streak = value as u32;
        }
        if let Some(value) = raw.get("failure_backoff_ms") {
            let value = value.as_i64().ok_or("supervisor_setting_invalid")?;
            if !(60_000..=3_600_000).contains(&value) {
                return Err("supervisor_setting_invalid");
            }
            next.failure_backoff_ms = Some(value);
        }
        if let Some(value) = raw.get("after_fill").and_then(Value::as_bool) {
            next.after_fill = value;
        }
        Ok(next)
    }
}

impl Wake {
    pub(super) fn validate(&self) -> std::result::Result<(), &'static str> {
        if self
            .failure_backoff_ms
            .is_some_and(|value| !(60_000..=3_600_000).contains(&value))
        {
            return Err("supervisor_setting_invalid");
        }
        Ok(())
    }
}

/// A configured owner failure backoff is active only for the exact model
/// selection that failed and only after that many actual owner failures.
/// Absent config, an old state without a recorded owner streak, an expired
/// window or a changed selection all keep old behaviour. The broad
/// `failures` counter is deliberately not consulted here: review, compaction
/// and host lifecycle failures must not arm an owner backoff.
pub(super) fn failure_backoff_until(
    state: &State,
    wake: &Wake,
    now: i64,
    current_selection: Option<&str>,
) -> Option<i64> {
    let backoff_ms = wake.failure_backoff_ms?;
    if state.owner_failure_streak? < wake.failure_streak {
        return None;
    }
    let last_failure_ms = state.last_failure_ms?;
    if state.last_failure_binding.as_deref() != current_selection {
        return None;
    }
    let until = last_failure_ms.saturating_add(backoff_ms);
    (now < until).then_some(until)
}

#[cfg(test)]
pub(crate) struct Inputs {
    pub minute: u32,
    pub in_session: bool,
    pub fill_count: usize,
    pub enabled: bool,
    pub in_flight: bool,
    pub budget_left: bool,
}

#[cfg(test)]
pub(super) fn due(
    state: &State,
    wake: &Wake,
    now: i64,
    input: &Inputs,
    hashes: &AppliedHashes,
) -> Option<&'static str> {
    if !input.enabled || state.tool_contract_blocked(hashes) {
        return None;
    }
    if !input.in_session || input.in_flight || !input.budget_left {
        return None;
    }
    let cooled = state
        .last_run_ms
        .is_none_or(|last| now.saturating_sub(last) >= wake.min_interval_ms);
    // Run first: a fresh session or a resume before the first successful
    // supervisor run does not wait for the scheduled open minute. Luna's first
    // digest is held by the caller while this run is in flight.
    if !state.open_fired
        && wake.run_first.on_session_start
        && cooled
        && (state.runs == 0 || input.minute >= wake.session_open_minute)
    {
        return Some("session_open");
    }
    if !state.open_fired
        && wake.run_first.on_resume
        && state.runs > 0
        && input.minute >= wake.session_open_minute
        && cooled
    {
        return Some("resume");
    }
    let changed = match state.applied.as_ref() {
        Some(prior) => hashes.changed_from(prior),
        // A legacy state may predate applied-hash tracking; a fresh session is
        // handled by the run-first start/resume path instead of a fake change.
        None => state.runs > 0,
    };
    if wake.run_first.on_change
        && changed
        && state.change_fired.as_ref() != Some(hashes)
        && state
            .change_last_ms
            .is_none_or(|last| now.saturating_sub(last) >= wake.change_min_gap_ms)
        && cooled
    {
        return Some("change");
    }
    if !state.open_fired && input.minute >= wake.session_open_minute && cooled {
        return Some("session_open");
    }
    if !state.pre_close_fired && input.minute >= wake.pre_close_minute && cooled {
        return Some("pre_close");
    }
    if input.fill_count > state.last_fill_count && wake.after_fill && cooled {
        return Some("post_fill");
    }
    if state.failures >= wake.failure_streak && cooled {
        return Some("failure_streak");
    }
    if state
        .last_run_ms
        .is_none_or(|last| now.saturating_sub(last) >= wake.idle_interval_ms)
        && cooled
    {
        return Some("idle");
    }
    None
}
