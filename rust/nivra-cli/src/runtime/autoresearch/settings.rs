//! The planner's `autoresearch` block: whether the loop runs, its campaign,
//! its IST-day ceilings and its per-turn reservation.
use crate::runtime::Result;
use nivra_core::autoresearch::{DayLimits, slug_ok, usd_micros};
use nivra_store::market_archive::BarUnit;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AutoresearchSettings {
    pub enabled: bool,
    pub campaign: CampaignSettings,
    pub day: DaySettings,
    pub turn: TurnSettings,
    /// IST minute on a trading day after which the loop waits for the seal.
    pub stop_minute: u32,
    /// The keep rule's drawdown limit; absent uses the strategy acceptance limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_drawdown_bps: Option<i64>,
    /// Consecutive infrastructure failures that end the batch.
    pub max_consecutive_failures: u32,
    /// Attempts of one frozen experiment before it is abandoned.
    pub max_attempts: u32,
    /// Scored non-improvements that end a family branch.
    pub family_stall: u32,
    /// How long the controller waits before it asks the loop again while
    /// the loop waits for tape, a budget or a window.
    pub idle_poll_ms: i64,
    /// Review learned skill drafts with a review-lane turn.
    pub skill_review: bool,
    /// Forward shadow sessions a paper candidate needs (phase 4).
    pub forward_sessions_paper: u32,
    /// Forward shadow sessions a live candidate would need. Live is out of scope.
    pub forward_sessions_live: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CampaignSettings {
    /// A new name starts a new campaign with fresh frozen bounds.
    pub name: String,
    pub unit: BarUnit,
    /// Previous exchange sessions of outcomes embargoed before each scored fold.
    pub embargo_sessions: u32,
    /// NSE_EQ keys; absent uses the archived cash universe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instruments: Option<Vec<String>>,
    /// Scored variants the campaign may spend. Every trial is counted.
    pub trial_cap: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DaySettings {
    pub variants: u32,
    pub turns: u32,
    pub calls: u64,
    pub usd: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct TurnSettings {
    /// Worst-case billed calls reserved before a turn starts.
    pub calls: u64,
    /// Worst-case cost reserved before a turn starts.
    pub usd: f64,
    pub max_tool_calls: usize,
    pub timeout_ms: u64,
}

impl Default for AutoresearchSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            campaign: CampaignSettings::default(),
            day: DaySettings::default(),
            turn: TurnSettings::default(),
            stop_minute: 8 * 60 + 30,
            max_drawdown_bps: None,
            max_consecutive_failures: 2,
            max_attempts: 3,
            family_stall: 3,
            idle_poll_ms: 300_000,
            skill_review: true,
            forward_sessions_paper: 1,
            forward_sessions_live: 10,
        }
    }
}

impl Default for CampaignSettings {
    fn default() -> Self {
        Self {
            name: "bars-v2".into(),
            unit: BarUnit::Day,
            embargo_sessions: 1,
            instruments: None,
            trial_cap: 60,
        }
    }
}

impl Default for DaySettings {
    fn default() -> Self {
        Self {
            variants: 30,
            turns: 60,
            calls: 200,
            usd: 8.0,
        }
    }
}

impl Default for TurnSettings {
    fn default() -> Self {
        Self {
            calls: 8,
            usd: 0.4,
            max_tool_calls: 8,
            timeout_ms: 120_000,
        }
    }
}

impl AutoresearchSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        let c = &self.campaign;
        let ok = slug_ok(&c.name, 48)
            && c.embargo_sessions <= 20
            && (1..=10_000).contains(&c.trial_cap)
            && c.instruments.as_ref().is_none_or(|keys| {
                !keys.is_empty()
                    && keys.len() <= 500
                    && keys.iter().all(|k| k.starts_with("NSE_EQ|"))
            })
            && (1..=1_000).contains(&self.day.variants)
            && (1..=1_000).contains(&self.day.turns)
            && (1..=100_000).contains(&self.day.calls)
            && self.day.usd.is_finite()
            && self.day.usd > 0.0
            && self.day.usd <= 1_000.0
            && (1..=200).contains(&self.turn.calls)
            && self.turn.usd.is_finite()
            && self.turn.usd > 0.0
            && self.turn.usd <= self.day.usd
            && (1..=40).contains(&self.turn.max_tool_calls)
            && (10_000..=900_000).contains(&self.turn.timeout_ms)
            && self.stop_minute < 24 * 60
            && self
                .max_drawdown_bps
                .is_none_or(|bps| (1..=10_000).contains(&bps))
            && (1..=10).contains(&self.max_consecutive_failures)
            && (1..=10).contains(&self.max_attempts)
            && (1..=20).contains(&self.family_stall)
            && (10_000..=3_600_000).contains(&self.idle_poll_ms)
            && (1..=60).contains(&self.forward_sessions_paper)
            && (1..=60).contains(&self.forward_sessions_live);
        if ok {
            Ok(())
        } else {
            Err("autoresearch_settings_invalid".into())
        }
    }

    pub(crate) fn day_limits(&self) -> DayLimits {
        DayLimits {
            variants: self.day.variants,
            turns: self.day.turns,
            calls: self.day.calls,
            usd_micros: usd_micros(self.day.usd),
        }
    }
}
