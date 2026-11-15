//! Reusable offline transport contracts; no session activation or provider client.
pub mod native_transport;
pub mod orchestrator;

pub mod bootstrap;
pub mod bootstrap_cli;
pub mod bootstrap_stage;

pub mod dashboard_host;
pub mod lock;
pub mod model_stream;

pub mod archive_reconcile;
pub mod candidate_execution;
pub mod catalog_measure;
pub mod strategy_actions;
pub mod strategy_lab;

pub mod strategy_archive;
pub mod strategy_pipeline;
pub mod strategy_scorer;

pub mod strategy_approval;

pub mod strategy_report;

pub mod scorer_compare;
