//! Explicit session, planner, and readiness phases that used to live as
//! scattered status strings. Wire values stay the historical snake_case names
//! so JSON, sqlite, and the dashboard keep parsing.

use std::fmt;

/// End-of-session review verdict written as `review.status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SealVerdict {
    EligibleToSeal,
    StopOrHaltHumanRequired,
    UnresolvedPositionsHumanRequired,
}

impl SealVerdict {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::EligibleToSeal => "eligible_to_seal",
            Self::StopOrHaltHumanRequired => "stop_or_halt_human_required",
            Self::UnresolvedPositionsHumanRequired => "unresolved_positions_human_required",
        }
    }

    /// Parse a persisted or API review status, including only the names this
    /// process has ever written.
    pub(crate) fn parse(raw: &str) -> Result<Self, &'static str> {
        match raw {
            "eligible_to_seal" => Ok(Self::EligibleToSeal),
            "stop_or_halt_human_required" => Ok(Self::StopOrHaltHumanRequired),
            "unresolved_positions_human_required" => Ok(Self::UnresolvedPositionsHumanRequired),
            _ => Err("seal_verdict_unknown"),
        }
    }

    /// Review outcome for a session book. `carry_admitted` means remaining
    /// positions are a model-led protective carry, not unresolved exposure.
    pub(crate) fn from_review(
        open_positions: u64,
        carry_admitted: bool,
        halt_needs_human: bool,
        stop_needs_human: bool,
    ) -> Self {
        if open_positions > 0 && !carry_admitted {
            Self::UnresolvedPositionsHumanRequired
        } else if halt_needs_human || stop_needs_human {
            Self::StopOrHaltHumanRequired
        } else {
            Self::EligibleToSeal
        }
    }
}

impl fmt::Display for SealVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Dashboard / preview status for whether the next session may roll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreviewStatus {
    WeekendIdle,
    HolidayIdle,
    CurrentSession,
    RolloverBlocked,
    MissedPreopenIdle,
    RolloverReady,
}

impl PreviewStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::WeekendIdle => "weekend_idle",
            Self::HolidayIdle => "holiday_idle",
            Self::CurrentSession => "current_session",
            Self::RolloverBlocked => "rollover_blocked",
            Self::MissedPreopenIdle => "missed_preopen_idle",
            Self::RolloverReady => "rollover_ready",
        }
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, &'static str> {
        match raw {
            "weekend_idle" => Ok(Self::WeekendIdle),
            "holiday_idle" => Ok(Self::HolidayIdle),
            "current_session" => Ok(Self::CurrentSession),
            "rollover_blocked" => Ok(Self::RolloverBlocked),
            "missed_preopen_idle" => Ok(Self::MissedPreopenIdle),
            "rollover_ready" => Ok(Self::RolloverReady),
            _ => Err("preview_status_unknown"),
        }
    }

    pub(crate) fn from_calendar(weekend: bool) -> Self {
        if weekend {
            Self::WeekendIdle
        } else {
            Self::HolidayIdle
        }
    }

    /// Advance from a completed review. Anything other than an eligible seal
    /// stays blocked; an eligible seal after the 09:15 IST pre-open minute
    /// waits for the next session instead of demanding inspection.
    pub(crate) fn from_review(verdict: Option<SealVerdict>, minute_of_day: u32) -> Self {
        match verdict {
            Some(SealVerdict::EligibleToSeal) if minute_of_day >= 555 => Self::MissedPreopenIdle,
            Some(SealVerdict::EligibleToSeal) => Self::RolloverReady,
            _ => Self::RolloverBlocked,
        }
    }
}

impl fmt::Display for PreviewStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Idle reason shown when no rollover advance happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IdleStatus {
    WeekendIdle,
    HolidayIdle,
    MissedPreopenIdle,
    RolloverBlockedInspectReviewHumanRequired,
}

impl IdleStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::WeekendIdle => "weekend_idle",
            Self::HolidayIdle => "holiday_idle",
            Self::MissedPreopenIdle => "missed_preopen_idle",
            Self::RolloverBlockedInspectReviewHumanRequired => {
                "rollover_blocked_inspect_review_human_required"
            }
        }
    }
}

impl fmt::Display for IdleStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Shared vocabulary for `planner_event_log.state` and `planner_attempt_log.state`.
///
/// Events start pending, then reserve, collapse, or expire. A reserved event
/// settles to the same terminal as its attempt: planned, deferred, superseded,
/// or failed. Attempts add `interrupted` on process restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlannerLogPhase {
    Pending,
    Reserved,
    Collapsed,
    Expired,
    Deferred,
    Planned,
    Superseded,
    Failed,
    Interrupted,
}

impl PlannerLogPhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Reserved => "reserved",
            Self::Collapsed => "collapsed",
            Self::Expired => "expired",
            Self::Deferred => "deferred",
            Self::Planned => "planned",
            Self::Superseded => "superseded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, &'static str> {
        match raw {
            "pending" => Ok(Self::Pending),
            "reserved" => Ok(Self::Reserved),
            "collapsed" => Ok(Self::Collapsed),
            "expired" => Ok(Self::Expired),
            "deferred" => Ok(Self::Deferred),
            "planned" => Ok(Self::Planned),
            "superseded" => Ok(Self::Superseded),
            "failed" => Ok(Self::Failed),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err("planner_log_phase_unknown"),
        }
    }

    /// Attempt settlement written after a reserved claim finishes.
    pub(crate) fn settle(deferred: bool, planned: bool, superseded: bool) -> Self {
        if deferred {
            Self::Deferred
        } else if planned {
            Self::Planned
        } else if superseded {
            Self::Superseded
        } else {
            Self::Failed
        }
    }
}

impl fmt::Display for PlannerLogPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
