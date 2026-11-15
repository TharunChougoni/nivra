//! Legacy paper-ledger and quote-archive inspection, without execution authority.
//!
//! Original candidate bytes are preserved, never regenerated with Rust's JSON
//! encoding. The inspectors do not open paths, initialize or migrate books, or
//! authorize recovery. `binding` opens only caller-supplied synthetic paths to
//! record a cooperative lease. It does not repair those files or grant trading
//! or installation authority. `journal` reopens a caller-supplied connection
//! and does not open a configured path or grant a replacement allowance.

pub mod archive;
pub mod archive_storage;
pub mod binding;
pub mod controller;
pub mod install_preflight;
pub mod journal;
pub mod legacy;
pub mod market_archive;
pub mod native_grant;
pub mod paper_source;
pub mod protection;
pub mod protection_storage;
pub mod research_jobs;
pub mod writer;

use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    Candidate,
    CandidateHash,
    Schema,
    Rows,
    Bound,
    Arithmetic,
    Snapshot,
    Archive,
    ArchiveHash,
    Quote,
    Protection,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Candidate => "legacy_candidate_invalid",
            Self::CandidateHash => "legacy_candidate_hash_mismatch",
            Self::Schema => "legacy_store_schema_unsupported",
            Self::Rows => "legacy_ledger_rows_invalid",
            Self::Bound => "legacy_store_limit_exceeded",
            Self::Arithmetic => "legacy_store_exact_arithmetic_unavailable",
            Self::Snapshot => "legacy_ledger_snapshot_unavailable",
            Self::Archive => "legacy_quote_archive_invalid",
            Self::ArchiveHash => "legacy_quote_archive_hash_mismatch",
            Self::Quote => "legacy_quote_rederivation_unavailable",
            Self::Protection => "paper_protection_evidence_unavailable",
        })
    }
}

impl Error for StoreError {}

pub mod native_binding;

pub mod native_lease;

pub mod native_call;

pub mod native_journal;

pub mod evidence_archive;

pub mod analytics_token;

pub mod timing_journal;

pub mod protection_diagnostics;

pub mod protection_timing;

pub mod research_attempt;

pub mod research_router;

pub mod research_runtime;

pub mod strategy_results;
