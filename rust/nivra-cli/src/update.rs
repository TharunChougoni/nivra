//! Token-free private Git distribution; paper-only release activation.
use chrono::{Datelike, Timelike, Utc};
use fs2::FileExt;
use nivra_cli::lock::AdvisoryLock;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    thread,
    time::{Duration, Instant},
};
const TARGET: &str = "x86_64-unknown-linux-musl";
const ASSET: &str = "nivra-x86_64-unknown-linux-musl";
const DEFAULT_PROMPTS_ASSET: &str = "default-prompts.json";
fn prompt_files() -> Vec<&'static str> {
    include_str!("../../../packaging/rust/prompts/inventory")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}
type Result<T> = std::result::Result<T, &'static str>;
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    tag: String,
    source_commit: String,
    target: String,
    sha256: String,
}
/// The only SSH host alias accepted as release transport. The host migration
/// (`nivra host-migrate`) is the one path that rebinds a retired remote.
const REMOTE_ALIAS: &str = "github-nivra:";
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    alert_webhook_file: Option<PathBuf>,
    remote: String,
    paper_config: PathBuf,
}
fn valid_remote(remote: &str) -> bool {
    remote.starts_with(REMOTE_ALIAS) && !remote.contains(char::is_whitespace)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u32, u32, u32);
impl Version {
    fn parse(s: &str) -> Result<Self> {
        let fields: Vec<_> = s.strip_prefix('v').unwrap_or(s).split('.').collect();
        if fields.len() != 3
            || fields.iter().any(|s| {
                s.is_empty()
                    || (s.len() > 1 && s.starts_with('0'))
                    || !s.bytes().all(|b| b.is_ascii_digit())
            })
        {
            return Err("stable_tag_required");
        }
        let n = |s: &str| s.parse().map_err(|_| "stable_tag_required");
        Ok(Self(n(fields[0])?, n(fields[1])?, n(fields[2])?))
    }
}
#[derive(Clone, Debug, Default)]
pub(crate) struct Selection {
    tag: Option<String>,
    sha: Option<String>,
}
impl Selection {
    fn new(tag: Option<&str>, sha: Option<&str>) -> Result<Self> {
        if tag.is_some() && sha.is_some() {
            return Err("update_select_one_target");
        }
        let tag = tag
            .map(|s| -> Result<String> {
                Version::parse(s)?;
                Ok(format!("v{}", s.strip_prefix('v').unwrap_or(s)))
            })
            .transpose()?;
        if sha.is_some_and(|s| !(7..=40).contains(&s.len()) || !hash(s, s.len())) {
            return Err("update_commit_invalid");
        }
        Ok(Self {
            tag,
            sha: sha.map(str::to_owned),
        })
    }
}
fn select_target(
    catalog: &str,
    running: &str,
    target: &Selection,
) -> Result<Option<(String, String)>> {
    let running = Version::parse(running)?;
    let rows: Vec<_> = catalog.lines().filter_map(|l| l.split_once('\t')).collect();
    let tags = rows
        .iter()
        .filter_map(|(sha, r)| {
            let tag = r.strip_prefix("refs/tags/v")?.strip_suffix("^{}")?;
            let tag = format!("v{tag}");
            let v = Version::parse(&tag).ok()?;
            (hash(sha, 40)
                && rows.iter().any(|(object, name)| {
                    *name == format!("refs/tags/{tag}") && hash(object, 40) && object != sha
                }))
            .then(|| (v, tag, sha.to_string()))
        })
        .collect::<Vec<_>>();
    if let Some(wanted) = &target.sha {
        let commits = tags
            .iter()
            .filter(|(_, _, sha)| sha.starts_with(wanted))
            .map(|(_, _, s)| s)
            .collect::<std::collections::BTreeSet<_>>();
        if commits.len() > 1 {
            return Err("update_commit_ambiguous");
        }
    }
    let chosen = tags
        .into_iter()
        .filter(|(v, tag, sha)| {
            if let Some(wanted) = &target.tag {
                tag == wanted
            } else if let Some(wanted) = &target.sha {
                sha.starts_with(wanted)
            } else {
                *v > running
            }
        })
        .max_by_key(|x| x.0)
        .map(|(_, t, c)| (t, c));
    if chosen.is_none() && (target.tag.is_some() || target.sha.is_some()) {
        return Err("update_annotated_release_not_found");
    }
    Ok(chosen)
}
#[cfg(test)]
fn select(catalog: &str, running: &str) -> Result<Option<(String, String)>> {
    select_target(catalog, running, &Selection::default())
}
/// Release activation admission. A forced admission never bypasses integrity,
/// hold, identity, halt, preflight or rollback guards; it only accepts that an
/// already open paper book is briefly unwatched while the controller restarts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    Safe,
    Forced(&'static str),
    Deferred(&'static str),
}
pub(crate) trait Host {
    fn fetch(&mut self, name: &str, limit: usize) -> Result<Vec<u8>>;
    fn fetch_optional(&mut self, _name: &str, _limit: usize) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn version(&mut self, binary: &Path) -> Result<String>;
    fn stop(&mut self) -> Result<()>;
    fn guard(&mut self, force: bool) -> Result<Admission>;
    fn smoke(&mut self, binary: &Path, tag: &str) -> Result<()>;
    /// The currently installed binary remains the rollback target after the
    /// flip, so prove it can still parse the reviewed paper config first.
    fn rollback_preflight(&mut self, binary: &Path) -> Result<()>;
    /// Record the planned-restart notice before the controller stops, so the
    /// old controller's service-stop alert is already classified as planned.
    fn plan_restart(&mut self, _tag: &str) -> Result<()> {
        Ok(())
    }
    fn restart(&mut self) -> Result<()>;
    fn healthy(&mut self) -> Result<()>;
    /// Injectable for failure-path tests; production always uses the atomic flip.
    fn swap(&mut self, current: &Path, target: &Path) -> Result<()> {
        swap(current, target)
    }
    /// Operator-stopped deploys swap the release and leave the controller stopped.
    fn keep_stopped(&self) -> bool {
        false
    }
}
fn now_utc() -> chrono::DateTime<Utc> {
    chrono::DateTime::from_timestamp_millis(crate::runtime::millis()).expect("system clock range")
}
fn hash(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// Weekday 09:00–15:45:59 IST activation blackout.
pub(crate) fn market_open(now: chrono::DateTime<chrono::FixedOffset>) -> bool {
    now.weekday().number_from_monday() <= 5
        && (9 * 3600..=15 * 3600 + 45 * 60 + 59).contains(&now.num_seconds_from_midnight())
}
#[cfg(test)]
fn window(now: chrono::DateTime<chrono::FixedOffset>, session_safe: bool) -> bool {
    session_safe && !market_open(now)
}
fn digest(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

mod apply;
mod host;
pub(crate) use apply::*;
pub(crate) use host::*;

// Keep both descriptors alive until the update and its status record are complete.
// A contended lock is normal coordination, not an updater failure.
fn with_update_locks(
    home: &Path,
    state: &Path,
    check: bool,
    dry: bool,
    run: impl FnOnce() -> Result<&'static str>,
) -> Result<&'static str> {
    let deploy = home.join("nivra-deploy");
    fs::create_dir_all(&deploy).map_err(|_| "update_directory_unavailable")?;
    let mut locks = Vec::new();
    let mut result = Ok("skipped_lock_busy");
    for path in [state.join("updater.lock"), deploy.join(".lock")] {
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "update_lock_unavailable")?;
        match lock.try_lock_exclusive() {
            Ok(()) => locks.push(AdvisoryLock::new(lock)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                eprintln!(
                    "update_lock_busy: another manual or timer updater owns the deployment lock; skipped"
                );
                break;
            }
            Err(_) => return Err("update_lock_unavailable"),
        }
    }
    if locks.len() == 2 {
        result = run();
    }
    let mut log = serde_json::json!({"status":result.as_ref().copied().unwrap_or_else(|e|e),"at":now_utc().to_rfc3339(),"check":check,"dry_run":dry});
    if let Some(preflight) = preflight_status(state) {
        log["preflight_failure"] = preflight;
    }
    // A skipped invocation must not collide with the active updater's temp file.
    let next = state.join(format!("update.{}.next", std::process::id()));
    write_new(
        &next,
        &serde_json::to_vec(&log).map_err(|_| "update_record_failed")?,
        false,
    )?;
    fs::rename(next, state.join("update.json")).map_err(|_| "update_record_failed")?;
    sync(state)?;
    result
}

#[allow(clippy::too_many_arguments)]
pub fn entry(
    path: &Path,
    check: bool,
    dry: bool,
    now: Option<chrono::DateTime<chrono::FixedOffset>>,
    tag: Option<&str>,
    sha: Option<&str>,
    operator_stopped: bool,
    force: bool,
) -> ExitCode {
    let alert_path = read(path, 16384)
        .ok()
        .and_then(|b| serde_json::from_slice::<Config>(&b).ok())
        .and_then(|c| c.alert_webhook_file);
    let outcome = (|| {
        if now.is_some() && !check && !dry {
            return Err("update_now_requires_preview");
        }
        let selection = Selection::new(tag, sha)?;
        if operator_stopped && selection.tag.is_none() && selection.sha.is_none() {
            return Err("update_operator_stopped_requires_selection");
        }
        let config: Config = serde_json::from_slice(&read(path, 16384)?)
            .map_err(|_| "invalid_update_configuration")?;
        // Only SSH transport: git uses the host's deploy-key alias; no tokens.
        if !valid_remote(&config.remote) {
            return Err("update_ssh_alias_required");
        }
        let home = PathBuf::from(std::env::var("HOME").map_err(|_| "home_unavailable")?);
        if !home.is_absolute() {
            return Err("update_absolute_home_required");
        }
        {
            use std::os::unix::fs::MetadataExt;
            let owner = fs::metadata("/proc/self")
                .map_err(|_| "update_owner_unavailable")?
                .uid();
            if fs::metadata(&home).map_err(|_| "home_unavailable")?.uid() != owner {
                return Err("update_run_as_home_owner");
            }
        }
        let state = home.join(".local/state/nivra");
        fs::create_dir_all(&state).map_err(|_| "update_directory_unavailable")?;
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))
            .map_err(|_| "update_permissions_failed")?;
        secure_directory(&state)?;
        with_update_locks(&home, &state, check, dry, || {
            fs::create_dir_all(home.join("nivra-releases"))
                .map_err(|_| "update_directory_unavailable")?;
            let paper: serde_json::Value =
                serde_json::from_slice(&read(&config.paper_config, 16384)?)
                    .map_err(|_| "invalid_paper_configuration")?;
            let token_file = paper["dashboard_token_file"]
                .as_str()
                .ok_or("invalid_paper_configuration")?
                .into();
            let mut host = RealHost {
                current: home.join("nivra-current"),
                agent: ureq::Agent::config_builder()
                    .timeout_global(Some(Duration::from_secs(10)))
                    .build()
                    .into(),
                token_file,
                remote: config.remote,
                cache: state.join("assets.git"),
                paper: config.paper_config,
                now,
                operator_stopped,
            };
            apply_selected(&home, &mut host, check, dry, force, &selection)
        })
    })();
    let state = std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".local/state/nivra"));
    if let Some(line) = state.as_deref().and_then(preflight_status).map(|p| {
        format!(
            "updater_preflight_failed: version={} reason={} phase={} since={}",
            p["version"].as_str().unwrap_or("unknown"),
            p["reason"].as_str().unwrap_or("unknown"),
            p["phase"].as_str().unwrap_or("unknown"),
            p["since"].as_str().unwrap_or("unknown"),
        )
    }) {
        eprintln!("{line}");
    }
    match outcome {
        Ok(code) => {
            println!("{code}");
            ExitCode::SUCCESS
        }
        Err(code) => {
            let alerts = crate::alert::Alerts::start(alert_path.as_deref(), false);
            alerts.emit(
                crate::alert::Event::UpdaterFailure,
                now_utc()
                    .with_timezone(&chrono::FixedOffset::east_opt(19800).expect("IST"))
                    .date_naive(),
                updater_failure_details(state.as_deref(), code),
            );
            alerts.finish();
            eprintln!("{code}");
            ExitCode::FAILURE
        }
    }
}
#[cfg(test)]
mod tests;
