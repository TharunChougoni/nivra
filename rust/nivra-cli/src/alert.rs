//! Optional HTTPS notifications. Only typed events and numeric/report details cross this boundary.
use crate::runtime::operational_health::{
    ALERT_MAX_OUTBOX_ROWS, ALERT_MAX_RETAINED_DELIVERY_ROWS, ALERT_OUTBOX_DEAD_SCOPE,
    ALERT_OUTBOX_ROW_SCOPE, ALERT_OUTBOX_SCOPE, ALERT_RETAINED_WINDOW_SCOPE,
    ALERT_RETENTION_WINDOW_SECONDS, ALERT_TIMESTAMP_SCOPE, ALERT_TRUNCATION_UNKNOWN, AlertCounts,
    AlertRetainedWindow, AlertScope, AlertStoreStatus, AlertTransportStatus,
};
use chrono::{FixedOffset, Utc};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    time::Duration,
};

type Result<T> = std::result::Result<T, ()>;
#[derive(Clone, Copy, clap::ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum Event {
    Test,
    Halt,
    Stop,
    RestartLoop,
    PlannedRestart,
    UpdaterFailure,
    RolloverBlocked,
    MissedPreopen,
    StaleFeed,
    MarketFeedFallback,
    MarketFeedRecovered,
    ScannerFreshDrop,
    ReadinessBlocked,
    SupervisorFailure,
    ModelCredentialUnavailable,
    ModelRefreshFailed,
    SessionStart,
    SessionEnd,
    ExperimentProfileApplied,
    ModelFallback,
}
impl Event {
    const ALL: [Self; 20] = [
        Self::Test,
        Self::Halt,
        Self::Stop,
        Self::RestartLoop,
        Self::PlannedRestart,
        Self::UpdaterFailure,
        Self::RolloverBlocked,
        Self::MissedPreopen,
        Self::StaleFeed,
        Self::MarketFeedFallback,
        Self::MarketFeedRecovered,
        Self::ScannerFreshDrop,
        Self::ReadinessBlocked,
        Self::SupervisorFailure,
        Self::ModelCredentialUnavailable,
        Self::ModelRefreshFailed,
        Self::SessionStart,
        Self::SessionEnd,
        Self::ExperimentProfileApplied,
        Self::ModelFallback,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Halt => "halt",
            Self::Stop => "stop",
            Self::RestartLoop => "restart_loop",
            Self::PlannedRestart => "planned_restart",
            Self::UpdaterFailure => "updater_failure",
            Self::RolloverBlocked => "rollover_blocked",
            Self::MissedPreopen => "missed_preopen",
            Self::StaleFeed => "stale_feed",
            Self::MarketFeedFallback => "market_feed_fallback",
            Self::MarketFeedRecovered => "market_feed_recovered",
            Self::ScannerFreshDrop => "scanner_fresh_drop",
            Self::ReadinessBlocked => "readiness_blocked",
            Self::SupervisorFailure => "supervisor_failure",
            Self::ModelCredentialUnavailable => "model_credential_unavailable",
            Self::ModelRefreshFailed => "model_refresh_failed",
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
            Self::ExperimentProfileApplied => "experiment_profile_applied",
            Self::ModelFallback => "model_fallback",
        }
    }
    /// Reconstruct an event from a persisted payload name. Retired Composer
    /// approval names stay readable so historic outbox rows are not quarantined
    /// after the live catalog dropped them.
    pub fn from_recorded_name(name: &str) -> Option<RecordedEvent> {
        if let Some(live) = Self::ALL.iter().copied().find(|event| event.name() == name) {
            return Some(RecordedEvent::Live(live));
        }
        match name {
            "approval_renewed" => Some(RecordedEvent::Retired(RetiredAlert::ApprovalRenewed)),
            "approval_renew_blocked" => {
                Some(RecordedEvent::Retired(RetiredAlert::ApprovalRenewBlocked))
            }
            _ => None,
        }
    }
    fn severity(self) -> &'static str {
        match self {
            Self::Test
            | Self::SessionStart
            | Self::SessionEnd
            | Self::PlannedRestart
            | Self::MarketFeedRecovered
            | Self::ExperimentProfileApplied => "info",
            Self::RolloverBlocked
            | Self::MissedPreopen
            | Self::StaleFeed
            | Self::MarketFeedFallback
            | Self::ScannerFreshDrop
            | Self::ReadinessBlocked
            | Self::ModelCredentialUnavailable
            | Self::ModelFallback => "warning",
            _ => "critical",
        }
    }
}

/// A persisted alert name, including names the live catalog no longer emits.
#[derive(Clone, Copy)]
pub enum RecordedEvent {
    Live(Event),
    Retired(RetiredAlert),
}

impl RecordedEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live(event) => event.name(),
            Self::Retired(RetiredAlert::ApprovalRenewed) => "approval_renewed",
            Self::Retired(RetiredAlert::ApprovalRenewBlocked) => "approval_renew_blocked",
        }
    }
}

/// Names written before Composer approval renew was removed. Never newly emitted.
#[derive(Clone, Copy)]
pub enum RetiredAlert {
    ApprovalRenewed,
    ApprovalRenewBlocked,
}

// No destination, credentials or book authority is stored in this expiring notice.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannedRestart {
    version: String,
    issued_at: i64,
    expires_at: i64,
}
fn planned_restart(state: &Path, at: i64) -> Option<PlannedRestart> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(state.join("planned-restart.json"))
        .ok()?;
    let m = file.metadata().ok()?;
    if !m.is_file()
        || m.mode() & 0o7777 != 0o600
        || m.nlink() != 1
        || m.uid() != fs::metadata("/proc/self").ok()?.uid()
    {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(1025).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 1024 {
        return None;
    }
    let marker: PlannedRestart = serde_json::from_slice(&bytes).ok()?;
    let parts: Vec<_> = marker.version.strip_prefix('v')?.split('.').collect();
    if marker.version.len() > 32
        || parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()))
        || marker.issued_at > at
        || marker.expires_at <= at
        || marker.expires_at.checked_sub(marker.issued_at)? != 600
    {
        return None;
    }
    Some(marker)
}
pub fn plan_restart(state: &Path, version: &str, at: i64) -> Result<()> {
    use std::io::Write;
    // Keep one notice across the update's forward restart and possible rollback.
    if planned_restart(state, at).is_some() {
        return Ok(());
    }
    let marker = PlannedRestart {
        version: version.into(),
        issued_at: at,
        expires_at: at.checked_add(600).ok_or(())?,
    };
    let tmp = state.join("planned-restart.next");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .map_err(|_| ())?;
    file.write_all(&serde_json::to_vec(&marker).map_err(|_| ())?)
        .map_err(|_| ())?;
    file.sync_all().map_err(|_| ())?;
    fs::rename(tmp, state.join("planned-restart.json")).map_err(|_| ())?;
    fs::File::open(state)
        .and_then(|f| f.sync_all())
        .map_err(|_| ())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    url: String,
    #[serde(default = "authorization")]
    header_name: String,
    header_value: String,
}
fn now() -> chrono::DateTime<Utc> {
    std::time::SystemTime::now().into()
}
fn authorization() -> String {
    "Authorization".into()
}
fn endpoint(path: &Path) -> Result<Endpoint> {
    let canonical = path.canonicalize().map_err(|_| ())?;
    // Reject repository paths even when launched from outside the checkout.
    if canonical.ancestors().any(|p| p.join(".git").exists())
        || canonical.starts_with(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .ok_or(())?,
        )
    {
        return Err(());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ())?;
    let m = file.metadata().map_err(|_| ())?;
    if !m.is_file()
        || m.mode() & 0o7777 != 0o600
        || m.uid() != fs::metadata("/proc/self").map_err(|_| ())?.uid()
        || m.nlink() != 1
    {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take(16385).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() > 16384 {
        return Err(());
    }
    let e: Endpoint = serde_json::from_slice(&bytes).map_err(|_| ())?;
    let uri: ureq::http::Uri = e.url.parse().map_err(|_| ())?;
    if uri.scheme_str() != Some("https")
        || uri.host().is_none()
        || uri.authority().is_some_and(|a| a.as_str().contains('@'))
        || e.url.contains('#')
    {
        return Err(());
    }
    ureq::http::HeaderName::from_bytes(e.header_name.as_bytes()).map_err(|_| ())?;
    ureq::http::HeaderValue::from_str(&e.header_value).map_err(|_| ())?;
    if [
        "host",
        "content-length",
        "transfer-encoding",
        "content-type",
    ]
    .contains(&e.header_name.to_ascii_lowercase().as_str())
    {
        return Err(());
    }
    Ok(e)
}
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or(())
}
fn destination(explicit: Option<&Path>) -> Result<Option<PathBuf>> {
    if let Some(p) = explicit {
        return Ok(Some(p.to_owned()));
    }
    let p = home()?.join(".config/nivra/alert-webhook.json");
    Ok(p.symlink_metadata().is_ok().then_some(p))
}
fn directory() -> Result<PathBuf> {
    let p = home()?.join(".local/state/nivra/alerts");
    fs::create_dir_all(&p).map_err(|_| ())?;
    let m = p.symlink_metadata().map_err(|_| ())?;
    if !m.is_dir() || m.uid() != fs::metadata("/proc/self").map_err(|_| ())?.uid() {
        return Err(());
    }
    fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).map_err(|_| ())?;
    Ok(p)
}
fn status_directory() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/nivra/alerts"))
}
/// Read-only transport projection. It never reads a payload, destination,
/// credential or path, and a sender HTTP 2xx is not a human read.
///
/// SQLite may create or maintain `delivery.sqlite-shm` lock metadata for an
/// existing WAL database even on a read-only connection. The main database,
/// schema and rows are never created or written here. `immutable=1` is
/// deliberately not used because it is unsafe for a live WAL, and locking is
/// not disabled.
pub(crate) fn transport_status(account: &str, session: &str, now_ms: i64) -> AlertTransportStatus {
    let scope = AlertScope::admitted(account, session);
    let Some(dir) = status_directory() else {
        return AlertTransportStatus::unavailable(AlertStoreStatus::Disabled, scope, false);
    };
    transport_status_at(&dir, account, session, now_ms)
}
pub(crate) fn transport_status_at(
    dir: &Path,
    account: &str,
    session: &str,
    now_ms: i64,
) -> AlertTransportStatus {
    let scope = AlertScope::admitted(account, session);
    let failure_marker_present = dir
        .join("last-failure")
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file());
    let path = dir.join("delivery.sqlite");
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return AlertTransportStatus::unavailable(
                AlertStoreStatus::Missing,
                scope,
                failure_marker_present,
            );
        }
        Err(_) => {
            return AlertTransportStatus::unavailable(
                AlertStoreStatus::Unreadable,
                scope,
                failure_marker_present,
            );
        }
        Ok(metadata) if !metadata.is_file() => {
            return AlertTransportStatus::unavailable(
                AlertStoreStatus::Malformed,
                scope,
                failure_marker_present,
            );
        }
        Ok(_) => {}
    }
    let db = match Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(db) => db,
        Err(_) => {
            return AlertTransportStatus::unavailable(
                AlertStoreStatus::Unreadable,
                scope,
                failure_marker_present,
            );
        }
    };
    if db.busy_timeout(Duration::from_millis(250)).is_err() {
        return AlertTransportStatus::unavailable(
            AlertStoreStatus::Unreadable,
            scope,
            failure_marker_present,
        );
    }
    match read_transport_counts(&db, now_ms) {
        Ok(counts) => {
            let (last_failure_at_ms, last_failure_category) = match counts.last_failure {
                Some((at, category)) => (Some(at), Some(category)),
                None => (None, None),
            };
            AlertTransportStatus {
                status: AlertStoreStatus::Available,
                store: "alert_outbox",
                counts_scope: "host_alert_outbox_not_account_filtered",
                scope,
                counts: Some(counts.counts),
                retained_window: Some(counts.retained_window),
                last_accepted_at_ms: counts.last_accepted_at_ms,
                last_accepted_scope: ALERT_TIMESTAMP_SCOPE,
                last_failure_at_ms,
                last_failure_category,
                last_failure_scope: ALERT_TIMESTAMP_SCOPE,
                failure_marker_present,
                failure_marker_definition: "marker_presence_not_attempt_outcome",
                retention_window_seconds: ALERT_RETENTION_WINDOW_SECONDS,
                timestamp_normalization: "delivery_at_unix_seconds_checked_mul_1000_unknown_on_overflow",
                dropped_source: "not_recorded",
                ack_meaning: "sender_http_2xx_receipt_not_human_read",
                delivery_guarantee: false,
            }
        }
        Err(_) => AlertTransportStatus::unavailable(
            AlertStoreStatus::Malformed,
            scope,
            failure_marker_present,
        ),
    }
}
struct TransportCounts {
    counts: AlertCounts,
    retained_window: AlertRetainedWindow,
    last_accepted_at_ms: Option<i64>,
    last_failure: Option<(i64, &'static str)>,
}
fn read_transport_counts(
    db: &Connection,
    now_ms: i64,
) -> std::result::Result<TransportCounts, rusqlite::Error> {
    let now_s = now_ms.div_euclid(1000);
    let window_start_s = now_s - ALERT_RETENTION_WINDOW_SECONDS;
    // The retained source stores Unix seconds, so the exposed millisecond
    // bounds are the exact second-resolution bounds used by the queries.
    let window_start_ms = window_start_s * 1000;
    let window_end_ms = now_s * 1000;
    let queued: i64 = db.query_row(
        "SELECT count(*) FROM outbox WHERE quarantined=0 AND attempts=0",
        [],
        |row| row.get(0),
    )?;
    let retrying: i64 = db.query_row(
        "SELECT count(*) FROM outbox WHERE quarantined=0 AND attempts>0",
        [],
        |row| row.get(0),
    )?;
    let dead: i64 = db.query_row(
        "SELECT count(*) FROM outbox WHERE quarantined=1",
        [],
        |row| row.get(0),
    )?;
    let retained_delivery_rows: i64 =
        db.query_row("SELECT count(*) FROM delivery", [], |row| row.get(0))?;
    let rows_in_window: i64 = db.query_row(
        "SELECT count(*) FROM delivery WHERE at >= ?1 AND at <= ?2",
        params![window_start_s, now_s],
        |row| row.get(0),
    )?;
    let clock_anomaly: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM delivery WHERE at > ?1)",
        [now_s],
        |row| row.get(0),
    )?;
    let transport_accepted: i64 = db.query_row(
        "SELECT count(*) FROM delivery WHERE status BETWEEN 200 AND 299 AND at >= ?1 AND at <= ?2",
        params![window_start_s, now_s],
        |row| row.get(0),
    )?;
    let last_accepted_at_s: Option<i64> = db.query_row(
        "SELECT max(at) FROM delivery WHERE status BETWEEN 200 AND 299 AND at >= ?1 AND at <= ?2",
        params![window_start_s, now_s],
        |row| row.get(0),
    )?;
    let last_failure: Option<(i64, i64)> = db
        .query_row(
            "SELECT at,status FROM delivery WHERE at IS NOT NULL AND status IS NOT NULL AND status <> 0 AND status NOT BETWEEN 200 AND 299 AND at >= ?1 AND at <= ?2 ORDER BY at DESC LIMIT 1",
            params![window_start_s, now_s],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let in_flight_or_transport_error: i64 = db.query_row(
        "SELECT count(*) FROM delivery WHERE (status = 0 OR status IS NULL) AND at >= ?1 AND at <= ?2",
        params![window_start_s, now_s],
        |row| row.get(0),
    )?;
    Ok(TransportCounts {
        counts: AlertCounts {
            queued,
            retrying,
            transport_accepted,
            transport_accepted_scope: ALERT_TIMESTAMP_SCOPE,
            dead,
            dead_definition: "quarantined_outbox_rows",
            dropped: None,
            in_flight_or_transport_error,
            in_flight_or_transport_error_definition: "status_zero_or_null_in_flight_or_transport_error_not_confirmed_failure",
            outbox_scope: ALERT_OUTBOX_SCOPE,
            outbox_max_rows: ALERT_MAX_OUTBOX_ROWS,
            queued_scope: ALERT_OUTBOX_ROW_SCOPE,
            retrying_scope: ALERT_OUTBOX_ROW_SCOPE,
            dead_scope: ALERT_OUTBOX_DEAD_SCOPE,
            in_flight_or_transport_error_scope: ALERT_TIMESTAMP_SCOPE,
        },
        retained_window: AlertRetainedWindow {
            start_ms: window_start_ms,
            end_ms: window_end_ms,
            scope: ALERT_RETAINED_WINDOW_SCOPE,
            max_retained_rows: ALERT_MAX_RETAINED_DELIVERY_ROWS,
            retained_delivery_rows,
            rows_in_window,
            truncation: ALERT_TRUNCATION_UNKNOWN,
            clock_anomaly,
        },
        last_accepted_at_ms: last_accepted_at_s.and_then(checked_seconds_to_ms),
        last_failure: last_failure.and_then(|(at, status)| {
            checked_seconds_to_ms(at).map(|at_ms| (at_ms, failure_category(status)))
        }),
    })
}
fn checked_seconds_to_ms(seconds: i64) -> Option<i64> {
    seconds.checked_mul(1000)
}
fn failure_category(status: i64) -> &'static str {
    match status {
        400..=499 => {
            if status == 429 {
                "rate_limited"
            } else {
                "receiver_rejected"
            }
        }
        500..=599 => "receiver_error",
        100..=199 => "informational",
        300..=399 => "redirect_or_unexpected",
        _ => "unknown",
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    source: String,
    event: String,
    severity: String,
    session: Option<chrono::NaiveDate>,
    at: String,
    version: String,
    summary: String,
    details: Value,
    dedupe_key: String,
}
impl Payload {
    fn valid(&self) -> bool {
        self.source == "nivra"
            && Event::from_recorded_name(&self.event)
                .is_some_and(|recorded| recorded.as_str() == self.event)
            && ["info", "warning", "critical"].contains(&self.severity.as_str())
            && self.details.is_object()
            && self.dedupe_key.len() <= 256
    }
}
impl Payload {
    fn new(event: Event, session: Option<chrono::NaiveDate>, mut details: Value) -> Self {
        let feed_incident = if matches!(event, Event::ModelCredentialUnavailable) {
            // Distinct models and blocker reasons must not suppress each other's notices.
            format!(
                ":{}:{}",
                details["model"].as_str().unwrap_or("configuration"),
                details["reason"].as_str().unwrap_or("unspecified")
            )
        } else if matches!(
            event,
            Event::MarketFeedFallback | Event::MarketFeedRecovered
        ) {
            format!(":{}", details["feed_generation"].as_u64().unwrap_or(0))
        } else if matches!(event, Event::UpdaterFailure) {
            // Retrying the same blocker stays deduped; a different reason alerts.
            format!(":{}", details["reason"].as_str().unwrap_or("update_failed"))
        } else {
            String::new()
        };
        let closed_incident =
            matches!(event, Event::Stop | Event::Halt) && details["closed"] == true;
        let planned_stop = matches!(event, Event::Stop)
            && details["operator_stopped"] == true
            && details["halted"] == false;
        if planned_stop {
            details["classification"] = json!("expected_not_failure");
        }
        Self {
            source: "nivra".into(),
            event: event.name().into(),
            severity: if closed_incident || planned_stop {
                "info".into()
            } else {
                event.severity().into()
            },
            session,
            at: now()
                .with_timezone(&FixedOffset::east_opt(19800).expect("IST"))
                .to_rfc3339(),
            version: env!("CARGO_PKG_VERSION").into(),
            summary: if closed_incident {
                "Closed paper book retained; no session reactivation".into()
            } else if planned_stop {
                "Expected protection (expected_not_failure): operator requested paper stop; explicit resume required".into()
            } else {
                match event {
                    Event::Stop => "Paper stop; operator review required".into(),
                    Event::Halt => "Paper risk halt; inspect original book".into(),
                    Event::SessionEnd => {
                        "Paper session ended; inspect reconciliation report".into()
                    }
                    Event::UpdaterFailure => format!(
                        "Updater failure: {}",
                        details["reason"].as_str().unwrap_or("update_failed")
                    ),
                    _ => event.name().into(),
                }
            },
            details,
            dedupe_key: format!(
                "{}:{}{}",
                event.name(),
                session
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "operator".into()),
                feed_incident
            ),
        }
    }
    fn with_restart(mut self, marker: Option<PlannedRestart>) -> Self {
        if matches!(
            self.event.as_str(),
            "stop" | "halt" | "restart_loop" | "session_start" | "planned_restart"
        ) && !(matches!(self.event.as_str(), "stop" | "halt") && self.details["closed"] == false)
            && let Some(marker) = marker
        {
            self.event = "planned_restart".into();
            self.severity = "info".into();
            self.summary = "Updater planned paper controller restart".into();
            self.session = None;
            self.details = json!({"target_version":marker.version});
            self.dedupe_key = format!("planned_restart:{}:{}", marker.version, marker.issued_at);
        }
        self
    }
}
trait Transport {
    fn send(&mut self, endpoint: &Endpoint, payload: &Payload) -> Result<u16>;
}
struct Https;
impl Transport for Https {
    fn send(&mut self, e: &Endpoint, p: &Payload) -> Result<u16> {
        // No redirects: credentials must never be forwarded to a different destination.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        let bytes = serde_json::to_vec(p).map_err(|_| ())?;
        agent
            .post(&e.url)
            .header(&e.header_name, &e.header_value)
            .header("Content-Type", "application/json")
            .send(bytes.as_slice())
            .map(|r| r.status().as_u16())
            .map_err(|_| ())
    }
}
fn database(dir: &Path) -> Result<Connection> {
    let p = dir.join("delivery.sqlite");
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&p)
        .map_err(|_| ())?;
    let db = Connection::open(p).map_err(|_| ())?;
    db.busy_timeout(Duration::from_millis(250))
        .map_err(|_| ())?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS attempts(key TEXT PRIMARY KEY,at INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS delivery(id INTEGER PRIMARY KEY,at INTEGER,status INTEGER); CREATE TABLE IF NOT EXISTS suppressed_keys(key TEXT PRIMARY KEY,count INTEGER NOT NULL,at INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS starts(at INTEGER); CREATE TABLE IF NOT EXISTS outbox(id INTEGER PRIMARY KEY,payload TEXT NOT NULL,enqueued_at INTEGER NOT NULL,next_attempt_at INTEGER NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,admitted INTEGER NOT NULL DEFAULT 0,quarantined INTEGER NOT NULL DEFAULT 0);")
        .map_err(|_| ())?;
    Ok(db)
}
const OUTBOX_LIMIT: i64 = ALERT_MAX_OUTBOX_ROWS;
const MAX_OUTBOX_PAYLOAD_BYTES: usize = 16_384;

fn enqueue(db: &mut Connection, payload: &Payload, now: i64) -> Result<()> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ())?;
    let count: i64 = tx
        .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))
        .map_err(|_| ())?;
    if count >= OUTBOX_LIMIT {
        return Err(());
    }
    let encoded = serde_json::to_string(payload).map_err(|_| ())?;
    if encoded.len() > MAX_OUTBOX_PAYLOAD_BYTES {
        return Err(());
    }
    tx.execute(
        "INSERT INTO outbox(payload,enqueued_at,next_attempt_at) VALUES(?,?,?)",
        params![encoded, now, now],
    )
    .map_err(|_| ())?;
    tx.commit().map_err(|_| ())
}

// Caller holds the cross-process sender lock. Admission is committed before
// HTTP so a restart can resume this exact outbox item without re-admitting it.
fn admit(
    db: &mut Connection,
    mut p: Payload,
    now: i64,
    outbox_id: Option<i64>,
) -> Result<Option<Payload>> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ())?;
    tx.execute("DELETE FROM attempts WHERE at <= ?", [now - 1800])
        .map_err(|_| ())?;
    tx.execute("DELETE FROM delivery WHERE at <= ?", [now - 3600])
        .map_err(|_| ())?;
    // Suppression history is per dedupe key, never shared across event types.
    tx.execute("DELETE FROM suppressed_keys WHERE at <= ?", [now - 604_800])
        .map_err(|_| ())?;
    let duplicate: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE key=?)",
            [&p.dedupe_key],
            |r| r.get(0),
        )
        .map_err(|_| ())?;
    let count: i64 = tx
        .query_row("SELECT count(*) FROM delivery", [], |r| r.get(0))
        .map_err(|_| ())?;
    if duplicate || count >= 20 {
        tx.execute(
            "INSERT INTO suppressed_keys(key,count,at) VALUES(?,1,?) ON CONFLICT(key) DO UPDATE SET count=count+1,at=excluded.at",
            params![&p.dedupe_key, now],
        )
        .map_err(|_| ())?;
        tx.commit().map_err(|_| ())?;
        return Ok(None);
    }
    let suppressed: i64 = tx
        .query_row(
            "SELECT COALESCE((SELECT count FROM suppressed_keys WHERE key=?),0)",
            [&p.dedupe_key],
            |r| r.get(0),
        )
        .map_err(|_| ())?;
    p.details["suppressed_count"] = json!(suppressed);
    tx.execute(
        "INSERT OR REPLACE INTO attempts VALUES(?,?)",
        params![p.dedupe_key, now],
    )
    .map_err(|_| ())?;
    if let Some(id) = outbox_id {
        let encoded = serde_json::to_string(&p).map_err(|_| ())?;
        tx.execute(
            "UPDATE outbox SET payload=?,admitted=1 WHERE id=?",
            params![encoded, id],
        )
        .map_err(|_| ())?;
    }
    tx.commit().map_err(|_| ())?;
    Ok(Some(p))
}

fn send_admitted(
    db: &Connection,
    e: &Endpoint,
    p: &Payload,
    now: i64,
    t: &mut impl Transport,
    retry: bool,
    outbox_id: Option<i64>,
) -> Result<u16> {
    db.execute("DELETE FROM delivery WHERE at <= ?", [now - 3600])
        .map_err(|_| ())?;
    let mut status = 0;
    for attempt in 0..3 {
        let count: i64 = db
            .query_row("SELECT count(*) FROM delivery", [], |r| r.get(0))
            .map_err(|_| ())?;
        if count >= 20 {
            break;
        }
        db.execute("INSERT INTO delivery(at,status) VALUES(?,0)", [now])
            .map_err(|_| ())?;
        let id = db.last_insert_rowid();
        status = t.send(e, p).unwrap_or(0);
        if (200..300).contains(&status) {
            db.execute_batch("BEGIN IMMEDIATE").map_err(|_| ())?;
            let finalized = db
                .execute(
                    "UPDATE delivery SET status=? WHERE id=?",
                    params![status, id],
                )
                .and_then(|_| {
                    db.execute("DELETE FROM suppressed_keys WHERE key=?", [&p.dedupe_key])
                })
                .and_then(|_| match outbox_id {
                    Some(outbox_id) => db.execute("DELETE FROM outbox WHERE id=?", [outbox_id]),
                    None => Ok(0),
                });
            if finalized.is_err() || db.execute_batch("COMMIT").is_err() {
                let _ = db.execute_batch("ROLLBACK");
                return Err(());
            }
            return Ok(status);
        }
        db.execute(
            "UPDATE delivery SET status=? WHERE id=?",
            params![status, id],
        )
        .map_err(|_| ())?;
        if (200..300).contains(&status) || !retry || (400..500).contains(&status) && status != 429 {
            break;
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_secs(1 << attempt));
        }
    }
    if status == 0 { Err(()) } else { Ok(status) }
}

// Caller holds the sender lock. Admission and per-attempt receipts are durable.
fn deliver(
    db: &mut Connection,
    e: &Endpoint,
    p: Payload,
    now: i64,
    t: &mut impl Transport,
    retry: bool,
) -> Result<u16> {
    match admit(db, p, now, None)? {
        Some(p) => send_admitted(db, e, &p, now, t, retry, None),
        None => Ok(0),
    }
}

fn locked<T>(dir: &Path, f: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("sender.lock"))
        .map_err(|_| ())?;
    lock.lock_exclusive().map_err(|_| ())?;
    f(&mut database(dir)?)
}
fn failure(dir: &Path) {
    use std::io::Write;
    if let Ok(mut file) = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join("last-failure"))
    {
        let _ = file.write_all(b"alert_failed_redacted\n");
    }
}
/// A later successful delivery clears the stale failure marker (#42).
fn recovered(dir: &Path) {
    let _ = fs::remove_file(dir.join("last-failure"));
}

/// Sends one due outbox item. A successful receiver response and local receipt
/// commit delete the queue row atomically; an interrupted send can be repeated.
fn process_one(
    dir: &Path,
    db: &mut Connection,
    e: &Endpoint,
    now: i64,
    t: &mut impl Transport,
    dropped: &AtomicU64,
    retry: bool,
) -> Result<bool> {
    let row = db
        .query_row(
            "SELECT id,payload,attempts,admitted FROM outbox WHERE quarantined=0 AND next_attempt_at<=? ORDER BY id LIMIT 1",
            [now],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|_| ())?;
    let Some((id, raw, attempts, already_admitted)) = row else {
        return Ok(false);
    };
    let payload: Payload = match serde_json::from_str::<Payload>(&raw) {
        Ok(payload) if payload.valid() => payload,
        _ => {
            // Preserve malformed durable evidence while allowing later alerts through.
            db.execute("UPDATE outbox SET quarantined=1 WHERE id=?", [id])
                .map_err(|_| ())?;
            failure(dir);
            return Ok(true);
        }
    };
    let payload = if already_admitted {
        let mut payload = payload;
        let suppressed: i64 = db
            .query_row(
                "SELECT COALESCE((SELECT count FROM suppressed_keys WHERE key=?),0)",
                [&payload.dedupe_key],
                |row| row.get(0),
            )
            .map_err(|_| ())?;
        payload.details["suppressed_count"] = json!(suppressed);
        let encoded = serde_json::to_string(&payload).map_err(|_| ())?;
        db.execute(
            "UPDATE outbox SET payload=? WHERE id=?",
            params![encoded, id],
        )
        .map_err(|_| ())?;
        payload
    } else {
        let mut payload = payload;
        let dropped_count = dropped.swap(0, Ordering::Relaxed);
        payload.details["queue_suppressed_count"] = json!(dropped_count);
        match admit(db, payload, now, Some(id))? {
            Some(payload) => payload,
            None => {
                db.execute("DELETE FROM outbox WHERE id=?", [id])
                    .map_err(|_| ())?;
                return Ok(true);
            }
        }
    };

    match send_admitted(db, e, &payload, now, t, retry, Some(id)) {
        Ok(status) if (200..300).contains(&status) => {
            recovered(dir);
        }
        Ok(status) if (400..500).contains(&status) && status != 429 => {
            // Permanent receiver rejection matches the existing delivery policy.
            db.execute("DELETE FROM outbox WHERE id=?", [id])
                .map_err(|_| ())?;
            failure(dir);
        }
        Ok(_) | Err(()) => {
            let shift = attempts.min(6);
            let delay = (5i64 * (1i64 << shift)).min(300);
            db.execute(
                "UPDATE outbox SET attempts=attempts+1,next_attempt_at=? WHERE id=?",
                params![now.saturating_add(delay), id],
            )
            .map_err(|_| ())?;
            failure(dir);
        }
    }
    Ok(true)
}

fn drain_outbox_one(dir: &Path, e: &Endpoint, dropped: &AtomicU64) -> Result<bool> {
    locked(dir, |db| {
        process_one(dir, db, e, now().timestamp(), &mut Https, dropped, true)
    })
}
pub struct Alerts {
    sender: Option<SyncSender<()>>,
    spool: Option<PathBuf>,
    dropped: Arc<AtomicU64>,
    worker: Option<std::thread::JoinHandle<()>>,
    clean: Arc<AtomicBool>,
}
impl Alerts {
    pub fn start(path: Option<&Path>, controller: bool) -> Self {
        let dropped = Arc::new(AtomicU64::new(0));
        let clean = Arc::new(AtomicBool::new(false));
        let mut result = Self {
            sender: None,
            spool: None,
            dropped: dropped.clone(),
            worker: None,
            clean: clean.clone(),
        };
        let Ok(Some(path)) = destination(path) else {
            return result;
        };
        let Ok(dir) = directory() else {
            return result;
        };
        let e = match endpoint(&path) {
            Ok(e) => e,
            Err(()) => {
                failure(&dir);
                return result;
            }
        };
        result.spool = Some(dir.clone());
        let (tx, rx) = mpsc::sync_channel::<()>(64);
        let failure_dir = dir.clone();
        if let Ok(worker) = std::thread::Builder::new()
            .name("alert-sender".into())
            .spawn(move || {
                if controller {
                    let startup = locked(&dir, |db| {
                        let now = now().timestamp();
                        if let Some(marker) = planned_restart(dir.parent().unwrap_or(&dir), now) {
                            let payload = Payload::new(Event::PlannedRestart, None, json!({}))
                                .with_restart(Some(marker));
                            enqueue(db, &payload, now)?;
                            return Ok(());
                        }
                        db.execute("DELETE FROM starts WHERE at < ?", [now - 1800])
                            .map_err(|_| ())?;
                        db.execute("INSERT INTO starts VALUES(?)", [now])
                            .map_err(|_| ())?;
                        let count: i64 = db
                            .query_row("SELECT count(*) FROM starts", [], |r| r.get(0))
                            .map_err(|_| ())?;
                        if count >= 3 {
                            let payload = Payload::new(
                                Event::RestartLoop,
                                None,
                                json!({"unclean_starts":count}),
                            )
                            .with_restart(planned_restart(dir.parent().unwrap_or(&dir), now));
                            enqueue(db, &payload, now)?;
                        }
                        Ok(())
                    });
                    if startup.is_err() {
                        failure(&dir);
                    }
                }
                loop {
                    if clean.load(Ordering::Relaxed) {
                        break;
                    }
                    match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {
                            if clean.load(Ordering::Relaxed) {
                                break;
                            }
                            if drain_outbox_one(&dir, &e, &dropped).is_err() {
                                failure(&dir);
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                if controller && clean.load(Ordering::Relaxed) {
                    let _ = locked(&dir, |db| {
                        db.execute("DELETE FROM starts", [])
                            .map(|_| ())
                            .map_err(|_| ())
                    });
                }
            })
        {
            result.sender = Some(tx);
            result.worker = Some(worker);
        } else {
            failure(&failure_dir);
        }
        result
    }
    pub fn finish(mut self) {
        self.clean.store(true, Ordering::Relaxed);
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    pub fn emit(&self, event: Event, session: chrono::NaiveDate, details: Value) {
        if let Some(dir) = &self.spool {
            let at = now().timestamp();
            let payload = Payload::new(event, Some(session), details)
                .with_restart(planned_restart(dir.parent().unwrap_or(dir), at));
            let queued = database(dir).and_then(|mut db| enqueue(&mut db, &payload, at));
            if queued.is_err() {
                failure(dir);
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            // The wake channel is only a hint. A full channel loses no payload:
            // every admitted alert is already durable in the SQLite outbox.
            if let Some(tx) = &self.sender {
                let _ = tx.try_send(());
            }
        }
    }
}
pub fn entry(config: Option<&Path>, test: bool, event: Option<Event>) -> ExitCode {
    let result = (|| {
        let settings: Value = match config {
            Some(p) => serde_json::from_slice(&crate::read_bounded(p, 16384)?).map_err(|_| ())?,
            None => json!({}),
        };
        let explicit = settings
            .get("alert_webhook_file")
            .and_then(Value::as_str)
            .map(Path::new);
        let path = destination(explicit)?.ok_or(())?;
        let dir = directory()?;
        let e = endpoint(&path).inspect_err(|_| failure(&dir))?;
        let event = if test { Event::Test } else { event.ok_or(())? };
        let session = settings
            .get("session")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok());
        let mut p = Payload::new(event, session, json!({})).with_restart(planned_restart(
            dir.parent().unwrap_or(&dir),
            now().timestamp(),
        ));
        // Explicit test is one HTTP attempt and bypasses event dedupe, still globally limited.
        if test {
            p.dedupe_key = format!("test:{}", now().timestamp_nanos_opt().unwrap_or(0));
        }
        let status = locked(&dir, |db| {
            deliver(db, &e, p, now().timestamp(), &mut Https, !test)
        })
        .inspect_err(|_| failure(&dir))?;
        if status != 0 && !(200..300).contains(&status) {
            failure(&dir);
        }
        Ok(status)
    })();
    match result {
        Ok(0) => {
            println!("suppressed_by_delivery_policy");
            ExitCode::SUCCESS
        }
        Ok(status @ 200..=299) => {
            println!("delivered {status}");
            ExitCode::SUCCESS
        }
        Ok(status) => {
            println!("failed {status}");
            ExitCode::FAILURE
        }
        Err(()) => {
            println!("failed 0");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
