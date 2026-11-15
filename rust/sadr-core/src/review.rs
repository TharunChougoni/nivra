//! Bounded after-close review. A failed research step does not block the sealed report.

use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewStep {
    policy_hash: String,
    planned: bool,
    denial: Option<ReviewError>,
}

impl ReviewStep {
    pub fn policy_hash(&self) -> &str {
        &self.policy_hash
    }

    pub fn planned(&self) -> bool {
        self.planned
    }

    pub fn executed(&self) -> bool {
        false
    }

    pub fn blocks_close_report(&self) -> bool {
        false
    }

    pub fn denial(&self) -> Option<ReviewError> {
        self.denial
    }

    pub fn trading_authorized(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewError {
    Budget,
    UnknownTool,
    PolicyChange,
}

impl fmt::Display for ReviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Budget => "review_budget_exhausted",
            Self::UnknownTool => "review_tool_unknown",
            Self::PolicyChange => "review_policy_change_rejected",
        })
    }
}

impl Error for ReviewError {}

pub fn attempt(
    turns_left: u32,
    tool: &str,
    allow: &[&str],
    policy_hash: &str,
    proposed_policy_hash: &str,
) -> ReviewStep {
    let denial = if turns_left == 0 {
        Some(ReviewError::Budget)
    } else if proposed_policy_hash != policy_hash {
        Some(ReviewError::PolicyChange)
    } else if !allow.contains(&tool) {
        Some(ReviewError::UnknownTool)
    } else {
        None
    };
    ReviewStep {
        policy_hash: policy_hash.to_string(),
        planned: denial.is_none(),
        denial,
    }
}
