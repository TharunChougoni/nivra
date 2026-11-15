//! Pure validation of persisted paper-protection observations.
//!
//! This component supplies neither entry authority nor a broker certificate. The
//! original prepared plan, original files/sidecars, account/session identity,
//! cooperative owner lock and post-writer-lock clock remain runtime obligations.
//! Both archives must be supplied by the caller from their bound original stores;
//! a supplied `latest_entry` is not independently proved latest by this module.

use crate::{
    StoreError,
    archive::{LegacyUpstoxCapture, canonical_plain_json},
};
use sadr_core::{
    market::{
        AccountSource, Calendar, Instrument, MAX_FINANCIAL_AGE_MICROS, ReceiptBoundSnapshot,
        Snapshot,
    },
    time::Timestamp,
};
use serde::{Deserialize, Serialize, de};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const MAX_OBSERVATIONS: u64 = 100_000;
const MAX_RESULT_BYTES: usize = 2_000_000;

fn hash_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Strict typed projection of the persisted singleton profile row.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectionProfile {
    value: String,
    observations: u64,
    last_at: Option<Timestamp>,
}
impl ProtectionProfile {
    pub fn new(value: String, observations: u64, last_at: Option<Timestamp>) -> Self {
        Self {
            value,
            observations,
            last_at,
        }
    }
    pub fn value(&self) -> &str {
        &self.value
    }
    pub fn observations(&self) -> u64 {
        self.observations
    }
    pub fn last_at(&self) -> Option<Timestamp> {
        self.last_at
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtectionJobState {
    #[serde(rename = "CLAIMED")]
    Claimed,
    #[serde(rename = "CAPTURED")]
    Captured,
    #[serde(rename = "DONE")]
    Done,
}

/// Historical DONE rows need not contain a usable result: the reference gate
/// validates the latest result and the states/count of all earlier rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectionJob {
    seq: u64,
    state: ProtectionJobState,
    capture: Option<String>,
    result: Option<String>,
    result_hash: Option<String>,
}
impl ProtectionJob {
    pub fn new(
        seq: u64,
        state: ProtectionJobState,
        capture: Option<String>,
        result: Option<String>,
        result_hash: Option<String>,
    ) -> Self {
        Self {
            seq,
            state,
            capture,
            result,
            result_hash,
        }
    }
    pub fn seq(&self) -> u64 {
        self.seq
    }
    pub fn state(&self) -> ProtectionJobState {
        self.state
    }
    pub fn capture(&self) -> Option<&str> {
        self.capture.as_deref()
    }
    pub fn result(&self) -> Option<&str> {
        self.result.as_deref()
    }
    pub fn result_hash(&self) -> Option<&str> {
        self.result_hash.as_deref()
    }
}

/// Validated quote/calendar binding only; deliberately not a legacy full plan.
#[derive(Debug, Clone)]
pub struct ProtectionBinding {
    expected_profile_hash: String,
    max_observations: u64,
    identity: AccountSource,
    instruments: Vec<Instrument>,
    calendar: Calendar,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtectionBindingFields {
    profile_hash: String,
    max_observations: u64,
    identity: AccountSource,
    instruments: Vec<Instrument>,
    calendar: Calendar,
}
impl<'de> Deserialize<'de> for ProtectionBinding {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = Value::deserialize(deserializer)?;
        let identity = value
            .get_mut("identity")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| de::Error::custom(StoreError::Protection))?;
        let instrument_key = match identity.remove("instrument_key") {
            Some(Value::String(value)) => Some(value),
            Some(_) => return Err(de::Error::custom(StoreError::Protection)),
            None => None,
        };
        let instrument_version = match identity.remove("instrument_version") {
            Some(Value::String(value)) => Some(value),
            Some(_) => return Err(de::Error::custom(StoreError::Protection)),
            None => None,
        };
        if instrument_key.is_some() != instrument_version.is_some() {
            return Err(de::Error::custom(StoreError::Protection));
        }
        let fields = ProtectionBindingFields::deserialize(value).map_err(de::Error::custom)?;
        if let (Some(key), Some(version)) = (&instrument_key, &instrument_version)
            && (fields.instruments.len() != 1
                || fields.instruments[0].key() != key
                || fields.instruments[0].version() != version)
        {
            return Err(de::Error::custom(StoreError::Protection));
        }
        Self::new(
            fields.profile_hash,
            fields.max_observations,
            fields.identity,
            fields.instruments,
            fields.calendar,
        )
        .map_err(de::Error::custom)
    }
}
impl ProtectionBinding {
    pub fn new(
        expected_profile_hash: String,
        max_observations: u64,
        identity: AccountSource,
        instruments: Vec<Instrument>,
        calendar: Calendar,
    ) -> Result<Self, StoreError> {
        if !hash_valid(&expected_profile_hash)
            || !(1..=MAX_OBSERVATIONS).contains(&max_observations)
            || !(1..=500).contains(&instruments.len())
            || identity.broker() != "upstox"
            || identity.archive_source() != "upstox-readonly-v1"
            || identity.quote_source() != "upstox-full-quotes-v2"
        {
            return Err(StoreError::Protection);
        }
        let mut keys = BTreeSet::new();
        for instrument in &instruments {
            if instrument.synthetic()
                || instrument.validate_paper_execution().is_err()
                || !keys.insert(instrument.key())
            {
                return Err(StoreError::Protection);
            }
            calendar
                .require_instrument(instrument)
                .map_err(|_| StoreError::Protection)?;
        }
        Ok(Self {
            expected_profile_hash,
            max_observations,
            identity,
            instruments,
            calendar,
        })
    }
    pub fn expected_profile_hash(&self) -> &str {
        &self.expected_profile_hash
    }
    pub fn profile_hash(&self) -> &str {
        &self.expected_profile_hash
    }
    pub fn max_observations(&self) -> u64 {
        self.max_observations
    }
    pub fn identity(&self) -> &AccountSource {
        &self.identity
    }
    pub fn instruments(&self) -> &[Instrument] {
        &self.instruments
    }
    pub fn calendar(&self) -> &Calendar {
        &self.calendar
    }
}

/// Successful local consistency observation, never an entry authorization.
#[derive(Debug, Clone)]
pub struct ProtectionObservation {
    sequence: u64,
    observed_at: Timestamp,
    claimed_at: Timestamp,
    original_result: String,
    original_result_hash: String,
    protected: ReceiptBoundSnapshot,
    entry: ReceiptBoundSnapshot,
}
impl ProtectionObservation {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn observed_at(&self) -> Timestamp {
        self.observed_at
    }
    pub fn claimed_at(&self) -> Timestamp {
        self.claimed_at
    }
    pub fn original_result(&self) -> &str {
        &self.original_result
    }
    pub fn original_result_hash(&self) -> &str {
        &self.original_result_hash
    }
    pub fn protected(&self) -> &ReceiptBoundSnapshot {
        &self.protected
    }
    pub fn entry(&self) -> &ReceiptBoundSnapshot {
        &self.entry
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value, StoreError> {
    value.get(key).ok_or(StoreError::Protection)
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, StoreError> {
    field(value, key)?.as_str().ok_or(StoreError::Protection)
}

/// Call only with the complete persisted job projection and an actual local
/// clock sampled after the relevant writer lock. Financial admission requires
/// the producer snapshot; `None` is denial, not an unprotected path. This pure
/// function cannot authenticate the writer lock, original files, owner lease or
/// prepared-plan continuity, and it never provides a historical bypass.
#[allow(clippy::too_many_arguments)]
pub fn validate_observation(
    binding: &ProtectionBinding,
    profile: &ProtectionProfile,
    jobs: &[ProtectionJob],
    halted: bool,
    now: Timestamp,
    protected: &LegacyUpstoxCapture,
    latest_entry: &LegacyUpstoxCapture,
    supplied_entry: Option<&Snapshot>,
) -> Result<ProtectionObservation, StoreError> {
    let Some(supplied_entry) = supplied_entry else {
        return Err(StoreError::Protection);
    };
    if halted
        || profile.value != binding.expected_profile_hash
        || profile.observations == 0
        || profile.observations > binding.max_observations
        || jobs.len() as u64 != profile.observations
        || !binding
            .calendar
            .is_open(now)
            .map_err(|_| StoreError::Protection)?
    {
        return Err(StoreError::Protection);
    }
    let mut sequences = BTreeSet::new();
    if jobs
        .iter()
        .any(|job| job.state != ProtectionJobState::Done || !sequences.insert(job.seq))
    {
        return Err(StoreError::Protection);
    }
    let latest = jobs
        .iter()
        .max_by_key(|job| job.seq)
        .ok_or(StoreError::Protection)?;
    if latest.seq != profile.observations {
        return Err(StoreError::Protection);
    }
    let result = latest.result.as_deref().ok_or(StoreError::Protection)?;
    let result_hash = latest
        .result_hash
        .as_deref()
        .ok_or(StoreError::Protection)?;
    let capture_id = latest.capture.as_deref().ok_or(StoreError::Protection)?;
    if result.len() > MAX_RESULT_BYTES
        || !hash_valid(result_hash)
        || !hash_valid(capture_id)
        || digest(result) != result_hash
        || protected.original_hash() != capture_id
        || canonical_plain_json(result).map_err(|_| StoreError::Protection)? != result
    {
        return Err(StoreError::Protection);
    }
    let parsed: Value = serde_json::from_str(result).map_err(|_| StoreError::Protection)?;
    let top = parsed.as_object().ok_or(StoreError::Protection)?;
    if top.len() != 2 || string(&parsed, "capture")? != capture_id {
        return Err(StoreError::Protection);
    }
    let report = field(&parsed, "watchdog")?;
    if !report.is_object()
        || string(report, "label")? != "PAPER_WATCHDOG"
        || string(report, "status")? != "OK"
        || field(report, "reasons")?
            .as_array()
            .is_none_or(|v| !v.is_empty())
        || field(report, "alerts")?
            .as_array()
            .is_none_or(|v| !v.is_empty())
        || field(report, "entry_cutoff")? != &Value::Bool(false)
        || field(report, "broker_reconciled")? != &Value::Bool(false)
    {
        return Err(StoreError::Protection);
    }
    let observed =
        Timestamp::parse(string(report, "observed_at")?).map_err(|_| StoreError::Protection)?;
    let claimed = profile.last_at.ok_or(StoreError::Protection)?;
    if claimed > observed
        || !now
            .elapsed_micros_since(observed)
            .is_ok_and(|age| (0..=MAX_FINANCIAL_AGE_MICROS).contains(&age))
        || protected.requested_at() < claimed
        || protected.received_at() > observed
    {
        return Err(StoreError::Protection);
    }
    let protected_receipt = protected
        .derive_receipt(&binding.instruments, &binding.identity, &binding.calendar)
        .map_err(|_| StoreError::Protection)?;
    let protected_bound = protected_receipt
        .snapshot()
        .bind_receipt(&protected_receipt, &binding.identity)
        .map_err(|_| StoreError::Protection)?;
    protected_bound
        .check_fresh(now)
        .map_err(|_| StoreError::Protection)?;
    let entry_receipt = latest_entry
        .derive_receipt(&binding.instruments, &binding.identity, &binding.calendar)
        .map_err(|_| StoreError::Protection)?;
    let entry_bound = supplied_entry
        .bind_receipt(&entry_receipt, &binding.identity)
        .map_err(|_| StoreError::Protection)?;
    entry_bound
        .check_fresh(now)
        .map_err(|_| StoreError::Protection)?;
    Ok(ProtectionObservation {
        sequence: latest.seq,
        observed_at: observed,
        claimed_at: claimed,
        original_result: result.to_owned(),
        original_result_hash: result_hash.to_owned(),
        protected: protected_bound,
        entry: entry_bound,
    })
}

/// Historical dashboard evidence only. The caller supplies rows from one read
/// snapshot and samples `now` at end of read; no files, leases or sources are
/// inspected. Missing/bad evidence returns a redacted unavailable projection.
pub fn dashboard_projection(
    required_hash: Option<&str>,
    guard: Option<(&str, &str)>,
    rows: Option<(&ProtectionProfile, &[ProtectionJob])>,
    now: Timestamp,
) -> Value {
    let empty = serde_json::json!({"state":"UNAVAILABLE","required":null,
        "observed_at":null,"claimed_at":null,"age_seconds":null,
        "observation_recent":false,"watchdog_status":null,"reasons":[],
        "pending":null,"observations":null,"observation_limit":null,
        "exit_alert_count":null,"entry_cutoff":null,"process_health":"UNVERIFIED",
        "source_verified":false,"entry_authorized":false});
    let project = || -> Result<Value, StoreError> {
        let mut result = empty.clone();
        let Some((hash, config)) = guard else {
            if required_hash.is_none() && rows.is_none() {
                result["state"] = "UNCONFIGURED".into();
                result["required"] = false.into();
                return Ok(result);
            }
            return Err(StoreError::Protection);
        };
        if !hash_valid(hash)
            || required_hash != Some(hash)
            || config.len() > 1_000_000
            || digest(config) != hash
        {
            return Err(StoreError::Protection);
        }
        let config: Value = serde_json::from_str(config).map_err(|_| StoreError::Protection)?;
        let cap = config["max_observations"]
            .as_u64()
            .filter(|n| (1..=MAX_OBSERVATIONS).contains(n))
            .ok_or(StoreError::Protection)?;
        result["required"] = true.into();
        result["observation_limit"] = cap.into();
        result["state"] = "WAITING".into();
        result["observations"] = 0.into();
        result["pending"] = 0.into();
        let Some((profile, jobs)) = rows else {
            return Ok(result);
        };
        if profile.value != hash
            || jobs.len() as u64 != profile.observations
            || profile.observations > cap
            || jobs.is_empty() != profile.last_at.is_none()
        {
            return Err(StoreError::Protection);
        }
        let mut ordered: Vec<_> = jobs.iter().collect();
        ordered.sort_by_key(|job| job.seq);
        if ordered
            .iter()
            .enumerate()
            .any(|(i, job)| job.seq != i as u64 + 1)
        {
            return Err(StoreError::Protection);
        }
        let pending = jobs
            .iter()
            .filter(|job| job.state != ProtectionJobState::Done)
            .count();
        result["observations"] = profile.observations.into();
        result["pending"] = pending.into();
        result["claimed_at"] =
            serde_json::to_value(profile.last_at).map_err(|_| StoreError::Protection)?;
        let Some(latest) = ordered.last() else {
            return Ok(result);
        };
        if (latest.state == ProtectionJobState::Claimed
            && (latest.capture.is_some()
                || latest.result.is_some()
                || latest.result_hash.is_some()))
            || (latest.state == ProtectionJobState::Captured
                && (latest.capture.as_deref().is_none_or(|s| !hash_valid(s))
                    || latest.result.is_some()
                    || latest.result_hash.is_some()))
        {
            return Err(StoreError::Protection);
        }
        let Some(done) = ordered
            .iter()
            .rev()
            .find(|job| job.state == ProtectionJobState::Done)
        else {
            result["state"] = "INCOMPLETE".into();
            return Ok(result);
        };
        let raw = done.result.as_deref().ok_or(StoreError::Protection)?;
        let capture = done
            .capture
            .as_deref()
            .filter(|s| hash_valid(s))
            .ok_or(StoreError::Protection)?;
        if raw.len() > 65536
            || done.result_hash.as_deref() != Some(digest(raw).as_str())
            || canonical_plain_json(raw).map_err(|_| StoreError::Protection)? != raw
        {
            return Err(StoreError::Protection);
        }
        let payload: Value = serde_json::from_str(raw).map_err(|_| StoreError::Protection)?;
        if payload.as_object().is_none_or(|p| p.len() != 2) || payload["capture"] != capture {
            return Err(StoreError::Protection);
        }
        let report = &payload["watchdog"];
        let reasons = report["reasons"].as_array().ok_or(StoreError::Protection)?;
        let alerts = report["alerts"].as_array().ok_or(StoreError::Protection)?;
        let cutoff = report["entry_cutoff"]
            .as_bool()
            .ok_or(StoreError::Protection)?;
        const REASONS: &[&str] = &[
            "clock_reversal",
            "stale_feed",
            "unreconciled_marks",
            "daily_loss",
            "drawdown",
            "cash_reconciliation_mismatch",
            "calendar_missing",
            "intraday_cutoff",
            "session_closed",
        ];
        if report.as_object().is_none_or(|r| r.len() != 7)
            || report["label"] != "PAPER_WATCHDOG"
            || report["broker_reconciled"] != false
            || !matches!(report["status"].as_str(), Some("OK" | "HALTED"))
            || reasons.len() > REASONS.len()
            || reasons
                .iter()
                .any(|r| r.as_str().is_none_or(|r| !REASONS.contains(&r)))
            || (report["status"] == "OK" && (!reasons.is_empty() || cutoff))
        {
            return Err(StoreError::Protection);
        }
        let stamp = string(report, "observed_at")?;
        if stamp.len() > 64 {
            return Err(StoreError::Protection);
        }
        let observed = Timestamp::parse(stamp).map_err(|_| StoreError::Protection)?;
        let claimed = profile.last_at.ok_or(StoreError::Protection)?;
        if done.seq == latest.seq && claimed > observed {
            return Err(StoreError::Protection);
        }
        let age = now
            .elapsed_micros_since(observed)
            .map_err(|_| StoreError::Protection)?;
        result["state"] = if pending == 0 {
            "RECORDED"
        } else {
            "INCOMPLETE"
        }
        .into();
        result["observed_at"] = stamp.into();
        result["age_seconds"] = serde_json::json!(age as f64 / 1_000_000.0);
        result["observation_recent"] =
            (pending == 0 && (0..=MAX_FINANCIAL_AGE_MICROS).contains(&age) && claimed <= now)
                .into();
        result["watchdog_status"] = report["status"].clone();
        result["reasons"] = reasons.clone().into();
        result["exit_alert_count"] = alerts.len().into();
        result["entry_cutoff"] = cutoff.into();
        Ok(result)
    };
    project().unwrap_or(empty)
}
