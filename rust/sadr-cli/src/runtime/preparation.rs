//! Bounded pre-open daily history acquisition and pure coverage assessment.
use super::{Config, Result, data, history_download};
use chrono::{NaiveDate, Timelike};
use sadr_core::execution::Instrument;
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
    /// Absolute Unix milliseconds; normally 09:10 IST on target_session.
    pub deadline_ms: i64,
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
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => {
            report.failure = Some("history_coverage_unreadable".into());
            return report;
        }
    };
    let mut receipt_count = 0usize;
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
        receipt_count += 1;
        if receipt_count > 50_000 {
            report.failure = Some("history_receipt_inventory_limit".into());
            return report;
        }
        if !matches!(entry.file_type(), Ok(t) if t.is_file()) {
            report.failure = Some("history_receipt_not_regular".into());
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
            report.missing_sessions.insert(
                instrument.key.clone(),
                required_sessions
                    .iter()
                    .filter(|day| !bars.contains_key(&(instrument.key.clone(), **day)))
                    .copied()
                    .collect(),
            );
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
}
impl PreparationWorker {
    pub fn spawn(request: PreparationRequest) -> Result<Self> {
        let now = super::millis();
        let local_now = super::local(now);
        let cutoff = request
            .target_session
            .and_hms_opt(
                request.config.history_preparation_cutoff_minute / 60,
                request.config.history_preparation_cutoff_minute % 60,
                0,
            )
            .ok_or("history_preparation_request")?
            .and_local_timezone(
                chrono::FixedOffset::east_opt(19_800).ok_or("history_preparation_request")?,
            )
            .single()
            .ok_or("history_preparation_request")?
            .timestamp_millis();
        if request.instruments.is_empty()
            || request.required_sessions.len() != request.config.daily_history_sessions
            || request.required_sessions.last() != Some(&request.previous_session)
            || request.required_sessions.windows(2).any(|w| w[0] >= w[1])
            || request.previous_session >= request.target_session
            || request.deadline_ms <= now
            || request.deadline_ms > cutoff
            || local_now.date_naive() != request.target_session
            || (local_now.hour() * 60 + local_now.minute())
                < request.config.history_preparation_start_minute
            || (local_now.hour() * 60 + local_now.minute())
                >= request.config.history_preparation_cutoff_minute
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
        let join = thread::Builder::new()
            .name("paper-history-preparation".into())
            .spawn(move || run(request, worker_cancel))?;
        Ok(Self {
            cancel,
            join: Some(join),
            failed,
        })
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

fn apply_run_outcome(mut report: CoverageReport, result: Result<usize>) -> CoverageReport {
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
