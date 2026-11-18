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

/// Last IST minute (15:00) at which a controller may still adopt the day's
/// session, from a sealed predecessor or as a first-ever book.
pub(crate) const LATE_START_CUTOFF_MINUTE: u32 = 900;

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
    /// stays blocked. An eligible seal rolls over from pre-open through the
    /// late-start cutoff; only after it does the day wait for the next session.
    pub(crate) fn from_review(verdict: Option<SealVerdict>, minute_of_day: u32) -> Self {
        match verdict {
            Some(SealVerdict::EligibleToSeal) if minute_of_day >= LATE_START_CUTOFF_MINUTE => {
                Self::MissedPreopenIdle
            }
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
/// Events start pending or reserved. A reserved event settles to the same
/// terminal as its attempt: planned or failed. Attempts add `interrupted` on
/// process restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlannerLogPhase {
    Pending,
    Reserved,
    Planned,
    Failed,
    Interrupted,
}

impl PlannerLogPhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Reserved => "reserved",
            Self::Planned => "planned",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

impl fmt::Display for PlannerLogPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
