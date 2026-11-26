use super::*;

use super::{Fail, failure_code};
use nivra_core::finance::{Amount, Price};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
const KEY: &str = "NSE_EQ|INF204KB14I2";
pub(super) const HISTORY_RESPONSE_BYTES: u64 = 20_000_000;
#[derive(Debug)]
pub(super) struct RateLimited {
    pub retry_at_ms: Option<i64>,
    pub provider_response: bool,
}
pub(super) const RATE_LIMITED: &str = "http_429_rate_limited";
pub(super) const RATE_LIMIT_COOLDOWN: &str = "history_rate_limit_cooldown";
impl fmt::Display for RateLimited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.provider_response {
            RATE_LIMITED
        } else {
            RATE_LIMIT_COOLDOWN
        })
    }
}
impl std::error::Error for RateLimited {}

fn retry_after(value: &str, now: i64) -> Option<i64> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        let seconds = value.parse::<i64>().unwrap_or(i64::MAX);
        return Some(now.saturating_add(seconds.saturating_mul(1000)));
    }
    DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| date.timestamp_millis().max(now))
}

pub struct Client(ureq::Agent);
impl Client {
    pub fn new(settings: &MarketDataTransportSettings) -> Result<Self> {
        Ok(Self(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_millis(settings.http_timeout_ms)))
                .max_redirects(0)
                .build()
                .into(),
        ))
    }
    pub(super) fn get(&self, url: &str, token: Option<&str>, max: u64) -> Result<String> {
        let mut req = self.0.get(url).header("Accept", "application/json").header(
            "User-Agent",
            concat!("Nivra-paper/", env!("CARGO_PKG_VERSION")),
        );
        if let Some(t) = token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let mut response = match req.config().http_status_as_error(false).build().call() {
            Ok(response) => response,
            Err(error) => return Err(Fail::new(http_failure_code(&error)).into()),
        };
        let status = response.status();
        if status.as_u16() == 429 {
            let now = millis();
            let retry_at_ms = response
                .headers()
                .get("Retry-After")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| retry_after(value, now));
            return Err(RateLimited {
                retry_at_ms,
                provider_response: true,
            }
            .into());
        }
        if status.is_client_error() || status.is_server_error() {
            return Err(
                Fail::new(http_failure_code(&ureq::Error::StatusCode(status.as_u16()))).into(),
            );
        }
        let mut bytes = vec![];
        response
            .body_mut()
            .as_reader()
            .take(max + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Fail::new("read_only_http_failed"))?;
        if bytes.len() as u64 > max {
            return Err(Fail::new("response_too_large").into());
        }
        String::from_utf8(bytes).map_err(|_| Fail::new("response_not_utf8").into())
    }
    pub(super) fn bytes(&self, url: &str, max: u64) -> Result<Vec<u8>> {
        let mut response = self
            .0
            .get(url)
            .header("User-Agent", "Mozilla/5.0 Nivra-paper")
            .call()
            .map_err(|_| "metadata_http")?;
        let mut out = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(max + 1)
            .read_to_end(&mut out)?;
        if out.len() as u64 > max {
            return Err("metadata_size".into());
        }
        Ok(out)
    }
    /// Read-only historical candles; quota and credential ownership match quotes.
    pub(super) fn historical(
        &self,
        c: &Config,
        key: &str,
        unit: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<(i64, i64, String)> {
        let url = historical_url(&c.quote_keys, key, unit, from, to)?;
        let token = application_token(c)?;
        reserve_history(c, millis())?;
        let requested = millis();
        let body = self.get(&url, Some(&token), HISTORY_RESPONSE_BYTES)?;
        // Sample once after the complete response; retain this original receipt.
        let received = millis();
        Ok((requested, received, body))
    }
    pub fn quote(&self, c: &Config) -> Result<(i64, i64, String)> {
        self.quotes(c, &[KEY])
    }
    pub fn quotes(&self, c: &Config, keys: &[&str]) -> Result<(i64, i64, String)> {
        if !valid_quote_keys(keys, &c.quote_keys) {
            return Err(Fail::new("quote_keys").into());
        }
        // Validate the token before consuming a shared-journal reservation.
        let token = application_token(c)?;
        reserve(c, millis())?;
        let requested = millis();
        let encoded = keys
            .iter()
            .map(|key| encode_key_path(key))
            .collect::<Vec<_>>()
            .join(",");
        let body = self.get(
            &format!("https://api.upstox.com/v2/market-quote/quotes?instrument_key={encoded}"),
            Some(&token),
            2_000_000,
        )?;
        // Canonical receipt, sampled once after complete response; never refreshed.
        let received = millis();
        Ok((requested, received, body))
    }
}
pub(super) fn application_token(c: &Config) -> Result<String> {
    match private(&c.token_file) {
        Ok(raw) => parse_analytics_token(&raw).map_err(|code| Fail::new(code).into()),
        Err(error) => {
            let code = if failure_code(error.as_ref()) == "private_file_required" {
                "private_file_required"
            } else {
                "token_file_unreadable"
            };
            Err(Fail::new(code).into())
        }
    }
}
/// Only the two interval-1 series used by history-download are admitted.
/// Availability, inclusive end date and maximum windows are documented at
/// https://upstox.com/developer/api-documentation/v3/get-historical-candle-data/.
/// A one-month minute window and decade daily window are provider ceilings;
/// the downloader may choose smaller windows.
pub(super) fn historical_url(
    keys: &QuoteKeySettings,
    key: &str,
    unit: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<String> {
    if !valid_quote_keys(&[key], keys) {
        return Err(Fail::new("history_instrument_key").into());
    }
    let (first, months) = match unit {
        "minutes" => (NaiveDate::from_ymd_opt(2022, 1, 1).unwrap(), 1),
        "days" => (NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(), 120),
        _ => return Err(Fail::new("history_unit").into()),
    };
    let limit = from.checked_add_months(chrono::Months::new(months));
    if from < first || to < from || limit.is_none_or(|limit| to > limit) {
        return Err(Fail::new("history_date_range").into());
    }
    Ok(format!(
        "https://api.upstox.com/v3/historical-candle/{}/{unit}/1/{to}/{from}",
        encode_key_path(key)
    ))
}
/// Percent-encode the whole instrument key for use as one URL path segment.
/// Upstox keys contain `|` and, for index keys, spaces (`Nifty 50`).
pub(super) fn encode_key_path(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
fn valid_quote_keys(keys: &[&str], settings: &QuoteKeySettings) -> bool {
    !keys.is_empty()
        && keys.len() <= settings.max_keys
        && keys.iter().all(|key| settings.admits(key))
}
/// Accept a raw token or `UPSTOX_ANALYTICS_TOKEN=<token>`. Optional `export ` prefix,
/// surrounding whitespace/newlines, comments and quotes. JWT claims are not parsed.
/// Errors are static codes and never include the token.
pub(super) fn parse_analytics_token(raw: &str) -> std::result::Result<String, &'static str> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let lines: Vec<&str> = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if lines.len() != 1 {
        return Err("credential_format");
    }
    let mut line = lines[0];
    if let Some(rest) = line.strip_prefix("export")
        && (rest.starts_with(' ') || rest.starts_with('\t'))
    {
        let rest = rest.trim_start();
        if rest.starts_with("UPSTOX_ANALYTICS_TOKEN") {
            line = rest;
        }
    }
    let value = if let Some(rest) = line.strip_prefix("UPSTOX_ANALYTICS_TOKEN") {
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('=').ok_or("credential_format")?;
        rest.trim()
    } else if looks_like_env_assignment(line) {
        return Err("credential_format");
    } else {
        line
    };
    let token = unquote(value)?;
    if token.is_empty()
        || token.len() > 16384
        || !token
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"\"'$`\\".contains(&b))
    {
        return Err("credential_format");
    }
    Ok(token.to_owned())
}
fn unquote(value: &str) -> std::result::Result<&str, &'static str> {
    let Some(first) = value.as_bytes().first().copied() else {
        return Ok(value);
    };
    if first == b'"' || first == b'\'' {
        if value.len() < 2 || value.as_bytes()[value.len() - 1] != first {
            return Err("credential_format");
        }
        return Ok(&value[1..value.len() - 1]);
    }
    Ok(value)
}
fn looks_like_env_assignment(line: &str) -> bool {
    let Some((key, _)) = line.split_once('=') else {
        return false;
    };
    let key = key.trim();
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
pub(super) fn http_failure_code(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(401) => "http_401_unauthorized".into(),
        ureq::Error::StatusCode(403) => "http_403_forbidden".into(),
        ureq::Error::StatusCode(429) => RATE_LIMITED.into(),
        ureq::Error::StatusCode(code) => format!("http_{code}"),
        ureq::Error::Timeout(_) => "http_timeout".into(),
        _ => "read_only_http_failed".into(),
    }
}
pub fn verify_journal(c: &Config) -> Result<()> {
    let m = fs::symlink_metadata(&c.read_journal)
        .map_err(|_| Fail::new("original_read_journal_changed"))?;
    if !m.is_file() || m.dev() != c.read_journal_device || m.ino() != c.read_journal_inode {
        return Err(Fail::new("original_read_journal_changed").into());
    }
    let db = Connection::open_with_flags(&c.read_journal, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| Fail::new("read_journal_unreadable"))?;
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM read_requests WHERE scope=?",
            [&c.read_scope],
            |r| r.get(0),
        )
        .map_err(|_| Fail::new("read_journal_unreadable"))?;
    nivra_store::journal::reopen_market_reads(
        &db,
        &c.read_scope,
        nivra_store::journal::LEGACY_READ_CAPS,
        nivra_store::journal::JournalSource::Original { high_water: count },
    )
    .map_err(|error| Fail::new(error.to_string()))?;
    Ok(())
}
pub(super) fn reserve(c: &Config, now: i64) -> Result<()> {
    reserve_with_history_headroom(c, now, [0; 3])
}

fn reserve_history(c: &Config, now: i64) -> Result<()> {
    reserve_with_history_headroom(
        c,
        now,
        [
            c.history_download.reserve_second_reads,
            c.history_download.reserve_minute_reads,
            c.history_download.reserve_half_hour_reads,
        ],
    )
}

/// Local read caps per window (seconds, requests) for this application's own
/// journal scope. https://upstox.com/developer/api-documentation/rate-limiting/
/// lists historical candles under Other Standard APIs at 50/s, 500/min and
/// 2000/30min, enforced per API and per user, and warns that exceeding them may
/// suspend access. Every read in the scope counts here, whichever API it uses, so
/// 90% of each published limit stays below every single API's own limit.
pub(crate) const READ_QUOTA_CAPS: [(f64, u32); 3] = [(1., 45), (60., 450), (1800., 1800)];

fn reserve_with_history_headroom(c: &Config, now: i64, headroom: [u32; 3]) -> Result<()> {
    // Existing account journal only. Missing file is never created. Failed HTTP
    // still consumes the reservation; windows expire (1s/60s/1800s). There is no
    // 401 latch and no quota reset.
    let meta = fs::symlink_metadata(&c.read_journal)
        .map_err(|_| Fail::new("original_read_journal_changed"))?;
    if !meta.is_file() || meta.dev() != c.read_journal_device || meta.ino() != c.read_journal_inode
    {
        return Err(Fail::new("original_read_journal_changed").into());
    }
    let mut db = Connection::open_with_flags(&c.read_journal, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| Fail::new("read_journal_unreadable"))?;
    db.busy_timeout(Duration::from_millis(
        c.storage.read_journal_busy_timeout_ms,
    ))
    .map_err(|_| Fail::new("read_journal_unreadable"))?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| Fail::new("read_journal_unreadable"))?;
    let time = now as f64 / 1000.;
    let latest: Option<f64> = tx
        .query_row(
            "SELECT MAX(at) FROM read_requests WHERE scope=?",
            [&c.read_scope],
            |r| r.get(0),
        )
        .map_err(|_| Fail::new("read_journal_unreadable"))?;
    if latest.is_some_and(|t| t > time) {
        return Err(Fail::new("read_clock_backwards").into());
    }
    for ((seconds, cap), reserved) in READ_QUOTA_CAPS.into_iter().zip(headroom) {
        let n: i64 = tx
            .query_row(
                "SELECT count(*) FROM read_requests WHERE scope=? AND at>?",
                rusqlite::params![c.read_scope, time - seconds],
                |r| r.get(0),
            )
            .map_err(|_| Fail::new("read_journal_unreadable"))?;
        let limit = i64::from(cap) - i64::from(reserved);
        if n >= limit {
            let code = match seconds as i64 {
                1 => "read_quota_wait_per_second",
                60 => "read_quota_wait_per_minute",
                _ => "read_quota_wait_per_half_hour",
            };
            return Err(Fail::new(code).into());
        }
    }
    // Preserve historical rows, including failed reservations.
    tx.execute(
        "INSERT INTO read_requests(scope,at) VALUES(?,?)",
        rusqlite::params![c.read_scope, time],
    )
    .map_err(|_| Fail::new("read_journal_unreadable"))?;
    tx.commit()
        .map_err(|_| Fail::new("read_journal_unreadable"))?;
    Ok(())
}
pub fn parse(raw: &str, receipt: i64) -> Result<Quote> {
    let body: serde_json::Value = serde_json::from_str(raw)?;
    if body["status"] != "success" {
        return Err("quote_status".into());
    }
    let rows = body["data"].as_object().ok_or("quote_data")?;
    if rows.len() != 1 {
        return Err("quote_count".into());
    }
    let r = rows.values().next().ok_or("quote_missing")?;
    parse_row(r, receipt, KEY, "NIFTYBEES", Price::parse("0.01")?)
}
pub(super) fn quote_source(r: &serde_json::Value) -> Result<i64> {
    let source = if let Some(s) = r["timestamp"].as_str() {
        if s.contains('T') {
            DateTime::parse_from_rfc3339(s)?.timestamp_millis()
        } else {
            s.parse()?
        }
    } else {
        r["timestamp"].as_i64().ok_or("quote_time")?
    };
    Ok(source)
}
pub(super) fn parse_row(
    r: &serde_json::Value,
    receipt: i64,
    key: &str,
    _symbol: &str,
    tick: Price,
) -> Result<Quote> {
    if r["instrument_token"] != key {
        return Err("quote_identity".into());
    }
    let source = quote_source(r)?;
    let depth = |side: &str, buy: bool| -> Result<(Price, u64)> {
        let mut levels = vec![];
        for l in r["depth"][side].as_array().ok_or("quote_depth")? {
            if !l["price"].is_number() {
                return Err("depth_price".into());
            }
            let p = Amount::parse(&l["price"].to_string())?;
            let n = l["quantity"].as_u64().ok_or("depth_units")?;
            if p < Amount::ZERO {
                return Err("negative_price".into());
            }
            if p > Amount::ZERO && n > 0 {
                levels.push((Price::try_from(p)?, n));
            }
        }
        levels.sort_by_key(|v| v.0);
        if buy {
            levels.pop()
        } else {
            levels.into_iter().next()
        }
        .ok_or_else(|| "empty_depth".into())
    };
    let (bid, bid_units) = depth("buy", true)?;
    let (ask, ask_units) = depth("sell", false)?;
    if bid > ask {
        return Err("crossed_depth".into());
    }
    nivra_core::finance::CashOrderGrid::new(1, tick)?.validate(1, bid)?;
    nivra_core::finance::CashOrderGrid::new(1, tick)?.validate(1, ask)?;
    Ok(Quote {
        bid,
        ask,
        bid_units,
        ask_units,
        source,
        receipt,
    })
}
#[cfg(test)]
fn check_security(bytes: &[u8]) -> Result<()> {
    let mut plain = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .take(20_000_001)
        .read_to_end(&mut plain)?;
    if plain.len() > 20_000_000 {
        return Err("security_size".into());
    }
    let mut reader = csv::Reader::from_reader(plain.as_slice());
    let header = reader.headers()?.clone();
    let col = |name: &str| {
        header
            .iter()
            .position(|h| h == name)
            .ok_or("security_column")
    };
    let symbol = col("TckrSymb")?;
    let series = col("SctySrs")?;
    let mut found = 0;
    for row in reader.records() {
        let row = row?;
        if row.get(symbol) == Some("NIFTYBEES") && row.get(series) == Some("EQ") {
            found += 1;
            for (name, value) in [
                ("ISIN", "INF204KB14I2"),
                ("NewBrdLotQty", "1"),
                ("ElgbltyNrmlMkt", "1"),
                ("ElgbltyClsgAuctnSsn", "0"),
            ] {
                if row.get(col(name)?) != Some(value) {
                    return Err("security_membership_changed".into());
                }
            }
        }
    }
    if found != 1 {
        return Err("security_membership_missing".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::OpenOptionsExt;
    fn body() -> serde_json::Value {
        serde_json::json!({"status":"success","data":{"NSE_EQ:NIFTYBEES":{"instrument_token":KEY,"symbol":"NIFTYBEES","timestamp":"2026-10-16T09:15:00+05:30","depth":{"buy":[{"price":300.1,"quantity":100}],"sell":[{"price":300.12,"quantity":100}]}}}})
    }
    #[test]
    fn historical_request_contract_preserves_shared_quota() {
        assert_eq!(retry_after("120", 1_000), Some(121_000));
        assert_eq!(
            retry_after("Thu, 01 Jan 1970 00:02:00 GMT", 0),
            Some(120_000)
        );
        assert_eq!(retry_after("garbage", 0), None);
        let date = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let default = QuoteKeySettings::default();
        let url = historical_url(
            &default,
            KEY,
            "minutes",
            date("2026-01-31"),
            date("2026-02-28"),
        )
        .unwrap();
        assert!(url.ends_with("NSE_EQ%7CINF204KB14I2/minutes/1/2026-02-28/2026-01-31"));
        for (key, unit, from, to) in [
            (KEY, "minutes", "2026-01-31", "2026-03-01"),
            (KEY, "minutes", "2021-12-31", "2022-01-01"),
            (KEY, "days", "1999-12-31", "2000-01-01"),
            (KEY, "days", "2000-01-01", "2010-01-02"),
            (KEY, "hours", "2026-01-01", "2026-01-02"),
            ("NSE_EQ|bad/path", "days", "2026-01-01", "2026-01-02"),
            (KEY, "days", "2026-01-02", "2026-01-01"),
        ] {
            assert!(historical_url(&default, key, unit, date(from), date(to)).is_err());
        }
        assert!(
            historical_url(
                &default,
                KEY,
                "days",
                date("2000-01-01"),
                date("2010-01-01")
            )
            .is_ok()
        );
        // The default allowlist still refuses index keys; a configured
        // NSE_INDEX| prefix admits them and the whole key is percent-encoded.
        assert!(
            historical_url(
                &default,
                "NSE_INDEX|Nifty 50",
                "days",
                date("2026-01-01"),
                date("2026-01-02")
            )
            .is_err()
        );
        let index = QuoteKeySettings {
            prefixes: vec!["NSE_EQ|".into(), "NSE_INDEX|".into()],
            max_keys: 200,
        };
        let url = historical_url(
            &index,
            "NSE_INDEX|Nifty 50",
            "days",
            date("2026-01-01"),
            date("2026-01-02"),
        )
        .unwrap();
        assert!(url.ends_with("NSE_INDEX%7CNifty%2050/days/1/2026-01-02/2026-01-01"));
        let d = tempfile::tempdir().unwrap();
        let mut c = super::super::tests::config(d.path());
        // Config-driven: enabling index feeds with the NSE_INDEX| prefix admits
        // the documented key on the v3 history path (fixture only, no sockets).
        c.quote_keys.prefixes.push("NSE_INDEX|".into());
        c.index_feeds.enabled = true;
        c.validate().unwrap();
        let url = historical_url(
            &c.quote_keys,
            "NSE_INDEX|Nifty 50",
            "days",
            date("2026-01-01"),
            date("2026-01-02"),
        )
        .unwrap();
        assert!(url.ends_with("NSE_INDEX%7CNifty%2050/days/1/2026-01-02/2026-01-01"));
        let db = Connection::open(&c.read_journal).unwrap();
        db.execute_batch("CREATE TABLE read_requests(scope TEXT NOT NULL,at REAL NOT NULL);")
            .unwrap();
        let metadata = fs::metadata(&c.read_journal).unwrap();
        c.read_journal_device = metadata.dev();
        c.read_journal_inode = metadata.ino();
        // Inject the reservation clock without transport or credential access.
        for _ in 0..READ_QUOTA_CAPS[0].1 {
            reserve(&c, 10_000).unwrap();
        }
        let error = reserve(&c, 10_000).unwrap_err();
        assert_eq!(failure_code(error.as_ref()), "read_quota_wait_per_second");
        let error = reserve(&c, 9_999).unwrap_err();
        assert_eq!(failure_code(error.as_ref()), "read_clock_backwards");
        reserve(&c, 11_000).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM read_requests", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            i64::from(READ_QUOTA_CAPS[0].1) + 1
        );
    }
    #[test]
    fn history_reserve_leaves_shared_capacity_until_window_expires() {
        let d = tempfile::tempdir().unwrap();
        let mut c = super::super::tests::config(d.path());
        let db = Connection::open(&c.read_journal).unwrap();
        db.execute_batch("CREATE TABLE read_requests(scope TEXT NOT NULL,at REAL NOT NULL);")
            .unwrap();
        let metadata = fs::metadata(&c.read_journal).unwrap();
        c.read_journal_device = metadata.dev();
        c.read_journal_inode = metadata.ino();
        let path = d.path().join("history-pacing.sqlite");
        let mut first = nivra_store::market_data::open(&path).unwrap();
        nivra_store::market_data::init_history_pacing(&first).unwrap();
        use nivra_store::market_data::{cool_down_history, pace_history};
        assert_eq!(
            pace_history(&mut first, &c.read_scope, 1000, 1250).unwrap(),
            None
        );
        let mut other = nivra_store::market_data::open(&path).unwrap();
        assert_eq!(
            pace_history(&mut other, &c.read_scope, 2249, 1250).unwrap(),
            Some((2250, 0))
        );
        cool_down_history(&first, &c.read_scope, 10_000).unwrap();
        cool_down_history(&other, &c.read_scope, 5_000).unwrap();
        drop(first);
        assert_eq!(
            pace_history(&mut other, &c.read_scope, 9_999, 1250).unwrap(),
            Some((2250, 10_000))
        );
        assert_eq!(
            pace_history(&mut other, &c.read_scope, 10_000, 1250).unwrap(),
            None
        );
        c.history_download.reserve_half_hour_reads = 2;
        // Seed the original shared journal with older reservations outside the
        // short windows, but inside the 30-minute window.
        for _ in 0..READ_QUOTA_CAPS[2].1 - 2 {
            db.execute(
                "INSERT INTO read_requests(scope,at) VALUES(?,?)",
                rusqlite::params![c.read_scope, 1.0],
            )
            .unwrap();
        }
        let error = reserve_history(&c, 100_000).unwrap_err();
        assert_eq!(
            failure_code(error.as_ref()),
            "read_quota_wait_per_half_hour"
        );
        reserve(&c, 100_000).unwrap();
        reserve(&c, 100_000).unwrap();
        assert_eq!(
            failure_code(reserve(&c, 100_000).unwrap_err().as_ref()),
            "read_quota_wait_per_half_hour"
        );
        reserve_history(&c, 1_801_000).unwrap();
        for (window, capacity, reserved, code) in [
            (
                1.0,
                READ_QUOTA_CAPS[0].1 as usize,
                2usize,
                "read_quota_wait_per_second",
            ),
            (
                60.0,
                READ_QUOTA_CAPS[1].1 as usize,
                30,
                "read_quota_wait_per_minute",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut c = super::super::tests::config(root.path());
            let db = Connection::open(&c.read_journal).unwrap();
            db.execute_batch("CREATE TABLE read_requests(scope TEXT NOT NULL,at REAL NOT NULL);")
                .unwrap();
            let metadata = fs::metadata(&c.read_journal).unwrap();
            c.read_journal_device = metadata.dev();
            c.read_journal_inode = metadata.ino();
            for _ in 0..capacity - reserved {
                db.execute(
                    "INSERT INTO read_requests(scope,at) VALUES(?,?)",
                    rusqlite::params![c.read_scope, if window == 1.0 { 100.0 } else { 99.0 }],
                )
                .unwrap();
            }
            assert_eq!(
                failure_code(reserve_history(&c, 100_000).unwrap_err().as_ref()),
                code
            );
            for n in 0..reserved {
                reserve(&c, 100_000 + (n as i64 / 5) * 1000).unwrap();
            }
            assert_eq!(
                db.query_row("SELECT count(*) FROM read_requests", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                capacity as i64
            );
            reserve_history(&c, if window == 1.0 { 101_000 } else { 160_000 }).unwrap();
            assert_eq!(
                db.query_row("SELECT count(*) FROM read_requests", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                (capacity + 1) as i64
            );
        }
    }
    #[test]
    fn quote_rederivation_and_identity() {
        let b = body();
        let receipt = DateTime::parse_from_rfc3339("2026-10-16T09:15:01+05:30")
            .unwrap()
            .timestamp_millis();
        let q = parse(&b.to_string(), receipt).unwrap();
        assert_eq!(q.receipt, receipt);
        assert_eq!(q.bid, Price::parse("300.1").unwrap());
        let mut bad = b.clone();
        bad["data"]["NSE_EQ:NIFTYBEES"]["instrument_token"] = serde_json::json!("NSE_EQ|WRONG");
        assert!(parse(&bad.to_string(), receipt).is_err());
        let mut bad = b.clone();
        bad["data"]["NSE_EQ:NIFTYBEES"]["depth"]["buy"][0]["price"] = serde_json::json!(301);
        assert!(parse(&bad.to_string(), receipt).is_err());
        let mut bad = b;
        bad["data"]["NSE_EQ:NIFTYBEES"]["depth"]["sell"][0]["quantity"] = serde_json::json!(0);
        assert!(parse(&bad.to_string(), receipt).is_err());
    }
    #[test]
    fn nse_membership_requires_exact_unique_row() {
        use std::io::Write;
        let zipped = |s: &str| {
            let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(s.as_bytes()).unwrap();
            z.finish().unwrap()
        };
        let h = "TckrSymb,SctySrs,ISIN,NewBrdLotQty,ElgbltyNrmlMkt,ElgbltyClsgAuctnSsn\n";
        let r = "NIFTYBEES,EQ,INF204KB14I2,1,1,0\n";
        assert!(check_security(&zipped(&format!("{h}{r}"))).is_ok());
        assert!(check_security(&zipped(&format!("{h}{r}{r}"))).is_err());
        assert!(check_security(&zipped(&format!("{h}{}", r.replace(",1,1,0", ",1,0,0")))).is_err());
        assert!(check_security(b"404").is_err());
    }
    fn assert_token(raw: &str, expected: &str) {
        assert_eq!(parse_analytics_token(raw).unwrap(), expected);
    }
    fn assert_token_err(raw: &str) {
        let error = parse_analytics_token(raw).unwrap_err();
        assert_eq!(error, "credential_format");
        assert!(!error.contains("secret"));
        assert!(!error.to_string().contains("secret"));
    }
    #[test]
    fn analytics_token_accepts_raw_and_key_value_forms() {
        const TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmaXh0dXJlIn0.sig";
        assert_token(TOKEN, TOKEN);
        assert_token(&format!("{TOKEN}\n"), TOKEN);
        assert_token(&format!("  {TOKEN}  \n"), TOKEN);
        assert_token(&format!("\n# comment\n{TOKEN}\n"), TOKEN);
        assert_token(&format!("\"{TOKEN}\""), TOKEN);
        assert_token(&format!("'{TOKEN}'"), TOKEN);
        assert_token(&format!("UPSTOX_ANALYTICS_TOKEN={TOKEN}"), TOKEN);
        assert_token(&format!("UPSTOX_ANALYTICS_TOKEN = {TOKEN}\n"), TOKEN);
        assert_token(&format!("export UPSTOX_ANALYTICS_TOKEN={TOKEN}"), TOKEN);
        assert_token(
            &format!("export\tUPSTOX_ANALYTICS_TOKEN=\"{TOKEN}\""),
            TOKEN,
        );
        assert_token(&format!("\u{feff}UPSTOX_ANALYTICS_TOKEN={TOKEN}\n"), TOKEN);
        assert_token(
            "fixture-token-with-padding==",
            "fixture-token-with-padding==",
        );
        assert_token_err("UPSTOX_ANALYTICS_TOKEN=secret-value\nOTHER=1");
        assert_token_err("OTHER_SECRET=secret-value");
        assert_token_err("UPSTOX_ANALYTICS_TOKEN=");
        assert_token_err("UPSTOX_ANALYTICS_TOKEN=secret value");
        assert_token_err("UPSTOX_ANALYTICS_TOKEN=secret$");
        assert_token_err("UPSTOX_ANALYTICS_TOKEN=\"secret");
        assert_token_err("");
        assert_token_err("# only comments\n");
        let leaked =
            parse_analytics_token("UPSTOX_ANALYTICS_TOKEN=must-not-appear-in-errors").unwrap();
        assert_eq!(leaked, "must-not-appear-in-errors");
        let error = parse_analytics_token("UPSTOX_ANALYTICS_TOKEN=must-not-appear-in-errors\nx")
            .unwrap_err();
        assert_eq!(error, "credential_format");
        assert!(!error.contains("must-not-appear-in-errors"));
    }
    #[test]
    fn nse_eq_keys_allow_underscore_and_reject_injection() {
        let default = QuoteKeySettings::default();
        assert!(valid_quote_keys(&["NSE_EQ|INF204KB14I2"], &default));
        assert!(valid_quote_keys(&["NSE_EQ|INE000000001"], &default));
        assert!(!valid_quote_keys(&["NSE-EQ|INF204KB14I2"], &default));
        assert!(!valid_quote_keys(&["NSE_EQ|INF204KB14I2;rm"], &default));
        assert!(!valid_quote_keys(&["BSE_EQ|INF204KB14I2"], &default));
        assert!(!valid_quote_keys(&[], &default));
        // Index keys contain a space and are refused until configured.
        assert!(!valid_quote_keys(&["NSE_INDEX|Nifty 50"], &default));
        let configured = QuoteKeySettings {
            prefixes: vec!["NSE_EQ|".into(), "NSE_INDEX|".into()],
            max_keys: 200,
        };
        assert!(valid_quote_keys(
            &["NSE_EQ|INF204KB14I2", "NSE_INDEX|Nifty 50"],
            &configured
        ));
        for rejected in [
            "NSE_INDEX|",
            "NSE_INDEX| Nifty 50",
            "NSE_INDEX|Nifty 50 ",
            "NSE_INDEX|Nifty 50;rm",
            "NSE_INDEX|Nifty/50",
            "NSE_INDEX|Nifty..50",
            "NSE_INDEX|Nifty\u{7c}50",
        ] {
            assert!(!valid_quote_keys(&[rejected], &configured), "{rejected}");
        }
        // The 200-key request cap is enforced by the policy.
        let at_cap = vec!["NSE_EQ|A"; 200];
        assert!(valid_quote_keys(&at_cap, &default));
        let over_cap = vec!["NSE_EQ|A"; 201];
        assert!(!valid_quote_keys(&over_cap, &default));
    }
    #[test]
    fn invalid_token_does_not_consume_read_quota() {
        let d = tempfile::tempdir().unwrap();
        let mut c = super::super::tests::config(d.path());
        let db = Connection::open(&c.read_journal).unwrap();
        db.execute_batch("CREATE TABLE read_requests(scope TEXT NOT NULL,at REAL NOT NULL);CREATE INDEX read_window ON read_requests(scope,at);").unwrap();
        let m = fs::metadata(&c.read_journal).unwrap();
        c.read_journal_device = m.dev();
        c.read_journal_inode = m.ino();
        c.token_file = d.path().join("token");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&c.token_file)
            .unwrap();
        std::io::Write::write_all(&mut file, b"secret token with spaces\n").unwrap();
        let client = Client::new(&c.market_data_transport).unwrap();
        let error = client.quotes(&c, &["NSE_EQ|INF204KB14I2"]).unwrap_err();
        assert_eq!(failure_code(error.as_ref()), "credential_format");
        assert!(!error.to_string().contains("secret"));
        assert_eq!(
            db.query_row("SELECT count(*) FROM read_requests", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
