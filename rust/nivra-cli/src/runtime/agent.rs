//! The pi agent service. pi owns every model call (providers, sessions,
//! compaction, cost); Rust decides when a lane turn runs, serves its tools,
//! validates its plan and enforces its budgets. Default off: without a
//! `planner.agent` block no agent process starts.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) mod check;
mod client;
mod lanes;
mod protocol;
mod tools;

pub(crate) use client::AgentClient;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LaneSettings {
    pub model: ModelSpec,
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
}

impl AgentSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        let ok = self.node.is_absolute()
            && self.entry.is_absolute()
            && self.agent_dir.is_absolute()
            && self.prompts.as_ref().is_none_or(|p| p.is_absolute())
            && !self.lanes.is_empty()
            && self.lanes.values().all(|lane| {
                !lane.model.provider.is_empty()
                    && !lane.model.id.is_empty()
                    && lane.daily_calls > 0
                    && lane.daily_cost_usd.is_finite()
                    && lane.daily_cost_usd > 0.0
                    && (1_000..=3_600_000).contains(&lane.turn_timeout_ms)
                    && (1..=200).contains(&lane.max_tool_calls)
            });
        if ok {
            Ok(())
        } else {
            Err("agent_settings_invalid".into())
        }
    }
    pub(crate) fn lane(&self, lane: Lane) -> Result<&LaneSettings> {
        self.lanes
            .get(&lane)
            .ok_or_else(|| "agent_lane_not_configured".into())
    }
}

#[cfg(test)]
mod tests;
