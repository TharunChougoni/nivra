//! The pi agent service. pi owns every model call (providers, sessions,
//! compaction, cost); Rust decides when a lane turn runs, serves its tools,
//! validates its plan and enforces its budgets. Default off: without a
//! `planner.agent` block no agent process starts.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod brief;
pub(crate) mod check;
mod client;
mod lanes;
pub(crate) mod paper;
mod protocol;
mod requests;
pub(crate) mod service;
pub(crate) mod shadow_report;
mod tools;
mod triggers;
pub(crate) mod turn_cli;

pub(crate) use client::{Agent, AgentClient};
pub(crate) use lanes::read_spend;
pub(crate) use triggers::{Ranked, TriggerSettings};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Lane {
    Decision,
    Research,
    Review,
}

impl Lane {
    #[cfg(test)]
    pub(crate) const ALL: [Lane; 3] = [Lane::Decision, Lane::Research, Lane::Review];
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Lane::Decision => "decision",
            Lane::Research => "research",
            Lane::Review => "review",
        }
    }
}

/// The provider and model pi uses for a lane. Switching models is a config
/// change only; the agent resolves any provider pi knows or `models.json` adds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelSpec {
    pub provider: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

impl ModelSpec {
    fn valid(&self) -> bool {
        !self.provider.is_empty() && !self.id.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LaneSettings {
    pub model: ModelSpec,
    /// Runs the turn instead when the primary fails before any output with a
    /// usage limit, an auth failure or a provider rejection; never after a
    /// tool ran. Each switch is journalled with its reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<ModelSpec>,
    /// Model requests per session date, counted from pi's usage reports.
    pub daily_calls: u64,
    /// Spend per session date in US dollars, from pi's cost reports.
    pub daily_cost_usd: f64,
    #[serde(default = "default_turn_timeout_ms")]
    pub turn_timeout_ms: u64,
    #[serde(default = "default_max_tool_calls")]
    pub max_tool_calls: usize,
}

fn default_turn_timeout_ms() -> u64 {
    180_000
}
fn default_max_tool_calls() -> usize {
    40
}

/// What a validated decision plan may do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentMode {
    /// Plans are validated and journalled, never executed. The harness keeps
    /// trading unchanged.
    #[default]
    Shadow,
    /// The sealed pi decision lane owns the paper plan head.
    Paper,
}

impl AgentMode {
    fn is_shadow(&self) -> bool {
        *self == Self::Shadow
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentSettings {
    /// Node 22.19+ executable.
    pub node: PathBuf,
    /// The built agent entry, `agent/dist/src/main.js`.
    pub entry: PathBuf,
    /// The application's own pi directory: `auth.json` and `models.json`.
    /// Rust never reads it; only the agent process does.
    pub agent_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PathBuf>,
    pub lanes: BTreeMap<Lane, LaneSettings>,
    #[serde(default, skip_serializing_if = "AgentMode::is_shadow")]
    pub mode: AgentMode,
    /// Environment variable names passed to the agent, such as a key that
    /// `models.json` reads. Nothing else is inherited besides `PATH`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pass_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggers: Option<TriggerSettings>,
}

impl AgentSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        let ok = self.node.is_absolute()
            && self.entry.is_absolute()
            && self.agent_dir.is_absolute()
            && self.prompts.as_ref().is_none_or(|p| p.is_absolute())
            && !self.lanes.is_empty()
            && self.lanes.values().all(|lane| {
                lane.model.valid()
                    && lane.fallback.as_ref().is_none_or(|fallback| {
                        fallback.valid()
                            && (&fallback.provider, &fallback.id)
                                != (&lane.model.provider, &lane.model.id)
                    })
                    && lane.daily_calls > 0
                    && lane.daily_cost_usd.is_finite()
                    && lane.daily_cost_usd > 0.0
                    && (1_000..=3_600_000).contains(&lane.turn_timeout_ms)
                    && (1..=200).contains(&lane.max_tool_calls)
            })
            && self.pass_env.len() <= 16
            && self.pass_env.iter().all(|name| {
                (1..=64).contains(&name.len())
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    && !matches!(name.as_str(), "PATH" | "HOME")
            })
            && self.triggers.as_ref().is_none_or(TriggerSettings::validate);
        if ok {
            Ok(())
        } else {
            Err("agent_settings_invalid".into())
        }
    }
    pub(crate) fn triggers(&self) -> TriggerSettings {
        self.triggers.clone().unwrap_or_default()
    }
    pub(crate) fn lane(&self, lane: Lane) -> Result<&LaneSettings> {
        self.lanes
            .get(&lane)
            .ok_or_else(|| "agent_lane_not_configured".into())
    }
}

#[cfg(test)]
mod tests;
