//! Bounded membership shared by paper action dispatch and tape replay.
use nivra_core::{
    execution::{Quote, Scanner},
    market::MAX_FINANCIAL_AGE_MS,
    strategy::{indexed_features, median_move, screening_move},
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

use super::Prospect;

pub(crate) const MAX_ENTRY_KEYS: usize = 20;
pub(crate) const REFRESH_MS: i64 = 1_000;
const RESIDENCY_MS: i64 = 10_000;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Order {
    Rising,
    Falling,
    LargestMove,
}

/// The optional `params.promotion` declaration belongs to the strategy's
/// versioned parameters. It can use only the price lookback available on LTPC.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Screen {
    order: Order,
    #[serde(default)]
    min_move_bps: f64,
}

impl Screen {
    pub(crate) fn parse(params: &Value) -> Result<Option<Self>, &'static str> {
        params.get("promotion").map(Self::decode).transpose()
    }

    fn decode(raw: &Value) -> Result<Self, &'static str> {
        let screen: Self =
            serde_json::from_value(raw.clone()).map_err(|_| "strategy_promotion_invalid")?;
        if !screen.min_move_bps.is_finite() || screen.min_move_bps < 0.0 {
            return Err("strategy_promotion_invalid");
        }
        Ok(screen)
    }

    fn score(&self, price_move: f64) -> Option<f64> {
        let score = match self.order {
            Order::Rising => price_move,
            Order::Falling => -price_move,
            Order::LargestMove => price_move.abs(),
        };
        (score > 0.0 && score >= self.min_move_bps).then_some(score)
    }
}

#[derive(Default)]
pub(crate) struct Selection {
    keys: Vec<String>,
    nominated_at: BTreeMap<String, i64>,
    market_median: Option<f64>,
}

impl Selection {
    pub(crate) fn keys(&self) -> &[String] {
        &self.keys
    }

    pub(crate) fn nominees(&self) -> impl Iterator<Item = &str> {
        self.nominated_at.keys().map(String::as_str)
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn refresh<'a>(
        &mut self,
        scan: &Scanner,
        now: i64,
        screen: Option<&Screen>,
        held: &BTreeSet<String>,
        ranked: impl IntoIterator<Item = &'a str>,
        slots: usize,
    ) {
        let moves: Vec<_> = scan
            .samples
            .iter()
            .filter_map(|(key, sample)| {
                screening_move(sample, now).map(|value| (key.as_str(), value))
            })
            .collect();
        self.market_median = median_move(moves.iter().map(|(_, value)| *value));
        let mut nominees = moves
            .into_iter()
            .filter(|(key, _)| !held.contains(*key))
            .filter_map(|(key, value)| screen?.score(value).map(|score| (key, score)))
            .collect::<Vec<_>>();
        nominees.sort_unstable_by(|(a, x), (b, y)| y.total_cmp(x).then(a.cmp(b)));

        let limit = slots.min(MAX_ENTRY_KEYS);
        let mut next = Vec::with_capacity(slots);
        // Retain residents in their prior order. The new ranking fills the
        // vacancies in that set, then the ranker fills any remaining slots.
        next.extend(
            self.keys
                .iter()
                .filter(|key| {
                    !held.contains(*key)
                        && self
                            .nominated_at
                            .get(*key)
                            .is_some_and(|at| now.saturating_sub(*at) < RESIDENCY_MS)
                })
                .take(limit)
                .cloned(),
        );
        for (key, _) in nominees {
            if next.len() >= limit {
                break;
            }
            if !next.iter().any(|candidate| candidate == key) {
                next.push(key.to_owned());
            }
        }
        self.nominated_at = next
            .iter()
            .map(|key| {
                (
                    key.clone(),
                    self.nominated_at.get(key).copied().unwrap_or(now),
                )
            })
            .collect();
        for key in ranked {
            if next.len() >= slots {
                break;
            }
            if !held.contains(key) && !next.iter().any(|candidate| candidate == key) {
                next.push(key.to_owned());
            }
        }
        self.keys = next;
    }

    pub(crate) fn prospects(
        &self,
        scan: &Scanner,
        now: i64,
        top_n: usize,
        covered: impl Fn(&str) -> bool,
    ) -> Vec<Prospect> {
        indexed_features(
            scan,
            self.keys
                .iter()
                .filter(|key| covered(key))
                .take(top_n.min(MAX_ENTRY_KEYS)),
            now,
            self.market_median,
        )
        .into_iter()
        .enumerate()
        .map(|(rank, row)| Prospect {
            key: row.key,
            rank: rank + 1,
            score: row
                .features
                .get(&nivra_core::strategy::Feature::AbsoluteMoveBps)
                .copied()
                .unwrap_or_default(),
            features: row.features,
        })
        .collect()
    }
}

pub(crate) fn executable(scan: &Scanner, key: &str, now: i64) -> Option<Quote> {
    scan.samples
        .get(key)?
        .executable
        .as_ref()
        .filter(|quote| {
            quote.bid_units > 0 && quote.ask_units > 0 && quote.fresh(now, MAX_FINANCIAL_AGE_MS)
        })
        .cloned()
}

#[cfg(test)]
mod tests;
