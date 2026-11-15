//! Original file, account, owner, and prepared-plan binding.
//!
//! A failed bind returns before any reservation or fill. The ledger row `id=1`
//! is not account identity. Releasing the flock does not reconcile a book and
//! does not permit a restart. This module does not chmod, repair, unlink, or
//! truncate the lock file, and it does not grant trading or installation authority.
//! Callers supply synthetic paths only. No credential, socket, or provider is used.

use crate::writer::{Effect, Fill, LedgerWriter, Reservation, WriteError};
use rusqlite::{Connection, OptionalExtension};
use std::{
    error::Error,
    fmt, fs,
    fs::OpenOptions,
    io::ErrorKind,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentity {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginalIdentity {
    pub files: Vec<FileIdentity>,
    pub lock: FileIdentity,
    pub ledger: PathBuf,
    pub account: String,
    pub plan_hash: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BindError {
    Symlink,
    UnsafeMode,
    LinkCount,
    Replaced,
    OrphanSidecar,
    UnsafeSidecar,
    AttachedDatabase,
    AccountMismatch,
    Contended,
    PlanMismatch,
    Unavailable,
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Symlink => "original_file_symlink",
            Self::UnsafeMode => "original_file_mode",
            Self::LinkCount => "original_file_link_count",
            Self::Replaced => "original_file_replaced",
            Self::OrphanSidecar => "original_sidecar_orphan",
            Self::UnsafeSidecar => "original_sidecar_unsafe",
            Self::AttachedDatabase => "original_database_attached",
            Self::AccountMismatch => "original_account_mismatch",
            Self::Contended => "original_owner_contended",
            Self::PlanMismatch => "prepared_plan_mismatch",
            Self::Unavailable => "original_binding_unavailable",
        })
    }
}

impl Error for BindError {}

#[derive(Debug, PartialEq, Eq)]
pub enum GateError {
    Bind(BindError),
    Write(WriteError),
}

impl fmt::Display for GateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(error) => write!(f, "{error}"),
            Self::Write(error) => write!(f, "{error}"),
        }
    }
}

impl Error for GateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Bind(error) => Some(error),
            Self::Write(error) => Some(error),
        }
    }
}

impl From<BindError> for GateError {
    fn from(error: BindError) -> Self {
        Self::Bind(error)
    }
}

/// Cooperative owner of the caller-supplied lock file.
///
/// Dropping the lease closes that descriptor and releases the flock. It does
/// not chmod, repair, unlink, or truncate the lock file.
pub struct Lease {
    file: fs::File,
    account: String,
    plan_hash: String,
}

/// Result of releasing a lease. Both flags stay false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    reconciled: bool,
    restart_permitted: bool,
}

impl Release {
    pub fn reconciled(self) -> bool {
        self.reconciled
    }

    pub fn restart_permitted(self) -> bool {
        self.restart_permitted
    }
}

impl fmt::Debug for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lease")
            .field("account", &self.account)
            .field("plan_hash", &self.plan_hash)
            .finish_non_exhaustive()
    }
}

impl Lease {
    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn plan_hash(&self) -> &str {
        &self.plan_hash
    }

    pub fn trading_authorized(&self) -> bool {
        false
    }

    pub fn installation_authorized(&self) -> bool {
        false
    }

    /// Release the flock only. This is not reconciliation and does not permit
    /// a restart. The lock file is left unchanged on disk.
    pub fn release(self) -> Release {
        drop(self.file);
        Release {
            reconciled: false,
            restart_permitted: false,
        }
    }
}

/// Bind before a reservation. A binding error does not call the writer.
pub fn reserve_if_bound(
    identity: &OriginalIdentity,
    confirmed_hash: &str,
    writer: &mut LedgerWriter,
    reservation: Reservation<'_>,
) -> Result<(Lease, Effect), GateError> {
    let lease = bind(writer.connection(), identity, confirmed_hash)?;
    let effect = writer.reserve(reservation).map_err(GateError::Write)?;
    Ok((lease, effect))
}

/// Bind before a fill. A binding error does not call the writer.
pub fn fill_if_bound(
    identity: &OriginalIdentity,
    confirmed_hash: &str,
    writer: &mut LedgerWriter,
    fill: Fill<'_>,
) -> Result<(Lease, Effect), GateError> {
    let lease = bind(writer.connection(), identity, confirmed_hash)?;
    let effect = writer.fill(fill).map_err(GateError::Write)?;
    Ok((lease, effect))
}

/// Check the original files, the explicit account, the prepared-plan hash, and
/// take the cooperative owner lease. No ledger row is written here.
pub fn bind(
    db: &Connection,
    identity: &OriginalIdentity,
    confirmed_hash: &str,
) -> Result<Lease, BindError> {
    if identity.plan_hash != confirmed_hash || !is_plan_hash(confirmed_hash) {
        return Err(BindError::PlanMismatch);
    }
    if !is_account(&identity.account) {
        return Err(BindError::AccountMismatch);
    }
    if identity.files.is_empty()
        || identity
            .files
            .iter()
            .any(|file| file.path == identity.lock.path)
        || !identity
            .files
            .iter()
            .any(|file| file.path == identity.ledger)
    {
        return Err(BindError::Unavailable);
    }
    for file in identity.files.iter().chain(std::iter::once(&identity.lock)) {
        inspect_file(file)?;
    }
    match_database(db, identity, confirmed_hash)?;
    let mut opened = OpenOptions::new();
    opened
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = opened.open(&identity.lock.path).map_err(open_error)?;
    let held = file.metadata().map_err(|_| BindError::Unavailable)?;
    if held.dev() != identity.lock.device || held.ino() != identity.lock.inode {
        return Err(BindError::Replaced);
    }
    if let Err(error) = file.try_lock() {
        return Err(match error {
            std::fs::TryLockError::WouldBlock => BindError::Contended,
            std::fs::TryLockError::Error(_) => BindError::Unavailable,
        });
    }
    Ok(Lease {
        file,
        account: identity.account.clone(),
        plan_hash: identity.plan_hash.clone(),
    })
}

fn open_error(error: std::io::Error) -> BindError {
    if error.kind() == ErrorKind::TooManyLinks || error.raw_os_error() == Some(libc::ELOOP) {
        BindError::Symlink
    } else {
        BindError::Unavailable
    }
}

fn inspect_file(file: &FileIdentity) -> Result<(), BindError> {
    reject_symlink_ancestors(&file.path)?;
    let meta = match fs::symlink_metadata(&file.path) {
        Ok(meta) => meta,
        Err(_) => return missing_original(&file.path),
    };
    if meta.file_type().is_symlink() {
        return Err(BindError::Symlink);
    }
    private_regular(&meta)?;
    if meta.dev() != file.device || meta.ino() != file.inode {
        return Err(BindError::Replaced);
    }
    private_directory(file.path.parent().ok_or(BindError::Unavailable)?)?;
    let mut opened = OpenOptions::new();
    opened
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let handle = opened.open(&file.path).map_err(open_error)?;
    let opened_meta = handle.metadata().map_err(|_| BindError::Unavailable)?;
    if opened_meta.dev() != file.device || opened_meta.ino() != file.inode {
        return Err(BindError::Replaced);
    }
    for suffix in SIDECARS {
        inspect_sidecar(&sidecar_path(&file.path, suffix))?;
    }
    Ok(())
}

fn missing_original(path: &Path) -> Result<(), BindError> {
    for suffix in SIDECARS {
        if fs::symlink_metadata(sidecar_path(path, suffix)).is_ok() {
            return Err(BindError::OrphanSidecar);
        }
    }
    Err(BindError::Unavailable)
}

fn inspect_sidecar(path: &Path) -> Result<(), BindError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(BindError::Unavailable),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(BindError::UnsafeSidecar);
    }
    if meta.uid() != current_uid()? || mode_bits(meta.mode()) != 0o600 || meta.nlink() != 1 {
        return Err(BindError::UnsafeSidecar);
    }
    let parent = path.parent().ok_or(BindError::Unavailable)?;
    if private_directory(parent).is_err() {
        return Err(BindError::UnsafeSidecar);
    }
    Ok(())
}

fn private_regular(meta: &fs::Metadata) -> Result<(), BindError> {
    if !meta.is_file() {
        return Err(BindError::Unavailable);
    }
    if meta.uid() != current_uid()? || mode_bits(meta.mode()) != 0o600 {
        return Err(BindError::UnsafeMode);
    }
    if meta.nlink() != 1 {
        return Err(BindError::LinkCount);
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<(), BindError> {
    let meta = fs::symlink_metadata(path).map_err(|_| BindError::Unavailable)?;
    if meta.file_type().is_symlink() {
        return Err(BindError::Symlink);
    }
    if !meta.is_dir() || meta.uid() != current_uid()? || mode_bits(meta.mode()) != 0o700 {
        return Err(BindError::UnsafeMode);
    }
    Ok(())
}

fn reject_symlink_ancestors(path: &Path) -> Result<(), BindError> {
    for ancestor in path.ancestors().skip(1) {
        let meta = fs::symlink_metadata(ancestor).map_err(|_| BindError::Unavailable)?;
        if meta.file_type().is_symlink() {
            return Err(BindError::Symlink);
        }
    }
    Ok(())
}

fn match_database(
    db: &Connection,
    identity: &OriginalIdentity,
    confirmed_hash: &str,
) -> Result<(), BindError> {
    let mut statement = db
        .prepare("PRAGMA database_list")
        .map_err(|_| BindError::Unavailable)?;
    let mut rows = statement.query([]).map_err(|_| BindError::Unavailable)?;
    let mut saw_main = false;
    while let Some(row) = rows.next().map_err(|_| BindError::Unavailable)? {
        let name: String = row.get(1).map_err(|_| BindError::Unavailable)?;
        let file: String = row.get(2).map_err(|_| BindError::Unavailable)?;
        if name == "temp" {
            continue;
        }
        if name != "main" {
            return Err(BindError::AttachedDatabase);
        }
        if saw_main || Path::new(&file) != identity.ledger {
            return Err(BindError::Replaced);
        }
        saw_main = true;
    }
    if !saw_main {
        return Err(BindError::Unavailable);
    }
    let account: Option<String> = db
        .query_row(
            "SELECT value FROM metadata WHERE key='account_alias'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| BindError::Unavailable)?;
    if account.as_deref() != Some(identity.account.as_str()) {
        return Err(BindError::AccountMismatch);
    }
    let plan: Option<String> = db
        .query_row(
            "SELECT value FROM metadata WHERE key='prepared_plan'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| BindError::Unavailable)?;
    if plan.as_deref() != Some(confirmed_hash) {
        return Err(BindError::PlanMismatch);
    }
    Ok(())
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn mode_bits(mode: u32) -> u32 {
    mode & 0o777
}

fn current_uid() -> Result<u32, BindError> {
    Ok(fs::metadata("/proc/self")
        .map_err(|_| BindError::Unavailable)?
        .uid())
}

fn is_plan_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_account(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    value.len() <= 32
        && first.is_ascii_lowercase()
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
}
