//! Streaming historical quote adapter for the shared dated simulation engine.
//! Declaration hashes do not authenticate a dataset, calendar or historical entitlement.
use crate::{
    execution::{Instrument, POLICY, Portfolio, Quote, Scanner},
    finance::{Amount, CashOrderGrid},
};
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Development,
    Training,
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptClock {
    VirtualHistorical,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Bindings {
    pub engine: String,
    pub strategy: String,
    pub data: String,
    pub calendar: String,
    pub fees: String,
    pub slippage: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub session: NaiveDate,
    pub warmup_start: NaiveDate,
    #[serde(default)]
    pub paper_costs: Option<crate::finance::FeeArithmetic>,
    pub policy: String,
    pub split: Split,
    pub receipt_clock: ReceiptClock,
    pub bindings: Bindings,
    pub seed: u64,
    pub universe: Vec<Instrument>,
}
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Protection,
    Screen,
    Entry,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub key: String,
    pub quote: Quote,
    pub lower_circuit_limit: Amount,
    pub upper_circuit_limit: Amount,
    pub volume: u64,
    #[serde(default)]
    pub session_vwap: Option<f64>,
    #[serde(default)]
    pub total_buy_quantity: Option<u64>,
    #[serde(default)]
    pub total_sell_quantity: Option<u64>,
    /// Original archived receipt SHA for THIS observation. A multi-key tape
    /// never shares one global archive SHA. Omitted for legacy tapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_sha256: Option<String>,
    /// Completion/event clock of the original receipt, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
}
impl Observation {
    pub fn fields(&self) -> crate::execution::MarketFields {
        crate::execution::MarketFields {
            session_vwap: self.session_vwap,
            best_bid: self.quote.bid.amount().to_string().parse().ok(),
            best_bid_qty: Some(self.quote.bid_units).filter(|q| *q > 0),
            best_ask: self.quote.ask.amount().to_string().parse().ok(),
            best_ask_qty: Some(self.quote.ask_units).filter(|q| *q > 0),
            total_buy_quantity: self.total_buy_quantity,
            total_sell_quantity: self.total_sell_quantity,
            lower_circuit_limit: self.lower_circuit_limit,
            upper_circuit_limit: self.upper_circuit_limit,
            volume: self.volume,
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub at: i64,
    pub available_at: i64,
    pub kind: EventKind,
    pub observations: Vec<Observation>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Manifest,
    Bound,
    Chronology,
    Leakage,
    Identity,
    Quote,
    Engine,
}
pub struct Runner {
    manifest: Manifest,
    portfolio: Portfolio,
    scanner: Scanner,
    last: Option<i64>,
    events: u64,
    max_drawdown: Amount,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub declared_bindings: Bindings,
    pub receipt_clock: ReceiptClock,
    pub dataset_split: Split,
    pub session: NaiveDate,
    pub warmup_start: NaiveDate,
    pub engine_policy: String,
    pub seed: u64,
    pub events: u64,
    pub portfolio: Portfolio,
    pub fees: Amount,
    pub turnover: Amount,
    pub realized_pnl_if_flat: Option<Amount>,
    pub maximum_drawdown: Amount,
    pub promotion_authorized: bool,
    pub operationally_accepted: bool,
    pub limitations: Vec<&'static str>,
}
fn date(at: i64) -> Result<NaiveDate, Error> {
    Ok(DateTime::<Utc>::from_timestamp_millis(at)
        .ok_or(Error::Chronology)?
        .with_timezone(&FixedOffset::east_opt(19800).ok_or(Error::Chronology)?)
        .date_naive())
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Runner {
    pub fn new(manifest: Manifest) -> Result<Self, Error> {
        let h = &manifest.bindings;
        if let Some(costs) = &manifest.paper_costs {
            costs.validate().map_err(|_| Error::Manifest)?;
        }
        if manifest.policy != POLICY
            || manifest.warmup_start > manifest.session
            || manifest.universe.is_empty()
            || manifest.universe.len() > 10000
            || [
                &h.engine,
                &h.strategy,
                &h.data,
                &h.calendar,
                &h.fees,
                &h.slippage,
            ]
            .iter()
            .any(|s| !hash(s))
        {
            return Err(Error::Manifest);
        }
        let mut keys = BTreeSet::new();
        for i in &manifest.universe {
            if i.key.is_empty()
                || i.key.len() > 128
                || i.symbol.is_empty()
                || i.symbol.len() > 64
                || i.series != "EQ"
                || !keys.insert(&i.key)
            {
                return Err(Error::Identity);
            }
        }
        let mut portfolio = Portfolio::new();
        portfolio.costs = manifest.paper_costs.clone();
        Ok(Self {
            manifest,
            portfolio,
            scanner: Scanner::default(),
            last: None,
            events: 0,
            max_drawdown: Amount::ZERO,
        })
    }
    pub fn apply(&mut self, event: Event) -> Result<(), Error> {
        if self.events >= 1_000_000 || event.observations.len() > 200 {
            return Err(Error::Bound);
        }
        if self.last.is_some_and(|last| event.at <= last)
            || date(event.at)? < self.manifest.warmup_start
            || date(event.at)? > self.manifest.session
        {
            return Err(Error::Chronology);
        }
        if event.available_at > event.at {
            return Err(Error::Leakage);
        }
        let mut quotes = BTreeMap::new();
        // Validate the whole event before mutating strategy/financial state.
        for obs in &event.observations {
            let i = self
                .manifest
                .universe
                .iter()
                .find(|i| i.key == obs.key)
                .ok_or(Error::Identity)?;
            if quotes.insert(obs.key.clone(), obs.quote.clone()).is_some() {
                return Err(Error::Identity);
            }
            let q = &obs.quote;
            if q.source > q.receipt || q.receipt > event.available_at {
                return Err(Error::Leakage);
            }
            let grid = CashOrderGrid::new(1, i.tick).map_err(|_| Error::Quote)?;
            if q.bid > q.ask || grid.validate(1, q.bid).is_err() || grid.validate(1, q.ask).is_err()
            {
                return Err(Error::Quote);
            }
        }
        let mut scanner = self.scanner.clone();
        if matches!(event.kind, EventKind::Screen) {
            for obs in &event.observations {
                let i = self
                    .manifest
                    .universe
                    .iter()
                    .find(|i| i.key == obs.key)
                    .ok_or(Error::Identity)?;
                scanner
                    .observe_fields(event.at, i, &obs.quote, &obs.fields())
                    .map_err(|_| Error::Engine)?;
            }
        }
        let mut entries = vec![];
        if matches!(event.kind, EventKind::Entry) {
            entries = scanner
                .ranked(event.at, &self.portfolio, &self.manifest.universe)
                .into_iter()
                .filter(|i| {
                    event
                        .observations
                        .iter()
                        .find(|o| o.key == i.key)
                        .is_some_and(|o| {
                            Scanner::market_allows_fields(event.at, i, &o.quote, &o.fields())
                        })
                })
                .collect();
        }
        // Data-only historical validation remains available without a cost policy.
        // A financial action cannot execute without one.
        if self.portfolio.costs.is_none()
            && (!entries.is_empty() || !self.portfolio.positions.is_empty())
        {
            return Err(Error::Manifest);
        }
        let mut next = self.portfolio.clone();
        next.apply_at(self.manifest.session, event.at, &quotes, &entries)
            .map_err(|_| Error::Engine)?;
        self.max_drawdown = self.max_drawdown.max(
            next.high_water
                .checked_sub(next.equity().map_err(|_| Error::Engine)?)
                .map_err(|_| Error::Engine)?,
        );
        self.portfolio = next;
        self.scanner = scanner;
        self.last = Some(event.at);
        self.events += 1;
        Ok(())
    }
    pub fn portfolio(&self) -> &Portfolio {
        &self.portfolio
    }
    pub fn finish(self) -> Result<Report, Error> {
        if self.events == 0 {
            return Err(Error::Bound);
        }
        let mut fees = Amount::ZERO;
        let mut turnover = Amount::ZERO;
        for f in &self.portfolio.fills {
            fees = fees.checked_add(f.fee).map_err(|_| Error::Engine)?;
            turnover = turnover
                .checked_add(
                    f.price
                        .amount()
                        .checked_mul(Amount::from_unsigned(f.units))
                        .map_err(|_| Error::Engine)?,
                )
                .map_err(|_| Error::Engine)?;
        }
        let pnl = if self.portfolio.positions.is_empty() {
            Some(
                self.portfolio
                    .cash
                    .checked_sub(self.portfolio.allocation().map_err(|_| Error::Engine)?)
                    .map_err(|_| Error::Engine)?,
            )
        } else {
            None
        };
        Ok(Report {
            declared_bindings: self.manifest.bindings,
            receipt_clock: self.manifest.receipt_clock,
            dataset_split: self.manifest.split,
            session: self.manifest.session,
            warmup_start: self.manifest.warmup_start,
            engine_policy: POLICY.into(),
            seed: self.manifest.seed,
            events: self.events,
            portfolio: self.portfolio,
            fees,
            turnover,
            realized_pnl_if_flat: pnl,
            maximum_drawdown: self.max_drawdown,
            promotion_authorized: false,
            operationally_accepted: false,
            limitations: vec![
                "declared hashes are not authenticated provenance",
                "historical virtual receipts do not certify real financial freshness",
                "point-in-time membership, payload coverage, cost stress and sealed holdout acceptance remain pending",
            ],
        })
    }
}
