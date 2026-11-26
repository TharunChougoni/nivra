//! Read-only candle acquisition. Candles are evidence, never executable quotes.
use super::*;
use chrono::{Days, Months};
use nivra_core::{execution::Instrument, finance::Amount};
use nivra_store::{history_pacing, market_archive as archive};
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Interval {
    Minutes,
    Days,
    Both,
}
impl Interval {
    fn units(self) -> &'static [archive::CandleUnit] {
        use archive::CandleUnit::{Daily, Minute};
        match self {
            Self::Minutes => &[Minute],
            Self::Days => &[Daily],
            Self::Both => &[Minute, Daily],
        }
    }
}
pub(super) fn unit_name(unit: archive::CandleUnit) -> &'static str {
    match unit {
        archive::CandleUnit::Daily => "days",
        archive::CandleUnit::Minute => "minutes",
    }
}
pub(crate) const MAX_RECEIPT_BYTES: u64 = data::HISTORY_RESPONSE_BYTES * 2 + 16_384;
pub(crate) trait Transport {
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
    pacing: Connection,
}
const READ_INTERVAL_MS: i64 = 1250;
pub(super) const RATE_LIMIT_RETRY_MS: i64 = 10 * 60_000;
impl<'a> Provider<'a> {
    fn new(config: &'a Config, control: Option<PreparationControl>) -> Result<Self> {
        let mut path = config.read_journal.as_os_str().to_owned();
        path.push(format!(
            ".history-{}-{}.sqlite",
            config.read_journal_device, config.read_journal_inode
        ));
        let pacing =
            history_pacing::open(Path::new(&path)).map_err(|_| "history_pacing_unavailable")?;
        history_pacing::init_history_pacing(&pacing).map_err(|_| "history_pacing_unavailable")?;
        Ok(Self {
            client: data::Client::new(&config.market_data_transport)?,
            config,
            control,
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
            let Some((next, cooldown)) = history_pacing::pace_history(
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
                        history_pacing::cool_down_history(
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
    pub(super) requested_ms: i64,
    pub(super) received_ms: i64,
    body: String,
    body_sha256: String,
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
pub(super) fn window_end(from: NaiveDate, end: NaiveDate, unit: &str) -> Result<NaiveDate> {
    Ok(from
        .checked_add_months(Months::new(if unit == "days" { 120 } else { 1 }))
        .and_then(|day| day.checked_sub_days(Days::new(1)))
        .ok_or("history_date")?
        .min(end))
}

mod download;
pub(super) use download::prepare_daily;
use download::{DownloadWindow, download};

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
                &request.config,
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
                    "covered_empty_bars": report.covered_empty_bars,
                    "excluded_bars": report.excluded_bars,
                    "retryable_missing_bars": report.retryable_missing_bars,
                    "failure": report.failure,
                })
            );
            return Ok(report.status == super::preparation::CoverageStatus::Ready);
        }
        data::verify_journal(&c)?;
        let db = super::market_archive::open(&c)?;
        let mut provider = Provider::new(&c, None)?;
        download(
            db,
            &mut provider,
            &instruments,
            DownloadWindow {
                start: request.required_sessions.first().copied(),
                end: Some(request.previous_session),
                interval: Interval::Days,
                sessions: Some(&request.required_sessions),
            },
            None,
        )?;
        println!("history_backfill_complete");
        Ok(true)
    })();
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(2),
        Err(error) => {
            eprintln!("history_backfill_failed: {}", failure_code(error.as_ref()));
            ExitCode::from(2)
        }
    }
}

pub fn entry(
    config: &Path,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    interval: Interval,
) -> ExitCode {
    let result = (|| {
        let c: Config = serde_json::from_slice(
            &crate::read_bounded(config, 32768).map_err(|_| "history_config")?,
        )?;
        let c = super::rollover::current(&c)?;
        c.validate()?;
        data::verify_journal(&c)?;
        let instruments = wide::history_instruments(&c)?;
        let db = super::market_archive::open(&c)?;
        let mut provider = Provider::new(&c, None)?;
        download(
            db,
            &mut provider,
            &instruments,
            DownloadWindow {
                start,
                end,
                interval,
                sessions: None,
            },
            None,
        )
    })();
    match result {
        Ok(_) => {
            println!("history_download_complete");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("history_download_failed: {}", failure_code(error.as_ref()));
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests;
