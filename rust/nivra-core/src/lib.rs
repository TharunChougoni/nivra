//! Offline deterministic contracts. No network, credentials or trading authority.

pub mod feature_store;
pub mod finance;
pub mod historical;
pub mod market;
pub mod native;
pub mod prefilter;
pub mod regime;
pub mod release;
pub mod resources;
pub mod time;

pub mod book_phase;

pub mod execution;

pub mod discovery;

pub mod protection_gap;

pub mod service_review;

pub mod research_acceptance;

pub mod redecision;
pub mod replay;

pub mod postclose;

pub mod daily_research;

pub mod strategy;
