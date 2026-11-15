//! Off-host immutable staging from the standard release assets to the dated
//! paper bootstrap bundle. Digests are independently reviewed inputs; a manifest
//! or checksum sidecar never creates its own trust. No binary/service is executed.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};
const ASSET: &str = "sadr-rs-x86_64-unknown-linux-musl";
const TARGET: &str = "x86_64-unknown-linux-musl";
const UNIT: &[u8] = include_bytes!("../../../packaging/rust/ind-sadr-rust-paper.service");
pub type Result<T> = std::result::Result<T, &'static str>;

/// Public paths and independently reviewed identities only. install_home is a
/// target binding; staging never reads or writes that home or its credentials.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub release_assets: PathBuf,
    pub paper_config: PathBuf,
    pub service_unit: PathBuf,
    pub output: PathBuf,
    pub install_home: PathBuf,
    /// Explicit post-transfer location on the installer host; never accessed here.
    pub installer_artifact: PathBuf,
    pub version: String,
    pub source_commit: String,
    pub binary_sha256: String,
    pub config_sha256: String,
    pub unit_sha256: String,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Staged {
    pub home: PathBuf,
    pub artifact: PathBuf,
    pub installer_artifact: PathBuf,
    pub version: String,
    pub source_commit: String,
    pub binary_sha256: String,
    pub config_sha256: String,
    pub unit_sha256: String,
    pub activation_performed: bool,
}
impl Staged {
    /// Complete installer input retaining the exact reviewed target binding.
    /// Serialize this separately from progress metadata for CLI handoff.
    pub fn installer_config(&self) -> crate::bootstrap::Config {
        crate::bootstrap::Config {
            home: self.home.clone(),
            artifact: self.installer_artifact.clone(),
            version: self.version.clone(),
            source_commit: self.source_commit.clone(),
            binary_sha256: self.binary_sha256.clone(),
            config_sha256: self.config_sha256.clone(),
            unit_sha256: self.unit_sha256.clone(),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    tag: String,
    source_commit: String,
    target: String,
    sha256: String,
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
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn stable(value: &str) -> bool {
    let Some(value) = value.strip_prefix('v') else {
        return false;
    };
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !(part.is_empty() || part.len() > 1 && part.starts_with('0'))
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && part.parse::<u32>().is_ok()
        })
}
fn absolute_normal(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("bootstrap_stage_absolute_normal_path_required");
    }
    Ok(())
}
struct Directory {
    file: File,
    logical: PathBuf,
}
impl Directory {
    fn path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
    }
    fn open(path: &Path) -> Result<Self> {
        absolute_normal(path)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open("/")
            .map_err(|_| "bootstrap_stage_directory_unavailable")?;
        let mut directory = Self {
            file,
            logical: PathBuf::from("/"),
        };
        for part in path.components() {
            if let Component::Normal(name) = part {
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                    .open(directory.path().join(name))
                    .map_err(|_| "bootstrap_stage_symlink_or_non_directory")?;
                directory = Self {
                    file,
                    logical: directory.logical.join(name),
                };
            }
        }
        Ok(directory)
    }
    fn private(&self) -> Result<()> {
        let meta = self
            .file
            .metadata()
            .map_err(|_| "bootstrap_stage_directory_unavailable")?;
        let owner = fs::metadata("/proc/self")
            .map_err(|_| "bootstrap_stage_owner_unavailable")?
            .uid();
        if meta.uid() != owner || meta.mode() & 0o077 != 0 {
            return Err("bootstrap_stage_private_owned_directory_required");
        }
        Ok(())
    }
    fn unchanged(&self) -> Result<()> {
        let current = Self::open(&self.logical)?;
        let current = current
            .file
            .metadata()
            .map_err(|_| "bootstrap_stage_namespace_changed")?;
        let original = self
            .file
            .metadata()
            .map_err(|_| "bootstrap_stage_namespace_changed")?;
        if current.dev() != original.dev() || current.ino() != original.ino() {
            return Err("bootstrap_stage_namespace_changed");
        }
        Ok(())
    }
}
fn read_at(directory: &Directory, name: &std::ffi::OsStr, limit: usize) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.path().join(name))
        .map_err(|_| "bootstrap_stage_file_unavailable")?;
    let before = file
        .metadata()
        .map_err(|_| "bootstrap_stage_file_unavailable")?;
    if !before.is_file() || before.nlink() != 1 {
        return Err("bootstrap_stage_regular_unshared_file_required");
    }
    let mut bytes = Vec::new();
    (&file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "bootstrap_stage_read_failed")?;
    let after = file
        .metadata()
        .map_err(|_| "bootstrap_stage_file_unavailable")?;
    if before.len() != after.len()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err("bootstrap_stage_input_changed");
    }
    if bytes.len() > limit {
        return Err("bootstrap_stage_file_too_large");
    }
    Ok(bytes)
}
fn read_input(path: &Path, limit: usize) -> Result<Vec<u8>> {
    absolute_normal(path)?;
    let directory = Directory::open(path.parent().ok_or("bootstrap_stage_parent_required")?)?;
    let bytes = read_at(
        &directory,
        path.file_name()
            .ok_or("bootstrap_stage_filename_required")?,
        limit,
    )?;
    directory.unchanged()?;
    Ok(bytes)
}
fn write_new(directory: &Directory, name: &str, bytes: &[u8], mode: u32) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.path().join(name))
        .map_err(|_| "bootstrap_stage_immutable_file_required")?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|_| "bootstrap_stage_permissions_failed")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "bootstrap_stage_write_failed")
}
fn private_output_file(directory: &Directory, name: &str, mode: u32) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.path().join(name))
        .map_err(|_| "bootstrap_stage_output_changed")?;
    let meta = file
        .metadata()
        .map_err(|_| "bootstrap_stage_output_changed")?;
    let owner = fs::metadata("/proc/self")
        .map_err(|_| "bootstrap_stage_owner_unavailable")?
        .uid();
    if !meta.is_file() || meta.nlink() != 1 || meta.uid() != owner || meta.mode() & 0o777 != mode {
        return Err("bootstrap_stage_output_changed");
    }
    Ok(())
}
fn validate_paper(bytes: &[u8], home: &Path) -> Result<()> {
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
        /// Optional session sections; staging only binds, runtime validates.
        #[serde(default, rename = "quote_keys")]
        _quote_keys: Option<serde_json::Value>,
        #[serde(default, rename = "paper_costs_by_segment")]
        _paper_costs_by_segment: Option<serde_json::Value>,
        #[serde(default, rename = "index_feeds")]
        _index_feeds: Option<serde_json::Value>,
    }
    let paper: Paper =
        serde_json::from_slice(bytes).map_err(|_| "bootstrap_stage_invalid_paper_config")?;
    paper
        .paper_costs
        .validate()
        .map_err(|_| "paper_costs_invalid")?;
    let private = home.join(".config/ind-sadr");
    if paper.account != "tharun-upstox"
        || paper.previous_session >= paper.session
        || paper.calendar_source != "NSE/CMTR/71775"
        || paper.read_scope != "upstox-market-tharun-upstox"
        || ![
            "deterministic_momentum_v1",
            "model_planner_v1",
            "model_abstain",
        ]
        .contains(&paper.strategy.as_str())
        || paper.directory != home.join("ind-sadr-data/rust-paper-20261016")
        || paper.token_file != private.join("upstox_analytics.token")
        || paper.dashboard_token_file != private.join("rust-dashboard.token")
        || paper.read_journal != private.join("upstox-shared-reads.db")
        || paper.read_journal_device == 0
        || paper.read_journal_inode == 0
    {
        return Err("bootstrap_stage_paper_binding_mismatch");
    }
    Ok(())
}

// Static ELF envelope inspection is a format gate, not an execution/self-check
// certificate. The installer independently checks the copied binary offline.
fn static_x86_64(bytes: &[u8]) -> bool {
    fn number(bytes: &[u8], offset: usize, size: usize) -> Option<u64> {
        Some(
            bytes
                .get(offset..offset.checked_add(size)?)?
                .iter()
                .enumerate()
                .fold(0, |value, (index, byte)| {
                    value | (u64::from(*byte) << (index * 8))
                }),
        )
    }
    let inspect = || -> Option<bool> {
        if bytes.get(..7)? != b"\x7fELF\x02\x01\x01"
            || !matches!(number(bytes, 16, 2)?, 2 | 3)
            || number(bytes, 18, 2)? != 62
            || number(bytes, 20, 4)? != 1
            || number(bytes, 52, 2)? != 64
        {
            return Some(false);
        }
        let offset = usize::try_from(number(bytes, 32, 8)?).ok()?;
        let size = usize::try_from(number(bytes, 54, 2)?).ok()?;
        let count = usize::try_from(number(bytes, 56, 2)?).ok()?;
        if size != 56 || count == 0 || count > 1024 {
            return Some(false);
        }
        let mut executable_load = false;
        for index in 0..count {
            let at = offset.checked_add(index.checked_mul(size)?)?;
            bytes.get(at..at.checked_add(size)?)?;
            let kind = number(bytes, at, 4)?;
            if kind == 3 {
                return Some(false);
            }
            if kind == 1 || kind == 2 {
                let start = usize::try_from(number(bytes, at.checked_add(8)?, 8)?).ok()?;
                let length = usize::try_from(number(bytes, at.checked_add(32)?, 8)?).ok()?;
                let memory_length = number(bytes, at.checked_add(40)?, 8)?;
                let data = bytes.get(start..start.checked_add(length)?)?;
                if memory_length < length as u64 {
                    return Some(false);
                }
                if kind == 1 && number(bytes, at.checked_add(4)?, 4)? & 1 != 0 && length > 0 {
                    executable_load = true;
                }
                if kind == 2 {
                    if data.len() % 16 != 0 {
                        return Some(false);
                    }
                    let mut terminated = false;
                    for entry in data.chunks_exact(16) {
                        match number(entry, 0, 8)? {
                            0 => {
                                terminated = true;
                                break;
                            }
                            1 => return Some(false),
                            _ => {}
                        }
                    }
                    if !terminated {
                        return Some(false);
                    }
                }
            }
        }
        Some(executable_load)
    };
    inspect().unwrap_or(false)
}

/// Stages six checksum-bound files in an absent private output directory. Any
/// partial output remains immutable on failure; staging never repairs/removes it.
/// The independently reviewed tag/commit must come from the release/tag review,
/// not from trusting release.json. No tags, source checkout or VM are changed.
pub fn stage(config: &Config) -> Result<Staged> {
    if !stable(&config.version)
        || !hex(&config.source_commit, 40)
        || !hex(&config.binary_sha256, 64)
        || !hex(&config.config_sha256, 64)
        || !hex(&config.unit_sha256, 64)
    {
        return Err("bootstrap_stage_invalid_reviewed_identity");
    }
    absolute_normal(&config.install_home)?;
    absolute_normal(&config.installer_artifact)?;
    absolute_normal(&config.output)?;
    if [
        &config.release_assets,
        &config.paper_config,
        &config.service_unit,
        &config.output,
    ]
    .iter()
    .any(|path| path.starts_with(&config.install_home))
    {
        return Err("bootstrap_stage_target_home_must_remain_untouched");
    }
    let assets = Directory::open(&config.release_assets)?;
    let manifest: Manifest = serde_json::from_slice(&read_at(
        &assets,
        std::ffi::OsStr::new("release.json"),
        16384,
    )?)
    .map_err(|_| "bootstrap_stage_invalid_manifest")?;
    if manifest.tag != config.version
        || manifest.source_commit != config.source_commit
        || manifest.target != TARGET
        || manifest.sha256 != config.binary_sha256
    {
        return Err("bootstrap_stage_release_identity_mismatch");
    }
    let binary = read_at(&assets, std::ffi::OsStr::new(ASSET), 32 * 1024 * 1024)?;
    let sidecar = read_at(
        &assets,
        std::ffi::OsStr::new(&format!("{ASSET}.sha256")),
        256,
    )?;
    if digest(&binary) != config.binary_sha256
        || sidecar != format!("{}  {ASSET}\n", config.binary_sha256).as_bytes()
    {
        return Err("bootstrap_stage_binary_checksum_mismatch");
    }
    if !static_x86_64(&binary) {
        return Err("bootstrap_stage_static_x86_64_required");
    }
    let paper = read_input(&config.paper_config, 64 * 1024)?;
    let unit = read_input(&config.service_unit, 16 * 1024)?;
    if digest(&paper) != config.config_sha256 || digest(&unit) != config.unit_sha256 || unit != UNIT
    {
        return Err("bootstrap_stage_reviewed_config_unit_mismatch");
    }
    validate_paper(&paper, &config.install_home)?;
    assets.unchanged()?;
    let parent = Directory::open(
        config
            .output
            .parent()
            .ok_or("bootstrap_stage_parent_required")?,
    )?;
    parent.private()?;
    let name = config
        .output
        .file_name()
        .ok_or("bootstrap_stage_filename_required")?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(parent.path().join(name))
        .map_err(|_| "bootstrap_stage_immutable_output_required")?;
    parent
        .file
        .sync_all()
        .map_err(|_| "bootstrap_stage_sync_failed")?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(parent.path().join(name))
        .map_err(|_| "bootstrap_stage_directory_unavailable")?;
    let output = Directory {
        file,
        logical: config.output.clone(),
    };
    output.private()?;
    let version = format!("{}\n", config.version).into_bytes();
    let revision = format!("{}\n", config.source_commit).into_bytes();
    let files = [
        ("sadr-rs", &binary, 0o755),
        ("VERSION", &version, 0o600),
        ("SOURCE_COMMIT", &revision, 0o600),
        ("paper-20261016.json", &paper, 0o600),
        ("ind-sadr-rust-paper.service", &unit, 0o600),
    ];
    let mut sums = String::new();
    for (name, bytes, mode) in files {
        write_new(&output, name, bytes, mode)?;
        sums.push_str(&format!("{}  {name}\n", digest(bytes)));
    }
    // Completion marker is published last. Installers require the complete list.
    write_new(&output, "SHA256SUMS", sums.as_bytes(), 0o600)?;
    output
        .file
        .sync_all()
        .map_err(|_| "bootstrap_stage_sync_failed")?;
    parent.unchanged()?;
    parent.private()?;
    output.unchanged()?;
    output.private()?;
    for (name, bytes, mode) in files {
        private_output_file(&output, name, mode)?;
        if read_at(&output, std::ffi::OsStr::new(name), 32 * 1024 * 1024)? != *bytes {
            return Err("bootstrap_stage_output_changed");
        }
    }
    private_output_file(&output, "SHA256SUMS", 0o600)?;
    if read_at(&output, std::ffi::OsStr::new("SHA256SUMS"), 4096)? != sums.as_bytes() {
        return Err("bootstrap_stage_output_changed");
    }
    parent.private()?;
    output.private()?;
    Ok(Staged {
        home: config.install_home.clone(),
        artifact: config.output.clone(),
        installer_artifact: config.installer_artifact.clone(),
        version: config.version.clone(),
        source_commit: config.source_commit.clone(),
        binary_sha256: config.binary_sha256.clone(),
        config_sha256: config.config_sha256.clone(),
        unit_sha256: config.unit_sha256.clone(),
        activation_performed: false,
    })
}
