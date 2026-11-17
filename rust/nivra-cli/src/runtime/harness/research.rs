//! Trusted registrations only. The model selects aliases, never code or host paths.
use super::*;
use nivra_cli::lock::AdvisoryLock;
use nivra_research::{
    bubblewrap,
    worker::{self, Outcome, RegisteredRun, ResourceLimits},
};
use nivra_store::research_jobs as jobs;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;

mod candidate;
mod capture;
mod carry;
mod continuation;
mod measure;
mod runtime;
mod settings;
mod tasks;
pub(crate) use candidate::*;
pub(crate) use capture::tick as capture_observation_tick;
pub(crate) use carry::*;
pub(crate) use continuation::*;
pub(crate) use runtime::*;
pub(crate) use settings::*;
pub(crate) use tasks::*;

#[cfg(test)]
mod tests;
