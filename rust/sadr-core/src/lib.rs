//! Offline deterministic contracts. No network, credentials or trading authority.

pub mod controller;
pub mod feature_store;
pub mod finance;
pub mod historical;
pub mod market;
pub mod native;
pub mod native_grant;
pub mod openai_grant;
pub mod prefilter;
pub mod regime;
pub mod resources;
pub mod time;

pub mod book_phase;

pub mod execution;

pub mod device_challenge;

pub mod discovery;

pub mod token_envelope;

pub mod native_stream;

pub mod protection_gap;

pub mod service_review;

pub mod research_acceptance;

pub mod redecision;
pub mod replay;

pub mod postclose;

pub mod daily_research;

pub mod strategy;
