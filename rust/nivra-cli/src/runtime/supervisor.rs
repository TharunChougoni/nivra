//! Intraday orchestrator supervisor. It may wake Luna and write directives.
//! It never creates, sizes or blocks an order, and a failure is a skip, not a halt.
use super::directives::{self, Limits};
use super::operational_health::{
    BackoffSettings, SupervisorAside, SupervisorBackoff, SupervisorBudget, SupervisorCompaction,
    SupervisorConfigSource, SupervisorContext, SupervisorLastRun, SupervisorOperationalStatus,
    SupervisorOwnerFailureBackoff, SupervisorParked, SupervisorPending, SupervisorPendingSource,
    SupervisorResearch, SupervisorStateSource, SupervisorStatus,
};
use super::orchestrator_model::{self, SessionTrigger, Trigger};
use super::planner::{PlannerConfig, Subscription};
use super::session;
use super::*;
mod consider;
mod decision;
pub(crate) mod research;
mod review;
mod state;
#[cfg(test)]
use consider::consider;
pub(crate) use consider::{Tick, consider_owner};
use decision::*;
mod wake;
#[cfg(test)]
use self::state::Parked;
use self::state::STATE_PATH;
use self::state::{
    CompactionFailure, ContextExhaustion, State, allowance_recovery_allowed,
    clear_context_exhaustion, load, park, record_context_exhaustion, record_failure,
    record_failure_with_evidence, record_request_envelope, record_skip, record_state_error,
    with_state,
};
use self::wake::due;
pub(crate) use self::wake::{Inputs, Wake};
use crate::alert::{Alerts, Event};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
mod accounting;
pub(crate) use accounting::*;
mod capture;
pub(crate) use capture::*;
mod worker;
pub(crate) use worker::*;
mod prompt;
pub(crate) use prompt::*;
mod turns;
pub(crate) use turns::*;
mod replies;
pub(crate) use replies::*;
mod aside;
pub(crate) use aside::*;
mod status;
pub(crate) use status::*;

/// Turns between host-triggered compaction summaries. The log is never
/// rewritten; the summary is appended and later prompts start after it (#144).
const COMPACT_AFTER_TURNS: usize = 12;
/// A note that keeps changing mid-turn can re-run a bounded number of times;
/// after that the turn is dropped with an alert rather than starving the day (#143).
const MAX_NOTE_RERUNS: u32 = 3;
/// Bounded supervisor state accepted by the read-only operational projection.
const MAX_STATUS_STATE_BYTES: u64 = 64 * 1024;

#[cfg(test)]
mod lane_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{exercise_chains, run_named};

/// Test-only projection of the verified research evidence. Production callers
/// use the admission and schedule paths, which apply the same projection.
#[cfg(test)]
pub(crate) fn research_due_evidence(
    c: &Config,
    cfg: &PlannerConfig,
) -> Result<Vec<owner_tasks::ResearchEvidenceV1>> {
    Ok(research::due_evidence(c, cfg)?.evidence)
}

// Test-only injected schedule source. It proves the controller projection
// reads the schedule once; production builds do not contain it.
#[cfg(test)]
thread_local! {
    static TEST_RESEARCH_SCHEDULES: std::cell::RefCell<
        std::collections::VecDeque<owner_tasks::ResearchSchedule>,
    > = const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
    static TEST_RESEARCH_SCHEDULE_READS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn push_research_test_schedule(schedule: owner_tasks::ResearchSchedule) {
    TEST_RESEARCH_SCHEDULES.with(|queue| queue.borrow_mut().push_back(schedule));
}

#[cfg(test)]
pub(crate) fn clear_research_test_schedules() {
    TEST_RESEARCH_SCHEDULES.with(|queue| queue.borrow_mut().clear());
}

#[cfg(test)]
pub(crate) fn reset_research_schedule_reads() {
    TEST_RESEARCH_SCHEDULE_READS.with(|reads| reads.set(0));
}

#[cfg(test)]
pub(crate) fn research_schedule_reads() -> usize {
    TEST_RESEARCH_SCHEDULE_READS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(super) fn test_research_schedule_override() -> Option<owner_tasks::ResearchSchedule> {
    TEST_RESEARCH_SCHEDULE_READS.with(|reads| reads.set(reads.get() + 1));
    TEST_RESEARCH_SCHEDULES.with(|queue| queue.borrow_mut().pop_front())
}
