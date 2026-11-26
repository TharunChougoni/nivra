//! Stored event state bound to the planner's paper book.
use crate::execution::{ExitIntent, Instrument, Quote};
use crate::finance::Price;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MarketObservation {
    instrument: Instrument,
    quote: Quote,
    cumulative_volume: Option<u64>,
    prior_close: Option<Price>,
    session_open: Option<Price>,
    exit_intent: Option<ExitIntent>,
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
fn name(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= 256
}
impl EventState {
    pub fn new(binding: String, session: String) -> Result<Self, &'static str> {
        if !name(&binding) || !name(&session) {
            return Err("event_binding_invalid");
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
}
