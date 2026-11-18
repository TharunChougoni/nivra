//! Paper-only runtime. No broker order APIs or model credentials exist here.
pub mod activity_attribution;
pub(crate) mod agent;
mod agent_cli;
pub mod closed_incident;
mod data;
mod decision_evidence;
mod decision_owner;
mod directives;
mod discovery;
mod engine;
pub mod exit_backtest;
pub(crate) mod experiment_ledger;
mod experiment_profile;
pub(crate) mod harness;
pub mod history_download;
pub(crate) mod host_migrate;
pub mod instrument_display;
mod lane_queue;
mod market_feed;
mod model_admission;
mod model_history;
pub(crate) mod model_select;
pub(crate) mod model_selection;
pub(crate) mod operational_health;
mod options;
pub(crate) mod orchestrator_model;
mod owner_tasks;
pub mod paper_replay;
pub(crate) mod phases;
pub mod planner;
mod postclose;
pub mod preparation;
pub mod readiness;
mod redecision;
pub mod research_dashboard;
pub mod retention;
pub mod rollover;
pub(crate) mod scorer_trigger;
mod session;
pub(crate) mod setup;
mod strategy_results;
pub mod subscription_plan;
mod supervisor;
mod transcript;
mod web;
mod web_tools;
mod wide;
mod workspace;
use chrono::{DateTime, FixedOffset, NaiveDate, Timelike, Utc};
use engine::{Book, Quote};
use fs2::FileExt;
use nivra_cli::dashboard_host::DashboardPublicHost;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt, fs,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
/// This binary has no live order path. Resume and dashboard both read this flag.
pub(crate) const LIVE_ENABLED: bool = false;
/// Non-secret failure code. Display never includes paths, tokens or response bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fail(String);
impl Fail {
    fn new(code: impl Into<String>) -> Self {
        Self(code.into())
    }
}
impl From<&'static str> for Fail {
    fn from(code: &'static str) -> Self {
        Self(code.to_owned())
    }
}
impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Fail {}
fn failure_code(err: &(dyn std::error::Error + 'static)) -> String {
    if let Some(fail) = err.downcast_ref::<Fail>() {
        return fail.0.clone();
    }
    if let Some(journal) = err.downcast_ref::<nivra_store::journal::JournalError>() {
        return journal.to_string();
    }
    // String errors in this runtime are static codes. Admit only code-shaped
    // strings, never paths, provider bodies or arbitrary error messages.
    let code = err.to_string();
    if !code.is_empty()
        && code.len() <= 160
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return code;
    }
    "paper_runtime_unavailable".into()
}

mod config;
pub use config::*;

pub fn millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as i64
}
fn local(now: i64) -> DateTime<FixedOffset> {
    DateTime::<Utc>::from_timestamp_millis(now)
        .expect("clock range")
        .with_timezone(&FixedOffset::east_opt(19800).expect("IST"))
}
fn minute(now: i64) -> u32 {
    let t = local(now);
    t.hour() * 60 + t.minute()
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn private(path: &Path) -> Result<String> {
    let mut f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Fail::new("private_file_unreadable"))?;
    let m = f
        .metadata()
        .map_err(|_| Fail::new("private_file_unreadable"))?;
    if !m.is_file()
        || m.mode() & 0o777 != 0o600
        || m.nlink() != 1
        || m.len() > 32768
        || m.uid()
            != fs::metadata("/proc/self")
                .map_err(|_| Fail::new("private_file_unreadable"))?
                .uid()
    {
        return Err(Fail::new("private_file_required").into());
    }
    let mut s = String::new();
    f.read_to_string(&mut s)
        .map_err(|_| Fail::new("private_file_unreadable"))?;
    Ok(s)
}
fn token_file_parent(path: &Path) -> Result<(fs::File, std::ffi::OsString)> {
    use std::path::Component;

    let name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or("token_file_path_invalid")?
        .to_os_string();
    let parent_path = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent_path = parent_path.unwrap_or_else(|| Path::new("."));
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut current = fs::File::from(
        rustix::fs::open(
            if parent_path.is_absolute() { "/" } else { "." },
            flags,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| "token_directory_failed")?,
    );

    for component in parent_path.components() {
        let component = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::ParentDir => std::ffi::OsStr::new(".."),
            Component::Normal(name) => name,
            Component::Prefix(_) => return Err("token_file_path_invalid".into()),
        };
        let next = rustix::fs::openat(&current, component, flags, rustix::fs::Mode::empty());
        let next = match next {
            Ok(fd) => fd,
            Err(error) if error == rustix::io::Errno::NOENT => {
                match rustix::fs::mkdirat(
                    &current,
                    component,
                    rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
                ) {
                    Ok(()) => {}
                    Err(error) if error == rustix::io::Errno::EXIST => {}
                    Err(_) => return Err("token_directory_failed".into()),
                }
                rustix::fs::openat(&current, component, flags, rustix::fs::Mode::empty())
                    .map_err(|_| "token_directory_failed")?
            }
            Err(_) => return Err("token_directory_failed".into()),
        };
        current = fs::File::from(next);
    }

    let metadata = current.metadata().map_err(|_| "token_directory_failed")?;
    let owner = fs::metadata("/proc/self")
        .map_err(|_| "token_directory_failed")?
        .uid();
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
        return Err("token_directory_not_private".into());
    }
    Ok((current, name))
}
fn book_binding(c: &Config) -> Result<String> {
    Ok(digest(&serde_json::to_vec(&rollover::original_config(c)?)?))
}
// Recognize only an otherwise identical existing binding with a different
// recorded cost policy. This is a refusal diagnostic, never a migration.
fn cost_policy_only_change(c: &Config, db: &Connection, binding: &str) -> bool {
    let body: String = match db.query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0)) {
        Ok(body) => body,
        Err(_) => return false,
    };
    let p: nivra_core::execution::Portfolio = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(_) => return false,
    };
    if p.costs == c.paper_costs {
        return false;
    }
    let mut original = c.clone();
    original.paper_costs = p.costs;
    book_binding(&original).is_ok_and(|candidate| candidate == binding)
}
fn open_book(c: &Config) -> Result<Connection> {
    let path = c.directory.join("paper.sqlite");
    let anchor = c.directory.join("book-identity.json");
    let binding = book_binding(c)?;
    if anchor.exists() {
        let original: serde_json::Value = serde_json::from_slice(&fs::read(&anchor)?)?;
        let m = fs::symlink_metadata(&path)?;
        if !m.is_file() || original["device"] != m.dev() || original["inode"] != m.ino() {
            return Err(Fail::new("original_paper_book_changed").into());
        }
        if original["binding"] != binding {
            if let Some(old) = original["binding"].as_str()
                && let Ok(db) = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                && cost_policy_only_change(c, &db, old)
            {
                return Err(Fail::new("paper_cost_policy_changed").into());
            }
            return Err(Fail::new("original_paper_book_changed").into());
        }
    } else if path.exists() {
        return Err(Fail::new("unbound_existing_paper_book").into());
    }
    let db = Connection::open(&path)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
        CREATE TABLE IF NOT EXISTS state(id INTEGER PRIMARY KEY CHECK(id=1), binding TEXT NOT NULL, body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS ticks(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, receipt INTEGER, body TEXT, event TEXT NOT NULL);")?;
    if !anchor.exists() {
        let m = fs::metadata(&path)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&anchor)?;
        std::io::Write::write_all(
            &mut file,
            &serde_json::to_vec(
                &serde_json::json!({"device":m.dev(),"inode":m.ino(),"binding":binding}),
            )?,
        )?;
        file.sync_all()?;
        fs::File::open(&c.directory)?.sync_all()?;
    }
    Ok(db)
}

/// Read a CLOSED predecessor SQLite file without creating or touching its
/// `-wal`/`-shm` sidecars. `immutable=1` is only safe for a checkpointed file;
/// a non-empty WAL is an explicit refusal, never a stale read.
pub(crate) fn open_sqlite_immutable(path: &Path, wal_code: &'static str) -> Result<Connection> {
    // A non-empty WAL OR SHM sidecar means the closed file may not be
    // checkpointed. Refuse conservatively; never read a stale snapshot, create
    // a sidecar, delete one or repair the source.
    for suffix in ["-wal", "-shm"] {
        let sidecar = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        if let Ok(metadata) = fs::symlink_metadata(&sidecar)
            && metadata.len() > 0
        {
            return Err(Fail::new(wal_code).into());
        }
    }
    let uri = format!("file:{}?immutable=1", path.display());
    let db = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    db.busy_timeout(std::time::Duration::ZERO)?;
    Ok(db)
}

/// Read a CLOSED predecessor paper book without creating or touching its
/// `-wal`/`-shm` sidecars. `immutable=1` is only safe for a checkpointed book;
/// a non-empty WAL is an explicit refusal, never a stale read.
pub(crate) fn open_book_immutable(path: &Path) -> Result<Connection> {
    open_sqlite_immutable(path, "source_book_wal_uncheckpointed")
}

/// Read a CLOSED predecessor research queue immutably. No lock file, schema,
/// row or sidecar is created or touched.
pub(crate) fn open_queue_immutable(path: &Path) -> Result<Connection> {
    open_sqlite_immutable(path, "research_queue_wal_uncheckpointed")
}

/// Read a CLOSED predecessor lifecycle ledger immutably.
pub(crate) fn open_ledger_immutable(path: &Path) -> Result<Connection> {
    open_sqlite_immutable(path, "source_lifecycle_wal_uncheckpointed")
}

/// Read the CURRENT live paper book read-only. Unlike a predecessor book this
/// supports a non-empty WAL (the controller may hold it open), but it still
/// never creates a schema, row, plan or order and never applies a quote.
pub(crate) fn open_book_current_read_only(path: &Path) -> Result<Connection> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(std::time::Duration::ZERO)?;
    db.pragma_update(None, "query_only", true)?;
    Ok(db)
}

pub(crate) use rollover::BookAdmission;
pub(crate) fn update_book(path: &Path) -> Result<BookAdmission> {
    rollover::update_book(path)
}
pub(crate) fn market_closed(path: &Path, now: i64) -> Result<bool> {
    rollover::market_closed(path, now)
}
pub(crate) fn stopped_book_safe(path: &Path) -> Result<bool> {
    rollover::stopped_book_safe(path)
}

#[cfg(test)]
fn tick(
    db: &mut Connection,
    c: &Config,
    b: &mut Book,
    q: Option<&Quote>,
    raw: Option<&str>,
    clock: impl FnOnce() -> i64,
) -> Result<()> {
    // Take the write lock BEFORE the financial clock sample. No network, model
    // or pending SQLite read snapshot can age a quote between lock and decision.
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let now = clock();
    let mut next = b.clone();
    let event = next.step(now, q, &c.strategy)?;
    next.reconcile()?;
    tx.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,?,?,?)",
        rusqlite::params![now, q.map(|q| q.receipt), raw, event],
    )?;
    tx.execute(
        "INSERT INTO state VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
        rusqlite::params![book_binding(c)?, serde_json::to_string(&next)?],
    )?;
    tx.commit()?;
    *b = next;
    Ok(())
}
#[cfg(test)]
fn load(db: &Connection, c: &Config) -> Result<Book> {
    let binding = book_binding(c)?;
    let mut stmt = db.prepare("SELECT binding,body FROM state WHERE id=1")?;
    let mut rows = stmt.query([])?;
    match rows.next()? {
        Some(r) => {
            let old: String = r.get(0)?;
            if old != binding {
                return Err("book_binding_changed".into());
            }
            let b: Book = serde_json::from_str(&r.get::<_, String>(1)?)?;
            b.reconcile()?;
            Ok(b)
        }
        None => Ok(Book::new(c.session)),
    }
}
#[cfg(test)]
fn save(
    db: &mut Connection,
    c: &Config,
    b: &Book,
    at: i64,
    raw: Option<&str>,
    receipt: Option<i64>,
    event: &str,
) -> Result<()> {
    b.reconcile()?;
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,?,?,?)",
        rusqlite::params![at, receipt, raw, event],
    )?;
    tx.execute(
        "INSERT INTO state VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
        rusqlite::params![book_binding(c)?, serde_json::to_string(b)?],
    )?;
    tx.commit()?;
    Ok(())
}
pub fn entry(path: &Path, probe: bool) -> ExitCode {
    // Never propagate provider/credential errors or response bodies to logs.
    match run(path, probe) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let reason = failure_code(error.as_ref());
            if probe {
                println!(
                    "{}",
                    serde_json::json!({
                        "read_only_http_ok": false,
                        "executable_depth": false,
                        "secrets_emitted": false,
                        "read_failure_reason": reason,
                    })
                );
            } else {
                if let Ok(bytes) = crate::read_bounded(path, 16384)
                    && let Ok(c) = serde_json::from_slice::<Config>(&bytes)
                {
                    let alerts =
                        crate::alert::Alerts::start(c.alert_webhook_file.as_deref(), false);
                    let closed = rollover::current(&c)
                        .and_then(|c| rollover::review(&c))
                        .ok()
                        .and_then(|v| v["closed"].as_bool());
                    alerts.emit(
                        crate::alert::Event::Stop,
                        c.session,
                        serde_json::json!({"operator_review_required":closed != Some(true),"closed":closed}),
                    );
                    alerts.finish();
                }
                eprintln!("paper_runtime_unavailable: {reason}");
            }
            ExitCode::FAILURE
        }
    }
}
pub fn planner_recover_entry(path: &Path, protection_only: bool, confirm: bool) -> ExitCode {
    let result = (|| -> Result<()> {
        let bytes = fs::read(path).map_err(|_| "config_unreadable")?;
        let c: Config = serde_json::from_slice(&bytes).map_err(|_| "config_invalid")?;
        if protection_only {
            wide::planner_recover_protection_only(&c, confirm)
        } else {
            wide::planner_recover(&c, confirm)
        }
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("planner_recover_refused: {}", failure_code(error.as_ref()));
            ExitCode::FAILURE
        }
    }
}
pub fn config_apply_entry(path: &Path, confirm: bool) -> ExitCode {
    let result = (|| -> Result<()> {
        let bytes = crate::read_bounded(path, 16384).map_err(|_| "config_unreadable")?;
        let c: Config = serde_json::from_slice(&bytes).map_err(|_| "config_invalid")?;
        wide::config_apply(&c, confirm)
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("config_apply_refused: {}", failure_code(error.as_ref()));
            ExitCode::FAILURE
        }
    }
}
pub fn history_recover_entry(
    path: &Path,
    now: Option<DateTime<FixedOffset>>,
    confirm: bool,
) -> ExitCode {
    let result = (|| -> Result<serde_json::Value> {
        let bytes = crate::read_bounded(path, 1_048_576).map_err(|_| "config_unreadable")?;
        let c: Config = serde_json::from_slice(&bytes).map_err(|_| "config_invalid")?;
        wide::history_recover(
            &c,
            now.map(|v| v.timestamp_millis()).unwrap_or_else(millis),
            confirm,
        )
    })();
    match result {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "history_recover_refused_original_state_preserved: {}",
                failure_code(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}
pub fn dashboard_token_rotate_entry(path: &Path, explicit_token: Option<&str>) -> ExitCode {
    let result = (|| -> Result<PathBuf> {
        let bytes = crate::read_bounded(path, 16384).map_err(|_| "config_unreadable")?;
        let c: Config = serde_json::from_slice(&bytes).map_err(|_| "config_invalid")?;
        let token = match explicit_token {
            Some(t) => {
                let trimmed = t.trim();
                if trimmed.len() < 32 {
                    return Err("dashboard_token_too_short".into());
                }
                if trimmed.len() > 1024 || !trimmed.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    return Err("dashboard_token_invalid".into());
                }
                trimmed.to_string()
            }
            None => {
                let mut entropy = [0u8; 32];
                let mut file = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open("/dev/urandom")
                    .map_err(|_| "entropy_unavailable")?;
                file.read_exact(&mut entropy)
                    .map_err(|_| "entropy_read_failed")?;
                entropy
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            }
        };
        let (parent, name) = token_file_parent(&c.dashboard_token_file)?;
        let flags = rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC;
        let fd = match rustix::fs::openat(&parent, &name, flags, rustix::fs::Mode::empty()) {
            Ok(fd) => fd,
            Err(error) if error == rustix::io::Errno::NOENT => rustix::fs::openat(
                &parent,
                &name,
                flags | rustix::fs::OFlags::CREATE | rustix::fs::OFlags::EXCL,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )
            .map_err(|_| "token_file_write_failed")?,
            Err(_) => return Err("token_file_write_failed".into()),
        };
        let mut file = fs::File::from(fd);
        let metadata = file.metadata().map_err(|_| "token_file_write_failed")?;
        let owner = fs::metadata("/proc/self")
            .map_err(|_| "token_file_write_failed")?
            .uid();
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != owner {
            return Err("token_file_not_private".into());
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| "token_file_permissions_failed")?;
        file.set_len(0).map_err(|_| "token_file_write_failed")?;
        use std::io::Write;
        writeln!(file, "{token}").map_err(|_| "token_file_write_failed")?;
        file.sync_all().map_err(|_| "token_file_sync_failed")?;
        Ok(c.dashboard_token_file)
    })();
    match result {
        Ok(token_path) => {
            println!(
                "{}",
                serde_json::json!({
                    "dashboard_token_rotated": true,
                    "token_file": token_path,
                    "restart_required": true,
                    "note": "Dashboard token rotated with mode 0600; controller restart required to adopt new token."
                })
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("dashboard_token_rotate_failed: {err}");
            ExitCode::FAILURE
        }
    }
}
pub fn resume_entry(path: &Path, confirm: bool) -> ExitCode {
    match resume_run(path, confirm) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", failure_code(error.as_ref()));
            ExitCode::FAILURE
        }
    }
}
pub fn stop_entry(path: &Path, confirm: bool) -> ExitCode {
    let result = (|| -> Result<serde_json::Value> {
        let bytes = crate::read_bounded(path, 16384).map_err(|_| "config_unreadable")?;
        let c: Config = serde_json::from_slice(&bytes).map_err(|_| "config_invalid")?;
        wide::stop(&c, confirm, millis())
    })();
    match result {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("stop_refused: {}", failure_code(error.as_ref()));
            ExitCode::FAILURE
        }
    }
}
fn resume_run(path: &Path, confirm: bool) -> Result<()> {
    let bytes = fs::read(path).map_err(|_| Fail::new("config_unreadable"))?;
    let c: Config = serde_json::from_slice(&bytes).map_err(|_| Fail::new("config_invalid"))?;
    wide::resume(&c, confirm)
}
fn run(path: &Path, probe: bool) -> Result<()> {
    let bytes = fs::read(path).map_err(|_| Fail::new("config_unreadable"))?;
    let c: Config = serde_json::from_slice(&bytes).map_err(|_| Fail::new("config_invalid"))?;
    c.validate()?;
    data::verify_journal(&c)?;
    c.calendar().map_err(|error| {
        if error.downcast_ref::<nivra_core::market::MarketError>()
            == Some(&nivra_core::market::MarketError::CalendarCoverageMissing)
        {
            Fail::new("calendar_coverage_missing")
        } else {
            Fail::new("invalid_paper_mandate")
        }
    })?;
    let client = data::Client::new(&c.market_data_transport)
        .map_err(|_| Fail::new("http_client_unavailable"))?;
    if probe {
        let (_, _, raw) = client.quote(&c)?;
        let valid = data::parse(&raw, millis()).is_ok();
        println!(
            "{}",
            serde_json::json!({
                "read_only_http_ok": true,
                "executable_depth": valid,
                "secrets_emitted": false,
                "read_failure_reason": if valid {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::from("quote_not_executable")
                },
            })
        );
        return Ok(());
    }
    wide::run(&rollover::current(&c)?, &client)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod remaining_settings_contract;
