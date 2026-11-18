use serde::Serialize;
use serde_json::{Value, json};

// The supported idle interval is the configured bound. Allow one minute for loop
// processing, independently of the two-second financial quote freshness gate.
const MAX_IDLE_MS: i64 = super::config::MAX_IDLE_POLL_INTERVAL_MS as i64;
const PROCESSING_GRACE_MS: i64 = 60_000;
const MAX_SNAPSHOT_AGE_MS: i64 = MAX_IDLE_MS + PROCESSING_GRACE_MS;

/// One loop completion owns both `as_of_ms` and this heartbeat. The deadline is
/// the completion plus the interval the loop actually sleeps, never a request time.
pub(crate) fn heartbeat(completed_at_ms: i64, interval_ms: u64, loop_generation: u64) -> Value {
    json!({
        "version":1,
        "completed_at_ms":completed_at_ms,
        "next_deadline_ms":completed_at_ms.saturating_add(interval_ms as i64),
        "loop_generation":loop_generation,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LivenessStatus {
    Healthy,
    Unknown,
    Stale,
    Future,
    Malformed,
}

#[derive(Debug, Serialize)]
pub(crate) struct ControllerLiveness {
    pub(crate) status: LivenessStatus,
    last_completed_at_ms: Option<i64>,
    age_ms: Option<i64>,
    max_age_ms: i64,
    expected_deadline_ms: Option<i64>,
    loop_generation: Option<u64>,
}

pub(crate) fn controller_liveness(snapshot: &Value, now_ms: i64) -> ControllerLiveness {
    let mut result = ControllerLiveness {
        status: LivenessStatus::Unknown,
        last_completed_at_ms: snapshot["as_of_ms"].as_i64(),
        age_ms: None,
        max_age_ms: MAX_SNAPSHOT_AGE_MS,
        expected_deadline_ms: None,
        loop_generation: None,
    };
    let Some(completed) = result.last_completed_at_ms else {
        if !snapshot["as_of_ms"].is_null() {
            result.status = LivenessStatus::Malformed;
        }
        return result;
    };
    if completed <= 0 || now_ms <= 0 {
        result.status = LivenessStatus::Malformed;
        return result;
    }
    if let Some(heartbeat) = snapshot.get("controller_heartbeat") {
        let deadline = heartbeat["next_deadline_ms"].as_i64();
        let generation = heartbeat["loop_generation"].as_u64();
        if heartbeat["version"].as_u64() != Some(1)
            || heartbeat["completed_at_ms"].as_i64() != Some(completed)
            || generation.is_none()
            || deadline
                .is_none_or(|at| at < completed || at.saturating_sub(completed) > MAX_IDLE_MS)
        {
            result.status = LivenessStatus::Malformed;
            return result;
        }
        result.expected_deadline_ms = deadline;
        result.loop_generation = generation;
        if let Some(deadline) = deadline {
            result.max_age_ms =
                (deadline - completed + PROCESSING_GRACE_MS).min(MAX_SNAPSHOT_AGE_MS);
        }
    }
    if completed > now_ms {
        result.status = LivenessStatus::Future;
        return result;
    }
    let age = now_ms - completed;
    result.age_ms = Some(age);
    result.status = if age > result.max_age_ms {
        LivenessStatus::Stale
    } else {
        LivenessStatus::Healthy
    };
    result
}

/// Retained transport receipts are pruned by the sender after this window.
pub(crate) const ALERT_RETENTION_WINDOW_SECONDS: i64 = 3600;
/// The existing sender caps the retained delivery table at 20 rows. Counts are
/// therefore a lower bound on activity in the window, not lifetime history.
pub(crate) const ALERT_MAX_RETAINED_DELIVERY_ROWS: i64 = 20;
pub(crate) const ALERT_RETAINED_WINDOW_SCOPE: &str =
    "retained_rows_within_window_lower_bound_not_lifetime";
pub(crate) const ALERT_TIMESTAMP_SCOPE: &str = "retained_window_only_not_lifetime";
pub(crate) const ALERT_TRUNCATION_UNKNOWN: &str = "unknown_cap_or_lazy_pruning";
/// The existing sender caps the current host outbox at 64 rows. This is a
/// different scope from the retained delivery window (one hour, 20 rows).
pub(crate) const ALERT_MAX_OUTBOX_ROWS: i64 = 64;
pub(crate) const ALERT_OUTBOX_SCOPE: &str = "current_host_outbox_not_delivery_window";
pub(crate) const ALERT_OUTBOX_ROW_SCOPE: &str = "current_outbox_rows_not_retained_delivery_window";
pub(crate) const ALERT_OUTBOX_DEAD_SCOPE: &str =
    "current_outbox_quarantined_rows_not_delivery_window";

#[derive(Debug, Serialize)]
pub(crate) struct CalendarCoverage {
    pub(crate) source: String,
    pub(crate) covered_from: String,
    pub(crate) covered_to: String,
    pub(crate) current_operational_date: String,
    pub(crate) covered: bool,
    pub(crate) requested_session: String,
    pub(crate) requested_session_covered: bool,
    pub(crate) historical_session: String,
    pub(crate) historical_session_covered: bool,
    pub(crate) days_remaining: i64,
    pub(crate) expiring: bool,
    pub(crate) expired: bool,
    pub(crate) expiry_warning_days: i64,
    pub(crate) expiry_warning_basis: &'static str,
    pub(crate) update_prerequisite: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AlertStoreStatus {
    Available,
    Missing,
    Malformed,
    Unreadable,
    Disabled,
}

#[derive(Debug, Serialize)]
pub(crate) struct AlertScope {
    pub(crate) account: String,
    pub(crate) session: String,
    pub(crate) store_scope: &'static str,
}

impl AlertScope {
    pub(crate) fn admitted(account: &str, session: &str) -> Self {
        Self {
            account: account.to_owned(),
            session: session.to_owned(),
            store_scope: "host_alert_outbox",
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct AlertRetainedWindow {
    pub(crate) start_ms: i64,
    pub(crate) end_ms: i64,
    pub(crate) scope: &'static str,
    pub(crate) max_retained_rows: i64,
    pub(crate) retained_delivery_rows: i64,
    pub(crate) rows_in_window: i64,
    pub(crate) truncation: &'static str,
    pub(crate) clock_anomaly: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct AlertCounts {
    pub(crate) queued: i64,
    pub(crate) retrying: i64,
    pub(crate) transport_accepted: i64,
    pub(crate) transport_accepted_scope: &'static str,
    pub(crate) dead: i64,
    pub(crate) dead_definition: &'static str,
    pub(crate) dropped: Option<i64>,
    pub(crate) in_flight_or_transport_error: i64,
    pub(crate) in_flight_or_transport_error_definition: &'static str,
    /// The queued/retrying/dead counts are current host-outbox rows, capped at
    /// 64, not rows in the retained delivery window.
    pub(crate) outbox_scope: &'static str,
    pub(crate) outbox_max_rows: i64,
    pub(crate) queued_scope: &'static str,
    pub(crate) retrying_scope: &'static str,
    pub(crate) dead_scope: &'static str,
    pub(crate) in_flight_or_transport_error_scope: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct AlertTransportStatus {
    pub(crate) status: AlertStoreStatus,
    pub(crate) store: &'static str,
    pub(crate) counts_scope: &'static str,
    pub(crate) scope: AlertScope,
    pub(crate) counts: Option<AlertCounts>,
    pub(crate) retained_window: Option<AlertRetainedWindow>,
    pub(crate) last_accepted_at_ms: Option<i64>,
    pub(crate) last_accepted_scope: &'static str,
    pub(crate) last_failure_at_ms: Option<i64>,
    pub(crate) last_failure_category: Option<&'static str>,
    pub(crate) last_failure_scope: &'static str,
    pub(crate) failure_marker_present: bool,
    pub(crate) failure_marker_definition: &'static str,
    pub(crate) retention_window_seconds: i64,
    pub(crate) timestamp_normalization: &'static str,
    pub(crate) dropped_source: &'static str,
    pub(crate) ack_meaning: &'static str,
    pub(crate) delivery_guarantee: bool,
}

impl AlertTransportStatus {
    pub(crate) fn unavailable(
        status: AlertStoreStatus,
        scope: AlertScope,
        failure_marker_present: bool,
    ) -> Self {
        Self {
            status,
            store: "alert_outbox",
            counts_scope: "host_alert_outbox_not_account_filtered",
            scope,
            counts: None,
            retained_window: None,
            last_accepted_at_ms: None,
            last_accepted_scope: ALERT_TIMESTAMP_SCOPE,
            last_failure_at_ms: None,
            last_failure_category: None,
            last_failure_scope: ALERT_TIMESTAMP_SCOPE,
            failure_marker_present,
            failure_marker_definition: "marker_presence_not_attempt_outcome",
            retention_window_seconds: ALERT_RETENTION_WINDOW_SECONDS,
            timestamp_normalization: "delivery_at_unix_seconds_checked_mul_1000_unknown_on_overflow",
            dropped_source: "not_recorded",
            ack_meaning: "sender_http_2xx_receipt_not_human_read",
            delivery_guarantee: false,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct OperationalHealth {
    pub(crate) version: u32,
    pub(crate) captured_at_ms: i64,
    pub(crate) calendar: CalendarCoverage,
    pub(crate) alert_transport: AlertTransportStatus,
}

pub(crate) fn capture(
    captured_at_ms: i64,
    calendar: CalendarCoverage,
    alert_transport: AlertTransportStatus,
) -> OperationalHealth {
    OperationalHealth {
        version: 2,
        captured_at_ms,
        calendar,
        alert_transport,
    }
}
