//! Reviewed prebuilt initial provisioning only. Installation never activates a session.
//!
//! The historical shell killed pristine pre-session v0.4.1/v0.4.2 workers. That
//! dated exception is deliberately not carried forward: any current pointer,
//! running process, unknown state or existing book requires operator review.
use crate::lock::AdvisoryLock;
use chrono::{DateTime, FixedOffset, Timelike, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

pub type Result<T> = std::result::Result<T, &'static str>;
const ALERT_UNIT: &[u8] = include_bytes!("../../../packaging/rust/ind-sadr-alert@.service");
const UNIT: &[u8] = include_bytes!("../../../packaging/rust/ind-sadr-rust-paper.service");
const FILES: [&str; 5] = [
    "sadr-rs",
    "VERSION",
    "SOURCE_COMMIT",
    "paper-20261016.json",
    "ind-sadr-rust-paper.service",
];

/// Digests are explicit independently reviewed inputs, not trust derived from
/// the artifact's own checksum list. This structure contains no credentials.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub home: PathBuf,
    pub artifact: PathBuf,
    pub version: String,
    pub source_commit: String,
    pub binary_sha256: String,
    pub config_sha256: String,
    pub unit_sha256: String,
}

/// Host must obtain fresh authoritative evidence for all relevant service and
/// process identities. Marker absence alone cannot produce Inactive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inactivity {
    Inactive,
    Active,
    Unknown,
}

/// Narrow system boundary. No method can start/stop/kill/enable a service.
/// The production adapter must use a bounded offline binary self-check and
/// validate the paper config without reading any credential file.
pub trait Host {
    fn trusted_now(&mut self) -> Result<DateTime<Utc>>;
    fn inactivity(&mut self) -> Result<Inactivity>;
    fn verify_binary_and_config(
        &mut self,
        binary: &Path,
        config: &Path,
        version: &str,
    ) -> Result<()>;
    fn random_token(&mut self, bytes: &mut [u8; 32]) -> Result<()>;
    fn daemon_reload(&mut self) -> Result<()>;
}

#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
    pub release: PathBuf,
    pub activation_performed: bool,
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn stable(value: &str) -> bool {
    let Some(value) = value.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !(part.is_empty() || part.len() > 1 && part.starts_with('0'))
                && part.bytes().all(|b| b.is_ascii_digit())
                && part.parse::<u32>().is_ok()
        })
}
fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}
fn regular_read(path: &Path, max: usize) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "bootstrap_file_unavailable")?;
    if !file
        .metadata()
        .map_err(|_| "bootstrap_file_unavailable")?
        .is_file()
    {
        return Err("bootstrap_regular_file_required");
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "bootstrap_read_failed")?;
    if bytes.len() > max {
        return Err("bootstrap_file_too_large");
    }
    Ok(bytes)
}
fn sync(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| "bootstrap_sync_failed")
}
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "bootstrap_immutable_file_exists")?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|_| "bootstrap_permissions_failed")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "bootstrap_write_failed")
}
fn no_symlink_components(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("bootstrap_absolute_normal_path_required");
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part.as_os_str());
        let meta = current
            .symlink_metadata()
            .map_err(|_| "bootstrap_directory_unavailable")?;
        if !meta.is_dir() {
            return Err("bootstrap_symlink_or_non_directory");
        }
    }
    Ok(())
}
fn private_dir(path: &Path) -> Result<()> {
    if !exists(path) {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|_| "bootstrap_directory_create_failed")?;
        sync(path.parent().ok_or("bootstrap_directory_parent_required")?)?;
    }
    let meta = path
        .symlink_metadata()
        .map_err(|_| "bootstrap_directory_unavailable")?;
    let owner = fs::metadata("/proc/self")
        .map_err(|_| "bootstrap_owner_unavailable")?
        .uid();
    if !meta.is_dir() || meta.uid() != owner || meta.mode() & 0o077 != 0 {
        return Err("bootstrap_private_owned_directory_required");
    }
    Ok(())
}
fn guard(config: &Config, host: &mut impl Host) -> Result<()> {
    if exists(&config.home.join("ind-sadr-deploy/HOLD")) {
        return Err("bootstrap_hold");
    }
    let now = host.trusted_now()?;
    let local = now.with_timezone(&FixedOffset::east_opt(19800).ok_or("bootstrap_timezone")?);
    // Preserve shell's after-15:35-only rule (15:35 itself remains blocked).
    if local.hour() * 60 + local.minute() <= 15 * 60 + 35 {
        return Err("bootstrap_blackout");
    }
    if host.inactivity()? != Inactivity::Inactive {
        return Err("bootstrap_active_or_unknown_book");
    }
    Ok(())
}
fn same_or_absent(path: &Path, expected: &[u8]) -> Result<()> {
    if exists(path) && regular_read(path, 64 * 1024)? != expected {
        return Err("bootstrap_original_binding_mismatch");
    }
    Ok(())
}
fn install_once(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    if exists(path) {
        same_or_absent(path, bytes)?;
        return Ok(());
    }
    let staged = path.with_extension("bootstrap-new");
    write_new(&staged, bytes, mode)?;
    // Hard link atomically publishes a complete file and cannot replace a raced
    // existing destination. Keep staging on failure as interruption evidence.
    fs::hard_link(&staged, path).map_err(|_| "bootstrap_publish_failed")?;
    sync(path.parent().ok_or("bootstrap_directory_parent_required")?)?;
    fs::remove_file(&staged).map_err(|_| "bootstrap_stage_cleanup_failed")?;
    sync(path.parent().ok_or("bootstrap_directory_parent_required")?)
}

// Linux descriptor-backed paths keep every writable parent pinned. O_NOFOLLOW
// applies to each child directory as it is opened; publication never traverses
// a replaceable ancestor from the original pathname.
struct PinnedDir {
    file: File,
    logical: PathBuf,
}
impl PinnedDir {
    fn path(&self) -> PathBuf {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
    }
    fn absolute(path: &Path, private: bool) -> Result<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err("bootstrap_absolute_normal_path_required");
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open("/")
            .map_err(|_| "bootstrap_directory_unavailable")?;
        let mut current = Self {
            file,
            logical: PathBuf::from("/"),
        };
        for part in path.components() {
            if let Component::Normal(name) = part {
                current = current.child_existing(name, false)?;
            }
        }
        if private {
            current.check_private()?;
        }
        Ok(current)
    }
    fn child_existing(&self, name: &std::ffi::OsStr, private: bool) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(self.path().join(name))
            .map_err(|_| "bootstrap_symlink_or_non_directory")?;
        let child = Self {
            file,
            logical: self.logical.join(name),
        };
        if private {
            child.check_private()?;
        }
        Ok(child)
    }
    fn child(&self, name: &str) -> Result<Self> {
        self.ensure()?;
        let path = self.path().join(name);
        private_dir(&path)?;
        self.child_existing(std::ffi::OsStr::new(name), true)
    }
    fn check_private(&self) -> Result<()> {
        let meta = self
            .file
            .metadata()
            .map_err(|_| "bootstrap_directory_unavailable")?;
        let owner = fs::metadata("/proc/self")
            .map_err(|_| "bootstrap_owner_unavailable")?
            .uid();
        if meta.uid() != owner || meta.mode() & 0o077 != 0 {
            return Err("bootstrap_private_owned_directory_required");
        }
        Ok(())
    }
    fn ensure(&self) -> Result<()> {
        no_symlink_components(&self.logical).map_err(|_| "bootstrap_namespace_changed")?;
        let original = self
            .logical
            .symlink_metadata()
            .map_err(|_| "bootstrap_namespace_changed")?;
        let pinned = self
            .file
            .metadata()
            .map_err(|_| "bootstrap_namespace_changed")?;
        if original.dev() != pinned.dev() || original.ino() != pinned.ino() {
            return Err("bootstrap_namespace_changed");
        }
        self.check_private()?;
        Ok(())
    }
}
#[derive(PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    size: u64,
    mode: u32,
    owner: u32,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn file_identity(path: &Path) -> Result<Identity> {
    let meta = path
        .symlink_metadata()
        .map_err(|_| "bootstrap_release_changed")?;
    let owner = fs::metadata("/proc/self")
        .map_err(|_| "bootstrap_owner_unavailable")?
        .uid();
    if !meta.is_file() || meta.uid() != owner || meta.nlink() != 1 {
        return Err("bootstrap_original_owned_file_required");
    }
    Ok(Identity {
        device: meta.dev(),
        inode: meta.ino(),
        size: meta.len(),
        mode: meta.mode(),
        owner: meta.uid(),
        links: meta.nlink(),
        modified: (meta.mtime(), meta.mtime_nsec()),
        changed: (meta.ctime(), meta.ctime_nsec()),
    })
}
// A dangling wants/requires link can survive a missing unit and become active
// after installation. Refuse retained enabling links without following them.
fn no_enabling_links(units: &Path) -> Result<()> {
    const TARGET: &str = "ind-sadr-rust-paper.service";
    fn check(path: &Path) -> Result<()> {
        let metadata = path
            .symlink_metadata()
            .map_err(|_| "bootstrap_enablement_unknown")?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(path).map_err(|_| "bootstrap_enablement_unknown")?;
            if path.file_name().is_some_and(|n| n == TARGET)
                || target.file_name().is_some_and(|n| n == TARGET)
            {
                return Err("bootstrap_existing_enablement_requires_review");
            }
        }
        Ok(())
    }
    for entry in fs::read_dir(units).map_err(|_| "bootstrap_enablement_unknown")? {
        let entry = entry.map_err(|_| "bootstrap_enablement_unknown")?;
        check(&entry.path())?;
        let name = entry.file_name();
        if name.as_encoded_bytes().ends_with(b".wants")
            || name.as_encoded_bytes().ends_with(b".requires")
        {
            let meta = entry
                .path()
                .symlink_metadata()
                .map_err(|_| "bootstrap_enablement_unknown")?;
            if !meta.is_dir() {
                return Err("bootstrap_enablement_unknown");
            }
            for link in fs::read_dir(entry.path()).map_err(|_| "bootstrap_enablement_unknown")? {
                let link = link.map_err(|_| "bootstrap_enablement_unknown")?;
                if link.file_name() == TARGET {
                    return Err("bootstrap_existing_enablement_requires_review");
                }
                check(&link.path())?;
            }
        }
    }
    Ok(())
}

/// Initial bootstrap with immutable release preservation. A failed installation
/// leaves its pending record/release for review; no retry, rollback or restart is
/// automatic. Existing data books (even stopped books) are never adopted here.
pub fn install(config: &Config, host: &mut impl Host) -> Result<Installed> {
    if !stable(&config.version)
        || !hex(&config.source_commit, 40)
        || !hex(&config.binary_sha256, 64)
        || !hex(&config.config_sha256, 64)
        || !hex(&config.unit_sha256, 64)
    {
        return Err("bootstrap_invalid_reviewed_identity");
    }
    let home = PinnedDir::absolute(&config.home, true)?;
    let artifact = PinnedDir::absolute(&config.artifact, false)?;
    guard(config, host)?;
    let bytes = FILES
        .iter()
        .map(|name| regular_read(&artifact.path().join(name), 32 * 1024 * 1024))
        .collect::<Result<Vec<_>>>()?;
    if bytes[1] != format!("{}\n", config.version).as_bytes()
        || bytes[2] != format!("{}\n", config.source_commit).as_bytes()
        || digest(&bytes[0]) != config.binary_sha256
        || digest(&bytes[3]) != config.config_sha256
        || digest(&bytes[4]) != config.unit_sha256
        || bytes[4] != UNIT
    {
        return Err("bootstrap_artifact_identity_mismatch");
    }
    let sums = regular_read(&artifact.path().join("SHA256SUMS"), 4096)?;
    let sums = std::str::from_utf8(&sums).map_err(|_| "bootstrap_invalid_checksums")?;
    let mut seen = std::collections::BTreeSet::new();
    for line in sums.lines() {
        let Some((sha, name)) = line.split_once("  ") else {
            return Err("bootstrap_invalid_checksums");
        };
        let Some(index) = FILES.iter().position(|file| *file == name) else {
            return Err("bootstrap_invalid_checksums");
        };
        if !seen.insert(name) || sha != digest(&bytes[index]) {
            return Err("bootstrap_checksum_mismatch");
        }
    }
    if seen.len() != FILES.len() {
        return Err("bootstrap_incomplete_checksums");
    }
    let paper: serde_json::Value =
        serde_json::from_slice(&bytes[3]).map_err(|_| "bootstrap_invalid_paper_config")?;
    let data = config.home.join("ind-sadr-data/rust-paper-20261016");
    let config_dir = config.home.join(".config/ind-sadr");
    if paper["session"]
        .as_str()
        .and_then(|s| s.parse::<chrono::NaiveDate>().ok())
        .is_none()
        || paper["directory"].as_str() != data.to_str()
        || paper["dashboard_token_file"].as_str()
            != config_dir.join("rust-dashboard.token").to_str()
        || paper.get("live_enabled").is_some_and(|v| v != false)
    {
        return Err("bootstrap_paper_binding_mismatch");
    }
    let deploy_dir = home.child("ind-sadr-deploy")?;
    let deploy = deploy_dir.path();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(deploy.join(".lock"))
        .map_err(|_| "bootstrap_lock_unavailable")?;
    let lock_meta = lock.metadata().map_err(|_| "bootstrap_lock_unavailable")?;
    if !lock_meta.is_file()
        || lock_meta.nlink() != 1
        || lock_meta.mode() & 0o022 != 0
        || lock_meta.uid()
            != home
                .file
                .metadata()
                .map_err(|_| "bootstrap_owner_unavailable")?
                .uid()
    {
        return Err("bootstrap_regular_lock_required");
    }
    lock.try_lock_exclusive()
        .map_err(|_| "bootstrap_lock_busy")?;
    let _lock = AdvisoryLock::new(lock);
    if exists(&deploy.join("bootstrap-pending.json"))
        || exists(&home.path().join("ind-sadr-rust-current"))
        || exists(&home.path().join("ind-sadr-rust-current.new"))
        || exists(&data)
    {
        return Err("bootstrap_existing_or_interrupted_install");
    }
    let dot_config = home.child(".config")?;
    let config_dir_handle = dot_config.child("ind-sadr")?;
    let systemd = dot_config.child("systemd")?;
    let units = systemd.child("user")?;
    no_enabling_links(&units.path())?;
    let data_parent = home.child("ind-sadr-data")?;
    let releases = home.child("ind-sadr-rust-releases")?;
    let config_dir = config_dir_handle.path();
    let cfg = config_dir.join("rust-paper.json");
    let unit = units.path().join("ind-sadr-rust-paper.service");
    same_or_absent(&cfg, &bytes[3])?;
    if exists(&cfg) {
        file_identity(&cfg)?;
        let meta = cfg
            .symlink_metadata()
            .map_err(|_| "bootstrap_file_unavailable")?;
        if meta.mode() & 0o077 != 0
            || meta.uid()
                != fs::metadata(&config.home)
                    .map_err(|_| "bootstrap_owner_unavailable")?
                    .uid()
        {
            return Err("bootstrap_private_config_required");
        }
    }
    let alert_unit = units.path().join("ind-sadr-alert@.service");
    same_or_absent(&alert_unit, ALERT_UNIT)?;
    same_or_absent(&unit, &bytes[4])?;
    if exists(&unit) {
        file_identity(&unit)?;
    }
    let token = config_dir.join("rust-dashboard.token");
    if exists(&token) {
        file_identity(&token)?;
        let meta = token
            .symlink_metadata()
            .map_err(|_| "bootstrap_token_unavailable")?;
        if !meta.is_file()
            || meta.mode() & 0o077 != 0
            || meta.uid()
                != fs::metadata(&config.home)
                    .map_err(|_| "bootstrap_owner_unavailable")?
                    .uid()
        {
            return Err("bootstrap_private_token_required");
        }
        // Never read, copy, print or replace an existing credential.
    }
    let retained = [&cfg, &unit, &token]
        .into_iter()
        .map(|path| {
            Ok((
                path,
                if exists(path) {
                    Some(file_identity(path)?)
                } else {
                    None
                },
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let verify_retained = || -> Result<()> {
        for (path, identity) in &retained {
            match identity {
                Some(original) if file_identity(path)? != *original => {
                    return Err("bootstrap_original_binding_changed");
                }
                None if exists(path) => return Err("bootstrap_original_binding_changed"),
                _ => {}
            }
        }
        Ok(())
    };
    let release_name = format!("{}-{}", config.version, config.source_commit);
    let release_logical = releases.logical.join(&release_name);
    let release = releases.path().join(&release_name);
    if exists(&release) {
        return Err("bootstrap_immutable_release_exists");
    }
    if fs::read_dir(releases.path())
        .map_err(|_| "bootstrap_directory_unavailable")?
        .next()
        .is_some()
    {
        return Err("bootstrap_existing_release_history_requires_review");
    }
    guard(config, host)?;
    write_new(
        &deploy.join("bootstrap-pending.json"),
        &serde_json::to_vec(config).map_err(|_| "bootstrap_record_failed")?,
        0o600,
    )?;
    sync(&deploy)?;
    let release_handle = releases.child(&release_name)?;
    let release = release_handle.path();
    for (index, name) in FILES.iter().enumerate() {
        write_new(
            &release.join(name),
            &bytes[index],
            if index == 0 { 0o755 } else { 0o600 },
        )?;
    }
    write_new(&release.join("SHA256SUMS"), sums.as_bytes(), 0o600)?;
    sync(&release)?;
    let identities = FILES
        .iter()
        .map(|name| file_identity(&release.join(name)))
        .collect::<Result<Vec<_>>>()?;
    let sum_identity = file_identity(&release.join("SHA256SUMS"))?;
    // Execute only the copied, checksum-bound immutable release.
    host.verify_binary_and_config(
        &release.join("sadr-rs"),
        &release.join(FILES[3]),
        &config.version,
    )?;
    let verify_release = || -> Result<()> {
        for (index, name) in FILES.iter().enumerate() {
            if file_identity(&release.join(name))? != identities[index]
                || regular_read(&release.join(name), 32 * 1024 * 1024)? != bytes[index]
            {
                return Err("bootstrap_release_changed");
            }
        }
        if file_identity(&release.join("SHA256SUMS"))? != sum_identity
            || regular_read(&release.join("SHA256SUMS"), 4096)? != sums.as_bytes()
        {
            return Err("bootstrap_release_changed");
        }
        Ok(())
    };
    verify_release()?;
    let namespace = [
        &home,
        &deploy_dir,
        &dot_config,
        &config_dir_handle,
        &systemd,
        &units,
        &data_parent,
        &releases,
        &release_handle,
    ];
    for directory in namespace {
        directory.ensure()?;
    }
    verify_retained()?;
    let data_handle = data_parent.child("rust-paper-20261016")?;
    install_once(&cfg, &bytes[3], 0o600)?;
    install_once(&unit, &bytes[4], 0o644)?;
    install_once(&alert_unit, ALERT_UNIT, 0o644)?;
    let cfg_identity = file_identity(&cfg)?;
    let unit_identity = file_identity(&unit)?;
    if !exists(&token) {
        let mut random = [0_u8; 32];
        host.random_token(&mut random)?;
        // Refuse obvious broken injected entropy sources. No token in errors/debug.
        if random.iter().all(|byte| *byte == random[0]) {
            return Err("bootstrap_entropy_failed");
        }
        let value: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let staged_token = token.with_extension("bootstrap-new");
        write_new(&staged_token, value.as_bytes(), 0o600)?;
        fs::hard_link(&staged_token, &token).map_err(|_| "bootstrap_token_publish_failed")?;
        sync(&config_dir)?;
        fs::remove_file(staged_token).map_err(|_| "bootstrap_stage_cleanup_failed")?;
        sync(&config_dir)?;
        random.fill(0);
    }
    let token_identity = file_identity(&token)?;
    let verify_installed = || -> Result<()> {
        data_handle.ensure()?;
        if file_identity(&cfg)? != cfg_identity
            || file_identity(&unit)? != unit_identity
            || file_identity(&token)? != token_identity
            || regular_read(&cfg, 64 * 1024)? != bytes[3]
            || regular_read(&unit, 64 * 1024)? != bytes[4]
        {
            return Err("bootstrap_installed_binding_changed");
        }
        Ok(())
    };
    guard(config, host)?;
    no_enabling_links(&units.path())?;
    for directory in namespace {
        directory.ensure()?;
    }
    verify_release()?;
    verify_installed()?;
    let staged = home.path().join("ind-sadr-rust-current.new");
    let target = PathBuf::from("ind-sadr-rust-releases").join(&release_name);
    std::os::unix::fs::symlink(target, &staged).map_err(|_| "bootstrap_pointer_stage_failed")?;
    // Link publication never overwrites a concurrent current pointer.
    fs::hard_link(&staged, home.path().join("ind-sadr-rust-current"))
        .map_err(|_| "bootstrap_pointer_publish_failed")?;
    sync(&home.path())?;
    fs::remove_file(staged).map_err(|_| "bootstrap_stage_cleanup_failed")?;
    sync(&home.path())?;
    host.daemon_reload()?;
    guard(config, host)?;
    no_enabling_links(&units.path())?;
    for directory in namespace {
        directory.ensure()?;
    }
    verify_release()?;
    verify_installed()?;
    fs::remove_file(deploy.join("bootstrap-pending.json"))
        .map_err(|_| "bootstrap_record_cleanup_failed")?;
    sync(&deploy)?;
    Ok(Installed {
        release: release_logical,
        activation_performed: false,
    })
}

/// Local production adapter. Construction requires a separately reviewed UTC
/// clock observation; every guard additionally requires system NTP sync and a
/// fresh (<=60 seconds) observation. Constructing this value performs no I/O.
pub struct LocalHost {
    home: PathBuf,
    reviewed_utc: DateTime<Utc>,
}
impl LocalHost {
    pub fn new(home: PathBuf, reviewed_utc: DateTime<Utc>) -> Self {
        Self { home, reviewed_utc }
    }
}
struct DiagnosticChild {
    child: std::process::Child,
    cleaned: bool,
}
impl DiagnosticChild {
    fn terminate_group(&mut self) -> Result<()> {
        use std::{
            thread,
            time::{Duration, Instant},
        };
        if self.cleaned {
            return Ok(());
        }
        self.cleaned = true;
        if let Some(pid) = rustix::process::Pid::from_raw(self.child.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.child.kill();
        let start = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Err(_) => return Err("bootstrap_command_cleanup_failed"),
                Ok(None) => {}
            }
            if start.elapsed() > Duration::from_millis(200) {
                return Err("bootstrap_command_cleanup_failed");
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for DiagnosticChild {
    fn drop(&mut self) {
        let _ = self.terminate_group();
    }
}
fn command_with_deadline(
    program: &Path,
    args: &[&str],
    deadline: std::time::Duration,
) -> Result<(i32, Vec<u8>)> {
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    let start = Instant::now();
    let mut child = DiagnosticChild {
        child: Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "bootstrap_command_unavailable")?,
        cleaned: false,
    };
    let mut output = child
        .child
        .stdout
        .take()
        .ok_or("bootstrap_command_capture_failed")?;
    let flags = rustix::fs::fcntl_getfl(&output).map_err(|_| "bootstrap_command_capture_failed")?;
    rustix::fs::fcntl_setfl(&output, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(|_| "bootstrap_command_capture_failed")?;
    let mut bytes = Vec::new();
    let mut exit = None;
    loop {
        // Nonblocking drain and exit polling share one deadline. No reader
        // threads, regular output files or inherited-pipe EOF waits exist.
        loop {
            if start.elapsed() > deadline {
                return Err("bootstrap_command_timeout");
            }
            let mut buffer = [0_u8; 4097];
            let remaining = 4097 - bytes.len();
            match output.read(&mut buffer[..remaining]) {
                Ok(0) => break,
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err("bootstrap_command_capture_failed"),
            }
            if bytes.len() > 4096 {
                return Err("bootstrap_command_output_limit");
            }
        }
        if let Some(status) = exit {
            return Ok((status, bytes));
        }
        if let Some(status) = child
            .child
            .try_wait()
            .map_err(|_| "bootstrap_command_failed")?
        {
            child.terminate_group()?;
            exit = Some(status.code().unwrap_or(-1));
            continue;
        }
        thread::sleep(Duration::from_millis(2));
    }
}
fn command(program: &Path, args: &[&str]) -> Result<(i32, Vec<u8>)> {
    command_with_deadline(program, args, std::time::Duration::from_secs(10))
}
// Only explicit, complete systemd evidence can establish inactivity. Missing
// optional historic units are allowed; a DBus error or incomplete output is not.
fn unit_inactivity(status: i32, bytes: &[u8], target_paper: bool) -> Inactivity {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Inactivity::Unknown;
    };
    let mut properties = std::collections::BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            return Inactivity::Unknown;
        };
        if !["LoadState", "ActiveState", "UnitFileState"].contains(&key)
            || properties.insert(key, value).is_some()
        {
            return Inactivity::Unknown;
        }
    }
    let (Some(load), Some(active), Some(enabled)) = (
        properties.get("LoadState"),
        properties.get("ActiveState"),
        properties.get("UnitFileState"),
    ) else {
        return Inactivity::Unknown;
    };
    if *load == "not-found"
        && *active == "inactive"
        && enabled.is_empty()
        && matches!(status, 0 | 1)
    {
        return Inactivity::Inactive;
    }
    if status != 0 || *load != "loaded" {
        return Inactivity::Unknown;
    }
    if ["active", "activating", "deactivating", "reloading"].contains(active) {
        return Inactivity::Active;
    }
    if *active != "inactive" || (target_paper && *enabled != "disabled") {
        return Inactivity::Unknown;
    }
    Inactivity::Inactive
}
fn wall_now() -> Result<DateTime<Utc>> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "bootstrap_clock_unavailable")?;
    DateTime::from_timestamp(
        i64::try_from(duration.as_secs()).map_err(|_| "bootstrap_clock_unavailable")?,
        duration.subsec_nanos(),
    )
    .ok_or("bootstrap_clock_unavailable")
}
fn runtime_argv(bytes: &[u8]) -> bool {
    let args: Vec<_> = bytes.split(|byte| *byte == 0).collect();
    let executable = args.first().copied().unwrap_or_default();
    let python = executable.windows(6).any(|part| part == b"python");
    let sadr = args
        .iter()
        .any(|arg| arg.windows(4).any(|part| part == b"sadr"));
    let worker = args
        .iter()
        .any(|arg| matches!(*arg, b"paper" | b"dashboard" | b"shadow" | b"controller"));
    sadr && (worker
        || (python
            && [
                b"paper".as_slice(),
                b"dashboard".as_slice(),
                b"shadow".as_slice(),
            ]
            .iter()
            .any(|needle| bytes.windows(needle.len()).any(|part| part == *needle))))
}
/// Offline binding check for the deployed paper config. It accepts the
/// optional session sections added for NSE F&O phases but still refuses
/// genuinely unknown keys; semantic validation stays with the runtime `Config`.
pub fn paper_config_binding(path: &Path, home: &Path) -> Result<()> {
    // Strict original schema; never open token/journal paths. Their reviewed
    // binding is preserved here; actual journal admission belongs to runtime.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Paper {
        account: String,
        session: chrono::NaiveDate,
        previous_session: chrono::NaiveDate,
        calendar_source: String,
        directory: PathBuf,
        token_file: PathBuf,
        dashboard_token_file: PathBuf,
        #[serde(default, rename = "dashboard_public_host")]
        _dashboard_public_host: Option<crate::dashboard_host::DashboardPublicHost>,
        read_journal: PathBuf,
        read_scope: String,
        strategy: String,
        #[serde(default, rename = "capital")]
        _capital: Option<sadr_core::finance::Amount>,
        #[serde(default, rename = "prior_config")]
        _prior_config: Option<PathBuf>,
        paper_costs: sadr_core::finance::FeeArithmetic,
        read_journal_device: u64,
        read_journal_inode: u64,
        /// Optional session sections; bootstrap only binds, runtime validates.
        #[serde(default, rename = "quote_keys")]
        _quote_keys: Option<serde_json::Value>,
        #[serde(default, rename = "paper_costs_by_segment")]
        _paper_costs_by_segment: Option<serde_json::Value>,
        #[serde(default, rename = "index_feeds")]
        _index_feeds: Option<serde_json::Value>,
    }
    let paper: Paper = serde_json::from_slice(&regular_read(path, 64 * 1024)?)
        .map_err(|_| "bootstrap_invalid_paper_config")?;
    paper
        .paper_costs
        .validate()
        .map_err(|_| "paper_costs_invalid")?;
    let private = home.join(".config/ind-sadr");
    if paper.account != "tharun-upstox"
        || paper.previous_session >= paper.session
        || paper.calendar_source != "NSE/CMTR/71775"
        || paper.directory != home.join("ind-sadr-data/rust-paper-20261016")
        || paper.token_file != private.join("upstox_analytics.token")
        || paper.dashboard_token_file != private.join("rust-dashboard.token")
        || paper.read_journal != private.join("upstox-shared-reads.db")
        || paper.read_scope != "upstox-market-tharun-upstox"
        || ![
            "deterministic_momentum_v1",
            "model_planner_v1",
            "model_abstain",
        ]
        .contains(&paper.strategy.as_str())
        || paper.read_journal_device == 0
        || paper.read_journal_inode == 0
    {
        return Err("bootstrap_paper_binding_mismatch");
    }
    Ok(())
}
fn fresh_clock_observation(age: chrono::TimeDelta) -> bool {
    age >= chrono::TimeDelta::zero() && age <= chrono::TimeDelta::seconds(60)
}
impl Host for LocalHost {
    fn trusted_now(&mut self) -> Result<DateTime<Utc>> {
        let (status, body) = command(
            Path::new("/usr/bin/timedatectl"),
            &["show", "--property=NTPSynchronized", "--value"],
        )?;
        let now = wall_now()?;
        let age = now.signed_duration_since(self.reviewed_utc);
        if status != 0 || body != b"yes\n" || !fresh_clock_observation(age) {
            return Err("bootstrap_clock_untrusted");
        }
        Ok(now)
    }
    fn inactivity(&mut self) -> Result<Inactivity> {
        let data = self.home.join("ind-sadr-data/rust-paper-20261016");
        if exists(&data) {
            let meta = data
                .symlink_metadata()
                .map_err(|_| "bootstrap_book_evidence_unavailable")?;
            if !meta.is_dir()
                || fs::read_dir(&data)
                    .map_err(|_| "bootstrap_book_evidence_unavailable")?
                    .next()
                    .is_some()
            {
                return Ok(Inactivity::Unknown);
            }
        }
        for unit in [
            "ind-sadr-rust-paper.service",
            "sadr-paper-dashboard-v3.service",
            "sadr-paper-20261015-r2.service",
            "ind-sadr-update.service",
            "ind-sadr-update.timer",
            "ind-sadr-rs-dashboard.service",
        ] {
            let (status, body) = command(
                Path::new("/usr/bin/systemctl"),
                &[
                    "--user",
                    "show",
                    "--all",
                    unit,
                    "--property=LoadState,ActiveState,UnitFileState",
                ],
            )?;
            let state = unit_inactivity(status, &body, unit == "ind-sadr-rust-paper.service");
            if state != Inactivity::Inactive {
                return Ok(state);
            }
        }
        let owner = fs::metadata("/proc/self")
            .map_err(|_| "bootstrap_process_evidence_unavailable")?
            .uid();
        for entry in fs::read_dir("/proc").map_err(|_| "bootstrap_process_evidence_unavailable")? {
            let entry = entry.map_err(|_| "bootstrap_process_evidence_unavailable")?;
            let name = entry.file_name();
            if !name.as_encoded_bytes().iter().all(u8::is_ascii_digit)
                || name == std::process::id().to_string().as_str()
            {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(_) => return Ok(Inactivity::Unknown),
            };
            if meta.uid() != owner {
                continue;
            }
            let bytes = match regular_read(&entry.path().join("cmdline"), 64 * 1024) {
                Ok(bytes) => bytes,
                Err(_) => return Ok(Inactivity::Unknown),
            };
            if runtime_argv(&bytes) {
                return Ok(Inactivity::Active);
            }
        }
        Ok(Inactivity::Inactive)
    }
    fn verify_binary_and_config(
        &mut self,
        binary: &Path,
        config: &Path,
        version: &str,
    ) -> Result<()> {
        paper_config_binding(config, &self.home)?;
        let (status, bytes) = command(binary, &["self-check", "--expected-version", version])?;
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "bootstrap_self_check_failed")?;
        if status != 0
            || body["self_check"] != "ok"
            || body["live_enabled"] != false
            || body["version"].as_str() != version.strip_prefix('v')
        {
            return Err("bootstrap_self_check_failed");
        }
        Ok(())
    }
    fn random_token(&mut self, bytes: &mut [u8; 32]) -> Result<()> {
        let mut source = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open("/dev/urandom")
            .map_err(|_| "bootstrap_entropy_failed")?;
        source
            .read_exact(bytes)
            .map_err(|_| "bootstrap_entropy_failed")
    }
    fn daemon_reload(&mut self) -> Result<()> {
        if command(
            Path::new("/usr/bin/systemctl"),
            &["--user", "daemon-reload"],
        )?
        .0 != 0
        {
            return Err("bootstrap_reload_failed");
        }
        Ok(())
    }
}

/// Explicit local installation entry point. The reviewed clock instant is an
/// installation guard, never a paper/live session mandate. Performs no launch.
pub fn install_local(config: &Config, reviewed_utc: DateTime<Utc>) -> Result<Installed> {
    install(
        config,
        &mut LocalHost::new(config.home.clone(), reviewed_utc),
    )
}
