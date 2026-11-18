//! Explicit offline paper simulation. No model subscription or market client is constructed.
use super::*;
use nivra_core::{execution::Instrument, finance::Amount};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
mod export;
mod receipts;
pub(crate) use receipts::*;
mod tape;
pub(crate) use tape::*;
