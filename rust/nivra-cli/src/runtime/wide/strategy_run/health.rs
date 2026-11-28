use super::*;
use nivra_core::finance::Amount;
use nivra_core::sleeve::{Sleeve, session_strategy_pnl};
use std::collections::VecDeque;
use std::ops::Range;

const MAX_PROSPECT_GAP_MS: i64 = 60_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HealthBasis {
    Accepted,
    Fallback,
    #[default]
    Blocked,
    StoodDown,
}

/// The executor's evidence at the time of a decision scan.
#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct StrategyHealth {
    pub active_strategy_id: Option<String>,
    pub active_basis: HealthBasis,
    pub blocked_reason: Option<&'static str>,
    pub eligible_strategy_id: Option<String>,
    pub fresh_prospects: bool,
    pub idle_minutes: u64,
    pub applied_actions: u64,
    pub completed_calls: u64,
    pub failed_calls: u64,
    pub recent_actions: u64,
    pub refusals_by_code: BTreeMap<String, u64>,
    pub session_net_pnl: Option<Amount>,
    pub session_net_pnl_bps: Option<f64>,
    pub cash_drawdown_bps: Option<f64>,
    pub replay_expected_bps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics_error: Option<String>,
}

impl StrategyHealth {
    pub(crate) const CALL_WINDOW: usize = 20;
}

pub(super) enum CallOutcome {
    Failed,
    Returned {
        applied: u64,
        refusals: BTreeMap<String, u64>,
    },
}

impl CallOutcome {
    pub(super) fn returned(results: Vec<ActionResult>) -> Self {
        let mut applied = 0;
        let mut refusals = BTreeMap::new();
        for result in results {
            if matches!(result.outcome.as_str(), "filled" | "exit_revised") {
                applied += 1;
            } else {
                *refusals.entry(result.outcome).or_default() += 1;
            }
        }
        Self::Returned { applied, refusals }
    }
}

struct ProspectStreak {
    since: i64,
    last_seen_at: i64,
    oldest_stamp: i64,
}

#[derive(Default)]
pub(super) struct Tracker {
    active_since: i64,
    peak_cash_pnl: Option<Amount>,
    prospects: Option<ProspectStreak>,
    applied_actions: u64,
    calls: VecDeque<CallOutcome>,
}

impl Tracker {
    pub(super) fn activate(&mut self, now: i64) {
        *self = Self {
            active_since: now,
            peak_cash_pnl: self.peak_cash_pnl,
            ..Self::default()
        };
    }

    pub(super) fn prospect(&mut self, now: i64, oldest_stamp: i64) {
        let since = self
            .prospects
            .as_ref()
            .filter(|streak| {
                (0..=MAX_PROSPECT_GAP_MS).contains(&now.saturating_sub(streak.last_seen_at))
            })
            .map_or(now, |streak| streak.since);
        self.prospects = Some(ProspectStreak {
            since,
            last_seen_at: now,
            oldest_stamp,
        });
    }

    pub(super) fn completed(&mut self, outcome: CallOutcome) {
        if let CallOutcome::Returned { applied, .. } = &outcome {
            self.applied_actions += applied;
            if *applied > 0 {
                self.prospects = None;
            }
        }
        if self.calls.len() == StrategyHealth::CALL_WINDOW {
            self.calls.pop_front();
        }
        self.calls.push_back(outcome);
    }

    pub(super) fn snapshot(&mut self, run: HealthRun<'_>, now: i64) -> StrategyHealth {
        let mut health = StrategyHealth {
            active_strategy_id: run.active.map(|active| active.run_id.clone()),
            active_basis: match (run.active.map(|a| &a.running), run.blocked_reason) {
                (_, Some("strategy_lane_stood_down")) => HealthBasis::StoodDown,
                (Some(Running::Fallback(_)), _) => HealthBasis::Fallback,
                (Some(Running::Accepted { .. }), None) => HealthBasis::Accepted,
                _ => HealthBasis::Blocked,
            },
            blocked_reason: run.blocked_reason,
            eligible_strategy_id: run.eligible_strategy_id.map(str::to_owned),
            applied_actions: self.applied_actions,
            completed_calls: self.calls.len() as u64,
            ..StrategyHealth::default()
        };
        if let Some(streak) = &self.prospects {
            health.fresh_prospects = (0..=nivra_core::market::MAX_FINANCIAL_AGE_MS)
                .contains(&now.saturating_sub(streak.oldest_stamp));
            if (0..=MAX_PROSPECT_GAP_MS).contains(&now.saturating_sub(streak.last_seen_at)) {
                health.idle_minutes =
                    streak.last_seen_at.saturating_sub(streak.since).max(0) as u64 / 60_000;
            }
        }
        for call in &self.calls {
            match call {
                CallOutcome::Failed => health.failed_calls += 1,
                CallOutcome::Returned { applied, refusals } => {
                    health.recent_actions += applied;
                    for (code, count) in refusals {
                        health.recent_actions += count;
                        *health.refusals_by_code.entry(code.clone()).or_default() += count;
                    }
                }
            }
        }
        if run.session.contains(&now)
            && let Err(error) = self.financials(&run, now, &mut health)
        {
            health.metrics_error = Some(failure_code(error.as_ref()));
        }
        health
    }

    fn financials(
        &mut self,
        run: &HealthRun<'_>,
        now: i64,
        health: &mut StrategyHealth,
    ) -> Result<()> {
        let capital = run.portfolio.sleeve_allocation(Sleeve::Cash)?;
        if capital <= Amount::ZERO {
            return Ok(());
        }
        let cash_pnl = run
            .portfolio
            .sleeve_equity(Sleeve::Cash)?
            .checked_sub(capital)?;
        let peak = self
            .peak_cash_pnl
            .map_or(cash_pnl, |peak| peak.max(cash_pnl));
        self.peak_cash_pnl = Some(peak);
        health.cash_drawdown_bps = Some(bps(peak.checked_sub(cash_pnl)?, capital)?);
        let Some(active) = run.active else {
            return Ok(());
        };
        health.session_net_pnl =
            session_strategy_pnl(run.portfolio, run.session.start, &active.run_id)?;
        health.session_net_pnl_bps = health
            .session_net_pnl
            .map(|pnl| bps(pnl, capital))
            .transpose()?;
        health.replay_expected_bps = active.replay_return_bps.map(|daily_bps| {
            let since = self.active_since.max(run.session.start);
            let share = now.saturating_sub(since).max(0) as f64
                / (run.session.end - run.session.start) as f64;
            daily_bps * share
        });
        Ok(())
    }
}

pub(super) struct HealthRun<'a> {
    pub(super) active: Option<&'a Active>,
    pub(super) eligible_strategy_id: Option<&'a str>,
    pub(super) blocked_reason: Option<&'static str>,
    pub(super) portfolio: &'a Portfolio,
    pub(super) session: Range<i64>,
}

fn bps(pnl: Amount, capital: Amount) -> Result<f64> {
    Ok(pnl.checked_div(capital)?.to_string().parse::<f64>()? * 10_000.0)
}

pub(super) fn replay_return_bps(running: &Running) -> Option<f64> {
    let Running::Accepted { entry, report } = running else {
        return None;
    };
    let number = |value: &Value| -> Option<Amount> { serde_json::from_value(value.clone()).ok() };
    let capital = number(&report["capital"]).filter(|capital| *capital > Amount::ZERO)?;
    let pnl = number(&entry.summary["net_pnl"])?;
    let days = entry.tape_dates.len();
    if days == 0 {
        return None;
    }
    Some(bps(pnl, capital).ok()? / days as f64)
}
