//! Bounded pre-open daily history acquisition and pure coverage assessment.
use super::{Config, Result, data, history_download};
use chrono::{NaiveDate, Timelike};
use nivra_core::execution::Instrument;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
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
    pub history_root: PathBuf,
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
    /// Derived only after the complete receipt inventory passes validation.
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
}

fn base_report(
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
    }
}

/// Inspect only immutable daily receipts. A provider's valid empty response is
/// valid evidence but contributes no dated bars. Any malformed or conflicting
/// receipt makes the result UNKNOWN and therefore ineligible for planning.
pub fn coverage_report(
    root: &Path,
    instruments: &[Instrument],
    required_sessions: &[NaiveDate],
    target_session: NaiveDate,
    previous_session: NaiveDate,
) -> CoverageReport {
    coverage_report_control(
        root,
        instruments,
        required_sessions,
        target_session,
        previous_session,
        None,
    )
}

/// Hard bound on receipt-directory entries examined per coverage pass. The
/// directory accumulates receipts over a configured range, so the bound grows
/// with the corpus instead of failing at a fixed size.
#[cfg(not(test))]
const MAX_RECEIPT_ENTRIES: usize = 1_000_000;
#[cfg(test)]
const MAX_RECEIPT_ENTRIES: usize = 28;
/// Hard bound on decoded receipts per coverage pass. Write-once receipts that
/// predate the required window cannot cover it and are skipped by mtime first.
#[cfg(not(test))]
const MAX_DECODED_RECEIPTS: usize = 250_000;
#[cfg(test)]
const MAX_DECODED_RECEIPTS: usize = 24;
/// Clock/timezone margin applied to the required-window start. A receipt's
/// provider rows can never postdate its own immutable write.
const RECEIPT_RECENCY_MARGIN_MS: i64 = 2 * 86_400_000;
/// IST midnight of a reviewed session, in Unix milliseconds.
fn session_start_millis(day: NaiveDate) -> Option<i64> {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1)?;
    day.signed_duration_since(epoch)
        .num_days()
        .checked_mul(86_400_000)?
        .checked_sub(19_800_000)
}
fn coverage_report_control(
    root: &Path,
    instruments: &[Instrument],
    required_sessions: &[NaiveDate],
    target_session: NaiveDate,
    previous_session: NaiveDate,
    control: Option<&history_download::PreparationControl>,
) -> CoverageReport {
    let required_bars = instruments.len().saturating_mul(required_sessions.len());
    let mut report = base_report(
        target_session,
        previous_session,
        instruments.len(),
        required_bars,
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
    let dates: BTreeSet<_> = required_sessions.iter().copied().collect();
    let mut bars = BTreeMap::<(String, NaiveDate), serde_json::Value>::new();
    let mut covered = BTreeMap::<String, Vec<(NaiveDate, NaiveDate)>>::new();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => {
            report.failure = Some("history_coverage_unreadable".into());
            return report;
        }
    };
    let mut receipt_count = 0usize;
    let mut examined = 0usize;
    // Receipts are write-once artifacts: a row for a required session can only
    // exist in a receipt written at or after that session, so older files cannot
    // contribute coverage. Two days absorb clock and timezone differences.
    let cutoff = required_sessions
        .first()
        .and_then(|day| session_start_millis(*day))
        .map(|start| {
            std::time::UNIX_EPOCH
                + std::time::Duration::from_millis(
                    start.saturating_sub(RECEIPT_RECENCY_MARGIN_MS).max(0) as u64,
                )
        });
    for entry in entries {
        if control.is_some_and(|control| control.check().is_err()) {
            report.failure = Some("history_preparation_cancelled_or_expired".into());
            return report;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                report.failure = Some("history_coverage_unreadable".into());
                return report;
            }
        };
        let name = entry.file_name();
        if !name.to_string_lossy().ends_with(".receipt.json") {
            continue;
        }
        examined += 1;
        if examined > MAX_RECEIPT_ENTRIES {
            report.failure = Some("history_receipt_inventory_limit".into());
            return report;
        }
        if !matches!(entry.file_type(), Ok(t) if t.is_file()) {
            report.failure = Some("history_receipt_not_regular".into());
            return report;
        }
        let recent = cutoff.is_none_or(|cutoff| {
            entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .is_none_or(|modified| modified >= cutoff)
        });
        if !recent {
            continue;
        }
        receipt_count += 1;
        if receipt_count > MAX_DECODED_RECEIPTS {
            report.failure = Some("history_receipt_decode_limit".into());
            return report;
        }
        let receipt: history_download::Receipt = match history_download::read_receipt(&entry.path())
        {
            Ok(receipt) => receipt,
            Err(_) => {
                report.failure = Some("history_receipt_decode".into());
                return report;
            }
        };
        if receipt.unit != "days" || !keys.contains(receipt.key.as_str()) {
            continue;
        }
        if receipt.start > receipt.end
            || name.to_string_lossy()
                != format!(
                    "{}.receipt.json",
                    history_download::range_id(
                        &receipt.key,
                        &receipt.unit,
                        receipt.start,
                        receipt.end
                    )
                )
        {
            report.failure = Some("history_receipt_binding".into());
            return report;
        }
        let rows = match history_download::candles(&receipt) {
            Ok(rows) => rows,
            Err(_) => {
                report.failure = Some("history_receipt_invalid".into());
                return report;
            }
        };
        covered
            .entry(receipt.key.clone())
            .or_default()
            .push((receipt.start, receipt.end));
        for (day, row) in rows {
            if !dates.contains(&day) {
                continue;
            }
            let key = (receipt.key.clone(), day);
            if let Some(old) = bars.insert(key, row.clone())
                && old != row
            {
                report.failure = Some("history_receipt_conflict".into());
                return report;
            }
        }
    }
    report.retryable_missing_bars = 0;
    for instrument in instruments {
        let count = required_sessions
            .iter()
            .filter(|day| bars.contains_key(&(instrument.key.clone(), **day)))
            .count();
        if count > 0 {
            let total: f64 = required_sessions
                .iter()
                .filter_map(|day| bars.get(&(instrument.key.clone(), *day)))
                .filter_map(|row| row[5].as_u64())
                .map(|volume| volume as f64)
                .sum();
            report
                .daily_volumes
                .insert(instrument.key.clone(), (total / count as f64, count as u32));
        }
        report.observed_bars += count;
        if count < required_sessions.len() {
            let ranges = covered.get(&instrument.key);
            // A receipt proves a day holds no bar only when the provider answered
            // past it: an older empty day sits behind a returned bar, while the
            // days after the last returned bar may simply not be published yet.
            // An instrument the provider answered no bar for in the whole window
            // is listing or suspension evidence, so the range stands as empty.
            let last_observed = required_sessions
                .iter()
                .rev()
                .find(|day| bars.contains_key(&(instrument.key.clone(), **day)))
                .copied();
            let mut missing = Vec::new();
            for day in required_sessions {
                if !bars.contains_key(&(instrument.key.clone(), *day)) {
                    let untouched_tail = last_observed.is_some_and(|last| *day >= last);
                    if ranges
                        .is_some_and(|ranges| ranges.iter().any(|(s, e)| *s <= *day && *day <= *e))
                        && !untouched_tail
                    {
                        report.covered_empty_bars += 1;
                    } else {
                        report.retryable_missing_bars += 1;
                    }
                    missing.push(*day);
                }
            }
            report
                .missing_sessions
                .insert(instrument.key.clone(), missing);
        }
        if count == required_sessions.len()
            && bars.contains_key(&(instrument.key.clone(), previous_session))
        {
            report.ready_instruments += 1;
        }
    }
    report.missing_bars = required_bars.saturating_sub(report.observed_bars);
    report.status = if report.ready_instruments == instruments.len() {
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
            &request.history_root,
            request.required_sessions[0],
            request.previous_session,
            control.clone(),
        )
    })();
    let report = coverage_report_control(
        &request.history_root,
        &request.instruments,
        &request.required_sessions,
        request.target_session,
        request.previous_session,
        Some(&control),
    );
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
