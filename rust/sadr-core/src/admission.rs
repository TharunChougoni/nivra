//! Offline install guard. A passing window still does not start a release.

use crate::time::LocalTime;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallGuard {
    pub now: LocalTime,
    pub exchange_hours: bool,
    pub hold: bool,
    pub work_active: bool,
    pub clock_trusted: bool,
    pub artifact_hash_ok: bool,
    pub resource_evidence_fresh: bool,
    pub interrupted: bool,
    pub downgrade: bool,
    pub prerelease: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardReason {
    Blackout,
    ExchangeHours,
    Hold,
    WorkActive,
    UntrustedClock,
    Artifact,
    Resources,
    Interrupted,
    Downgrade,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    admitted: bool,
    reasons: Vec<GuardReason>,
    prerelease: bool,
}

impl Admission {
    pub fn admitted(&self) -> bool {
        self.admitted
    }

    pub fn would_start(&self) -> bool {
        false
    }

    pub fn stable_python_wheel(&self) -> bool {
        false
    }

    pub fn prerelease(&self) -> bool {
        self.prerelease
    }

    pub fn reasons(&self) -> &[GuardReason] {
        &self.reasons
    }

    pub fn trading_authorized(&self) -> bool {
        false
    }

    pub fn installation_authorized(&self) -> bool {
        false
    }
}

pub fn admit(guard: &InstallGuard) -> Admission {
    let mut reasons = Vec::new();
    if in_blackout(guard.now) {
        reasons.push(GuardReason::Blackout);
    }
    if guard.exchange_hours {
        reasons.push(GuardReason::ExchangeHours);
    }
    if guard.hold {
        reasons.push(GuardReason::Hold);
    }
    if guard.work_active {
        reasons.push(GuardReason::WorkActive);
    }
    if !guard.clock_trusted {
        reasons.push(GuardReason::UntrustedClock);
    }
    if !guard.artifact_hash_ok {
        reasons.push(GuardReason::Artifact);
    }
    if !guard.resource_evidence_fresh {
        reasons.push(GuardReason::Resources);
    }
    if guard.interrupted {
        reasons.push(GuardReason::Interrupted);
    }
    if guard.downgrade {
        reasons.push(GuardReason::Downgrade);
    }
    Admission {
        admitted: reasons.is_empty(),
        reasons,
        prerelease: guard.prerelease,
    }
}

fn in_blackout(now: LocalTime) -> bool {
    let start = LocalTime::parse("09:00").expect("constant local time");
    let end = LocalTime::parse("15:35").expect("constant local time");
    now >= start && now <= end
}

impl fmt::Display for GuardReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Blackout => "install_blackout",
            Self::ExchangeHours => "install_exchange_hours",
            Self::Hold => "install_hold",
            Self::WorkActive => "install_work_active",
            Self::UntrustedClock => "install_untrusted_clock",
            Self::Artifact => "install_artifact_untrusted",
            Self::Resources => "install_resources_stale",
            Self::Interrupted => "install_interrupted",
            Self::Downgrade => "install_downgrade_refused",
        })
    }
}
