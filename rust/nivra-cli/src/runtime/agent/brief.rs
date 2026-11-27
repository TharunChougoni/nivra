//! The brief: deterministic facts Rust hands a lane turn, built from
//! controller state. Fields serialize in declaration order with the content
//! that changes least first, and every list is bounded.
use super::Lane;
use super::lanes::ReviewMemory;
use super::protocol::Usage;
use super::triggers::{Held, Phase, Ranked, Trigger};
use crate::runtime::Result;
use nivra_core::execution::Portfolio;
use nivra_core::finance::{FeeArithmetic, Price};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_FILLS: usize = 10;
const MAX_POSITIONS: usize = 20;
const MAX_RANKING: usize = 20;
const MAX_STRATEGIES: usize = 20;
const MAX_TRIGGERS: usize = 32;
/// Gate refusals a decision brief restates.
pub(crate) const MAX_REFUSALS: usize = 5;
/// A brief over this size is refused rather than cut silently.
pub(crate) const MAX_BRIEF_BYTES: usize = 96_000;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct RegimeFacts {
    pub source: &'static str,
    pub label: String,
    pub session_return: f64,
    pub opening_gap: f64,
    pub realised_volatility: f64,
    pub breadth: f64,
    pub instrument_count: usize,
    pub minute_count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct EligibleStrategyFacts {
    pub strategy_id: String,
    pub active: bool,
    pub pinned: bool,
    pub registered_at_ms: i64,
    pub author: crate::runtime::strategy_library::Author,
    pub trades: Value,
    pub net_pnl: Value,
    pub worst_drawdown: Value,
    pub tape_dates: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct StrategyLibraryFacts {
    pub execution_enabled: bool,
    /// What the executor runs: an accepted strategy id or `fallback:<name>`.
    pub active_strategy_id: Option<String>,
    /// `accepted` or `fallback` when something runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_basis: Option<&'static str>,
    /// Why no accepted version runs, beside a fallback or nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_blocked_reason: Option<&'static str>,
    /// Configured fallback references, best first. Their sources are in the
    /// workspace under `reference/strategies/`.
    pub fallback: Vec<String>,
    pub owner_pinned: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane_selection: Option<crate::runtime::strategy_library::LaneSelection>,
    pub active_live_outcomes: BTreeMap<String, u64>,
    pub eligible: Vec<EligibleStrategyFacts>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Brief {
    /// The last review's memory text, carried into the next session.
    pub memory: Option<ReviewMemory>,
    pub session: SessionFacts,
    pub book: BookFacts,
    pub budget: BudgetFacts,
    pub positions: Vec<PositionFacts>,
    pub ranking: Vec<RankFacts>,
    pub fills: Vec<FillFacts>,
    /// Current deterministic intraday regime summary, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regime: Option<RegimeFacts>,
    /// Eligible action strategies and active strategy live outcomes, for
    /// decision turns when `strategy_execution` is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategies: Option<StrategyLibraryFacts>,
    /// The latest actions the execution gate refused, newest first: who
    /// proposed each, the action, the reason in plain words and the numbers
    /// the check used. Decision turns only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub execution_refusals: Vec<Value>,
    /// Research answers not yet delivered to a decision turn.
    pub research_updates: Vec<Value>,
    /// What this turn is about: a research question, an agenda, the day.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub task: Value,
    pub triggers: Vec<Trigger>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SessionFacts {
    pub date: String,
    pub phase: Phase,
    pub minute: u32,
    pub now_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BookFacts {
    pub cash: String,
    pub positions: usize,
    pub fills_total: usize,
    pub halted: bool,
    pub stopped: bool,
    pub closed: bool,
    pub execution: &'static str,
    /// The book's paper cost policy for cash equities: per-order fee, buy and
    /// sell bps, slippage bps per side and the fee rounding increment.
    pub paper_costs: Option<FeeArithmetic>,
    /// Non-cash segments whose costs differ, keyed by segment.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub paper_costs_by_segment: BTreeMap<&'static str, FeeArithmetic>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct BudgetFacts {
    pub lane: Lane,
    pub calls_used: u64,
    pub calls_allowed: u64,
    pub cost_used_usd: f64,
    pub cost_allowed_usd: f64,
    pub entry_score_bar: f64,
    pub entry_top_k: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct PositionFacts {
    pub instrument_key: String,
    pub units: u64,
    pub entry: String,
    pub mark: String,
    pub pnl_bps: Option<f64>,
    pub stop: Option<String>,
    pub stop_distance_bps: Option<f64>,
    pub target: Option<String>,
    pub target_distance_bps: Option<f64>,
    pub exit_time_ms: Option<i64>,
    pub rank: Option<usize>,
    pub score: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RankFacts {
    pub rank: usize,
    pub instrument_key: String,
    pub score: f64,
    pub held: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct FillFacts {
    pub at_ms: i64,
    pub instrument_key: String,
    pub side: &'static str,
    pub units: u64,
    pub price: String,
    pub fee: String,
}

fn number(price: Price) -> Option<f64> {
    price
        .amount()
        .to_string()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn text(price: Price) -> String {
    price.amount().to_string()
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Held positions as the trigger engine and the brief read them.
pub(crate) fn held(p: &Portfolio) -> Vec<Held> {
    p.positions
        .values()
        .filter_map(|position| {
            let intent = position.exit_intent.as_ref();
            Some(Held {
                instrument_key: position.instrument.key.clone(),
                entry: number(position.entry)?,
                mark: number(position.mark)?,
                stop: intent.and_then(|i| i.stop).and_then(number),
                target: intent.and_then(|i| i.target).and_then(number),
            })
        })
        .collect()
}

/// What the controller knows when a turn is due.
pub(crate) struct Facts<'a> {
    pub session: SessionFacts,
    pub portfolio: &'a Portfolio,
    pub ranking: &'a [Ranked],
    pub memory: Option<&'a ReviewMemory>,
    pub execution: &'static str,
    pub regime: Option<&'a RegimeFacts>,
}

impl Brief {
    pub(crate) fn build(
        facts: &Facts<'_>,
        budget: BudgetFacts,
        task: Value,
        triggers: &[Trigger],
    ) -> Self {
        let p = facts.portfolio;
        let rank_of = |key: &str| {
            facts
                .ranking
                .iter()
                .position(|row| row.instrument_key == key)
                .map(|index| (index + 1, facts.ranking[index].score))
        };
        let positions = p
            .positions
            .values()
            .take(MAX_POSITIONS)
            .map(|position| {
                let mark = number(position.mark);
                let entry = number(position.entry);
                let intent = position.exit_intent.as_ref();
                let stop = intent.and_then(|i| i.stop);
                let target = intent.and_then(|i| i.target);
                let distance = |from: Option<f64>, to: Option<f64>| match (from, to) {
                    (Some(from), Some(to)) if from > 0.0 => Some(round((to - from) / from * 1e4)),
                    _ => None,
                };
                let ranked = rank_of(&position.instrument.key);
                PositionFacts {
                    instrument_key: position.instrument.key.clone(),
                    units: position.units,
                    entry: text(position.entry),
                    mark: text(position.mark),
                    pnl_bps: distance(entry, mark),
                    stop: stop.map(text),
                    stop_distance_bps: distance(mark, stop.and_then(number)).map(|d| -d),
                    target: target.map(text),
                    target_distance_bps: distance(mark, target.and_then(number)),
                    exit_time_ms: intent.and_then(|i| i.time),
                    rank: ranked.map(|(rank, _)| rank),
                    score: ranked.map(|(_, score)| score),
                }
            })
            .collect();
        let ranking = facts
            .ranking
            .iter()
            .take(MAX_RANKING)
            .enumerate()
            .map(|(index, row)| RankFacts {
                rank: index + 1,
                instrument_key: row.instrument_key.clone(),
                score: round(row.score),
                held: p.positions.contains_key(&row.instrument_key),
            })
            .collect();
        let fills = p
            .fills
            .iter()
            .rev()
            .take(MAX_FILLS)
            .rev()
            .map(|fill| FillFacts {
                at_ms: fill.at,
                instrument_key: fill.instrument.key.clone(),
                side: if fill.buy { "buy" } else { "sell" },
                units: fill.units,
                price: text(fill.price),
                fee: fill.fee.to_string(),
            })
            .collect();
        Self {
            memory: facts.memory.cloned(),
            session: facts.session.clone(),
            book: BookFacts {
                cash: p.cash.to_string(),
                positions: p.positions.len(),
                fills_total: p.fills.len(),
                halted: p.halted,
                stopped: p.stopped,
                closed: p.closed,
                execution: facts.execution,
                paper_costs: p
                    .fee_schedule
                    .as_ref()
                    .map(|schedule| schedule.cash().clone())
                    .or_else(|| p.costs.clone()),
                paper_costs_by_segment: p
                    .fee_schedule
                    .iter()
                    .flat_map(|schedule| schedule.segments())
                    .map(|(segment, fees)| (segment.as_str(), fees.clone()))
                    .collect(),
            },
            budget,
            positions,
            ranking,
            fills,
            regime: facts.regime.cloned(),
            strategies: None,
            execution_refusals: Vec::new(),
            research_updates: Vec::new(),
            task,
            triggers: triggers.iter().take(MAX_TRIGGERS).cloned().collect(),
        }
    }

    /// The exact text pi receives: serialized in field order, bounded.
    pub(crate) fn render(&self) -> Result<String> {
        let text = serde_json::to_string_pretty(self)?;
        if text.len() > MAX_BRIEF_BYTES {
            return Err("agent_brief_too_large".into());
        }
        Ok(text)
    }
}

/// Factual library summary and today's live outcome counts for decision turns.
pub(crate) fn strategy_facts(
    db: &crate::runtime::Connection,
    c: &crate::runtime::Config,
    cfg: &crate::runtime::planner::PlannerConfig,
) -> Option<StrategyLibraryFacts> {
    if !cfg.runs_strategies() {
        return None;
    }
    let root = crate::runtime::strategy_library::root(c).ok()?;
    let library = crate::runtime::strategy_library::Library::at(root);
    let fallback = cfg.strategy_fallback().to_vec();
    let choice = match crate::runtime::strategy_library::choice::choose(&library, &fallback) {
        Ok(choice) => choice,
        Err(_) => {
            return Some(StrategyLibraryFacts {
                execution_enabled: true,
                active_strategy_id: None,
                active_basis: None,
                active_blocked_reason: Some("strategy_library_unavailable"),
                fallback,
                owner_pinned: None,
                lane_selection: None,
                active_live_outcomes: BTreeMap::new(),
                eligible: Vec::new(),
            });
        }
    };
    let resolved = &choice.resolved;
    let eligible: Vec<EligibleStrategyFacts> = resolved
        .eligible()
        .rev()
        .take(MAX_STRATEGIES)
        .map(|entry| EligibleStrategyFacts {
            strategy_id: entry.strategy_id.clone(),
            active: resolved.active.as_deref() == Some(entry.strategy_id.as_str()),
            pinned: resolved.pinned.as_deref() == Some(entry.strategy_id.as_str()),
            registered_at_ms: entry.registered_at_ms,
            author: entry.author.clone(),
            trades: entry.summary["trades"].clone(),
            net_pnl: entry.summary["net_pnl"].clone(),
            worst_drawdown: entry.summary["worst_drawdown"].clone(),
            tape_dates: entry.tape_dates.clone(),
            blocked_reason: match library.report(entry) {
                Ok(report) => crate::runtime::strategy_library::build_mismatch(&report),
                Err(_) => Some("strategy_library_report_unreadable"),
            },
        })
        .collect();
    let running = choice.running.as_ref();
    let active_strategy_id = running.map(|r| r.run_id());
    let active_live_outcomes = active_strategy_id
        .as_deref()
        .map(|id| crate::runtime::wide::strategy_run::outcome_counts(db, id))
        .unwrap_or_default();
    Some(StrategyLibraryFacts {
        execution_enabled: true,
        active_basis: running.map(|r| match r {
            crate::runtime::strategy_library::choice::Running::Accepted { .. } => "accepted",
            crate::runtime::strategy_library::choice::Running::Fallback(_) => "fallback",
        }),
        active_strategy_id,
        active_blocked_reason: choice.reason,
        fallback,
        owner_pinned: resolved.pinned.clone(),
        lane_selection: resolved.lane_selection.clone(),
        active_live_outcomes,
        eligible,
    })
}

pub(crate) fn budget(
    lane: Lane,
    spent: &Usage,
    calls: u64,
    cost_usd: f64,
    bar: (f64, usize),
) -> BudgetFacts {
    BudgetFacts {
        lane,
        calls_used: spent.calls,
        calls_allowed: calls,
        cost_used_usd: (spent.cost_usd * 1e4).round() / 1e4,
        cost_allowed_usd: cost_usd,
        entry_score_bar: round(bar.0),
        entry_top_k: bar.1,
    }
}
