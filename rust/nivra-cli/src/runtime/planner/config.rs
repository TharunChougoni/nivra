//! Role model configuration and validation shared by planner call sites.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ModelRole {
    /// Independent application credential/approval configuration for this role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_config: Option<PathBuf>,
    pub model_id: String,
    #[serde(default)]
    pub transport: nivra_cli::native_transport::Transport,
    pub enabled: bool,
    pub tools: Vec<String>,
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_timeout_ms: Option<u64>,
    pub output_cap: u32,
    pub daily_call_budget: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_read_tools: Option<usize>,
    #[serde(skip_serializing_if = "ModelRole::default_max_analysis_tools")]
    pub max_analysis_tools: usize,
    /// Explicit fallback for one call whose primary failed with a usage, rate
    /// or quota limit (#245). Absent keeps every historical policy byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<FallbackProfile>,
}
/// Reviewed fallback profile. It is admitted only because the sealed policy
/// declares it; it has no call budget of its own and never replaces the
/// primary for any other failure. The credential is an application-owned
/// model config naming a private API key file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FallbackProfile {
    pub model_id: String,
    pub transport: nivra_cli::native_transport::FallbackTransport,
    pub model_config: PathBuf,
    /// `max_tokens` of the fallback request; omission uses the role cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_cap: Option<u32>,
    /// Provider `reasoning_effort`, sent only when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}
/// Largest fallback `max_tokens` accepted (DeepSeek Flash on Mistral: 128k).
pub(crate) const MAX_FALLBACK_OUTPUT_CAP: u32 = 131_072;
impl FallbackProfile {
    pub(crate) fn invalid(&self) -> bool {
        self.model_id.is_empty()
            || self.model_id.len() > 128
            || !self
                .model_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-._:/".contains(c))
            || !self.model_config.is_absolute()
            || self
                .output_cap
                .is_some_and(|cap| !(64..=MAX_FALLBACK_OUTPUT_CAP).contains(&cap))
            || self
                .reasoning_effort
                .as_deref()
                .is_some_and(|effort| !["low", "medium", "high", "max"].contains(&effort))
    }
    #[cfg(test)]
    pub(crate) fn max_tokens(&self, role: &ModelRole) -> u32 {
        self.output_cap.unwrap_or(role.output_cap)
    }
}
impl Default for ModelRole {
    fn default() -> Self {
        Self {
            model_config: None,
            model_id: String::new(),
            transport: Default::default(),
            enabled: false,
            tools: vec![],
            timeout_ms: 20000,
            run_timeout_ms: None,
            output_cap: 4096,
            daily_call_budget: 0,
            budget_unit: None,
            max_turns: None,
            max_read_tools: None,
            max_analysis_tools: 4,
            fallback: None,
        }
    }
}
/// Serialized provider envelope bound as `output_cap` tokens times this many
/// bytes. 2026-10-29 paper grok-4.7 turns with web_search serialized to
/// ~128–139 KiB, clipping the previous ×16 (128 KiB) check after HTTP 200.
pub(crate) const OUTPUT_BYTE_CAP_MULTIPLIER: usize = 64;
impl ModelRole {
    fn default_max_analysis_tools(v: &usize) -> bool {
        *v == 4
    }
    pub(crate) fn run_budget_ms(&self) -> u64 {
        self.run_timeout_ms.unwrap_or(self.timeout_ms)
    }
    pub(crate) fn output_byte_limit(&self) -> usize {
        (self.output_cap as usize).saturating_mul(OUTPUT_BYTE_CAP_MULTIPLIER)
    }
    /// Live provider path (`planner_output_cap`) and `call_guarded` share this
    /// bound. Empty output and a body past `output_cap × 64` are refused.
    /// A body longer than the old `output_cap × 16` cap and within this limit
    /// is accepted.
    pub(crate) fn provider_envelope_code(&self, text: &str) -> Option<&'static str> {
        if text.is_empty() || text.len() > self.output_byte_limit() {
            Some("planner_output_cap")
        } else {
            None
        }
    }
}
pub(super) fn role_invalid(role: &ModelRole, max_run: u64) -> bool {
    role.fallback.as_ref().is_some_and(FallbackProfile::invalid)
        || role.max_analysis_tools > 64
        || role.max_turns == Some(0)
        || role.max_read_tools == Some(0)
        || role
            .run_timeout_ms
            .is_some_and(|n| n < role.timeout_ms || n > max_run)
        || !(1..=max_run.min(150_000)).contains(&role.timeout_ms)
        || !(64..=16384).contains(&role.output_cap)
        || role
            .budget_unit
            .as_deref()
            .is_some_and(|s| !["request", "event"].contains(&s))
        || role
            .tools
            .iter()
            .any(|s| !["x_search", "web_search"].contains(&s.as_str()))
        || (role.enabled
            && (role.model_id.is_empty()
                || role.model_id.len() > 128
                || role
                    .model_id
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace())
                || role.daily_call_budget == 0))
}
