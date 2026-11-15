//! Bootstrap command adapters. Reviewed configuration is public data; errors
//! never include its fields, arbitrary parser messages, artifact bytes or tokens.
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::de::DeserializeOwned;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
    process::ExitCode,
};
const CONFIG_LIMIT: usize = 64 * 1024;
type Result<T> = std::result::Result<T, &'static str>;

fn read_config<T: DeserializeOwned>(path: &Path) -> Result<T> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("bootstrap_cli_absolute_config_path_required");
    }
    let parent = path
        .parent()
        .ok_or("bootstrap_cli_config_filename_required")?;
    let name = path
        .file_name()
        .ok_or("bootstrap_cli_config_filename_required")?;
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open("/")
        .map_err(|_| "bootstrap_cli_config_unavailable")?;
    for part in parent.components() {
        if let Component::Normal(name) = part {
            let anchored =
                PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
            directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(anchored)
                .map_err(|_| "bootstrap_cli_config_parent_invalid")?;
        }
    }
    let anchored = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(anchored)
        .map_err(|_| "bootstrap_cli_config_unavailable")?;
    let before = file
        .metadata()
        .map_err(|_| "bootstrap_cli_config_unavailable")?;
    let owner = fs::metadata("/proc/self")
        .map_err(|_| "bootstrap_cli_owner_unavailable")?
        .uid();
    if !before.is_file()
        || before.nlink() != 1
        || before.uid() != owner
        || before.mode() & 0o022 != 0
    {
        return Err("bootstrap_cli_owned_unshared_config_required");
    }
    if before.len() > CONFIG_LIMIT as u64 {
        return Err("bootstrap_cli_config_too_large");
    }
    let mut bytes = Vec::new();
    (&file)
        .take(CONFIG_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "bootstrap_cli_config_unavailable")?;
    let after = file
        .metadata()
        .map_err(|_| "bootstrap_cli_config_unavailable")?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || before.mode() != after.mode()
        || before.nlink() != after.nlink()
    {
        return Err("bootstrap_cli_config_changed");
    }
    if bytes.len() > CONFIG_LIMIT {
        return Err("bootstrap_cli_config_too_large");
    }
    // Deserialize directly into deny_unknown_fields structs: a Value intermediary
    // would erase duplicate keys and weaken the reviewed-input boundary.
    serde_json::from_slice(&bytes).map_err(|_| "bootstrap_cli_invalid_config")
}
fn reviewed_utc(text: &str) -> Result<DateTime<Utc>> {
    if text.len() > 64 || !(text.ends_with('Z') || text.ends_with("+00:00")) {
        return Err("bootstrap_cli_invalid_reviewed_utc");
    }
    let without_offset = text
        .strip_suffix('Z')
        .or_else(|| text.strip_suffix("+00:00"))
        .ok_or("bootstrap_cli_invalid_reviewed_utc")?;
    if let Some((_, fraction)) = without_offset.split_once('.')
        && (fraction.is_empty()
            || fraction.len() > 9
            || !fraction.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err("bootstrap_cli_invalid_reviewed_utc");
    }
    let timestamp =
        DateTime::parse_from_rfc3339(text).map_err(|_| "bootstrap_cli_invalid_reviewed_utc")?;
    if !timestamp.year_ce().0
        || timestamp.year_ce().1 > 9999
        || timestamp.nanosecond() >= 1_000_000_000
    {
        return Err("bootstrap_cli_invalid_reviewed_utc");
    }
    Ok(timestamp.with_timezone(&Utc))
}
fn stage_reply(staged: &crate::bootstrap_stage::Staged) -> Result<Vec<u8>> {
    let mut reply = serde_json::to_vec(&staged.installer_config())
        .map_err(|_| "bootstrap_cli_serialization_failed")?;
    reply.push(b'\n');
    Ok(reply)
}
/// Off-host staging only. Successful stdout contains exactly one installer
/// configuration JSON object. All failures use static redacted stderr messages.
pub fn stage_entry(config_path: &Path) -> ExitCode {
    let result = (|| -> Result<()> {
        let config: crate::bootstrap_stage::Config = read_config(config_path)?;
        let staged = crate::bootstrap_stage::stage(&config)?;
        let reply = stage_reply(&staged)?;
        std::io::stdout()
            .lock()
            .write_all(&reply)
            .map_err(|_| "bootstrap_cli_output_failed")
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
/// Explicit installation-only invocation. Timestamp and strict input admission
/// occur before constructing the local host adapter. No session is activated.
pub fn install_entry(config_path: &Path, reviewed_utc_text: &str) -> ExitCode {
    let result = (|| -> Result<()> {
        let now = reviewed_utc(reviewed_utc_text)?;
        let config: crate::bootstrap::Config = read_config(config_path)?;
        crate::bootstrap::install_local(&config, now)?;
        std::io::stdout()
            .lock()
            .write_all(b"bootstrap_installed_no_activation\n")
            .map_err(|_| "bootstrap_cli_output_failed")
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
