//! Offline deterministic contracts. No network, credentials or trading authority.

pub mod admission;
pub mod backtest;
pub mod candidate;
pub mod capability;
pub mod captured_source;
pub mod controller;
pub mod feature_store;
pub mod finance;
pub mod historical;
pub mod market;
pub mod market_capture;
pub mod native;
pub mod native_grant;
pub mod openai_grant;
pub mod paper_bundle;
pub mod paper_source;
pub mod prefilter;
pub mod regime;
pub mod renewal;
pub mod resources;
pub mod review;
pub mod risk;
pub mod sandbox;
pub mod scanner;
pub mod time;

pub mod book_phase;

pub mod execution;

pub mod device_challenge;

pub mod discovery;

pub mod token_envelope;

pub mod native_stream;

pub mod native_request;

pub mod native_spec;

pub mod research_context;

pub mod research_proposal;

pub mod research_script;

pub mod research_review;

pub mod research_evidence;

pub mod research_signal;

pub mod decimal_text;

pub mod research_prompt;

pub mod protection_gap;

pub mod service_review;

pub mod research_acceptance;

pub mod redecision;
pub mod replay;

pub mod postclose;

pub mod daily_research;

pub mod strategy;
