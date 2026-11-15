//! Frozen scanner evidence for Luna. Screening observations never authorize fills.
use super::*;
use sadr_core::execution::{Instrument, Scanner};
use serde_json::{Value, json};

#[cfg(test)]
mod handoff;
mod projection;
mod settings;
#[cfg(test)]
pub(crate) use handoff::*;
pub(crate) use projection::*;
pub(crate) use settings::*;

pub(crate) fn snapshot(
    c: &Config,
    instruments: &[Instrument],
    settings: &Settings,
    now: i64,
) -> Result<Value> {
    if !c.directory.join("paper.sqlite").exists() {
        return Ok(project(&Scanner::default(), instruments, settings, now));
    }
    let db = Connection::open_with_flags(
        c.directory.join("paper.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    use rusqlite::OptionalExtension;
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='scanner')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(project(&Scanner::default(), instruments, settings, now));
    }
    let Some(bytes) = db
        .query_row("SELECT body FROM scanner WHERE id=1", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    else {
        return Ok(project(&Scanner::default(), instruments, settings, now));
    };
    if bytes.len() > 16_000_000 {
        return Err("planner_scanner_snapshot_invalid".into());
    }
    let scan: Scanner = serde_json::from_str(&bytes)?;
    Ok(project(&scan, instruments, settings, now))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PositionReview {
    pub instrument_key: String,
    pub opinion: String,
    pub thesis_status: String,
    pub worth_keeping_reason: String,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    pub confidence: String,
    #[serde(default)]
    pub suggested_exit: Option<Value>,
}

/// Evidence novelty, with a small configurable floor against model loops.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GateSettings {
    /// Retained for configuration/hash compatibility; no longer enforced.
    pub min_interval_ms: i64,
    /// Retained for configuration/hash compatibility; no longer enforced.
    pub max_per_hour: usize,
    pub material_move_bps: f64,
    pub exit_proximity_bps: f64,
    pub require_thesis: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anti_loop_floor_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material_quote_change_bps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_max_bytes: Option<usize>,
    /// A digest whose `handoff` is null or absent is a Luna `wait`, not an invalid plan (#43).
    /// Omission means true; explicit false keeps the strict `luna_handoff_required` rejection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub null_handoff_as_wait: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_review_floor_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_review_admits: Option<Vec<String>>,
}
impl Default for GateSettings {
    fn default() -> Self {
        Self {
            min_interval_ms: 0,
            max_per_hour: 0,
            material_move_bps: 75.0,
            exit_proximity_bps: 50.0,
            require_thesis: false,
            anti_loop_floor_ms: None,
            material_quote_change_bps: None,
            history_max_bytes: None,
            null_handoff_as_wait: None,
            position_review_floor_ms: None,
            position_review_admits: None,
        }
    }
}
impl GateSettings {
    #[cfg(test)]
    pub fn null_handoff_as_wait(&self) -> bool {
        self.null_handoff_as_wait.unwrap_or(true)
    }
    pub fn validate(&self) -> Result<()> {
        if self
            .anti_loop_floor_ms
            .is_some_and(|n| !(0..=86_400_000).contains(&n))
            || self
                .material_quote_change_bps
                .is_some_and(|n| !n.is_finite() || n <= 0.0)
            || self
                .history_max_bytes
                .is_some_and(|n| !(1024..=1_073_741_824).contains(&n))
            || self
                .position_review_floor_ms
                .is_some_and(|n| !(0..=86_400_000).contains(&n))
            || self.position_review_admits.as_ref().is_some_and(|admits| {
                admits.is_empty()
                    || admits.len() > 3
                    || admits
                        .iter()
                        .any(|opinion| !["exit", "trim", "revise_exit"].contains(&opinion.as_str()))
            })
            || !self.material_move_bps.is_finite()
            || self.material_move_bps <= 0.0
            || !self.exit_proximity_bps.is_finite()
            || self.exit_proximity_bps < 0.0
        {
            return Err("planner_handoff_gate_invalid".into());
        }
        Ok(())
    }
}

/// The same upper median used by scanner triggers (#20).
pub(crate) fn median(mut values: Vec<f64>) -> f64 {
    values.retain(|v| v.is_finite());
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

#[cfg(test)]
mod thesis_tests;

#[cfg(test)]
mod executable_quote_tests;
#[cfg(test)]
mod strategy_tests;
