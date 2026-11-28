//! Which action strategy the executor runs now. One fold answers the
//! executor, the lane briefs and the dashboard, so they never disagree.
//!
//! Order: an owner pin, then the lane's choice of an accepted version, then
//! the newest eligible version. An accepted version that cannot run on this
//! build, or no accepted version at all, hands over to the configured
//! fallback: the lane's chosen fallback when it is still configured, else the
//! first one. A lane stand-down or an owner pin that cannot run here runs
//! nothing. Paper only.
use super::reference::{self, FALLBACK_PREFIX, ReferenceAction};
use super::{Entry, Library, Resolved, build_mismatch};
use crate::runtime::Result;
use serde_json::Value;

/// What runs and on what authority.
#[derive(Clone, Debug)]
pub(crate) enum Running {
    Accepted { entry: Box<Entry>, report: Value },
    Fallback(ReferenceAction),
}

impl Running {
    /// The id strategy actions, fills and refusals carry.
    pub(crate) fn run_id(&self) -> String {
        match self {
            Self::Accepted { entry, .. } => entry.strategy_id.clone(),
            Self::Fallback(reference) => reference.run_id(),
        }
    }
    pub(crate) fn params(&self) -> &Value {
        match self {
            Self::Accepted { entry, .. } => &entry.params,
            Self::Fallback(reference) => &reference.params,
        }
    }
    /// Prospects shown to each call: the accepted replay's, else the default.
    pub(crate) fn top_n(&self) -> Result<usize> {
        match self {
            Self::Accepted { report, .. } => Ok(report["parameters"]["top_n"]
                .as_u64()
                .map_or(Ok(20), usize::try_from)?),
            Self::Fallback(_) => Ok(20),
        }
    }
}

/// What the planner policy says about strategy choice: the configured
/// fallbacks, and the planner policy hash that each lane choice is bound to.
#[derive(Clone, Debug)]
pub(crate) struct ChoicePolicy {
    pub fallback: Vec<String>,
    pub policy_sha256: String,
    pub python_path: std::path::PathBuf,
    pub allow_research_env: bool,
}

impl Default for ChoicePolicy {
    fn default() -> Self {
        Self {
            fallback: Vec::new(),
            policy_sha256: String::new(),
            python_path: std::path::PathBuf::from("/usr/bin/python3"),
            allow_research_env: false,
        }
    }
}

pub(crate) struct Choice {
    pub resolved: Resolved,
    pub running: Option<Running>,
    /// Why no accepted version runs: set for a fallback and for nothing.
    pub reason: Option<&'static str>,
}

impl Choice {
    /// An entry the lane could select on this build with the current review policy.
    pub(crate) fn eligible_id(&self, library: &Library, policy: &ChoicePolicy) -> Option<&str> {
        if let Some(Running::Accepted { entry, .. }) = &self.running {
            return Some(&entry.strategy_id);
        }
        self.resolved
            .eligible()
            .rev()
            .find(|entry| {
                library
                    .entry_build_mismatch(entry)
                    .is_ok_and(|reason| reason.is_none())
                    && library
                        .entry_review_mismatch(
                            entry,
                            &policy.python_path,
                            policy.allow_research_env,
                        )
                        .is_ok_and(|reason| reason.is_none())
            })
            .map(|entry| entry.strategy_id.as_str())
    }
}

/// A run id names a fallback reference.
pub(crate) fn is_fallback(run_id: &str) -> bool {
    run_id.starts_with(FALLBACK_PREFIX)
}

/// A fill's decision id came from the executor: an accepted strategy id or a
/// fallback run id.
pub(crate) fn is_strategy_run(decision_id: &str) -> bool {
    is_fallback(decision_id)
        || (decision_id.len() == 64 && decision_id.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub(crate) fn choose(library: &Library, policy: &ChoicePolicy) -> Result<Choice> {
    let fallback = &policy.fallback;
    let resolved = library.resolve_for(&policy.policy_sha256)?;
    let lane_choice = resolved
        .lane_selection
        .as_ref()
        .map(|s| s.strategy_id.as_deref());
    let reason = match resolved.active.as_deref() {
        Some(id) => {
            let entry = resolved
                .entries
                .iter()
                .find(|e| e.strategy_id == id)
                .ok_or("strategy_not_found")?
                .clone();
            let report = library.report(&entry)?;
            let mismatch = build_mismatch(&report).or(library.entry_review_mismatch(
                &entry,
                &policy.python_path,
                policy.allow_research_env,
            )?);
            match mismatch {
                None => {
                    return Ok(Choice {
                        resolved,
                        running: Some(Running::Accepted {
                            entry: Box::new(entry),
                            report,
                        }),
                        reason: None,
                    });
                }
                Some(reason) if resolved.pinned.as_deref() == Some(id) => {
                    return Ok(Choice {
                        resolved,
                        running: None,
                        reason: Some(reason),
                    });
                }
                Some(reason) => reason,
            }
        }
        None if resolved.pinned.is_none() && lane_choice == Some(None) => {
            return Ok(Choice {
                resolved,
                running: None,
                reason: Some("strategy_lane_stood_down"),
            });
        }
        None => "no_active_strategy",
    };
    let preferred = lane_choice
        .flatten()
        .and_then(|id| id.strip_prefix(FALLBACK_PREFIX))
        .filter(|name| fallback.iter().any(|f| f == name));
    let running = preferred
        .or(fallback.first().map(String::as_str))
        .and_then(reference::action)
        .filter(|reference| reference.is_reviewed(&policy.python_path))
        .map(Running::Fallback);
    Ok(Choice {
        resolved,
        running,
        reason: Some(reason),
    })
}
