//! Pure detection of evidence for planning; no clocks, I/O or trading authority.
use crate::execution::{ExitIntent, Fill, Instrument, Quote};
use crate::finance::{Amount, Price};
use crate::market::MAX_FINANCIAL_AGE_MS;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventPolicy {
    pub price_move_bps: Option<u32>,
    pub volume_step_units: Option<u64>,
    pub gap_bps: Option<u32>,
    pub proximity_bps: Option<u32>,
    pub max_event_age_ms: i64,
    pub max_news_age_ms: i64,
    pub max_instruments: usize,
    pub max_identities: usize,
    /// When set, price moves are measured against the median return of all observed
    /// instruments (vs prior close) once at least this many have a prior close, so a
    /// broad market move alone does not fire per-instrument events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_relative_min_breadth: Option<usize>,
}
impl EventPolicy {
    pub fn validate(&self) -> Result<()> {
        if !(1..=86_400_000).contains(&self.max_event_age_ms)
            || !(1..=86_400_000).contains(&self.max_news_age_ms)
            || !(1..=10_000).contains(&self.max_instruments)
            || !(1..=100_000).contains(&self.max_identities)
            || self.volume_step_units == Some(0)
            || self.market_relative_min_breadth == Some(0)
            || [self.price_move_bps, self.gap_bps, self.proximity_bps]
                .into_iter()
                .flatten()
                .any(|n| n == 0 || n > 100_000)
        {
            return Err("event_policy_invalid".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    PriceMove,
    VolumeMove,
    Gap,
    Proximity,
    News,
    Fill,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: String,
    pub kind: EventKind,
    pub instrument_key: String,
    pub observed_ms: i64,
    pub evidence: Value,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketObservation {
    pub instrument: Instrument,
    pub quote: Quote,
    pub cumulative_volume: Option<u64>,
    pub prior_close: Option<Price>,
    pub session_open: Option<Price>,
    pub exit_intent: Option<ExitIntent>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewsObservation {
    pub id: String,
    pub instrument_key: String,
    pub published_ms: i64,
    pub received_ms: i64,
    pub citation: String,
    pub content_hash: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MarketState {
    observation: MarketObservation,
    price_anchor: Price,
    volume_anchor: Option<u64>,
    gap_emitted: bool,
    target_near: bool,
    stop_near: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relative_anchor_bps: Option<f64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventState {
    binding: String,
    session: String,
    last_now: Option<i64>,
    markets: BTreeMap<String, MarketState>,
    identities: BTreeMap<String, String>,
}
fn hash(v: &impl Serialize) -> Result<String> {
    Ok(Sha256::digest(serde_json::to_vec(v)?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn name(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= 256
}
fn age(now: i64, at: i64, limit: i64) -> bool {
    now.checked_sub(at)
        .is_some_and(|a| (0..=limit).contains(&a))
}
/// Return versus prior close in bps, for trigger arithmetic only.
fn return_bps(o: &MarketObservation) -> Option<f64> {
    let bid = o.quote.bid.amount().to_string().parse::<f64>().ok()?;
    let close = o.prior_close?.amount().to_string().parse::<f64>().ok()?;
    (close > 0.0).then(|| (bid / close - 1.0) * 10_000.0)
}
/// Median return versus prior close across observed instruments.
pub fn market_bps<'a>(
    observations: impl Iterator<Item = &'a MarketObservation>,
    min_breadth: usize,
) -> Option<f64> {
    let mut r = observations.filter_map(return_bps).collect::<Vec<_>>();
    if r.len() < min_breadth.max(1) {
        return None;
    }
    r.sort_by(f64::total_cmp);
    let n = r.len();
    Some(if n % 2 == 1 {
        r[n / 2]
    } else {
        (r[n / 2 - 1] + r[n / 2]) / 2.0
    })
}
fn distance(a: Price, b: Price) -> Result<Amount> {
    Ok(if a >= b {
        a.amount().checked_sub(b.amount())?
    } else {
        b.amount().checked_sub(a.amount())?
    })
}
fn moved(a: Price, b: Price, bps: u32) -> Result<bool> {
    Ok(distance(a, b)?.checked_mul(Amount::from_integer(10_000))?
        >= b.amount()
            .checked_mul(Amount::from_unsigned(u64::from(bps)))?)
}
fn near(a: Price, b: Price, bps: u32) -> Result<bool> {
    Ok(distance(a, b)?.checked_mul(Amount::from_integer(10_000))?
        <= b.amount()
            .checked_mul(Amount::from_unsigned(u64::from(bps)))?)
}
impl EventState {
    pub fn new(binding: String, session: String) -> Result<Self> {
        if !name(&binding) || !name(&session) {
            return Err("event_binding_invalid".into());
        }
        Ok(Self {
            binding,
            session,
            last_now: None,
            markets: BTreeMap::new(),
            identities: BTreeMap::new(),
        })
    }
    pub fn binding(&self) -> &str {
        &self.binding
    }
    pub fn session(&self) -> &str {
        &self.session
    }
    fn check(&self, p: &EventPolicy, now: i64) -> Result<()> {
        p.validate()?;
        if !name(&self.binding)
            || !name(&self.session)
            || now < 0
            || self.last_now.is_some_and(|last| now < last)
            || self.markets.len() > p.max_instruments
            || self.identities.len() > p.max_identities
        {
            return Err("event_state_or_clock_invalid".into());
        }
        Ok(())
    }
    fn event(&self, kind: EventKind, key: &str, at: i64, evidence: Value) -> Result<Event> {
        let id = hash(&json!([
            self.binding,
            self.session,
            kind,
            key,
            at,
            evidence
        ]))?;
        Ok(Event {
            id,
            kind,
            instrument_key: key.into(),
            observed_ms: at,
            evidence,
        })
    }
    /// Price and volume anchors advance only on events. Gap fires once per key;
    /// proximity fires on entry into each band. Thresholds have no enabled defaults.
    pub fn observe_market(
        &mut self,
        p: &EventPolicy,
        o: &MarketObservation,
        now: i64,
    ) -> Result<Vec<Event>> {
        self.check(p, now)?;
        let key = &o.instrument.key;
        if !name(key)
            || !name(&o.instrument.symbol)
            || !name(&o.instrument.series)
            || !o.quote.fresh(now, MAX_FINANCIAL_AGE_MS)
            || o.quote.source > o.quote.receipt
            || o.quote.bid > o.quote.ask
            || !o
                .quote
                .bid
                .amount()
                .is_multiple_of(o.instrument.tick.amount())?
            || !o
                .quote
                .ask
                .amount()
                .is_multiple_of(o.instrument.tick.amount())?
        {
            return Err("event_market_invalid".into());
        }
        let old = self.markets.get(key);
        if let Some(old) = old {
            let q = &old.observation.quote;
            if old.observation.instrument != o.instrument
                || o.quote.source < q.source
                || o.quote.receipt < q.receipt
                || (o.quote.receipt == q.receipt
                    && (old.observation.quote != o.quote
                        || old.observation.cumulative_volume != o.cumulative_volume))
                || old
                    .observation
                    .prior_close
                    .is_some_and(|v| Some(v) != o.prior_close)
                // Upstox can replace ohlc.open before the first trade. Freeze it
                // only after traded volume has established the session open.
                || (old.observation.cumulative_volume.is_some_and(|n| n > 0)
                    && old
                        .observation
                        .session_open
                        .is_some_and(|v| Some(v) != o.session_open))
                || old
                    .observation
                    .cumulative_volume
                    .is_some_and(|a| o.cumulative_volume.is_none_or(|b| b < a))
            {
                return Err("event_market_identity_or_time_changed".into());
            }
        } else if self.markets.len() >= p.max_instruments {
            return Err("event_instrument_capacity".into());
        }
        let mut next = old.cloned().unwrap_or(MarketState {
            observation: o.clone(),
            price_anchor: o.quote.bid,
            volume_anchor: o.cumulative_volume,
            gap_emitted: false,
            target_near: false,
            stop_near: false,
            relative_anchor_bps: None,
        });
        let mut events = Vec::new();
        let relative = p
            .market_relative_min_breadth
            .and_then(|b| market_bps(self.markets.values().map(|m| &m.observation), b))
            .zip(return_bps(o));
        if let Some((market, own)) = relative {
            let rel = own - market;
            match next.relative_anchor_bps {
                Some(anchor)
                    if old.is_some()
                        && p.price_move_bps
                            .is_some_and(|bps| (rel - anchor).abs() >= f64::from(bps)) =>
                {
                    events.push(self.event(
                        EventKind::PriceMove,
                        key,
                        o.quote.receipt,
                        json!({"anchor":next.price_anchor,"relative_anchor_bps":anchor.round(),"relative_bps":rel.round(),"market_bps":market.round(),"observation":o}),
                    )?);
                    next.price_anchor = o.quote.bid;
                    next.relative_anchor_bps = Some(rel);
                }
                Some(_) => {}
                None => next.relative_anchor_bps = Some(rel),
            }
        } else if old.is_some()
            && let Some(bps) = p.price_move_bps
            && moved(o.quote.bid, next.price_anchor, bps)?
        {
            events.push(self.event(
                EventKind::PriceMove,
                key,
                o.quote.receipt,
                json!({"anchor":next.price_anchor,"observation":o}),
            )?);
            next.price_anchor = o.quote.bid;
        }
        if let Some(step) = p.volume_step_units
            && let Some(current) = o.cumulative_volume
        {
            if let Some(anchor) = next.volume_anchor
                && current.saturating_sub(anchor) >= step
            {
                events.push(self.event(
                    EventKind::VolumeMove,
                    key,
                    o.quote.receipt,
                    json!({"anchor":anchor,"observation":o}),
                )?);
                next.volume_anchor = Some(current);
            } else if next.volume_anchor.is_none() {
                next.volume_anchor = Some(current);
            }
        }
        if !next.gap_emitted
            && o.cumulative_volume.is_some_and(|n| n > 0)
            && let (Some(close), Some(open)) = (o.prior_close, o.session_open)
        {
            if let Some(bps) = p.gap_bps
                && moved(open, close, bps)?
            {
                events.push(self.event(
                    EventKind::Gap,
                    key,
                    o.quote.receipt,
                    json!({"observation":o}),
                )?);
            }
            next.gap_emitted = true;
        }
        let target = o.exit_intent.as_ref().and_then(|e| e.target);
        let stop = o.exit_intent.as_ref().and_then(|e| e.stop);
        for (label, level, was_near) in [
            ("target", target, &mut next.target_near),
            ("stop", stop, &mut next.stop_near),
        ] {
            let is_near = match (p.proximity_bps, level) {
                (Some(bps), Some(level)) => near(o.quote.bid, level, bps)?,
                _ => false,
            };
            if is_near && !*was_near {
                events.push(self.event(
                    EventKind::Proximity,
                    key,
                    o.quote.receipt,
                    json!({"trigger":label,"observation":o}),
                )?);
            }
            *was_near = is_near;
        }
        next.observation = o.clone();
        self.markets.insert(key.clone(), next);
        self.last_now = Some(now);
        Ok(events)
    }
    #[allow(clippy::too_many_arguments)] // Explicit externally identified evidence and injected time.
    fn identified(
        &mut self,
        p: &EventPolicy,
        kind: EventKind,
        id: &str,
        key: &str,
        at: i64,
        now: i64,
        evidence: Value,
    ) -> Result<Vec<Event>> {
        self.check(p, now)?;
        if !name(id) || !name(key) {
            return Err("event_identified_invalid".into());
        }
        let identity = hash(&json!([kind, id]))?;
        let body = hash(&evidence)?;
        if let Some(old) = self.identities.get(&identity) {
            if *old != body {
                return Err("event_identity_changed".into());
            }
            self.last_now = Some(now);
            return Ok(vec![]);
        }
        if !age(now, at, p.max_event_age_ms) {
            return Err("event_identified_invalid".into());
        }
        if self.identities.len() >= p.max_identities {
            return Err("event_identity_capacity".into());
        }
        let event = self.event(kind, key, at, evidence)?;
        self.identities.insert(identity, body);
        self.last_now = Some(now);
        Ok(vec![event])
    }
    pub fn observe_fill(
        &mut self,
        p: &EventPolicy,
        id: &str,
        fill: &Fill,
        now: i64,
    ) -> Result<Vec<Event>> {
        if fill.units == 0 || fill.fee < Amount::ZERO || !name(&fill.instrument.key) {
            return Err("event_fill_invalid".into());
        }
        if let Some(m) = self.markets.get(&fill.instrument.key)
            && m.observation.instrument != fill.instrument
        {
            return Err("event_fill_identity_changed".into());
        }
        self.identified(
            p,
            EventKind::Fill,
            id,
            &fill.instrument.key,
            fill.at,
            now,
            json!({"fill_id":id,"fill":fill}),
        )
    }
    pub fn observe_news(
        &mut self,
        p: &EventPolicy,
        n: &NewsObservation,
        now: i64,
    ) -> Result<Vec<Event>> {
        p.validate()?;
        let url = url::Url::parse(&n.citation).map_err(|_| "event_news_citation_invalid")?;
        if n.citation.len() > 2048
            || url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || n.content_hash.len() != 64
            || !n
                .content_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || n.published_ms > n.received_ms
            || n.received_ms > now
        {
            return Err("event_news_invalid".into());
        }
        self.check(p, now)?;
        let identity = hash(&json!([EventKind::News, n.id]))?;
        if let Some(old) = self.identities.get(&identity) {
            if *old != hash(&json!(n))? {
                return Err("event_identity_changed".into());
            }
            self.last_now = Some(now);
            return Ok(vec![]);
        }
        if !age(now, n.published_ms, p.max_news_age_ms)
            || !age(now, n.received_ms, p.max_event_age_ms)
        {
            return Ok(vec![]);
        }
        self.identified(
            p,
            EventKind::News,
            &n.id,
            &n.instrument_key,
            n.received_ms,
            now,
            json!(n),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::a;
    fn price(s: &str) -> Price {
        Price::try_from(a(s)).unwrap()
    }
    #[test]
    fn market_relative_price_moves_ignore_broad_market_moves() {
        let p = EventPolicy {
            price_move_bps: Some(100),
            volume_step_units: None,
            gap_bps: None,
            proximity_bps: None,
            max_event_age_ms: 10_000,
            max_news_age_ms: 60_000,
            max_instruments: 50,
            max_identities: 8,
            market_relative_min_breadth: Some(5),
        };
        let mut state = EventState::new("binding".into(), "2026-10-19".into()).unwrap();
        let obs = |key: &str, bid: &str, at: i64| MarketObservation {
            instrument: Instrument {
                key: key.into(),
                symbol: key.into(),
                series: "EQ".into(),
                tick: price("0.05"),
            },
            quote: Quote {
                bid: price(bid),
                ask: price(bid),
                bid_units: 10,
                ask_units: 10,
                source: at,
                receipt: at,
            },
            cumulative_volume: None,
            prior_close: Some(price("100")),
            session_open: None,
            exit_intent: None,
        };
        let keys = ["A", "B", "C", "D", "E", "F"];
        for k in keys {
            state
                .observe_market(&p, &obs(k, "100", 1000), 1000)
                .unwrap();
        }
        // Whole market falls 3%: absolute 300bps moves, but no relative move.
        let mut fired = 0;
        for k in keys {
            fired += state
                .observe_market(&p, &obs(k, "97", 1100), 1100)
                .unwrap()
                .len();
        }
        assert_eq!(fired, 0, "a broad market move alone is not a trigger");
        // F falls a further 2% against an unchanged market: a relative move fires.
        let events = state
            .observe_market(&p, &obs("F", "95", 1200), 1200)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, EventKind::PriceMove);
        assert_eq!(events[0].evidence["market_bps"], -300.0);
    }
    #[test]
    fn bounded_events_preserve_identity_freshness_and_deduplication() {
        let p = EventPolicy {
            price_move_bps: Some(100),
            volume_step_units: Some(10),
            gap_bps: Some(100),
            proximity_bps: Some(100),
            max_event_age_ms: 10_000,
            max_news_age_ms: 60_000,
            max_instruments: 2,
            max_identities: 8,
            market_relative_min_breadth: None,
        };
        let mut state = EventState::new("binding".into(), "2026-10-19".into()).unwrap();
        let mut o = MarketObservation {
            instrument: Instrument {
                key: "NSE_EQ|FIXTURE".into(),
                symbol: "FIXTURE".into(),
                series: "EQ".into(),
                tick: price("0.05"),
            },
            quote: Quote {
                bid: price("100"),
                ask: price("100.05"),
                bid_units: 10,
                ask_units: 10,
                source: 1000,
                receipt: 1000,
            },
            cumulative_volume: Some(100),
            prior_close: Some(price("98")),
            session_open: Some(price("100")),
            exit_intent: Some(ExitIntent {
                target: Some(price("101")),
                stop: Some(price("99")),
                time: None,
            }),
        };
        let events = state.observe_market(&p, &o, 1000).unwrap();
        assert!(events.iter().any(|e| e.kind == EventKind::Gap));
        assert!(
            events
                .iter()
                .any(|e| e.kind == EventKind::Proximity && e.evidence["trigger"] == "target")
        );
        assert!(state.observe_market(&p, &o, 1000).unwrap().is_empty());
        let mut restored: EventState =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        o.quote.bid = price("101");
        o.quote.ask = price("101.05");
        o.quote.source = 1100;
        o.quote.receipt = 1100;
        o.cumulative_volume = Some(110);
        let changes = state.observe_market(&p, &o, 1100).unwrap();
        assert_eq!(changes, restored.observe_market(&p, &o, 1100).unwrap());
        assert!(changes.iter().any(|e| e.kind == EventKind::PriceMove));
        assert!(changes.iter().any(|e| e.kind == EventKind::VolumeMove));
        assert!(!changes.iter().any(|e| e.kind == EventKind::Gap));
        let mut stop_state = state.clone();
        let mut stop_observation = o.clone();
        stop_observation.quote.bid = price("99");
        stop_observation.quote.ask = price("99.05");
        stop_observation.quote.source = 1200;
        stop_observation.quote.receipt = 1200;
        assert!(
            stop_state
                .observe_market(&p, &stop_observation, 1200)
                .unwrap()
                .iter()
                .any(|e| e.kind == EventKind::Proximity && e.evidence["trigger"] == "stop")
        );
        let before = state.clone();
        assert!(state.observe_market(&p, &o, 3101).is_err());
        assert_eq!(state, before);
        assert!(state.observe_market(&p, &o, 1099).is_err());
        let mut bad = o.clone();
        bad.quote.bid = price("100.95");
        assert!(state.observe_market(&p, &bad, 1100).is_err());
        assert_eq!(state, before);
        bad = o.clone();
        bad.quote.source = 1200;
        bad.quote.receipt = 1200;
        bad.cumulative_volume = Some(109);
        assert!(state.observe_market(&p, &bad, 1200).is_err());
        bad = o.clone();
        bad.instrument.series = "BE".into();
        assert!(state.observe_market(&p, &bad, 1100).is_err());
        let fill = Fill {
            at: 1100,
            instrument: o.instrument.clone(),
            units: 1,
            buy: true,
            price: price("101"),
            fee: a("25"),
            costs: None,
            expected_price: None,
            slippage_bps: None,
            decision_id: Some("decision".into()),
        };
        let filled = state.observe_fill(&p, "fill-0", &fill, 1100).unwrap();
        assert_eq!(filled.len(), 1);
        assert_eq!(filled[0].kind, EventKind::Fill);
        assert!(
            state
                .observe_fill(&p, "fill-0", &fill, 1100)
                .unwrap()
                .is_empty()
        );
        let mut changed = fill.clone();
        changed.units = 2;
        let before = state.clone();
        assert!(state.observe_fill(&p, "fill-0", &changed, 1100).is_err());
        assert_eq!(state, before);
        let news = NewsObservation {
            id: "news-1".into(),
            instrument_key: o.instrument.key.clone(),
            published_ms: 1000,
            received_ms: 1100,
            citation: "https://example.org/story".into(),
            content_hash: "a".repeat(64),
        };
        assert_eq!(
            state.observe_news(&p, &news, 1100).unwrap()[0].kind,
            EventKind::News
        );
        assert!(state.observe_news(&p, &news, 1100).unwrap().is_empty());
        let mut changed = news.clone();
        changed.content_hash = "b".repeat(64);
        assert!(state.observe_news(&p, &changed, 1100).is_err());
        changed = news.clone();
        changed.published_ms = 1200;
        assert!(state.observe_news(&p, &changed, 1100).is_err());
        changed = news.clone();
        changed.citation = "http://example.org".into();
        assert!(state.observe_news(&p, &changed, 1100).is_err());
        assert!(state.observe_fill(&p, "late", &fill, 11101).is_err());
        let mut disabled = p.clone();
        disabled.price_move_bps = None;
        disabled.gap_bps = None;
        disabled.volume_step_units = None;
        disabled.proximity_bps = None;
        let mut quiet = EventState::new("binding".into(), "session".into()).unwrap();
        assert!(
            quiet
                .observe_market(&disabled, &o, 3100)
                .unwrap()
                .is_empty()
        );
        disabled.max_event_age_ms = 0;
        assert!(disabled.validate().is_err());
        let mut capped = p.clone();
        capped.max_identities = 1;
        let mut one = EventState::new("binding".into(), "session".into()).unwrap();
        one.observe_fill(&capped, "one", &fill, 1100).unwrap();
        assert!(one.observe_fill(&capped, "two", &fill, 1100).is_err());
        let mut aged = state.clone();
        assert!(aged.observe_news(&p, &news, 100_000).unwrap().is_empty());
        let mut changed = news.clone();
        changed.content_hash = "b".repeat(64);
        assert!(aged.observe_news(&p, &changed, 100_000).is_err());
        let mut late_gap = EventState::new("binding".into(), "session".into()).unwrap();
        let mut late = o.clone();
        late.prior_close = None;
        late.session_open = Some(price("98"));
        late_gap.observe_market(&p, &late, 1100).unwrap();
        late.prior_close = Some(price("98"));
        late.quote.source = 1200;
        late.quote.receipt = 1200;
        assert!(
            !late_gap
                .observe_market(&p, &late, 1200)
                .unwrap()
                .iter()
                .any(|e| e.kind == EventKind::Gap)
        );
        late.exit_intent = Some(ExitIntent {
            target: Some(price("101")),
            ..ExitIntent::default()
        });
        assert!(late_gap.observe_market(&p, &late, 1200).is_ok());
        // A zero-volume open is indicative. Revising it once volume prints is
        // the live quote shape; changing it again after trades is not.
        let mut indicative = EventState::new("binding".into(), "session".into()).unwrap();
        let mut early = MarketObservation {
            instrument: o.instrument.clone(),
            quote: Quote {
                bid: price("100"),
                ask: price("100.05"),
                bid_units: 10,
                ask_units: 10,
                source: 1000,
                receipt: 1000,
            },
            cumulative_volume: Some(0),
            prior_close: Some(price("98")),
            session_open: Some(price("97")),
            exit_intent: None,
        };
        assert!(
            indicative
                .observe_market(&p, &early, 1000)
                .unwrap()
                .iter()
                .all(|e| e.kind != EventKind::Gap)
        );
        early.session_open = Some(price("90"));
        early.cumulative_volume = Some(10);
        early.quote.source = 1100;
        early.quote.receipt = 1100;
        assert!(
            indicative
                .observe_market(&p, &early, 1100)
                .unwrap()
                .iter()
                .any(|e| e.kind == EventKind::Gap)
        );
        early.session_open = Some(price("91"));
        early.cumulative_volume = Some(11);
        early.quote.source = 1200;
        early.quote.receipt = 1200;
        assert!(indicative.observe_market(&p, &early, 1200).is_err());
    }
}
