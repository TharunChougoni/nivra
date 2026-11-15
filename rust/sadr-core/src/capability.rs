//! Offline capability readiness. Fixture evidence cannot become autonomous READY.
//!
//! ₹0 configured paid spend and disabled paid fallback are fixed. This module
//! does not call a provider, open a socket, or authorize installation or trading.

use crate::time::Timestamp;
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofKind {
    Missing,
    OfflineOnly,
    WebOnly,
    XOnly,
    Independent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityManifest {
    pub evidence_present: bool,
    pub exact_model_id: Option<String>,
    pub evidence_expires_at: Option<Timestamp>,
    pub now: Timestamp,
    pub proof: ProofKind,
    /// Paise. The contract is ₹0, so only zero is accepted.
    pub configured_paid_paise: i64,
    pub paid_fallback_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocker {
    MissingEvidence,
    UnknownExpiry,
    ExpiredEvidence,
    AbsentExactModel,
    WebOnlyProof,
    XOnlyProof,
    OfflineOnlyEvidence,
    ExternalGateOpen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessReport {
    blockers: Vec<Blocker>,
}

impl ReadinessReport {
    pub fn blockers(&self) -> &[Blocker] {
        &self.blockers
    }

    pub fn autonomous_ready(&self) -> bool {
        false
    }

    pub fn trading_authorized(&self) -> bool {
        false
    }

    pub fn installation_authorized(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityError {
    WeakenedSpend,
    WeakenedFallback,
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WeakenedSpend => "paid_spend_weakened",
            Self::WeakenedFallback => "paid_fallback_weakened",
        })
    }
}

impl Error for CapabilityError {}

/// Deny autonomous readiness. A clean fixture still leaves the external gate open.
pub fn assess_capability(
    manifest: &CapabilityManifest,
) -> Result<ReadinessReport, CapabilityError> {
    if manifest.configured_paid_paise != 0 {
        return Err(CapabilityError::WeakenedSpend);
    }
    if manifest.paid_fallback_enabled {
        return Err(CapabilityError::WeakenedFallback);
    }
    let mut blockers = Vec::new();
    if !manifest.evidence_present || manifest.proof == ProofKind::Missing {
        blockers.push(Blocker::MissingEvidence);
    }
    match manifest.evidence_expires_at {
        None => blockers.push(Blocker::UnknownExpiry),
        Some(expires) if expires <= manifest.now => blockers.push(Blocker::ExpiredEvidence),
        Some(_) => {}
    }
    if !exact_model(&manifest.exact_model_id) {
        blockers.push(Blocker::AbsentExactModel);
    }
    match manifest.proof {
        ProofKind::WebOnly => blockers.push(Blocker::WebOnlyProof),
        ProofKind::XOnly => blockers.push(Blocker::XOnlyProof),
        ProofKind::OfflineOnly => blockers.push(Blocker::OfflineOnlyEvidence),
        ProofKind::Missing | ProofKind::Independent => {}
    }
    blockers.push(Blocker::ExternalGateOpen);
    Ok(ReadinessReport { blockers })
}

fn exact_model(value: &Option<String>) -> bool {
    let Some(value) = value.as_deref() else {
        return false;
    };
    let mut count = 0;
    for byte in value.bytes() {
        count += 1;
        let allowed = byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-');
        if !allowed || count > 120 {
            return false;
        }
    }
    count >= 1
}
