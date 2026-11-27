//! Per-instrument state for market features carried together by a receipt.
use crate::{
    execution::{MarketFields, Quote, local},
    strategy::Feature,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    pub source: i64,
    pub receipt: i64,
}

impl Clock {
    pub fn fresh(self, now: i64, limit: i64) -> bool {
        [self.source, self.receipt].into_iter().all(|at| {
            now.checked_sub(at)
                .is_some_and(|age| (0..=limit).contains(&age))
        })
    }

    pub fn same_session(self, now: i64) -> bool {
        local(self.source)
            .ok()
            .zip(local(self.receipt).ok())
            .zip(local(now).ok())
            .is_some_and(|((source, receipt), now)| {
                source.date_naive() == receipt.date_naive()
                    && receipt.date_naive() == now.date_naive()
            })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Levels {
    pub bid: Option<(f64, u64)>,
    pub ask: Option<(f64, u64)>,
}

impl Levels {
    fn valid(self) -> Self {
        let valid = |&(price, units): &(f64, u64)| price.is_finite() && price > 0.0 && units > 0;
        Self {
            bid: self.bid.filter(valid),
            ask: self.ask.filter(valid),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Receipt {
    pub clock: Clock,
    pub vwap: Option<f64>,
    pub volume: Option<u64>,
    pub buy_quantity: Option<u64>,
    pub sell_quantity: Option<u64>,
    pub touch: Option<Quote>,
    pub levels: Levels,
}

impl Receipt {
    pub fn from_fields(clock: Clock, fields: &MarketFields, touch: Option<Quote>) -> Self {
        let levels = touch.as_ref().map_or_else(
            || Levels {
                bid: fields.best_bid.zip(fields.best_bid_qty),
                ask: fields.best_ask.zip(fields.best_ask_qty),
            },
            |quote| Levels {
                bid: quote
                    .bid
                    .amount()
                    .to_string()
                    .parse()
                    .ok()
                    .zip(Some(quote.bid_units)),
                ask: quote
                    .ask
                    .amount()
                    .to_string()
                    .parse()
                    .ok()
                    .zip(Some(quote.ask_units)),
            },
        );
        Self {
            clock,
            vwap: fields.session_vwap,
            volume: Some(fields.volume),
            buy_quantity: fields.total_buy_quantity,
            sell_quantity: fields.total_sell_quantity,
            touch,
            levels,
        }
    }
}

/// A new receipt replaces every field it describes, including an empty book.
/// A price-only tick never runs the reducer and cannot refresh these clocks.
#[derive(Clone, Debug)]
pub struct Features {
    pub clock: Clock,
    pub vwap: Option<f64>,
    pub volume: Option<u64>,
    pub imbalance: Option<f64>,
    pub touch: Option<Quote>,
    levels: Levels,
}

impl Features {
    pub fn reduce(receipt: Receipt) -> Self {
        let imbalance = receipt
            .buy_quantity
            .zip(receipt.sell_quantity)
            .and_then(|(buy, sell)| {
                let total = buy as f64 + sell as f64;
                (total > 0.0).then(|| buy as f64 / total)
            });
        let touch = receipt.touch.filter(|touch| {
            touch.source == receipt.clock.source
                && touch.receipt == receipt.clock.receipt
                && touch.ask >= touch.bid
                && touch.bid_units > 0
                && touch.ask_units > 0
        });
        Self {
            clock: receipt.clock,
            vwap: receipt
                .vwap
                .filter(|value| value.is_finite() && *value > 0.0),
            volume: receipt.volume,
            imbalance,
            touch,
            levels: receipt.levels.valid(),
        }
    }

    pub fn fresh(&self, now: i64, limit: i64) -> bool {
        self.clock.same_session(now) && self.clock.fresh(now, limit)
    }

    pub(crate) fn touch_features(&self) -> impl Iterator<Item = (Feature, f64)> {
        let bid = self.levels.bid;
        let ask = self.levels.ask;
        let spread = bid.zip(ask).and_then(|((bid, _), (ask, _))| {
            (ask >= bid).then(|| (ask - bid) / (bid / 2.0 + ask / 2.0))
        });
        [
            (Feature::BestBid, bid.map(|(price, _)| price)),
            (Feature::BestAsk, ask.map(|(price, _)| price)),
            (Feature::BestBidQty, bid.map(|(_, units)| units as f64)),
            (Feature::BestAskQty, ask.map(|(_, units)| units as f64)),
            (Feature::SpreadFraction, spread),
        ]
        .into_iter()
        .filter_map(|(feature, value)| value.map(|value| (feature, value)))
    }
}

/// Accept integral JSON floats as well as integers. The floating-point upper
/// bound rounds to 2^64, so it is exclusive.
pub fn integral_quantity(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0.0 && value < u64::MAX as f64 && value.fract() == 0.0)
        .then_some(value as u64)
}

pub fn json_quantity(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| integral_quantity(value.as_f64()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_keep_original_clocks_and_clear_at_rollover() {
        let midnight = chrono::DateTime::parse_from_rfc3339("2026-11-27T00:00:00+05:30")
            .unwrap()
            .timestamp_millis();
        let features = Features::reduce(Receipt {
            clock: Clock {
                source: midnight - 10,
                receipt: midnight - 5,
            },
            vwap: Some(99.5),
            volume: Some(15_000),
            buy_quantity: Some(300),
            sell_quantity: Some(100),
            touch: None,
            levels: Levels::default(),
        });
        assert_eq!(features.imbalance, Some(0.75));
        assert_eq!(features.vwap, Some(99.5));
        assert_eq!(features.volume, Some(15_000));
        assert!(features.fresh(midnight - 1, 10));
        assert!(!features.fresh(midnight - 1, 8));
        assert!(!features.fresh(midnight, 2_000));
        assert_eq!(integral_quantity(36379.0), Some(36379));
        assert_eq!(integral_quantity(0.5), None);
        assert_eq!(integral_quantity(u64::MAX as f64), None);
    }
}
