//! Read-only candle acquisition. Candles are evidence, never executable quotes.
use super::*;
use chrono::{Days, Months};
use nivra_cli::lock::AdvisoryLock;
use nivra_core::{execution::Instrument, finance::Amount};
use nivra_store::market_data;
use serde_json::Value;
// Workspace feature unification can enable serde_json/preserve_order. History
// bytes must retain the sorted object layout used by existing archives.
macro_rules! json {
    ($($tokens:tt)*) => {{
        let mut value = serde_json::json!($($tokens)*);
        value.sort_all_objects();
        value
    }};
}
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Interval {
    Minutes,
    Days,
    Both,
}
impl Interval {
    fn units(self) -> &'static [&'static str] {
        match self {
            Self::Minutes => &["minutes"],
            Self::Days => &["days"],
            Self::Both => &["minutes", "days"],
        }
    }
}
const LIMITATION: &str = "Candle evidence only: no executable quotes, liquidity, historical calendar or point-in-time universe acceptance; empty-observation replay is not strategy performance.";
pub(crate) const MAX_RECEIPT_BYTES: u64 = data::HISTORY_RESPONSE_BYTES * 2 + 16_384;
pub(crate) trait Transport {
    fn receipts_only(&self) -> bool {
        false
    }
    fn skip_unavailable(&self) -> bool {
        false
    }
    /// Per-instrument fetch failures tolerated before the whole run aborts.
    fn max_fetch_failures(&self) -> usize {
        0
    }
    fn fetch(
        &mut self,
        key: &str,
        unit: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<(i64, i64, String)>;
}
/// Only transient provider reads may be skipped. Identity, credentials, quota,
/// receipt validation and journal failures must stop the pass immediately (#45).
fn transient_fetch_failure(code: &str) -> bool {
    matches!(code, "http_timeout" | "read_only_http_failed")
        || code
            .strip_prefix("http_")
            .and_then(|s| s.parse::<u16>().ok())
            .is_some_and(|status| (500..600).contains(&status))
}
struct Provider<'a> {
    client: data::Client,
    config: &'a Config,
    control: Option<PreparationControl>,
    receipts_only: bool,
    pacing: Connection,
}
const READ_INTERVAL_MS: i64 = 1250;
pub(super) const RATE_LIMIT_RETRY_MS: i64 = 10 * 60_000;
impl<'a> Provider<'a> {
    fn new(
        config: &'a Config,
        control: Option<PreparationControl>,
        receipts_only: bool,
    ) -> Result<Self> {
        let mut path = config.read_journal.as_os_str().to_owned();
        path.push(format!(
            ".history-{}-{}.sqlite",
            config.read_journal_device, config.read_journal_inode
        ));
        let pacing =
            market_data::open(Path::new(&path)).map_err(|_| "history_pacing_unavailable")?;
        market_data::init_history_pacing(&pacing).map_err(|_| "history_pacing_unavailable")?;
        Ok(Self {
            client: data::Client::new(&config.market_data_transport)?,
            config,
            control,
            receipts_only,
            pacing,
        })
    }

    fn wait(&self, duration: Duration) -> Result<()> {
        if let Some(control) = &self.control {
            control.sleep(duration)
        } else {
            thread::sleep(duration);
            Ok(())
        }
    }

    fn pace(&mut self) -> Result<()> {
        loop {
            if let Some(control) = &self.control {
                control.check()?;
            }
            let now = millis();
            let Some((next, cooldown)) = market_data::pace_history(
                &mut self.pacing,
                &self.config.read_scope,
                now,
                READ_INTERVAL_MS,
            )
            .map_err(|_| "history_pacing_unavailable")?
            else {
                return Ok(());
            };
            if cooldown > now {
                return Err(data::RateLimited {
                    retry_at_ms: Some(cooldown),
                    provider_response: false,
                }
                .into());
            }
            self.wait(Duration::from_millis(
                next.saturating_sub(now).min(READ_INTERVAL_MS) as u64,
            ))?;
        }
    }
}
impl Transport for Provider<'_> {
    fn receipts_only(&self) -> bool {
        self.receipts_only
    }
    fn skip_unavailable(&self) -> bool {
        self.config.history_skip_unavailable
    }
    fn max_fetch_failures(&self) -> usize {
        // Only bounded pre-open preparation; manual downloads still fail closed.
        if self.control.is_some() {
            self.config.history_preparation_max_fetch_failures
        } else {
            0
        }
    }
    fn fetch(
        &mut self,
        key: &str,
        unit: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<(i64, i64, String)> {
        // A denied shared-journal reservation has not sent an HTTP request.
        // Wait without resetting caps; HTTP failures are never retried here.
        for _ in 0..=self.config.history_download.quota_wait_max_attempts {
            self.pace()?;
            match self.client.historical(self.config, key, unit, start, end) {
                Err(error) if failure_code(error.as_ref()).starts_with("read_quota_wait_per_") => {
                    self.wait(Duration::from_millis(
                        self.config.history_download.quota_wait_poll_ms,
                    ))?;
                }
                result => {
                    if let Err(error) = &result
                        && let Some(limit) = error.downcast_ref::<data::RateLimited>()
                    {
                        let minimum = millis().saturating_add(RATE_LIMIT_RETRY_MS);
                        market_data::cool_down_history(
                            &self.pacing,
                            &self.config.read_scope,
                            limit.retry_at_ms.unwrap_or(minimum).max(minimum),
                        )
                        .map_err(|_| "history_pacing_unavailable")?;
                    }
                    return result;
                }
            }
        }
        Err("history_shared_quota_wait_timeout".into())
    }
}

#[derive(Clone)]
pub(crate) struct PreparationControl {
    pub(crate) cancelled: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) deadline_ms: i64,
}
impl PreparationControl {
    pub(crate) fn check(&self) -> Result<()> {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err("history_preparation_cancelled".into());
        }
        if millis() >= self.deadline_ms {
            return Err("history_preparation_deadline".into());
        }
        Ok(())
    }
    fn sleep(&self, duration: Duration) -> Result<()> {
        let until = Instant::now() + duration;
        loop {
            self.check()?;
            let remaining = until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            thread::sleep(remaining.min(Duration::from_millis(100)));
        }
    }
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub(crate) key: String,
    pub(crate) unit: String,
    pub(crate) start: NaiveDate,
    pub(crate) end: NaiveDate,
    requested_ms: i64,
    received_ms: i64,
    body: String,
    body_sha256: String,
}
/// Read one immutable receipt under a bound derived from the provider response
/// ceiling and the maximum JSON escaping/metadata overhead.
pub(crate) fn read_receipt(path: &Path) -> std::result::Result<Receipt, &'static str> {
    let bytes =
        crate::read_bounded(path, MAX_RECEIPT_BYTES).map_err(|_| "history_receipt_unreadable")?;
    serde_json::from_slice(&bytes).map_err(|_| "history_receipt_decode")
}
// Archived empty-observation shards from v0.5.2–v0.5.6 retain their original
// engine provenance. This does not authorize replay under a different engine.
#[cfg(test)]
const LEGACY_ENGINE: &str = "2ff64f81981d7710a7978b6944de7a364bd81076fab12b5cfb3ffabd22a6e525";
// v0.5.58 source before configured scanner screens. Retain descriptive shards
// exactly; this never permits replay under the new execution fingerprint.
#[cfg(test)]
const PRE_SCANNER_ENGINE: &str = "7f254509db3b95e40aaa5972fca0ed37bb83c190f0088dac4be6b7cd8a8bbc2d";
fn is_engine_fingerprint(engine: &str) -> bool {
    engine.len() == 64
        && engine
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn reusable_manifest(prior: &[u8], rebuilt: &[u8]) -> bool {
    let (Ok(mut prior_value), Ok(mut rebuilt_value)) = (
        serde_json::from_slice::<Value>(prior),
        serde_json::from_slice::<Value>(rebuilt),
    ) else {
        return false;
    };
    // Exact canonical bytes only: no reformatted or hand-edited predecessor.
    if serde_json::to_vec(&prior_value).ok().as_deref() != Some(prior) {
        return false;
    }
    let engine = prior_value["bindings"]["engine"].clone();
    if !engine.as_str().is_some_and(is_engine_fingerprint) {
        return false;
    }
    rebuilt_value["bindings"]["engine"] = engine;
    for value in [&mut prior_value, &mut rebuilt_value] {
        match value["universe"].as_array_mut().map(Vec::as_mut_slice) {
            Some([instrument]) if instrument["symbol"].is_string() => {
                instrument["symbol"] = Value::Null
            }
            _ => return false,
        }
    }
    prior_value == rebuilt_value
}
fn immutable(path: &Path, bytes: &[u8]) -> Result<()> {
    let kind = match path.extension().and_then(|s| s.to_str()) {
        Some("jsonl") => "events",
        _ => path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.rsplit('.').nth(1))
            .unwrap_or("artifact"),
    };
    immutable_bytes(path, bytes).map_err(|error| {
        let reason = if error.to_string() == "history_artifact_changed" {
            "artifact_changed"
        } else {
            "artifact_io"
        };
        Box::new(Fail::new(format!("history_{kind}_{reason}"))) as Box<dyn std::error::Error>
    })
}
fn immutable_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        if !path.symlink_metadata()?.is_file() || fs::read(path)? != bytes {
            return Err("history_artifact_changed".into());
        }
        return Ok(());
    }
    let parent = path.parent().ok_or("history_parent")?;
    let mut attempt = 0u64;
    let (stage, mut f) = loop {
        let stage = parent.join(format!(".history-stage-{}-{attempt}", std::process::id()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&stage)
        {
            Ok(f) => break (stage, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt = attempt.checked_add(1).ok_or("history_stage_limit")?
            }
            Err(e) => return Err(e.into()),
        }
    };
    f.write_all(bytes)?;
    f.sync_all()?;
    // Link is atomic and refuses overwrite. Interrupted staging files remain as evidence.
    fs::hard_link(&stage, path)?;
    fs::File::open(parent)?.sync_all()?;
    fs::remove_file(stage)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub(crate) fn candles(r: &Receipt) -> Result<Vec<(NaiveDate, Value)>> {
    if r.requested_ms > r.received_ms || digest(r.body.as_bytes()) != r.body_sha256 {
        return Err("history_receipt_invalid".into());
    }
    let value: Value = serde_json::from_str(&r.body)?;
    if value["status"] != "success" {
        return Err("history_provider_status".into());
    }
    let rows = value["data"]["candles"]
        .as_array()
        .ok_or("history_candles_missing")?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for row in rows {
        let fields = row.as_array().ok_or("history_candle_schema")?;
        if fields.len() != 7 {
            return Err("history_candle_schema".into());
        }
        let timestamp =
            DateTime::parse_from_rfc3339(fields[0].as_str().ok_or("history_timestamp")?)?;
        let day = timestamp.date_naive();
        if timestamp.offset().local_minus_utc() != 19800
            || day < r.start
            || day > r.end
            || timestamp.timestamp_millis() > r.received_ms
            || timestamp.second() != 0
            || timestamp.nanosecond() != 0
            || !seen.insert(timestamp.timestamp_millis())
        {
            return Err("history_candle_time".into());
        }
        if r.unit == "days" && (timestamp.hour() != 0 || timestamp.minute() != 0) {
            return Err("history_daily_grid".into());
        }
        let mut prices = Vec::new();
        for field in &fields[1..5] {
            if !field.is_number() {
                return Err("history_price".into());
            }
            let amount = Amount::parse(&field.to_string())?;
            if amount <= Amount::ZERO {
                return Err("history_price".into());
            }
            prices.push(amount);
        }
        if prices[2] > prices[0]
            || prices[2] > prices[3]
            || prices[1] < prices[0]
            || prices[1] < prices[3]
            || prices[2] > prices[1]
            || fields[5].as_u64().is_none()
            || fields[6].as_u64().is_none()
        {
            return Err("history_candle_values".into());
        }
        out.push((day, row.clone()));
    }
    out.sort_by_key(|(_, row)| row[0].as_str().unwrap_or("").to_owned());
    Ok(out)
}
pub(crate) fn range_id(key: &str, unit: &str, start: NaiveDate, end: NaiveDate) -> String {
    digest(format!("{key}:{unit}:{start}:{end}").as_bytes())
}
fn window_end(from: NaiveDate, end: NaiveDate, unit: &str) -> Result<NaiveDate> {
    Ok(from
        .checked_add_months(Months::new(if unit == "days" { 120 } else { 1 }))
        .and_then(|day| day.checked_sub_days(Days::new(1)))
        .ok_or("history_date")?
        .min(end))
}

mod download;
pub(crate) use download::*;

/// Explicit manual acquisition is separate from the automatic pre-open window.
/// Use the same reviewed-session gap coalescing and original quota, without
/// relaxing the automatic worker's deadline or creating any financial state.
pub fn backfill_entry(
    path: &Path,
    check_only: bool,
    prepare_universe: bool,
    upcoming: bool,
) -> ExitCode {
    let result = (|| -> Result<bool> {
        let c: Config = serde_json::from_slice(
            &crate::read_bounded(path, 32768).map_err(|_| "history_config")?,
        )?;
        let c = super::rollover::current(&c)?;
        c.validate()?;
        let instruments = if prepare_universe {
            wide::prepare_history_instruments(&c)?
        } else {
            wide::history_instruments(&c)?
        };
        let mut target = c.clone();
        if upcoming {
            (target.session, target.previous_session) =
                c.next_sessions(local(millis()).date_naive())?;
        }
        let session_clock =
            chrono::DateTime::parse_from_rfc3339(&format!("{}T00:00:00+05:30", target.session))?
                .timestamp_millis();
        let request = wide::preparation_request(
            &target,
            &instruments,
            session_clock,
            super::preparation::PreparationWindow::PreOpen,
        )?;
        if check_only {
            let report = super::preparation::coverage_report(
                &request.history_root,
                &instruments,
                &request.required_sessions,
                request.target_session,
                request.previous_session,
            );
            println!(
                "{}",
                json!({
                    "status": report.status,
                    "target_session": report.target_session,
                    "previous_session": report.previous_session,
                    "instruments": report.instruments,
                    "ready_instruments": report.ready_instruments,
                    "required_bars": report.required_bars,
                    "observed_bars": report.observed_bars,
                    "missing_bars": report.missing_bars,
                    "failure": report.failure,
                })
            );
            return Ok(report.status == super::preparation::CoverageStatus::Ready);
        }
        data::verify_journal(&c)?;
        let start = *request
            .required_sessions
            .first()
            .ok_or("history_required_sessions_missing")?;
        let mut provider = Provider::new(&c, None, true)?;
        download_controlled(
            &mut provider,
            millis,
            &instruments,
            &request.history_root,
            Some(start),
            Some(request.previous_session),
            Interval::Days,
            None,
            Some(&c),
        )?;
        println!("history_backfill_complete; original receipts preserved");
        Ok(true)
    })();
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(error) => {
            eprintln!(
                "history_backfill_failed: {}; original receipts preserved",
                failure_code(error.as_ref())
            );
            ExitCode::from(2)
        }
    }
}

pub fn entry(
    config: &Path,
    output: &Path,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    interval: Interval,
    receipts_only: bool,
) -> ExitCode {
    let result = (|| {
        let c: Config = serde_json::from_slice(
            &crate::read_bounded(config, 32768).map_err(|_| "history_config")?,
        )?;
        let c = super::rollover::current(&c)?;
        c.validate()?;
        data::verify_journal(&c)?;
        let instruments = wide::history_instruments(&c)?;
        let mut provider = Provider::new(&c, None, receipts_only)?;
        download(
            &mut provider,
            millis,
            &instruments,
            output,
            start,
            end,
            interval,
        )
    })();
    match result {
        Ok(()) => {
            println!("history_download_complete; {LIMITATION}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "history_download_failed: {}; original receipts preserved",
                failure_code(error.as_ref())
            );
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests;
