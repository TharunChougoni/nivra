//! F&O triggers: the Nifty option chain, not the cash scanner, starts gate turns.
//! A trigger fires when spot, a near-money premium or its open interest moves
//! past a threshold since the last trigger or decision turn, and only while a
//! contract near the money is liquid and affordable enough to act on.
use super::{Phase, Trigger, TriggerEngine, TriggerKind, bps};
use crate::runtime::options::chain::{Chain, ChainRow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct FoTriggers {
    /// Nifty spot move since the reference, in bps.
    pub spot_move_bps: f64,
    /// Mid-premium move of a near-money contract since its reference, in bps.
    pub premium_move_bps: f64,
    /// Open-interest change of a near-money contract since its reference, in %.
    pub oi_shift_pct: f64,
    /// Strikes either side of the at-the-money strike in the chain.
    pub strikes_near: usize,
    /// Widest spread a contract may show, in ticks.
    pub max_spread_ticks: u64,
    /// Lots the thinner side of the best level must hold.
    pub min_depth_lots: u64,
    /// Highest round-trip fees, slippage and spread, in bps of the premium.
    pub max_cost_bps: f64,
    /// Quiet time per instrument and reason after a trigger.
    pub cooldown_ms: i64,
    /// Exchange-local minute after which expiry day raises no trigger.
    pub expiry_day_last_minute: u32,
    /// Thresholds scale by `1 + threshold_rise × spent` as the decision lane
    /// spends its daily budget.
    pub threshold_rise: f64,
}

impl Default for FoTriggers {
    fn default() -> Self {
        Self {
            spot_move_bps: 30.0,
            premium_move_bps: 1_500.0,
            oi_shift_pct: 10.0,
            strikes_near: 2,
            max_spread_ticks: 4,
            min_depth_lots: 2,
            max_cost_bps: 300.0,
            cooldown_ms: 10 * 60_000,
            expiry_day_last_minute: 13 * 60 + 30,
            threshold_rise: 1.0,
        }
    }
}

/// The thresholds in force once the decision lane spent part of its budget.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct FoBar {
    pub spot_move_bps: f64,
    pub premium_move_bps: f64,
    pub oi_shift_pct: f64,
}

impl FoTriggers {
    pub(super) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(super) fn validate(&self) -> bool {
        [
            self.spot_move_bps,
            self.premium_move_bps,
            self.oi_shift_pct,
            self.max_cost_bps,
            self.threshold_rise,
        ]
        .iter()
        .all(|value| value.is_finite() && *value > 0.0)
            && (1..=10).contains(&self.strikes_near)
            && (1..=1_000).contains(&self.max_spread_ticks)
            && (1..=1_000).contains(&self.min_depth_lots)
            && (0..=86_400_000).contains(&self.cooldown_ms)
            && self.expiry_day_last_minute < 24 * 60
    }

    pub(crate) fn bar(&self, spent: f64) -> FoBar {
        let scale = 1.0 + self.threshold_rise * spent.clamp(0.0, 1.0);
        let round = |value: f64| (value * scale * 100.0).round() / 100.0;
        FoBar {
            spot_move_bps: round(self.spot_move_bps),
            premium_move_bps: round(self.premium_move_bps),
            oi_shift_pct: round(self.oi_shift_pct),
        }
    }

    /// A contract the decision lane could enter now: a tight spread, depth
    /// for the minimum lots, at least one affordable lot and room after costs.
    pub(crate) fn tradable(&self, row: &ChainRow) -> bool {
        row.spread_ticks <= self.max_spread_ticks
            && row.depth_lots >= self.min_depth_lots
            && row.affordable_lots >= 1
            && row.round_trip_cost_bps <= self.max_cost_bps
    }
}

/// References each move is measured from. A trigger moves its reference to
/// the value it fired on; a decision turn moves every reference.
#[derive(Default)]
pub(super) struct FoState {
    spot: Option<f64>,
    premium: BTreeMap<String, f64>,
    oi: BTreeMap<String, u64>,
    cooldown_until: BTreeMap<(String, &'static str), i64>,
    rebase: bool,
}

impl FoState {
    pub(super) fn rebase(&mut self) {
        self.rebase = true;
    }

    /// True, and the cooldown armed, when this instrument and reason may fire.
    fn arm(&mut self, key: &str, reason: &'static str, now: i64, cooldown_ms: i64) -> bool {
        let slot = (key.to_owned(), reason);
        if self
            .cooldown_until
            .get(&slot)
            .is_some_and(|until| now < *until)
        {
            return false;
        }
        self.cooldown_until.insert(slot, now + cooldown_ms);
        true
    }
}

fn signal(key: &str, reason: &str, now: i64, score: f64) -> Trigger {
    let mut trigger = Trigger::new(TriggerKind::FoSignal, reason, now);
    trigger.instrument_key = Some(key.to_owned());
    trigger.score = Some((score * 100.0).round() / 100.0);
    trigger
}

/// One scan cycle's option chain as the engine sees it.
pub(crate) struct FoObservation<'a> {
    pub now: i64,
    pub minute: u32,
    pub phase: Phase,
    /// Share of the decision lane's daily calls or cost already spent, 0..=1.
    pub decision_spent: f64,
    pub chain: &'a Chain,
}

impl TriggerEngine {
    /// Evaluate one cycle's chain. Returns the F&O triggers that fired; they
    /// are also queued for the decision lane, which the gate screens first.
    pub(crate) fn observe_fo(&mut self, o: &FoObservation<'_>) -> Vec<Trigger> {
        let s = &self.settings.fo;
        let state = &mut self.fo;
        let chain = o.chain;
        if std::mem::take(&mut state.rebase) {
            state.spot = Some(chain.spot_value);
            for row in &chain.rows {
                state.premium.insert(row.instrument_key.clone(), row.mid);
                if let Some(oi) = row.open_interest {
                    state.oi.insert(row.instrument_key.clone(), oi);
                }
            }
        }
        let live =
            o.phase == Phase::Open && !(chain.expiry_day && o.minute >= s.expiry_day_last_minute);
        let bar = s.bar(o.decision_spent);
        let mut fired = Vec::new();
        let reference = *state.spot.get_or_insert(chain.spot_value);
        let spot_move = bps(reference, chain.spot_value);
        if live
            && spot_move.abs() >= bar.spot_move_bps
            && chain.rows.iter().any(|row| s.tradable(row))
            && state.arm(&chain.underlying_key, "spot_move", o.now, s.cooldown_ms)
        {
            state.spot = Some(chain.spot_value);
            fired.push(signal(&chain.underlying_key, "spot_move", o.now, spot_move));
        }
        for row in &chain.rows {
            let key = &row.instrument_key;
            let tradable = live && s.tradable(row);
            let reference = *state.premium.entry(key.clone()).or_insert(row.mid);
            let premium_move = bps(reference, row.mid);
            if tradable
                && premium_move.abs() >= bar.premium_move_bps
                && state.arm(key, "premium_move", o.now, s.cooldown_ms)
            {
                state.premium.insert(key.clone(), row.mid);
                fired.push(signal(key, "premium_move", o.now, premium_move));
            }
            let Some(oi) = row.open_interest else {
                continue;
            };
            let reference = *state.oi.entry(key.clone()).or_insert(oi);
            if reference == 0 {
                state.oi.insert(key.clone(), oi);
                continue;
            }
            let shift = (oi as f64 - reference as f64) / reference as f64 * 100.0;
            if tradable
                && shift.abs() >= bar.oi_shift_pct
                && state.arm(key, "oi_shift", o.now, s.cooldown_ms)
            {
                state.oi.insert(key.clone(), oi);
                fired.push(signal(key, "oi_shift", o.now, shift));
            }
        }
        for trigger in &fired {
            self.push(trigger.clone());
        }
        fired
    }
}
