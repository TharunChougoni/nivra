//! The brief: deterministic facts Rust hands a lane turn, built from
//! controller state. Fields serialize in declaration order with the content
//! that changes least first, and every list is bounded.
use super::Lane;
use super::lanes::ReviewMemory;
use super::protocol::Usage;
use super::triggers::{Held, Phase, Ranked, Trigger};
use crate::runtime::Result;
use nivra_core::execution::Portfolio;
use nivra_core::finance::Price;
use serde::Serialize;
use serde_json::Value;

const MAX_FILLS: usize = 10;
const MAX_POSITIONS: usize = 20;
const MAX_RANKING: usize = 20;
const MAX_TRIGGERS: usize = 32;
/// A brief over this size is refused rather than cut silently.
pub(crate) const MAX_BRIEF_BYTES: usize = 96_000;

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
            },
            budget,
            positions,
            ranking,
            fills,
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
