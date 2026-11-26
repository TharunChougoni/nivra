//! Interrupted history-attempt recovery and the preparation-window predicate.
use super::*;

pub(crate) const HISTORY_ATTEMPT_INTERRUPTED: &str = "history_attempt_interrupted_no_replay";
pub(crate) const HISTORY_ATTEMPT_RECOVERED: &str =
    "history_attempt_interrupted_recovered_no_replay";
const HISTORY_RECOVERY_KIND: &str = "operator_reviewed_no_replay";
pub(crate) const HISTORY_RECOVERY_RECEIPT_KIND: &str = "history_attempt_interrupted_recovery";
const MAX_HISTORY_ATTEMPT_STATE_BYTES: u64 = 1_048_576;
const MAX_HISTORY_RECOVERY_REPORT_BYTES: usize = 8_192;
/// Completed partial and failed attempts may be retried within the preparation window.
/// Interrupted claims and admission failures remain blocked for review.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryAttempts {
    pub(crate) account: String,
    pub(crate) records: Vec<HistoryAttempt>,
    pub(crate) session: Option<NaiveDate>,
    pub(crate) count: usize,
    pub(crate) retry_at: Option<i64>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryAttempt {
    pub(crate) started_ms: i64,
    pub(crate) completed_ms: Option<i64>,
    pub(crate) report: serde_json::Value,
}
impl HistoryAttempts {
    pub(crate) fn path(c: &Config) -> std::path::PathBuf {
        c.directory.join("history-attempts.json")
    }
    pub(crate) fn load(c: &Config) -> Result<Self> {
        match fs::symlink_metadata(Self::path(c)) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Err(Fail::new("history_attempt_state_not_regular").into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(_) => return Err(Fail::new("history_attempt_state_unreadable").into()),
        }
        let bytes =
            fs::read(Self::path(c)).map_err(|_| Fail::new("history_attempt_state_unreadable"))?;
        let state: Self = serde_json::from_slice(&bytes)
            .map_err(|_| Fail::new("history_attempt_state_invalid"))?;
        if state.account != c.account
            || state.session != Some(c.session)
            || state.count != state.records.len()
            || state.count == 0
            || state
                .records
                .iter()
                .take(state.count - 1)
                .any(|r| r.completed_ms.is_none())
            || (state.retry_at.is_some()
                && state.records.last().is_none_or(|r| {
                    r.completed_ms.is_none()
                        || !matches!(r.report["status"].as_str(), Some("PARTIAL" | "FAILED"))
                }))
        {
            return Err(Fail::new("history_attempt_state_identity_or_sequence").into());
        }
        Ok(state)
    }
    pub(crate) fn save(&self, c: &Config) -> Result<()> {
        rollover::atomic(&Self::path(c), &serde_json::to_value(self)?)
    }
    pub(crate) fn last_report(&self) -> serde_json::Value {
        self.records
            .last()
            .map(|r| r.report.clone())
            .unwrap_or(serde_json::Value::Null)
    }

    pub(crate) fn due(&self, c: &Config, now: i64) -> bool {
        c.history_preparation_max_attempts > 0
            && (self.session != Some(c.session)
                || (self.count < c.history_preparation_max_attempts
                    && (self.count == 0 || self.retry_at.is_some_and(|at| now >= at))))
    }
    pub(crate) fn started(&mut self, c: &Config, now: i64) -> Result<()> {
        if self.session != Some(c.session) {
            self.session = Some(c.session);
            self.count = 0;
            self.records.clear();
            self.account = c.account.clone();
        }
        if !self.due(c, now) {
            return Err(Fail::new("history_attempt_not_due").into());
        }
        self.count += 1;
        self.retry_at = None;
        self.records.push(HistoryAttempt {
            started_ms: now,
            completed_ms: None,
            report: serde_json::json!({"status":"IN_PROGRESS"}),
        });
        self.save(c)
    }
    pub(crate) fn completed(
        &mut self,
        c: &Config,
        now: i64,
        report: &serde_json::Value,
    ) -> Result<()> {
        let record = self
            .records
            .last_mut()
            .ok_or("history_attempt_not_started")?;
        if record.completed_ms.is_some() {
            return Err(Fail::new("history_attempt_already_completed").into());
        }
        record.completed_ms = Some(now);
        record.report = report.clone();
        // A PARTIAL pass whose missing bars are all covered by valid empty
        // receipts (and recorded zero fetch failures) has no fillable gaps left.
        let retry_partial =
            report["failure"].is_string() || report["retryable_missing_bars"].as_u64() != Some(0);
        let delay = match report["status"].as_str() {
            Some("PARTIAL") if retry_partial => Some(c.history_preparation_retry_ms.max(0)),
            Some("FAILED") => Some(c.history_preparation_failed_retry_ms.max(1)),
            _ => None,
        };
        self.retry_at = delay.map(|delay| now.saturating_add(delay));
        self.save(c)
    }
    /// Operator recovery needs distinct bounded refusals for a missing,
    /// malformed, mismatched or non-regular state file. This is read-only.
    pub(crate) fn load_recovery(c: &Config) -> Result<Self> {
        let path = Self::path(c);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Err(Fail::new("history_recover_state_not_regular").into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Fail::new("history_recover_state_missing").into());
            }
            Err(_) => return Err(Fail::new("history_recover_state_unreadable").into()),
        }
        let bytes = crate::read_bounded(&path, MAX_HISTORY_ATTEMPT_STATE_BYTES)
            .map_err(|_| Fail::new("history_recover_state_unreadable"))?;
        let state: Self = serde_json::from_slice(&bytes)
            .map_err(|_| Fail::new("history_recover_state_invalid"))?;
        if state.account != c.account {
            return Err(Fail::new("history_recover_account_mismatch").into());
        }
        if state.session != Some(c.session) {
            return Err(Fail::new("history_recover_session_mismatch").into());
        }
        if state.count == 0 || state.count != state.records.len() {
            return Err(Fail::new("history_recover_state_invalid").into());
        }
        if state
            .records
            .iter()
            .take(state.count - 1)
            .any(|record| record.completed_ms.is_none())
        {
            return Err(Fail::new("history_recover_state_sequence_invalid").into());
        }
        if state.retry_at.is_some()
            && state.records.last().is_none_or(|record| {
                record.completed_ms.is_none()
                    || !matches!(record.report["status"].as_str(), Some("PARTIAL" | "FAILED"))
            })
        {
            return Err(Fail::new("history_recover_state_invalid").into());
        }
        Ok(state)
    }
}

struct HistoryRecoveryMarker {
    completed_ms: i64,
    retry_at: i64,
    recovered_at_ms: i64,
    original_report: serde_json::Value,
}

/// Crash-safe claims are `IN_PROGRESS` with `completed_ms:null`. Legacy files
/// pre-wrote `BLOCKED`/`history_attempt_interrupted_no_replay` for the same
/// in-flight state; both are operator-recoverable and never auto-replayed.
fn interrupted_incomplete(record: &HistoryAttempt) -> bool {
    record.completed_ms.is_none()
        && (record.report["status"].as_str() == Some("IN_PROGRESS")
            || record.report["reason"].as_str() == Some(HISTORY_ATTEMPT_INTERRUPTED))
}

fn history_recovery_marker(record: &HistoryAttempt) -> Option<HistoryRecoveryMarker> {
    let completed_ms = record.completed_ms?;
    let marker = &record.report["recovery"];
    if record.report["status"].as_str() != Some("FAILED")
        || record.report["reason"].as_str() != Some(HISTORY_ATTEMPT_RECOVERED)
        || marker["kind"].as_str() != Some(HISTORY_RECOVERY_KIND)
        || marker["confirmed_paper_review"].as_bool() != Some(true)
        || marker["auto_replay"].as_bool() != Some(false)
    {
        return None;
    }
    Some(HistoryRecoveryMarker {
        completed_ms,
        retry_at: marker["retry_at"].as_i64()?,
        recovered_at_ms: marker["recovered_at_ms"].as_i64()?,
        original_report: record.report.get("original_report")?.clone(),
    })
}

pub(crate) fn history_recovery_receipt_path(
    c: &Config,
    sequence: usize,
    started_ms: i64,
) -> std::path::PathBuf {
    c.directory
        .join(format!("history-recovery-{sequence}-{started_ms}.json"))
}

fn history_recovery_report(
    c: &Config,
    sequence: usize,
    previous: &serde_json::Value,
    now: i64,
    retry_at: i64,
) -> Result<serde_json::Value> {
    if serde_json::to_vec(previous)?.len() > MAX_HISTORY_RECOVERY_REPORT_BYTES {
        return Err(Fail::new("history_recover_state_invalid").into());
    }
    Ok(serde_json::json!({
        "status":"FAILED",
        "reason":HISTORY_ATTEMPT_RECOVERED,
        "failure":HISTORY_ATTEMPT_INTERRUPTED,
        "recovery":{
            "kind":HISTORY_RECOVERY_KIND,
            "confirmed_paper_review":true,
            "recovered_at_ms":now,
            "retry_at":retry_at,
            "attempt":sequence,
            "max_attempts":c.history_preparation_max_attempts,
            "auto_replay":false
        },
        "original_report":previous
    }))
}

fn history_recovery_receipt(
    c: &Config,
    sequence: usize,
    started_ms: i64,
    completed_ms: i64,
    retry_at: i64,
    recovered_at_ms: i64,
    previous: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "kind":HISTORY_RECOVERY_RECEIPT_KIND,
        "version":1,
        "paper_only":true,
        "live_enabled":false,
        "account":c.account,
        "session":c.session.to_string(),
        "attempt":{
            "sequence":sequence,
            "max_attempts":c.history_preparation_max_attempts,
            "started_ms":started_ms,
            "previous_completed_ms":serde_json::Value::Null,
            "previous_report":previous,
            "new_completed_ms":completed_ms,
            "new_status":"FAILED",
            "new_reason":HISTORY_ATTEMPT_RECOVERED,
            "retry_at":retry_at,
            "replay":"none"
        },
        "operator":{
            "confirmed_paper_review":true,
            "confirmation":"operator_reviewed_interrupted_history_no_replay"
        },
        "recorded_at_ms":recovered_at_ms,
        "unchanged":{
            "paper_sqlite":true,
            "read_journal":true,
            "quota_receipts":true,
            "hash_sealed_state":true
        }
    })
}

/// Write the receipt once. A byte-identical existing receipt is retained; a
/// different file at the same deterministic path is a conflict, never replaced.
fn write_history_recovery_receipt(path: &Path, receipt: &serde_json::Value) -> Result<bool> {
    let expected = serde_json::to_vec_pretty(receipt)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            if metadata.mode() & 0o777 != 0o600 || metadata.nlink() != 1 {
                return Err(Fail::new("history_recover_audit_receipt_unreadable").into());
            }
            let existing = crate::read_bounded(path, MAX_HISTORY_ATTEMPT_STATE_BYTES)
                .map_err(|_| Fail::new("history_recover_audit_receipt_unreadable"))?;
            if existing != expected {
                return Err(Fail::new("history_recover_audit_receipt_conflict").into());
            }
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            rollover::atomic(path, receipt)
                .map_err(|_| Fail::new("history_recover_audit_receipt_write_failed"))?;
            Ok(true)
        }
        Ok(_) => Err(Fail::new("history_recover_audit_receipt_conflict").into()),
        Err(_) => Err(Fail::new("history_recover_audit_receipt_unreadable").into()),
    }
}

struct HistoryRecoverySummary<'a> {
    status: &'a str,
    sequence: usize,
    started_ms: i64,
    completed_ms: i64,
    retry_at: i64,
    receipt: &'a Path,
    receipt_written: bool,
}

fn history_recovery_summary(c: &Config, summary: &HistoryRecoverySummary<'_>) -> serde_json::Value {
    serde_json::json!({
        "status":summary.status,
        "paper_only":true,
        "live_enabled":false,
        "account":c.account,
        "session":c.session.to_string(),
        "attempt":summary.sequence,
        "max_attempts":c.history_preparation_max_attempts,
        "started_ms":summary.started_ms,
        "completed_ms":summary.completed_ms,
        "retry_at":summary.retry_at,
        "status_reason":HISTORY_ATTEMPT_RECOVERED,
        "operator_confirmed_paper_review":true,
        "audit_receipt":summary.receipt.file_name().and_then(|name| name.to_str()),
        "audit_receipt_written":summary.receipt_written,
        "paper_sqlite_touched":false,
        "read_journal_touched":false,
        "quota_receipts_touched":false
    })
}

/// Settle the latest interrupted current-session history attempt as a reviewed
/// failed/no-replay record, then let normal admission start a fresh bounded pass.
/// This never replays the ambiguous request or touches book/quota state.
pub(crate) fn history_recover(c: &Config, now: i64, confirmed: bool) -> Result<serde_json::Value> {
    if !confirmed {
        return Err(Fail::new("history_recover_confirm_paper_review_required").into());
    }
    c.validate()?;
    if LIVE_ENABLED {
        return Err(Fail::new("history_recover_live_enabled").into());
    }
    let _root_lock =
        rollover::controller_lock(c).map_err(|_| Fail::new("history_recover_controller_busy"))?;
    let active = rollover::current(c)
        .map_err(|_| Fail::new("history_recover_current_session_unavailable"))?;
    let c = &active;
    c.validate()?;
    if LIVE_ENABLED {
        return Err(Fail::new("history_recover_live_enabled").into());
    }
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(c.directory.join("controller.lock"))
        .map_err(|_| Fail::new("history_recover_controller_busy"))?;
    lock.try_lock_exclusive()
        .map_err(|_| Fail::new("history_recover_controller_busy"))?;
    let _lock = rollover::ControllerLock(lock);
    let mut state = HistoryAttempts::load_recovery(c)?;
    let sequence = state.count;
    let record = state
        .records
        .last()
        .ok_or_else(|| Fail::new("history_recover_state_invalid"))?;
    if record.completed_ms.is_some() {
        if let Some(marker) = history_recovery_marker(record) {
            let receipt_path = history_recovery_receipt_path(c, sequence, record.started_ms);
            let receipt = history_recovery_receipt(
                c,
                sequence,
                record.started_ms,
                marker.completed_ms,
                marker.retry_at,
                marker.recovered_at_ms,
                &marker.original_report,
            );
            let written = write_history_recovery_receipt(&receipt_path, &receipt)?;
            return Ok(history_recovery_summary(
                c,
                &HistoryRecoverySummary {
                    status: "already_recovered",
                    sequence,
                    started_ms: record.started_ms,
                    completed_ms: marker.completed_ms,
                    retry_at: marker.retry_at,
                    receipt: &receipt_path,
                    receipt_written: written,
                },
            ));
        }
        return Err(Fail::new("history_recover_attempt_completed").into());
    }
    if !interrupted_incomplete(record) {
        return Err(Fail::new("history_recover_attempt_not_interrupted").into());
    }
    if c.history_preparation_max_attempts == 0 || sequence >= c.history_preparation_max_attempts {
        return Err(Fail::new("history_recover_attempt_budget_exhausted").into());
    }
    if serde_json::to_vec(&record.report)?.len() > MAX_HISTORY_RECOVERY_REPORT_BYTES {
        return Err(Fail::new("history_recover_state_invalid").into());
    }
    let previous = record.report.clone();
    let started_ms = record.started_ms;
    let retry_at = now.saturating_add(c.history_preparation_failed_retry_ms.max(1));
    let receipt_path = history_recovery_receipt_path(c, sequence, started_ms);
    match fs::symlink_metadata(&receipt_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => return Err(Fail::new("history_recover_audit_receipt_conflict").into()),
        Err(_) => return Err(Fail::new("history_recover_audit_receipt_unreadable").into()),
    }
    let report = history_recovery_report(c, sequence, &previous, now, retry_at)?;
    let record = state
        .records
        .last_mut()
        .ok_or_else(|| Fail::new("history_recover_state_invalid"))?;
    record.completed_ms = Some(now);
    record.report = report;
    state.retry_at = Some(retry_at);
    state.save(c)?;
    let receipt = history_recovery_receipt(c, sequence, started_ms, now, retry_at, now, &previous);
    let written = write_history_recovery_receipt(&receipt_path, &receipt)?;
    Ok(history_recovery_summary(
        c,
        &HistoryRecoverySummary {
            status: "recovered",
            sequence,
            started_ms,
            completed_ms: now,
            retry_at,
            receipt: &receipt_path,
            receipt_written: written,
        },
    ))
}

pub(crate) fn history_attempt_window(c: &Config, day: NaiveDate, minute: u32) -> bool {
    day == c.session
        && (c.history_preparation_start_minute..c.history_preparation_cutoff_minute)
            .contains(&minute)
}

/// A session adopted after the pre-open cutoff prepares history immediately and
/// keeps the ordinary bounded retry rule until the late-start cutoff: a recorded
/// failure retries after its delay, while an attempt left in progress by a crash
/// is never replayed.
pub(crate) fn late_preparation_due(
    c: &Config,
    day: NaiveDate,
    minute: u32,
    now: i64,
    attempts: &HistoryAttempts,
) -> bool {
    day == c.session
        && attempts.due(c, now)
        && preparation::PreparationWindow::LateStart.admits(c, minute)
}
