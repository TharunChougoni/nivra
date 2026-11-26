//! Pure receipt-driven minute features; no clock, feed or runtime wiring.
//!
//! Each bar occupies 24 bytes (`u16` minute, four `u32` integer-paise prices,
//! `u32` tick count): 375 bars use ~9 KiB per instrument, plus the key,
//! deque/map metadata and allocator overhead. The cap includes the in-progress
//! bar. Instrument count is caller-owned.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MinuteBar {
    /// IST minute since midnight (09:15 = 555); date belongs to the store session.
    start_minute: u16,
    open_paise: u32,
    high_paise: u32,
    low_paise: u32,
    close_paise: u32,
    tick_count: u32,
}

impl MinuteBar {
    /// Construct a bar directly from whole-paise prices and `u16` IST minute.
    pub const fn from_paise(
        start_minute: u16,
        open_paise: u32,
        high_paise: u32,
        low_paise: u32,
        close_paise: u32,
        tick_count: u32,
    ) -> Self {
        Self {
            start_minute,
            open_paise,
            high_paise,
            low_paise,
            close_paise,
            tick_count,
        }
    }

    pub const fn start_minute(&self) -> u32 {
        self.start_minute as u32
    }

    pub fn open(&self) -> f64 {
        f64::from(self.open_paise) / 100.0
    }

    pub fn high(&self) -> f64 {
        f64::from(self.high_paise) / 100.0
    }

    pub fn low(&self) -> f64 {
        f64::from(self.low_paise) / 100.0
    }

    pub fn close(&self) -> f64 {
        f64::from(self.close_paise) / 100.0
    }

    pub const fn tick_count(&self) -> u32 {
        self.tick_count
    }
}

fn price_to_paise(price: f64) -> Result<u32, String> {
    if !price.is_finite() || price <= 0.0 {
        return Err("feature store requires a key and positive finite price".into());
    }
    let rounded = (price * 100.0).round();
    if !rounded.is_finite() || rounded < 1.0 || rounded > f64::from(u32::MAX) {
        return Err("feature store price overflows paise range".into());
    }
    let paise = rounded as u32;
    if (f64::from(paise) / 100.0).to_bits() != price.to_bits() {
        return Err("feature store price is not on whole-paise grid".into());
    }
    Ok(paise)
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
        if key.is_empty() {
            return Err("feature store requires a key and positive finite price".into());
        }
        let price_paise = price_to_paise(price)?;
        let date = local(receipt_ms)
            .map_err(|_| "feature store timestamp range")?
            .date_naive();
        let start_minute = minute(receipt_ms).map_err(|_| "feature store timestamp range")?;
        let start_minute_u16 =
            u16::try_from(start_minute).map_err(|_| "feature store timestamp range")?;
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
            if start_minute < current.start_minute() {
                self.ignored_out_of_order = self.ignored_out_of_order.saturating_add(1);
                return Ok(());
            }
            if start_minute == current.start_minute() {
                current.high_paise = current.high_paise.max(price_paise);
                current.low_paise = current.low_paise.min(price_paise);
                current.close_paise = price_paise;
                current.tick_count = current.tick_count.saturating_add(1);
                return Ok(());
            }
        }
        if bars.len() == self.settings.max_bars {
            bars.pop_front();
        }
        bars.push_back(MinuteBar::from_paise(
            start_minute_u16,
            price_paise,
            price_paise,
            price_paise,
            price_paise,
            1,
        ));
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
        if limit == 0 {
            return Vec::new();
        }
        let mut rows = self
            .last_receipts
            .iter()
            .filter_map(|(key, receipt)| Some((key, self.bars.get(key)?.back()?.close(), *receipt)))
            .collect::<Vec<_>>();
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
                let value = bar.close() / prev.close() - 1.0;
                (bar.start_minute() == prev.start_minute() + 1
                    && self.latest_minute.is_some_and(|m| bar.start_minute() < m)
                    && value.is_finite()
                    && value > -1.0)
                    .then_some((bar.start_minute(), value))
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
        assert_eq!(store.bars("b").unwrap()[0].tick_count(), 1);
        store.observe("a", 110.0, receipt(556, 0)).unwrap();
        let bars = store.bars("a").unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(
            bars[0],
            MinuteBar::from_paise(555, 10_000, 10_500, 9_900, 9_900, 3)
        );
        store.observe("a", 500.0, receipt(555, 59_999)).unwrap();
        assert_eq!(store.ignored_out_of_order(), 0);
        assert_eq!(store.status()["duplicate_ticks"], 4);
        assert_eq!(store.bars("a").unwrap()[0].close(), 99.0);
        assert!(store.last_n_returns("a", 10).is_empty());
        store.observe("a", 121.0, receipt(557, 0)).unwrap();
        assert!((store.last_n_returns("a", 1)[0] - (110.0 / 99.0 - 1.0)).abs() < 1e-12);
        store.observe("a", 125.0, receipt(558, 0)).unwrap();
        assert_eq!(store.bars("a").unwrap().len(), 3);
        assert_eq!(store.bars("a").unwrap()[0].start_minute(), 556);
        assert!((store.last_n_returns("a", 10)[0] - 0.1).abs() < 1e-12);
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
        assert_eq!(store.bars("a").unwrap()[0].tick_count(), 1);
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
        store.observe("c", 140.0, receipt(558, 0)).unwrap();
        store.observe("a", 140.0, receipt(559, 0)).unwrap();
        assert_eq!(store.last_n_returns("c", 10).len(), 1);
    }

    #[test]
    fn compact_minute_bar_preserves_recorded_features_and_price_grid() {
        assert!(std::mem::size_of::<MinuteBar>() <= 24);
        assert_eq!(std::mem::size_of::<MinuteBar>(), 24);

        // Prove correctly rounded division matches decimal string parsing across the paise grid.
        let check_paise = |paise: u32| {
            let text = format!("{}.{:02}", paise / 100, paise % 100);
            let parsed: f64 = text.parse().unwrap();
            let divided = f64::from(paise) / 100.0;
            assert_eq!(
                divided.to_bits(),
                parsed.to_bits(),
                "paise={paise} text={text}"
            );
            assert_eq!(price_to_paise(parsed).unwrap(), paise, "paise={paise}");
        };
        for paise in 1..=50_000 {
            check_paise(paise);
        }
        for exp in 16..32 {
            let base = 1_u32 << exp;
            for delta in -16_i64..=16 {
                if let Ok(paise) = u32::try_from(i64::from(base) + delta)
                    && paise > 0
                {
                    check_paise(paise);
                }
            }
        }
        for paise in (u32::MAX - 500)..=u32::MAX {
            check_paise(paise);
        }

        // Recorded observation sequence with exact frozen baseline f64 bit patterns.
        let mut store = FeatureStore::new(FeatureStoreSettings {
            max_bars: 375,
            min_instruments: 2,
        })
        .unwrap();
        let obs = [
            ("RELIANCE", 555, 0, 1280.25),
            ("RELIANCE", 555, 15_000, 1285.50),
            ("RELIANCE", 555, 30_000, 1279.05),
            ("RELIANCE", 555, 55_000, 1284.10),
            ("RELIANCE", 556, 0, 1284.10),
            ("RELIANCE", 556, 40_000, 1291.75),
            ("RELIANCE", 557, 10_000, 1290.00),
            ("RELIANCE", 557, 50_000, 1288.45),
            ("RELIANCE", 558, 20_000, 1295.30),
            ("RELIANCE", 559, 5_000, 1299.95),
            ("TCS", 555, 5_000, 3890.15),
            ("TCS", 555, 45_000, 3895.00),
            ("TCS", 556, 10_000, 3902.80),
            ("TCS", 558, 15_000, 3910.40),
            ("TCS", 559, 25_000, 3905.05),
            ("INFY", 555, 1_000, 0.29),
            ("INFY", 556, 2_000, 1.13),
            ("INFY", 557, 3_000, 19.99),
            ("INFY", 558, 4_000, 133.10),
            ("INFY", 559, 5_000, 42_949_672.95),
        ];
        for (key, min, ms, price) in obs {
            store.observe(key, price, receipt(min, ms)).unwrap();
        }

        let to_bits = |vals: Vec<f64>| vals.into_iter().map(f64::to_bits).collect::<Vec<_>>();
        assert_eq!(
            to_bits(store.last_n_returns("RELIANCE", 10)),
            vec![
                4573518528897894912,
                13791409136315793152,
                4572779489580868096
            ]
        );
        assert_eq!(
            to_bits(store.last_n_returns("TCS", 10)),
            vec![4566764028552465920]
        );
        assert_eq!(
            to_bits(store.last_n_returns("INFY", 10)),
            vec![
                4613704873432760885,
                4625391109771299668,
                4618056730706676008
            ]
        );
        assert_eq!(
            to_bits(store.market_minute_returns(2)),
            vec![
                4573518528897894912,
                4620886791067154916,
                4613559116887339872
            ]
        );

        let mut latest = store.latest_prices(3);
        latest.sort_by(|a, b| a.0.cmp(&b.0));
        let latest_bits: Vec<(String, u64, i64)> = latest
            .into_iter()
            .map(|(k, p, r)| (k, p.to_bits(), r))
            .collect();
        assert_eq!(
            latest_bits,
            vec![
                ("INFY".to_owned(), 4721033417378601370, 1793072945000),
                ("RELIANCE".to_owned(), 4653432056008461517, 1793072945000),
                ("TCS".to_owned(), 4660805710837815706, 1793072965000),
            ]
        );

        let reliance_bars: Vec<(u32, u64, u64, u64, u64, u32)> = store
            .bars("RELIANCE")
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b.start_minute(),
                    b.open().to_bits(),
                    b.high().to_bits(),
                    b.low().to_bits(),
                    b.close().to_bits(),
                    b.tick_count(),
                )
            })
            .collect();
        assert_eq!(
            reliance_bars,
            vec![
                (
                    555,
                    0x4094010000000000,
                    0x4094160000000000,
                    0x4093fc3333333333,
                    0x4094106666666666,
                    4
                ),
                (
                    556,
                    0x4094106666666666,
                    0x40942f0000000000,
                    0x4094106666666666,
                    0x40942f0000000000,
                    2
                ),
                (
                    557,
                    0x4094280000000000,
                    0x4094280000000000,
                    0x409421cccccccccd,
                    0x409421cccccccccd,
                    2
                ),
                (
                    558,
                    0x40943d3333333333,
                    0x40943d3333333333,
                    0x40943d3333333333,
                    0x40943d3333333333,
                    1
                ),
                (
                    559,
                    0x40944fcccccccccd,
                    0x40944fcccccccccd,
                    0x40944fcccccccccd,
                    0x40944fcccccccccd,
                    1
                ),
            ]
        );
    }

    #[test]
    fn off_grid_and_overflow_prices_return_error_without_mutation() {
        let mut store = FeatureStore::new(FeatureStoreSettings::default()).unwrap();
        for bad in [
            0.005,
            100.001,
            100.004,
            19.995,
            42_949_672.96,
            1e12,
            0.0,
            -10.0,
            f64::NAN,
            f64::INFINITY,
        ] {
            assert!(
                store.observe("OFFGRID", bad, receipt(555, 0)).is_err(),
                "expected error for {bad}"
            );
            assert!(store.bars("OFFGRID").is_none());
            assert!(store.last_receipt("OFFGRID").is_none());
            assert!(store.session().is_none());
        }

        store.observe("VALID", 100.05, receipt(555, 0)).unwrap();
        let before_bars = store.bars("VALID").unwrap().clone();
        let before_status = store.status();
        assert!(
            store
                .observe("VALID", 100.055, receipt(555, 1_000))
                .is_err()
        );
        assert!(
            store
                .observe("VALID", 42_949_672.96, receipt(556, 0))
                .is_err()
        );
        assert_eq!(store.bars("VALID").unwrap(), &before_bars);
        assert_eq!(store.last_receipt("VALID"), Some(receipt(555, 0)));
        assert_eq!(store.status(), before_status);
    }
}
