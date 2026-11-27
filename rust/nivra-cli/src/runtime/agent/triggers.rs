//! Deterministic triggers: Rust decides when a lane turn runs, the model
//! decides what to do. The controller feeds one observation per scan cycle;
//! the engine fires each trigger once, merges what arrives while a turn runs
//! and hands the runner one lane at a time, decision first.
use super::Lane;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod fo;
pub(crate) use fo::{FoBar, FoObservation, FoTriggers};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TriggerSettings {
    #[serde(default)]
    pub decision: DecisionTriggers,
    #[serde(default)]
    pub research: ResearchTriggers,
    #[serde(default)]
    pub review: ReviewTriggers,
    /// F&O triggers that start gate turns ([fo]). Left out at its defaults so
    /// a sealed policy hash does not change.
    #[serde(default, skip_serializing_if = "FoTriggers::is_default")]
    pub fo: FoTriggers,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DecisionTriggers {
    /// Ranks of the live ranking an unheld instrument must reach.
    pub top_k: usize,
    /// Scan cycles a candidate must stay in the top K before it fires.
    pub persist_cycles: u32,
    /// Per-instrument quiet time after an entry candidate fired.
    pub cooldown_ms: i64,
    /// Minimum live-ranking score of an entry candidate at an unspent budget.
    pub entry_score: f64,
    /// Added to `entry_score` as the lane's daily budget runs out; the top K
    /// shrinks in proportion too.
    pub entry_score_rise: f64,
    /// A held instrument this close to its stop or target, in bps of the mark.
    pub proximity_bps: f64,
    /// A held instrument's fall since the last decision turn, in bps.
    pub adverse_move_bps: f64,
    /// A held instrument ranked below this score; absent turns the check off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_score: Option<f64>,
    /// New top-K members that make the hourly schedule turn worth running.
    pub material_change: usize,
    /// Exchange-local minute of the pre-close turn.
    pub pre_close_minute: u32,
    /// Exchange-local minute of the pre-open strategy choice, once history
    /// is prepared. Fires only with strategy execution on.
    #[serde(
        default = "default_pre_open_minute",
        skip_serializing_if = "is_default_pre_open_minute"
    )]
    pub pre_open_minute: u32,
    /// Minimum time between `regime_changed` supervision triggers, in ms.
    #[serde(
        default = "default_regime_cooldown_ms",
        skip_serializing_if = "is_default_regime_cooldown_ms"
    )]
    pub regime_cooldown_ms: i64,
}

fn default_pre_open_minute() -> u32 {
    9 * 60
}
fn is_default_pre_open_minute(v: &u32) -> bool {
    *v == default_pre_open_minute()
}
fn default_regime_cooldown_ms() -> i64 {
    5 * 60_000
}
fn is_default_regime_cooldown_ms(v: &i64) -> bool {
    *v == default_regime_cooldown_ms()
}

impl Default for DecisionTriggers {
    fn default() -> Self {
        Self {
            top_k: 5,
            persist_cycles: 2,
            cooldown_ms: 15 * 60_000,
            entry_score: 10.0,
            entry_score_rise: 40.0,
            proximity_bps: 50.0,
            adverse_move_bps: 150.0,
            exit_score: None,
            material_change: 2,
            pre_close_minute: 15 * 60,
            pre_open_minute: default_pre_open_minute(),
            regime_cooldown_ms: default_regime_cooldown_ms(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ResearchTriggers {
    /// No decision turn for this long, in session and with budget left.
    pub quiet_ms: i64,
    pub request_ttl_ms: i64,
    pub max_open_requests: usize,
    /// Absolute gap between a ranker's live and backtest hit rate.
    pub drift_threshold: f64,
    pub drift_min_samples: u32,
}

impl Default for ResearchTriggers {
    fn default() -> Self {
        Self {
            quiet_ms: 30 * 60_000,
            request_ttl_ms: 2 * 3_600_000,
            max_open_requests: 4,
            drift_threshold: 0.15,
            drift_min_samples: 20,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ReviewTriggers {
    pub seal: bool,
}

impl Default for ReviewTriggers {
    fn default() -> Self {
        Self { seal: true }
    }
}

impl TriggerSettings {
    pub(crate) fn validate(&self) -> bool {
        let d = &self.decision;
        let r = &self.research;
        (1..=20).contains(&d.top_k)
            && (1..=60).contains(&d.persist_cycles)
            && (0..=86_400_000).contains(&d.cooldown_ms)
            && [
                d.entry_score,
                d.entry_score_rise,
                d.proximity_bps,
                d.adverse_move_bps,
            ]
            .iter()
            .chain(d.exit_score.iter())
            .all(|value| value.is_finite())
            && d.entry_score_rise >= 0.0
            && d.proximity_bps >= 0.0
            && d.adverse_move_bps > 0.0
            && (1..=20).contains(&d.material_change)
            && d.pre_close_minute < 24 * 60
            && d.pre_open_minute < 24 * 60
            && (0..=86_400_000).contains(&d.regime_cooldown_ms)
            && (60_000..=86_400_000).contains(&r.quiet_ms)
            && (60_000..=86_400_000).contains(&r.request_ttl_ms)
            && (1..=32).contains(&r.max_open_requests)
            && r.drift_threshold.is_finite()
            && (0.0..=1.0).contains(&r.drift_threshold)
            && r.drift_min_samples > 0
            && self.fo.validate()
    }
}

/// Every trigger, decision kinds first in priority order. The derived order is
/// the priority: a merged batch runs under its smallest kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TriggerKind {
    PositionRisk,
    Fill,
    EntryCandidate,
    /// A Nifty option chain move; the gate screens these batches.
    FoSignal,
    Schedule,
    /// The pre-open turn that chooses today's strategy.
    PreOpen,
    Request,
    /// The once-a-session research turn after the seal and the review.
    AfterClose,
    Quiet,
    Agenda,
    Drift,
    /// No accepted strategy can run today: replay references over recent
    /// sessions and register what passes.
    StrategySearch,
    Seal,
}

impl TriggerKind {
    pub(crate) const fn lane(self) -> Lane {
        match self {
            Self::PositionRisk
            | Self::Fill
            | Self::EntryCandidate
            | Self::FoSignal
            | Self::Schedule
            | Self::PreOpen => Lane::Decision,
            Self::Request
            | Self::AfterClose
            | Self::Quiet
            | Self::Agenda
            | Self::Drift
            | Self::StrategySearch => Lane::Research,
            Self::Seal => Lane::Review,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Trigger {
    pub kind: TriggerKind,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument_key: Option<String>,
    pub rank: Option<usize>,
    pub score: Option<f64>,
    pub observed_ms: i64,
}

impl Trigger {
    pub(crate) fn new(kind: TriggerKind, reason: &str, observed_ms: i64) -> Self {
        Self {
            kind,
            reason: reason.to_owned(),
            instrument_key: None,
            rank: None,
            score: None,
            observed_ms,
        }
    }
    fn on(mut self, key: &str, ranking: &[Ranked]) -> Self {
        self.instrument_key = Some(key.to_owned());
        if let Some((rank, row)) = ranking
            .iter()
            .enumerate()
            .find(|(_, row)| row.instrument_key == key)
        {
            self.rank = Some(rank + 1);
            self.score = Some(row.score);
        }
        self
    }
}

/// Where the session is. `Sealed` follows the close once the day is reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    PreOpen,
    Open,
    PreClose,
    Closed,
    Sealed,
}

impl Phase {
    pub(crate) fn at(minute: u32, start: u32, pre_close: u32, end: u32, sealed: bool) -> Self {
        if sealed {
            Self::Sealed
        } else if minute < start {
            Self::PreOpen
        } else if minute >= end {
            Self::Closed
        } else if minute >= pre_close {
            Self::PreClose
        } else {
            Self::Open
        }
    }
    pub(crate) const fn trading(self) -> bool {
        matches!(self, Self::Open | Self::PreClose)
    }
}

/// One row of the live ranking, best first. Until promoted rankers exist this
/// is the built-in scanner ranking.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Ranked {
    pub instrument_key: String,
    pub score: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Held {
    pub instrument_key: String,
    pub entry: f64,
    pub mark: f64,
    pub stop: Option<f64>,
    pub target: Option<f64>,
}

/// A live ranker's realised and backtested hit rate, for the drift trigger.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct RankerStats {
    pub name: String,
    pub live_hit_rate: f64,
    pub backtest_hit_rate: f64,
    pub samples: u32,
}

/// The promoted rankers in live use. Rankers arrive in Phase 4; until then the
/// built-in ranking has no backtest to drift from and the registry is empty.
pub(crate) fn ranker_registry() -> Vec<RankerStats> {
    Vec::new()
}

/// One scan cycle as the engine sees it.
pub(crate) struct Observation<'a> {
    pub now: i64,
    pub minute: u32,
    pub open_minute: u32,
    pub phase: Phase,
    pub held: &'a [Held],
    pub fills: usize,
    pub ranking: &'a [Ranked],
    pub ranked_at: i64,
    /// Share of the decision lane's daily calls or cost already spent, 0..=1.
    pub decision_spent: f64,
    pub research_budget_left: bool,
    pub requests_queued: usize,
    pub rankers: &'a [RankerStats],
    /// A research lane is configured and this session's after-close turn has
    /// not been claimed on disk.
    pub after_close_due: bool,
    /// A review turn is running; the after-close turn waits for it.
    pub review_running: bool,
    /// An active runnable strategy owns cash entries: suppress cash
    /// `entry_candidate`, `hourly_top_k_changed` and strategy-fill triggers.
    pub strategy_active: bool,
    /// Strategy execution is enabled: the decision lane chooses the strategy
    /// before the open and re-evaluates it on `regime_changed`.
    pub strategy_supervision: bool,
    /// An accepted library version, not a fallback, is what runs.
    pub strategy_accepted: bool,
    /// Fills belonging to an active strategy (excluded from `Fill` triggers
    /// while `strategy_active` is true).
    pub strategy_fills: usize,
    /// Current deterministic intraday regime label, when available.
    pub regime: Option<&'a str>,
}

#[derive(Clone, Copy, Default)]
struct EntryStreak {
    scans: u32,
    fired: bool,
}

#[derive(Default)]
pub(crate) struct TriggerEngine {
    settings: TriggerSettings,
    fills_seen: Option<usize>,
    non_strategy_fills_seen: Option<usize>,
    /// Risk conditions now true, by instrument and reason. A condition fires
    /// when it becomes true and again only after it has cleared.
    risk: BTreeSet<(String, &'static str)>,
    /// Marks the last decision turn saw; adverse moves are measured from here.
    reference: BTreeMap<String, f64>,
    streak: BTreeMap<String, EntryStreak>,
    last_entry_scan: Option<i64>,
    cooldown_until: BTreeMap<String, i64>,
    schedule: BTreeSet<&'static str>,
    hour: Option<u32>,
    /// Top K at the last decision turn, for the hourly material-change test.
    top_k_seen: BTreeSet<String>,
    last_decision_ms: Option<i64>,
    open_seen_ms: Option<i64>,
    quiet_at: Option<i64>,
    regime_seen: Option<String>,
    regime_cooldown_until: Option<i64>,
    drifted: BTreeSet<String>,
    pre_open: bool,
    strategy_search: bool,
    sealed: bool,
    after_close: bool,
    fo: fo::FoState,
    pending: BTreeMap<Lane, Vec<Trigger>>,
}

fn bps(from: f64, to: f64) -> f64 {
    (to - from) / from * 10_000.0
}

impl TriggerEngine {
    pub(crate) fn new(settings: TriggerSettings) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }

    pub(crate) fn settings(&self) -> &TriggerSettings {
        &self.settings
    }

    /// The entry score and rank an entry candidate must reach once `spent` of
    /// the decision budget is used.
    pub(crate) fn entry_bar(&self, spent: f64) -> (f64, usize) {
        let d = &self.settings.decision;
        let spent = spent.clamp(0.0, 1.0);
        let k = ((d.top_k as f64) * (1.0 - spent)).ceil() as usize;
        (d.entry_score + d.entry_score_rise * spent, k.max(1))
    }

    /// Evaluate one scan cycle. Returns the triggers that fired, in priority
    /// order; they are also queued for their lane.
    pub(crate) fn observe(&mut self, o: &Observation<'_>) -> Vec<Trigger> {
        let mut fired = Vec::new();
        if o.phase.trading() {
            self.open_seen_ms.get_or_insert(o.now);
            self.position_risk(o, &mut fired);
            self.fills(o, &mut fired);
            if o.phase == Phase::Open && o.strategy_active {
                self.streak.clear();
            } else if o.phase == Phase::Open && self.last_entry_scan != Some(o.ranked_at) {
                self.last_entry_scan = Some(o.ranked_at);
                self.entry_candidates(o, &mut fired);
            }
            self.schedule(o, &mut fired);
            self.regime_change(o, &mut fired);
            self.quiet(o, &mut fired);
        } else {
            self.fills_seen = Some(o.fills);
            self.non_strategy_fills_seen = Some(o.fills.saturating_sub(o.strategy_fills));
            self.pre_open(o, &mut fired);
        }
        self.strategy_search(o, &mut fired);
        if o.requests_queued > 0 && !self.queued(TriggerKind::Request) {
            fired.push(Trigger::new(
                TriggerKind::Request,
                "decision_request",
                o.now,
            ));
        }
        self.drift(o, &mut fired);
        if o.phase == Phase::Sealed && self.settings.review.seal && !self.sealed {
            self.sealed = true;
            fired.push(Trigger::new(TriggerKind::Seal, "session_sealed", o.now));
        }
        // After the seal, once the review has run or been skipped. It does
        // not wait for review text: a failed review still gets research.
        if o.phase == Phase::Sealed
            && o.after_close_due
            && !o.review_running
            && !self.after_close
            && !self.queued(TriggerKind::Seal)
            && !fired
                .iter()
                .any(|trigger| trigger.kind == TriggerKind::Seal)
        {
            self.after_close = true;
            fired.push(Trigger::new(TriggerKind::AfterClose, "post_close", o.now));
        }
        fired.sort_by_key(|trigger| trigger.kind);
        for trigger in &fired {
            self.push(trigger.clone());
        }
        fired
    }

    /// Queue a trigger raised outside the scan cycle, such as the agenda a
    /// review left.
    pub(crate) fn push(&mut self, trigger: Trigger) {
        self.pending
            .entry(trigger.kind.lane())
            .or_default()
            .push(trigger);
    }

    fn queued(&self, kind: TriggerKind) -> bool {
        self.pending
            .get(&kind.lane())
            .is_some_and(|queue| queue.iter().any(|trigger| trigger.kind == kind))
    }

    /// A decision trigger is waiting: a running research or review turn yields.
    pub(crate) fn decision_waiting(&self) -> bool {
        self.pending
            .get(&Lane::Decision)
            .is_some_and(|queue| !queue.is_empty())
    }

    /// The next lane to run and every trigger merged into its turn: decision
    /// first, then research, then review.
    pub(crate) fn next(&mut self) -> Option<(Lane, Vec<Trigger>)> {
        for lane in [Lane::Decision, Lane::Research, Lane::Review] {
            if let Some(mut batch) = self.pending.remove(&lane).filter(|queue| !queue.is_empty()) {
                batch.sort_by_key(|trigger| trigger.kind);
                return Some((lane, batch));
            }
        }
        None
    }

    /// Put a batch back after its turn could not start, ahead of anything newer.
    pub(crate) fn requeue(&mut self, lane: Lane, mut batch: Vec<Trigger>) {
        let queue = self.pending.entry(lane).or_default();
        batch.append(queue);
        *queue = batch;
    }

    /// A decision turn started on these facts: adverse moves and the hourly
    /// comparison restart from here.
    pub(crate) fn decision_started(&mut self, now: i64, held: &[Held], ranking: &[Ranked]) {
        self.last_decision_ms = Some(now);
        self.quiet_at = None;
        self.reference = held
            .iter()
            .map(|h| (h.instrument_key.clone(), h.mark))
            .collect();
        self.risk.retain(|(_, reason)| *reason != "adverse_move");
        self.fo.rebase();
        self.top_k_seen = ranking
            .iter()
            .take(self.settings.decision.top_k)
            .map(|r| r.instrument_key.clone())
            .collect();
    }

    fn position_risk(&mut self, o: &Observation<'_>, fired: &mut Vec<Trigger>) {
        let d = &self.settings.decision;
        let mut now_true = BTreeSet::new();
        for h in o.held {
            if !(h.mark.is_finite() && h.mark > 0.0 && h.entry > 0.0) {
                continue;
            }
            let reference = self
                .reference
                .get(&h.instrument_key)
                .copied()
                .unwrap_or(h.entry);
            let score = o
                .ranking
                .iter()
                .find(|r| r.instrument_key == h.instrument_key)
                .map(|r| r.score);
            let conditions = [
                (
                    "near_stop",
                    h.stop
                        .is_some_and(|stop| bps(stop, h.mark) <= d.proximity_bps),
                ),
                (
                    "near_target",
                    h.target
                        .is_some_and(|target| bps(h.mark, target) <= d.proximity_bps),
                ),
                (
                    "score_below_exit",
                    d.exit_score
                        .zip(score)
                        .is_some_and(|(exit, score)| score < exit),
                ),
                (
                    "adverse_move",
                    bps(reference, h.mark) <= -d.adverse_move_bps,
                ),
            ];
            for (reason, active) in conditions {
                if !active {
                    continue;
                }
                let key = (h.instrument_key.clone(), reason);
                if !self.risk.contains(&key) {
                    fired.push(
                        Trigger::new(TriggerKind::PositionRisk, reason, o.now)
                            .on(&h.instrument_key, o.ranking),
                    );
                }
                now_true.insert(key);
            }
        }
        self.risk = now_true;
    }

    fn fills(&mut self, o: &Observation<'_>, fired: &mut Vec<Trigger>) {
        let seen = *self.fills_seen.get_or_insert(o.fills);
        let non_strategy = o.fills.saturating_sub(o.strategy_fills);
        let seen_non_strategy = *self.non_strategy_fills_seen.get_or_insert(non_strategy);
        let delta = if o.strategy_active {
            non_strategy.saturating_sub(seen_non_strategy)
        } else {
            o.fills.saturating_sub(seen)
        };
        if delta > 0 {
            let mut trigger = Trigger::new(TriggerKind::Fill, "fill_batch", o.now);
            trigger.score = Some(delta as f64);
            fired.push(trigger);
        }
        self.fills_seen = Some(o.fills);
        self.non_strategy_fills_seen = Some(non_strategy);
    }

    fn entry_candidates(&mut self, o: &Observation<'_>, fired: &mut Vec<Trigger>) {
        let (bar, k) = self.entry_bar(o.decision_spent);
        let d = &self.settings.decision;
        let held: BTreeSet<&str> = o.held.iter().map(|h| h.instrument_key.as_str()).collect();
        let mut streak = BTreeMap::new();
        for row in o.ranking.iter().take(k) {
            if held.contains(row.instrument_key.as_str()) || row.score < bar {
                continue;
            }
            let mut entry = self
                .streak
                .get(&row.instrument_key)
                .copied()
                .unwrap_or_default();
            entry.scans = entry.scans.saturating_add(1);
            let cooled = self
                .cooldown_until
                .get(&row.instrument_key)
                .is_none_or(|until| o.now >= *until);
            if entry.scans >= d.persist_cycles && !entry.fired && cooled {
                entry.fired = true;
                self.cooldown_until
                    .insert(row.instrument_key.clone(), o.now + d.cooldown_ms);
                fired.push(
                    Trigger::new(TriggerKind::EntryCandidate, "entered_top_k", o.now)
                        .on(&row.instrument_key, o.ranking),
                );
            }
            streak.insert(row.instrument_key.clone(), entry);
        }
        self.streak = streak;
    }

    fn schedule(&mut self, o: &Observation<'_>, fired: &mut Vec<Trigger>) {
        let d = &self.settings.decision;
        // A controller restarted late in the session does not replay the open.
        if !self.schedule.contains("open") {
            self.schedule.insert("open");
            if o.phase == Phase::Open && o.minute < o.open_minute + 30 {
                fired.push(Trigger::new(TriggerKind::Schedule, "open", o.now));
            }
        }
        if o.phase == Phase::PreClose && self.schedule.insert("pre_close") {
            fired.push(Trigger::new(TriggerKind::Schedule, "pre_close", o.now));
        }
        let hour = o.minute / 60;
        if o.phase == Phase::Open
            && !o.strategy_active
            && self.hour.is_some_and(|last| last != hour)
        {
            let changed = o
                .ranking
                .iter()
                .take(d.top_k)
                .filter(|r| !self.top_k_seen.contains(&r.instrument_key))
                .count();
            if changed >= d.material_change {
                let mut trigger =
                    Trigger::new(TriggerKind::Schedule, "hourly_top_k_changed", o.now);
                trigger.score = Some(changed as f64);
                fired.push(trigger);
            }
        }
        self.hour = Some(hour);
    }

    fn regime_change(&mut self, o: &Observation<'_>, fired: &mut Vec<Trigger>) {
        if !o.strategy_supervision {
            return;
        }
        let current = o.regime.filter(|label| !label.is_empty());
        if fired
            .iter()
            .any(|t| t.kind == TriggerKind::Schedule && t.reason == "open")
        {
            self.regime_seen = Some(current.unwrap_or("unknown").to_owned());
            return;
        }
        let Some(label) = current else {
            return;
        };
        let Some(previous) = self.regime_seen.as_deref() else {
            self.regime_seen = Some(label.to_owned());
            return;
        };
        if label != "unknown"
            && previous != label
            && self
                .regime_cooldown_until
                .is_none_or(|until| o.now >= until)
        {
            fired.push(Trigger::new(TriggerKind::Schedule, "regime_changed", o.now));
            self.regime_seen = Some(label.to_owned());
            self.regime_cooldown_until = Some(o.now + self.settings.decision.regime_cooldown_ms);
        }
    }

    /// Once a session, at `pre_open_minute` before the open: choose today's
    /// strategy while there is time to ask research for one.
    fn pre_open(&mut self, o: &Observation<'_>, fired: &mut Vec<Trigger>) {
        if o.phase == Phase::PreOpen
            && o.strategy_supervision
            && o.minute >= self.settings.decision.pre_open_minute
            && !self.pre_open
        {
            self.pre_open = true;
            fired.push(Trigger::new(TriggerKind::PreOpen, "choose_strategy", o.now));
        }
    }

    /// Once a session, from the pre-open turn or the open, when no accepted
    /// version runs: research replays references over recent sessions.
    fn strategy_search(&mut self, o: &Observation<'_>, fired: &mut Vec<Trigger>) {
        let due = fired.iter().any(|t| {
            t.kind == TriggerKind::PreOpen
                || (t.kind == TriggerKind::Schedule && t.reason == "open")
        });
        if due
            && o.strategy_supervision
            && !o.strategy_accepted
            && o.research_budget_left
            && !self.strategy_search
        {
            self.strategy_search = true;
            fired.push(Trigger::new(
                TriggerKind::StrategySearch,
                "no_accepted_strategy",
                o.now,
            ));
        }
    }

    fn quiet(&mut self, o: &Observation<'_>, fired: &mut Vec<Trigger>) {
        // Quiet since the open, the last decision turn or the last quiet
        // trigger, whichever came last.
        let since = [self.open_seen_ms, self.last_decision_ms, self.quiet_at]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(o.now);
        if o.research_budget_left && o.now - since >= self.settings.research.quiet_ms {
            self.quiet_at = Some(o.now);
            fired.push(Trigger::new(TriggerKind::Quiet, "no_decision_turn", o.now));
        }
    }

    fn drift(&mut self, o: &Observation<'_>, fired: &mut Vec<Trigger>) {
        let r = &self.settings.research;
        for ranker in o.rankers {
            let gap = ranker.live_hit_rate - ranker.backtest_hit_rate;
            if ranker.samples >= r.drift_min_samples
                && gap.abs() >= r.drift_threshold
                && self.drifted.insert(ranker.name.clone())
            {
                let mut trigger = Trigger::new(TriggerKind::Drift, "hit_rate_drift", o.now);
                trigger.instrument_key = Some(format!("ranker:{}", ranker.name));
                trigger.score = Some(gap);
                fired.push(trigger);
            }
        }
    }
}

#[cfg(test)]
mod tests;
