use super::{Lane, Observation, Trigger, TriggerEngine, TriggerKind};
use crate::runtime::wide::strategy_run::{HealthBasis, StrategyHealth};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct HealthTriggers {
    pub idle_minutes: u32,
    pub loss_bps: u32,
    pub drawdown_bps: u32,
    pub replay_gap_bps: u32,
    pub min_calls: u32,
    pub failure_rate_pct: u32,
    pub min_actions: u32,
    pub refusal_rate_pct: u32,
    /// Applied separately to each reason.
    pub cooldown_ms: i64,
}

impl Default for HealthTriggers {
    fn default() -> Self {
        Self {
            idle_minutes: 15,
            loss_bps: 100,
            drawdown_bps: 150,
            replay_gap_bps: 75,
            min_calls: 5,
            failure_rate_pct: 50,
            min_actions: 5,
            refusal_rate_pct: 50,
            cooldown_ms: 15 * 60_000,
        }
    }
}

impl HealthTriggers {
    pub(super) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(super) fn validate(&self) -> bool {
        (1..=1440).contains(&self.idle_minutes)
            && [self.loss_bps, self.drawdown_bps, self.replay_gap_bps]
                .iter()
                .all(|value| (1..=10_000).contains(value))
            && (1..=StrategyHealth::CALL_WINDOW as u32).contains(&self.min_calls)
            && (1..=100).contains(&self.failure_rate_pct)
            && (1..=(StrategyHealth::CALL_WINDOW * crate::runtime::strategy::MAX_ACTIONS) as u32)
                .contains(&self.min_actions)
            && (1..=100).contains(&self.refusal_rate_pct)
            && (1_000..=86_400_000).contains(&self.cooldown_ms)
    }
}

impl TriggerEngine {
    pub(super) fn strategy_health(
        &mut self,
        observation: &Observation<'_>,
        fired: &mut Vec<Trigger>,
    ) {
        let Some(health) = observation
            .strategy_health
            .filter(|_| observation.decision_spent < 1.0)
        else {
            return;
        };
        let thresholds = &self.settings.decision.strategy_health;
        let pct_at_least = |count, total, pct| count * 100 >= total * u64::from(pct);
        let conditions = [
            (
                "idle",
                health.fresh_prospects && health.idle_minutes >= u64::from(thresholds.idle_minutes),
            ),
            (
                "losing",
                health.active_strategy_id.is_some()
                    && (health
                        .session_net_pnl_bps
                        .is_some_and(|pnl| pnl <= -f64::from(thresholds.loss_bps))
                        || health.cash_drawdown_bps.is_some_and(|drawdown| {
                            drawdown >= f64::from(thresholds.drawdown_bps)
                        })),
            ),
            (
                "below_expectation",
                health
                    .session_net_pnl_bps
                    .zip(health.replay_expected_bps)
                    .is_some_and(|(pnl, expected)| {
                        expected - pnl >= f64::from(thresholds.replay_gap_bps)
                    }),
            ),
            (
                "failing",
                health.completed_calls >= u64::from(thresholds.min_calls)
                    && pct_at_least(
                        health.failed_calls,
                        health.completed_calls,
                        thresholds.failure_rate_pct,
                    ),
            ),
            (
                "refusals",
                health.recent_actions >= u64::from(thresholds.min_actions)
                    && pct_at_least(
                        health.refusals_by_code.values().sum(),
                        health.recent_actions,
                        thresholds.refusal_rate_pct,
                    ),
            ),
            (
                "fallback_while_eligible",
                matches!(
                    health.active_basis,
                    HealthBasis::Fallback | HealthBasis::Blocked
                ) && health.eligible_strategy_id.is_some(),
            ),
        ];
        for (reason, due) in conditions {
            if due
                && self
                    .health_cooldown_until
                    .get(reason)
                    .is_none_or(|until| observation.now >= *until)
                && !self.pending.get(&Lane::Decision).is_some_and(|pending| {
                    pending.iter().any(|trigger| {
                        trigger.kind == TriggerKind::StrategyHealth && trigger.reason == reason
                    })
                })
            {
                self.health_cooldown_until.insert(
                    reason,
                    observation.now.saturating_add(thresholds.cooldown_ms),
                );
                fired.push(Trigger::new(
                    TriggerKind::StrategyHealth,
                    reason,
                    observation.now,
                ));
            }
        }
    }
}
