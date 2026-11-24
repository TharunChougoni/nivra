//! Pure receipt-driven minute features; no clock, feed or runtime wiring.
//!
//! Each bar occupies approximately 48 bytes on a 64-bit target: 375 bars use
//! about 18 KiB per instrument, plus the key, deque/map metadata and allocator
//! overhead. The cap includes the in-progress bar. Instrument count is caller-owned.
use crate::execution::{local, minute};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct FeatureStoreSettings {
    pub max_bars: usize,
    pub min_instruments: usize,
}
impl FeatureStoreSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for FeatureStoreSettings {
    fn default() -> Self {
        Self {
            max_bars: 375,
            min_instruments: 200,
        }
    }
}

impl FeatureStoreSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_bars < 2 || self.min_instruments == 0 {
            return Err("feature store requires at least two bars and one contributor".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MinuteBar {
    /// IST minute since midnight (09:15 = 555); date belongs to the store session.
    pub start_minute: u32,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub tick_count: u64,
}

/// Bounded in-memory feed-health history used only to classify missing display
/// minutes. It is not trading evidence and never synthesizes a bar or price.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedInterval {
    pub from_ms: i64,
    pub to_ms: i64,
    pub healthy: bool,
}

const MAX_FEED_INTERVALS: usize = 256;

#[derive(Debug)]
pub struct FeatureStore {
    settings: FeatureStoreSettings,
    session: Option<NaiveDate>,
    latest_minute: Option<u32>,
    bars: BTreeMap<String, VecDeque<MinuteBar>>,
    last_receipts: BTreeMap<String, i64>,
    duplicate_ticks: u64,
    ignored_out_of_order: u64,
    history_start_ms: Option<i64>,
    session_open_ms: Option<i64>,
    feed_intervals: VecDeque<FeedInterval>,
}

impl FeatureStore {
    pub fn new(settings: FeatureStoreSettings) -> Result<Self, String> {
        settings.validate()?;
        Ok(Self {
            settings,
            session: None,
            latest_minute: None,
            bars: BTreeMap::new(),
            last_receipts: BTreeMap::new(),
            duplicate_ticks: 0,
            ignored_out_of_order: 0,
            history_start_ms: None,
            session_open_ms: None,
            feed_intervals: VecDeque::new(),
        })
    }

    /// Clears all bars, receipt watermarks, tick counters and coverage history,
    /// even for the same date. Observations never automatically reset or cross
    /// session dates.
    pub fn reset_for_session(&mut self, date: NaiveDate) {
        self.reset_for_session_at(date, None, None);
    }

    /// Reset with explicit history start and configured session-open clocks.
    /// The store-start sample explains why a restarted chart begins mid-session;
    /// neither value changes a bar or invents earlier observations.
    pub fn reset_for_session_at(
        &mut self,
        date: NaiveDate,
        history_start_ms: Option<i64>,
        session_open_ms: Option<i64>,
    ) {
        self.session = Some(date);
        self.latest_minute = None;
        self.bars.clear();
        self.last_receipts.clear();
        self.duplicate_ticks = 0;
        self.ignored_out_of_order = 0;
        self.history_start_ms = history_start_ms.filter(|at| *at > 0);
        self.session_open_ms = session_open_ms.filter(|at| *at > 0);
        self.feed_intervals.clear();
    }

    /// Record the latest observed feed-health sample. Consecutive samples in the
    /// same state merge, so the bounded ring holds transitions rather than every
    /// loop tick. A sample gap stays uncovered and therefore classifies as
    /// unobserved; this path never extends a healthy interval backwards.
    pub fn record_feed_health(&mut self, at_ms: i64, healthy: bool) {
        if at_ms <= 0 {
            return;
        }
        match self.feed_intervals.back_mut() {
            Some(last) if last.healthy == healthy => {
                last.to_ms = last.to_ms.max(at_ms);
                return;
            }
            Some(last) if at_ms < last.to_ms => return,
            Some(_) => {}
            None => {
                let from_ms = self
                    .history_start_ms
                    .map_or(at_ms, |start| start.min(at_ms));
                self.feed_intervals.push_back(FeedInterval {
                    from_ms,
                    to_ms: at_ms,
                    healthy,
                });
                return;
            }
        }
        self.feed_intervals.push_back(FeedInterval {
            from_ms: at_ms,
            to_ms: at_ms,
            healthy,
        });
        while self.feed_intervals.len() > MAX_FEED_INTERVALS {
            self.feed_intervals.pop_front();
        }
    }

    /// True only when the store was live for the whole minute and one contiguous
    /// healthy feed interval covers it. Missing minutes under any fallback,
    /// restart or sample hole remain unobserved.
    pub fn minute_was_observed(&self, from_ms: i64, to_ms: i64) -> bool {
        if from_ms <= 0
            || to_ms <= from_ms
            || self.history_start_ms.is_none_or(|start| from_ms < start)
        {
            return false;
        }
        let mut cursor = from_ms;
        for interval in &self.feed_intervals {
            if interval.to_ms <= cursor {
                continue;
            }
            if interval.from_ms > cursor || !interval.healthy {
                return false;
            }
            if interval.to_ms >= to_ms {
                return true;
            }
            cursor = interval.to_ms;
        }
        false
    }

    pub fn history_start_ms(&self) -> Option<i64> {
        self.history_start_ms
    }

    pub fn session_open_ms(&self) -> Option<i64> {
        self.session_open_ms
    }

    /// The first valid receipt establishes the session if no reset was supplied.
    /// Invalid prices/timestamps and other-session receipts return an error without
    /// mutation. Receipts equal to or older than the instrument's last observation
    /// count as duplicate ticks without changing bars. No missing minutes are synthesized.
    pub fn observe(&mut self, key: &str, price: f64, receipt_ms: i64) -> Result<(), String> {
        if key.is_empty() || !price.is_finite() || price <= 0.0 {
            return Err("feature store requires a key and positive finite price".into());
        }
        let date = local(receipt_ms)
            .map_err(|_| "feature store timestamp range")?
            .date_naive();
        let start_minute = minute(receipt_ms).map_err(|_| "feature store timestamp range")?;
        if self.session.is_some_and(|session| session != date) {
            return Err("feature store session mismatch; explicit reset required".into());
        }
        if self
            .last_receipts
            .get(key)
            .is_some_and(|last| receipt_ms <= *last)
        {
            self.duplicate_ticks = self.duplicate_ticks.saturating_add(1);
            return Ok(());
        }
        self.last_receipts.insert(key.to_owned(), receipt_ms);
        let bars = self.bars.entry(key.to_owned()).or_default();
        if let Some(current) = bars.back_mut() {
            if start_minute < current.start_minute {
                self.ignored_out_of_order = self.ignored_out_of_order.saturating_add(1);
                return Ok(());
            }
            if start_minute == current.start_minute {
                current.high = current.high.max(price);
                current.low = current.low.min(price);
                current.close = price;
                current.tick_count = current.tick_count.saturating_add(1);
                return Ok(());
            }
        }
        if bars.len() == self.settings.max_bars {
            bars.pop_front();
        }
        bars.push_back(MinuteBar {
            start_minute,
            open: price,
            high: price,
            low: price,
            close: price,
            tick_count: 1,
        });
        self.session = Some(date);
        self.latest_minute = Some(
            self.latest_minute
                .map_or(start_minute, |m| m.max(start_minute)),
        );
        Ok(())
    }

    /// Retained bars and policy-qualified completed market-return minutes.
    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({
            "instruments": self.bars.len(),
            "bars_total": self.bars.values().map(VecDeque::len).sum::<usize>(),
            "ignored_ticks": self.ignored_out_of_order,
            "duplicate_ticks": self.duplicate_ticks,
            "completed_minutes": self.market_minute_returns(self.settings.min_instruments).len(),
        })
    }

    pub fn session(&self) -> Option<NaiveDate> {
        self.session
    }

    pub fn last_receipt(&self, key: &str) -> Option<i64> {
        self.last_receipts.get(key).copied()
    }

    pub fn bars(&self, key: &str) -> Option<&VecDeque<MinuteBar>> {
        self.bars.get(key)
    }

    /// The `limit` most recently observed instruments, in no order, so a
    /// caller holding the store lock does not pay for sorting. The last bar
    /// close is the latest receipt price, not a certified exchange close or
    /// a volume. Ties on receipt time keep the lowest instrument keys.
    pub fn latest_prices(&self, limit: usize) -> Vec<(String, f64, i64)> {
        let mut rows = self
            .last_receipts
            .iter()
            .filter_map(|(key, receipt)| Some((key, self.bars.get(key)?.back()?.close, *receipt)))
            .collect::<Vec<_>>();
        if limit == 0 {
            return Vec::new();
        }
        if rows.len() > limit {
            rows.select_nth_unstable_by(limit - 1, |a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(b.0)));
            rows.truncate(limit);
        }
        rows.into_iter()
            .map(|(key, close, receipt)| (key.clone(), close, receipt))
            .collect()
    }

    pub fn ignored_out_of_order(&self) -> u64 {
        self.ignored_out_of_order
    }

    fn returns(&self, key: &str) -> Vec<(u32, f64)> {
        let Some(bars) = self.bars.get(key) else {
            return Vec::new();
        };
        bars.iter()
            .zip(bars.iter().skip(1))
            .filter_map(|(prev, bar)| {
                let value = bar.close / prev.close - 1.0;
                (bar.start_minute == prev.start_minute + 1
                    && self.latest_minute.is_some_and(|m| bar.start_minute < m)
                    && value.is_finite()
                    && value > -1.0)
                    .then_some((bar.start_minute, value))
            })
            .collect()
    }

    /// Chronological completed consecutive-minute returns, most recent n only.
    pub fn last_n_returns(&self, key: &str, n: usize) -> Vec<f64> {
        let values = self.returns(key);
        values
            .iter()
            .skip(values.len().saturating_sub(n))
            .map(|(_, r)| *r)
            .collect()
    }

    /// Unannualised population standard deviation; absent without any returns.
    pub fn realised_vol(&self, key: &str, n: usize) -> Option<f64> {
        let returns = self.last_n_returns(key, n);
        if returns.is_empty() {
            return None;
        }
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        Some(
            (returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64).sqrt(),
        )
    }

    /// Chronological cross-sectional medians for completed minutes. Completion
    /// means a valid receipt in a later minute has been observed anywhere in the
    /// store. Results reflect currently retained bars; zero contributors is invalid
    /// and yields no values. Pass settings.min_instruments (default 200) for policy.
    pub fn market_minute_returns(&self, min_instruments: usize) -> Vec<f64> {
        if min_instruments == 0 {
            return Vec::new();
        }
        let mut minutes: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
        for key in self.bars.keys() {
            for (minute, value) in self.returns(key) {
                minutes.entry(minute).or_default().push(value);
            }
        }
        minutes
            .into_values()
            .filter_map(|mut values| {
                if values.len() < min_instruments {
                    return None;
                }
                values.sort_by(f64::total_cmp);
                let mid = values.len() / 2;
                Some(if values.len() % 2 == 0 {
                    values[mid - 1] / 2.0 + values[mid] / 2.0
                } else {
                    values[mid]
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Explicit receipt clock: no system time or sockets.
    fn receipt(minute: u32, millis: i64) -> i64 {
        NaiveDate::from_ymd_opt(2026, 10, 27)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
            - 19_800_000
            + i64::from(minute) * 60_000
            + millis
    }

    #[test]
    fn receipt_bars_eviction_and_reset() {
        let defaults: FeatureStoreSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults, FeatureStoreSettings::default());
        assert!(
            FeatureStore::new(FeatureStoreSettings {
                max_bars: 1,
                ..defaults.clone()
            })
            .is_err()
        );
        assert!(
            FeatureStoreSettings {
                min_instruments: 0,
                ..defaults
            }
            .validate()
            .is_err()
        );
        let mut store = FeatureStore::new(FeatureStoreSettings {
            max_bars: 3,
            min_instruments: 1,
        })
        .unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
        let start = receipt(555, 0);
        store.reset_for_session_at(date, Some(start), Some(start - 60_000));
        store.record_feed_health(start, true);
        store.record_feed_health(receipt(556, 0), true);
        assert_eq!(store.history_start_ms(), Some(start));
        assert_eq!(store.session_open_ms(), Some(start - 60_000));
        assert!(store.minute_was_observed(start, receipt(556, 0)));
        assert!(!store.minute_was_observed(start - 60_000, start));
        store.record_feed_health(receipt(556, 0) + 30_000, false);
        assert!(!store.minute_was_observed(receipt(556, 0), receipt(557, 0)));
        for (price, offset) in [(100.0, 0), (105.0, 20_000), (99.0, 59_999)] {
            store.observe("a", price, receipt(555, offset)).unwrap();
        }
        let original = store.bars("a").unwrap().clone();
        for (price, offset) in [(99.0, 59_999), (500.0, 59_999), (1.0, 20_000)] {
            store.observe("a", price, receipt(555, offset)).unwrap();
        }
        assert_eq!(store.bars("a").unwrap(), &original);
        assert_eq!(store.status()["duplicate_ticks"], 3);
        assert_eq!(store.status()["ignored_ticks"], 0);
        // Receipt watermarks belong to each instrument independently.
        store.observe("b", 50.0, receipt(555, 0)).unwrap();
        assert_eq!(store.bars("b").unwrap()[0].tick_count, 1);
        store.observe("a", 110.0, receipt(556, 0)).unwrap();
        let bars = store.bars("a").unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(
            bars[0],
            MinuteBar {
                start_minute: 555,
                open: 100.0,
                high: 105.0,
                low: 99.0,
                close: 99.0,
                tick_count: 3
            }
        );
        store.observe("a", 500.0, receipt(555, 59_999)).unwrap();
        assert_eq!(store.ignored_out_of_order(), 0);
        assert_eq!(store.status()["duplicate_ticks"], 4);
        assert_eq!(store.bars("a").unwrap()[0].close, 99.0);
        assert!(store.last_n_returns("a", 10).is_empty());
        store.observe("a", 121.0, receipt(557, 0)).unwrap();
        assert!((store.last_n_returns("a", 1)[0] - (110.0 / 99.0 - 1.0)).abs() < 1e-12);
        store.observe("a", 125.0, receipt(558, 0)).unwrap();
        assert_eq!(store.bars("a").unwrap().len(), 3);
        assert_eq!(store.bars("a").unwrap()[0].start_minute, 556);
        assert!((store.last_n_returns("a", 10)[0] - 0.1).abs() < 1e-12);
        assert_eq!(store.realised_vol("a", 1), Some(0.0));
        assert_eq!(store.realised_vol("a", 0), None);
        assert!(store.observe("a", f64::NAN, receipt(558, 1)).is_err());
        assert!(
            store
                .observe("a", 10.0, receipt(558, 0) + 86_400_000)
                .is_err()
        );
        store.reset_for_session(NaiveDate::from_ymd_opt(2026, 10, 28).unwrap());
        assert!(store.bars("a").is_none());
        assert_eq!(store.ignored_out_of_order(), 0);
        assert_eq!(store.status()["duplicate_ticks"], 0);
        assert_eq!(store.history_start_ms(), None);
        assert!(!store.minute_was_observed(start, receipt(556, 0)));
        assert!(store.market_minute_returns(1).is_empty());
        store
            .observe("a", 20.0, receipt(555, 0) + 86_400_000)
            .unwrap();
        assert!(store.last_n_returns("a", 10).is_empty());
        store.reset_for_session(NaiveDate::from_ymd_opt(2026, 10, 27).unwrap());
        store.observe("a", 100.0, receipt(555, 0)).unwrap();
        assert_eq!(store.bars("a").unwrap()[0].tick_count, 1);
        assert_eq!(store.status()["duplicate_ticks"], 0);
    }

    #[test]
    fn latest_prices_keep_the_newest_receipts_and_are_bounded() {
        let mut store = FeatureStore::new(FeatureStoreSettings::default()).unwrap();
        store.observe("old", 10.0, receipt(555, 0)).unwrap();
        store.observe("new", 20.0, receipt(556, 0)).unwrap();
        store.observe("new", 21.0, receipt(556, 30_000)).unwrap();
        let mut rows = store.latest_prices(10);
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            rows,
            vec![
                ("new".to_owned(), 21.0, receipt(556, 30_000)),
                ("old".to_owned(), 10.0, receipt(555, 0)),
            ]
        );
        assert_eq!(
            store.latest_prices(1),
            vec![("new".to_owned(), 21.0, receipt(556, 30_000))]
        );
        assert!(store.latest_prices(0).is_empty());
        store.reset_for_session(NaiveDate::from_ymd_opt(2026, 10, 28).unwrap());
        assert!(store.latest_prices(10).is_empty());
    }

    #[test]
    fn completed_cross_section_medians_and_missing_minutes() {
        let mut store = FeatureStore::new(FeatureStoreSettings::default()).unwrap();
        for (key, close) in [("a", 110.0), ("b", 120.0), ("c", 130.0)] {
            store.observe(key, 100.0, receipt(555, 0)).unwrap();
            store.observe(key, close, receipt(556, 0)).unwrap();
        }
        assert!(store.market_minute_returns(3).is_empty());
        store.observe("a", 121.0, receipt(557, 0)).unwrap();
        assert!((store.market_minute_returns(3)[0] - 0.2).abs() < 1e-12);
        assert!(store.market_minute_returns(4).is_empty());
        assert!(store.market_minute_returns(0).is_empty());
        store.observe("b", 144.0, receipt(557, 0)).unwrap();
        store.observe("a", 133.1, receipt(558, 0)).unwrap();
        let medians = store.market_minute_returns(2);
        assert_eq!(medians.len(), 2);
        assert!((medians[1] - 0.15).abs() < 1e-12);
        assert!((store.realised_vol("a", 2).unwrap()).abs() < 1e-12);
        store.observe("c", 140.0, receipt(558, 0)).unwrap();
        store.observe("a", 140.0, receipt(559, 0)).unwrap();
        assert_eq!(store.last_n_returns("c", 10).len(), 1);
    }
}
