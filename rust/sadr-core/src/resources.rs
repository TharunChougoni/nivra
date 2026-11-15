//! Conservative memory arithmetic over explicitly supplied evidence.
//!
//! This module neither measures a host nor authenticates workload qualification.
//! A passing calculation never authorizes installation or session activation.

use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

const MIB: u64 = 1024 * 1024;
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub minimum_reserve_bytes: u64,
    pub reserve_divisor: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            minimum_reserve_bytes: 256 * MIB,
            reserve_divisor: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Fixture,
    DeclaredMeasurement,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEvidence {
    pub schema_version: u32,
    pub evidence_kind: EvidenceKind,
    pub host_total_bytes: u64,
    /// None means no cgroup limit was supplied, not proof that no limit exists.
    pub cgroup_limit_bytes: Option<u64>,
    /// Supplied effective available memory, accounting for cgroup headroom.
    pub available_bytes: u64,
    pub other_services_peak_bytes: Option<u64>,
    pub candidate_peak_bytes: Option<u64>,
    pub update_overlap_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryConclusion {
    InsufficientEvidence,
    FitsDeclaredBudget,
    ExceedsDeclaredBudget,
}

#[derive(Debug, Serialize)]
pub struct MemoryReport {
    pub schema_version: u32,
    pub evidence_kind: EvidenceKind,
    pub conclusion: MemoryConclusion,
    pub effective_total_bytes: u64,
    pub reserve_bytes: u64,
    pub required_total_bytes: Option<u64>,
    pub required_available_bytes: Option<u64>,
    pub installation_authorized: bool,
    pub trading_authorized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    UnsupportedSchema,
    InvalidCapacity,
    ArithmeticOverflow,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnsupportedSchema => "unsupported_memory_schema",
            Self::InvalidCapacity => "invalid_memory_capacity",
            Self::ArithmeticOverflow => "memory_arithmetic_overflow",
        })
    }
}

impl Error for MemoryError {}

pub fn assess_memory(input: &MemoryEvidence) -> Result<MemoryReport, MemoryError> {
    assess_memory_with_settings(input, &Settings::default())
}

pub fn assess_memory_with_settings(
    input: &MemoryEvidence,
    settings: &Settings,
) -> Result<MemoryReport, MemoryError> {
    if settings.reserve_divisor == 0 {
        return Err(MemoryError::InvalidCapacity);
    }
    if input.schema_version != 1 {
        return Err(MemoryError::UnsupportedSchema);
    }
    let total = input
        .cgroup_limit_bytes
        .map_or(input.host_total_bytes, |limit| {
            limit.min(input.host_total_bytes)
        });
    if total == 0 || input.available_bytes > total {
        return Err(MemoryError::InvalidCapacity);
    }
    // Round the configured proportional reserve upward.
    let reserve = total
        .div_ceil(settings.reserve_divisor)
        .max(settings.minimum_reserve_bytes);
    let mut report = MemoryReport {
        schema_version: 1,
        evidence_kind: input.evidence_kind,
        conclusion: MemoryConclusion::InsufficientEvidence,
        effective_total_bytes: total,
        reserve_bytes: reserve,
        required_total_bytes: None,
        required_available_bytes: None,
        installation_authorized: false,
        trading_authorized: false,
    };
    let (Some(other), Some(candidate), Some(overlap)) = (
        input.other_services_peak_bytes,
        input.candidate_peak_bytes,
        input.update_overlap_bytes,
    ) else {
        return Ok(report);
    };
    if candidate == 0 {
        return Err(MemoryError::InvalidCapacity);
    }
    let available_needed = candidate
        .checked_add(overlap)
        .and_then(|value| value.checked_add(reserve))
        .ok_or(MemoryError::ArithmeticOverflow)?;
    let total_needed = available_needed
        .checked_add(other)
        .ok_or(MemoryError::ArithmeticOverflow)?;
    report.required_total_bytes = Some(total_needed);
    report.required_available_bytes = Some(available_needed);
    report.conclusion = if total_needed <= total && available_needed <= input.available_bytes {
        MemoryConclusion::FitsDeclaredBudget
    } else {
        MemoryConclusion::ExceedsDeclaredBudget
    };
    Ok(report)
}

#[cfg(test)]
mod settings_defaults {
    use super::*;
    #[test]
    fn omitted_settings_preserve_original_constants() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.minimum_reserve_bytes, 256 * 1024 * 1024);
        assert_eq!(s.reserve_divisor, 4);
    }
}
