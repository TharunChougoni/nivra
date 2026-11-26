//! Bounded pre-open daily history acquisition and pure coverage assessment.
use super::{Config, Result, data, history_download, market_archive};
use chrono::{NaiveDate, Timelike};
use nivra_core::execution::Instrument;
use nivra_store::market_archive::{DailyCoverage, daily_coverage};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

#[derive(Clone)]
pub struct PreparationRequest {
    pub config: Config,
    pub instruments: Vec<Instrument>,
    pub target_session: NaiveDate,
    pub previous_session: NaiveDate,
    /// Exactly the reviewed trading sessions whose daily bars qualify readiness.
    pub required_sessions: Vec<NaiveDate>,
    pub window: PreparationWindow,
    /// Absolute Unix milliseconds, bounded by `window`.
    pub deadline_ms: i64,
}

/// When a preparation attempt runs relative to the configured pre-open cutoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreparationWindow {
    /// The configured pre-open window; acquisition ends at the cutoff.
    PreOpen,
    /// A session adopted after the cutoff runs one attempt right away. It keeps
    /// the pre-open window's length and never runs past the session end.
    LateStart,
}

impl PreparationWindow {
    /// Whether an attempt may start at `minute` (IST minute of day).
    pub fn admits(self, c: &Config, minute: u32) -> bool {
        match self {
            Self::PreOpen => (c.history_preparation_start_minute
                ..c.history_preparation_cutoff_minute)
                .contains(&minute),
            Self::LateStart => (c.history_preparation_cutoff_minute
                ..super::phases::LATE_START_CUTOFF_MINUTE)
                .contains(&minute),
        }
    }

    /// The latest acquisition deadline for an attempt started at `now`.
    pub fn deadline_ms(self, c: &Config, session: NaiveDate, now: i64) -> Result<i64> {
        let cutoff = session_minute_ms(session, c.history_preparation_cutoff_minute)?;
        Ok(match self {
            Self::PreOpen => cutoff,
            Self::LateStart => {
                let length = i64::from(
                    c.history_preparation_cutoff_minute - c.history_preparation_start_minute,
                ) * 60_000;
                now.saturating_add(length)
                    .min(session_minute_ms(session, c.execution.session_end_minute)?)
            }
        })
    }
}

fn session_minute_ms(session: NaiveDate, minute: u32) -> Result<i64> {
    Ok(session
        .and_hms_opt(minute / 60, minute % 60, 0)
        .ok_or("history_preparation_request")?
        .and_local_timezone(
            chrono::FixedOffset::east_opt(19_800).ok_or("history_preparation_request")?,
        )
        .single()
        .ok_or("history_preparation_request")?
        .timestamp_millis())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CoverageStatus {
    Ready,
    Partial,
    Failed,
    Blocked,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoverageReport {
    /// Derived from genuine daily bars in the reviewed sessions.
    #[serde(skip)]
    pub daily_volumes: BTreeMap<String, (f64, u32)>,
    pub target_session: NaiveDate,
    pub previous_session: NaiveDate,
    pub status: CoverageStatus,
    pub instruments: usize,
    pub ready_instruments: usize,
    pub required_bars: usize,
    pub observed_bars: usize,
    pub missing_bars: usize,
    /// Missing bars a validated provider receipt answered past, or an answer with
    /// no bar at all for the instrument (e.g. listing or suspension gaps).
    #[serde(default)]
    pub covered_empty_bars: usize,
    /// Missing bars a receipt cannot vouch for (unfetched, or after the last
    /// bar the provider returned) and that an attempt may still fill.
    #[serde(default)]
    pub retryable_missing_bars: usize,
    /// Exact unobserved sessions per instrument; never infer bars from exclusions.
    #[serde(default)]
    pub missing_sessions: BTreeMap<String, Vec<NaiveDate>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<i64>,
}

pub(super) fn base_report(
    target_session: NaiveDate,
    previous_session: NaiveDate,
    instruments: usize,
    required_bars: usize,
) -> CoverageReport {
    CoverageReport {
        daily_volumes: BTreeMap::new(),
        target_session,
        previous_session,
        status: CoverageStatus::Unknown,
        instruments,
        ready_instruments: 0,
        required_bars,
        observed_bars: 0,
        missing_bars: required_bars,
        covered_empty_bars: 0,
        retryable_missing_bars: required_bars,
        missing_sessions: BTreeMap::new(),
        failure: None,
        retry_at_ms: None,
    }
}

pub fn coverage_report(
    config: &Config,
    instruments: &[Instrument],
    required_sessions: &[NaiveDate],
    target_session: NaiveDate,
    previous_session: NaiveDate,
) -> CoverageReport {
    let mut report = base_report(
        target_session,
        previous_session,
        instruments.len(),
        instruments.len().saturating_mul(required_sessions.len()),
    );
    if instruments.is_empty()
        || required_sessions.is_empty()
        || required_sessions.last() != Some(&previous_session)
        || required_sessions.windows(2).any(|w| w[0] >= w[1])
        || previous_session >= target_session
    {
        report.failure = Some("history_coverage_request".into());
        return report;
    }
    let keys: BTreeSet<_> = instruments.iter().map(|i| i.key.as_str()).collect();
    if keys.len() != instruments.len() {
        report.failure = Some("history_universe_duplicate".into());
        return report;
    }
    let keys: Vec<_> = keys.into_iter().collect();
    match market_archive::open(config)
        .and_then(|mut db| daily_coverage(&mut db, &keys, required_sessions))
    {
        Ok(rows) => apply_coverage(report, rows),
        Err(error) => {
            report.failure = Some(
                match super::failure_code(error.as_ref()).as_str() {
                    "archive_config_missing" => "archive_config_missing",
                    _ => "history_coverage_unreadable",
                }
                .into(),
            );
            report
        }
    }
}

pub(super) fn apply_coverage(
    mut report: CoverageReport,
    rows: Vec<DailyCoverage>,
) -> CoverageReport {
    for row in rows {
        report.observed_bars += row.observed as usize;
        report.covered_empty_bars += row.covered_empty;
        if let Some(average) = row.average_volume {
            report
                .daily_volumes
                .insert(row.key.clone(), (average, row.observed));
        }
        if row.missing.is_empty() {
            report.ready_instruments += 1;
        } else {
            report.missing_sessions.insert(row.key, row.missing);
        }
    }
    report.missing_bars = report.required_bars - report.observed_bars;
    report.retryable_missing_bars = report.missing_bars - report.covered_empty_bars;
    report.status = if report.ready_instruments == report.instruments {
        CoverageStatus::Ready
    } else {
        CoverageStatus::Partial
    };
    report
}

pub struct PreparationWorker {
    cancel: Arc<AtomicBool>,
    join: Option<JoinHandle<CoverageReport>>,
    failed: CoverageReport,
    deadline_ms: i64,
}
impl PreparationWorker {
    pub fn spawn(request: PreparationRequest) -> Result<Self> {
        let now = super::millis();
        let local_now = super::local(now);
        let latest = request
            .window
            .deadline_ms(&request.config, request.target_session, now)?;
        if request.instruments.is_empty()
            || request.required_sessions.len() != request.config.daily_history_sessions
            || request.required_sessions.last() != Some(&request.previous_session)
            || request.required_sessions.windows(2).any(|w| w[0] >= w[1])
            || request.previous_session >= request.target_session
            || request.deadline_ms <= now
            || request.deadline_ms > latest
            || local_now.date_naive() != request.target_session
            || !request
                .window
                .admits(&request.config, local_now.hour() * 60 + local_now.minute())
        {
            return Err("history_preparation_request".into());
        }
        let mut failed = base_report(
            request.target_session,
            request.previous_session,
            request.instruments.len(),
            request
                .instruments
                .len()
                .saturating_mul(request.config.daily_history_sessions),
        );
        failed.status = CoverageStatus::Blocked;
        failed.failure = Some("history_worker_failed".into());
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let deadline_ms = request.deadline_ms;
        let join = thread::Builder::new()
            .name("paper-history-preparation".into())
            .spawn(move || run(request, worker_cancel))?;
        Ok(Self {
            cancel,
            join: Some(join),
            failed,
            deadline_ms,
        })
    }

    pub fn deadline_ms(&self) -> i64 {
        self.deadline_ms
    }

    /// Returns the final report once, then `None` on all later polls.
    pub fn poll(&mut self) -> Option<CoverageReport> {
        let join = self.join.as_ref()?;
        if !join.is_finished() {
            return None;
        }
        self.join
            .take()
            .map(|join| join.join().unwrap_or_else(|_| self.failed.clone()))
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl Drop for PreparationWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn run(request: PreparationRequest, cancelled: Arc<AtomicBool>) -> CoverageReport {
    let control = history_download::PreparationControl {
        cancelled,
        deadline_ms: request.deadline_ms,
    };
    let result = (|| {
        request.config.validate()?;
        data::verify_journal(&request.config)?;
        history_download::prepare_daily(
            &request.config,
            &request.instruments,
            &request.required_sessions,
            &control,
        )
    })();
    let report = if control.check().is_err() {
        let mut report = base_report(
            request.target_session,
            request.previous_session,
            request.instruments.len(),
            request.instruments.len() * request.required_sessions.len(),
        );
        report.failure = Some("history_preparation_cancelled_or_expired".into());
        report
    } else {
        coverage_report(
            &request.config,
            &request.instruments,
            &request.required_sessions,
            request.target_session,
            request.previous_session,
        )
    };
    apply_run_outcome(report, result)
}

/// Cancelled and deadline-stopped passes are FAILED with the original code so
/// remaining attempt budget can retry before cutoff. Admission failures stay
/// BLOCKED at the controller. String errors keep their snake_case codes.
fn preparation_failure_code(error: &(dyn std::error::Error + 'static)) -> String {
    if error.downcast_ref::<super::Fail>().is_some() {
        super::failure_code(error)
    } else if error.downcast_ref::<std::io::Error>().is_some() {
        "history_preparation_io".into()
    } else {
        let code = super::failure_code(error);
        if code == "paper_runtime_unavailable" {
            "history_preparation_failed".into()
        } else {
            code
        }
    }
}

pub(crate) fn apply_run_outcome(
    mut report: CoverageReport,
    result: Result<usize>,
) -> CoverageReport {
    if report.failure.as_deref() == Some("history_preparation_cancelled_or_expired") {
        report.status = CoverageStatus::Failed;
    }
    match result {
        Ok(skipped) if skipped > 0 && report.failure.is_none() => {
            report.failure = Some(format!("history_fetch_failures_skipped:{skipped}"));
        }
        Ok(_) => {}
        Err(error) => {
            report.status = CoverageStatus::Failed;
            report.failure = Some(preparation_failure_code(error.as_ref()));
            report.retry_at_ms = error
                .downcast_ref::<data::RateLimited>()
                .and_then(|limit| limit.retry_at_ms);
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> CoverageReport {
        base_report(
            NaiveDate::from_ymd_opt(2026, 10, 29).unwrap(),
            NaiveDate::from_ymd_opt(2026, 10, 28).unwrap(),
            1,
            21,
        )
    }

    #[test]
    fn cancelled_or_deadline_preparation_is_failed_with_specific_code() {
        for code in [
            "history_preparation_cancelled",
            "history_preparation_deadline",
        ] {
            let finished = apply_run_outcome(report(), Err(code.into()));
            assert_eq!(finished.status, CoverageStatus::Failed);
            assert_ne!(finished.status, CoverageStatus::Blocked);
            assert_eq!(finished.failure.as_deref(), Some(code));
            let stored = serde_json::to_value(&finished).unwrap();
            assert_eq!(stored["status"], "FAILED");
            assert_ne!(stored["status"], "BLOCKED");
            assert_eq!(stored["failure"], code);
        }
        let mut inventory = report();
        inventory.failure = Some("history_preparation_cancelled_or_expired".into());
        let finished = apply_run_outcome(inventory, Ok(0));
        assert_eq!(finished.status, CoverageStatus::Failed);
        assert_eq!(
            finished.failure.as_deref(),
            Some("history_preparation_cancelled_or_expired")
        );
        let mut inventory = report();
        inventory.failure = Some("history_preparation_cancelled_or_expired".into());
        let finished = apply_run_outcome(inventory, Err("history_preparation_deadline".into()));
        assert_eq!(finished.status, CoverageStatus::Failed);
        assert_eq!(
            finished.failure.as_deref(),
            Some("history_preparation_deadline")
        );
        let io = apply_run_outcome(report(), Err(std::io::Error::other("fixture").into()));
        assert_eq!(io.status, CoverageStatus::Failed);
        assert_eq!(io.failure.as_deref(), Some("history_preparation_io"));
        let generic = apply_run_outcome(report(), Err("not a code-shaped message".into()));
        assert_eq!(generic.status, CoverageStatus::Failed);
        assert_eq!(
            generic.failure.as_deref(),
            Some("history_preparation_failed")
        );
    }
}
