//! Read-only independent-app namespace. Never imports other applications' auth caches.
use nivra_core::native_grant::{GrantRecord, MAX_GRANT_BYTES};
use std::{
    fmt, fs,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("independent_grant_store_invalid")
    }
}
impl std::error::Error for Error {}
/// Explicit standalone grant file, outside repositories and foreign auth namespaces.
pub fn file_path_ok(path: &Path) -> Result<(), Error> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Error);
    }
    for ancestor in path.ancestors() {
        if ancestor.file_name().is_some_and(|n| {
            [".hermes", ".grok", ".codex", ".openclaw"]
                .iter()
                .any(|v| n == *v)
        }) || ancestor.join(".git").exists()
            || ancestor
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(Error);
        }
    }
    Ok(())
}
fn accept_xai(bytes: &[u8]) -> Result<(), Error> {
    GrantRecord::parse(bytes).map(|_| ()).map_err(|_| Error)
}
fn accept_openai(bytes: &[u8]) -> Result<(), Error> {
    nivra_core::openai_grant::GrantRecord::parse(bytes)
        .map(|_| ())
        .map_err(|_| Error)
}
fn read_checked(path: &Path, accept: fn(&[u8]) -> Result<(), Error>) -> Result<Vec<u8>, Error> {
    use std::os::unix::fs::OpenOptionsExt;
    file_path_ok(path)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Error)?;
    let m = file.metadata().map_err(|_| Error)?;
    if !m.is_file()
        || m.uid() != fs::metadata("/proc/self").map_err(|_| Error)?.uid()
        || m.mode() & 0o777 != 0o600
        || m.nlink() != 1
    {
        return Err(Error);
    }
    let mut bytes = Vec::new();
    file.take(MAX_GRANT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error)?;
    if bytes.len() > MAX_GRANT_BYTES {
        return Err(Error);
    }
    accept(&bytes)?;
    Ok(bytes)
}
pub fn read_file(path: &Path) -> Result<Vec<u8>, Error> {
    read_checked(path, accept_xai)
}
/// One application-owned provider API key: a private (0600, owner, single
/// link) file outside repositories and foreign auth caches. The key is
/// returned trimmed and is never parsed, logged or echoed.
pub fn read_api_key_file(path: &Path) -> Result<String, Error> {
    let bytes = read_checked(path, accept_api_key)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| Error)?;
    Ok(text.trim().to_owned())
}
fn accept_api_key(bytes: &[u8]) -> Result<(), Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error)?.trim();
    if (16..=512).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_graphic()) {
        Ok(())
    } else {
        Err(Error)
    }
}
pub fn read_openai_file(path: &Path) -> Result<Vec<u8>, Error> {
    read_checked(path, accept_openai)
}
fn create_checked(
    path: &Path,
    bytes: &[u8],
    accept: fn(&[u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    file_path_ok(path)?;
    accept(bytes)?;
    let parent = path.parent().ok_or(Error)?;
    fs::create_dir_all(parent).map_err(|_| Error)?;
    file_path_ok(path)?;
    // Never replace an existing grant or an ambiguous login result.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| Error)?;
    file.write_all(bytes).map_err(|_| Error)?;
    file.sync_all().map_err(|_| Error)?;
    fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|_| Error)
}
pub fn create_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    create_checked(path, bytes, accept_xai)
}
pub fn create_openai_file(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    create_checked(path, bytes, accept_openai)
}

/// One owner across grant inode replacements. The lock file is never removed.
/// Callers retain this guard through refresh and the associated provider request.
pub struct FileOwner {
    path: PathBuf,
    _lock: fs::File,
}
impl FileOwner {
    pub fn acquire(path: &Path) -> Result<Self, Error> {
        Self::acquire_within(path, std::time::Duration::ZERO)
    }
    /// Like `acquire`, but waits up to `wait` while another owner holds the
    /// lock. Roles sharing one grant (digest and supervisor) only hold it to
    /// read or refresh the grant, so a short bounded wait replaces a spurious
    /// `grant_busy_or_unavailable` failure. Invalid lock files fail at once.
    pub fn acquire_within(path: &Path, wait: std::time::Duration) -> Result<Self, Error> {
        use std::os::unix::fs::OpenOptionsExt;
        file_path_ok(path)?;
        let mut name = path.as_os_str().to_os_string();
        name.push(".lock");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(Path::new(&name))
            .map_err(|_| Error)?;
        let m = lock.metadata().map_err(|_| Error)?;
        if !m.is_file()
            || m.uid() != fs::metadata("/proc/self").map_err(|_| Error)?.uid()
            || m.mode() & 0o777 != 0o600
            || m.nlink() != 1
        {
            return Err(Error);
        }
        let deadline = std::time::Instant::now() + wait;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(_) => return Err(Error),
            }
        }
        Ok(Self {
            path: path.to_owned(),
            _lock: lock,
        })
    }
    pub fn read(&self) -> Result<Vec<u8>, Error> {
        read_file(&self.path)
    }
    pub fn read_openai(&self) -> Result<Vec<u8>, Error> {
        read_openai_file(&self.path)
    }
    /// Compare before swapping. Failed staging is retained and blocks another rewrite.
    pub fn replace(&self, expected: &[u8], replacement: &[u8]) -> Result<(), Error> {
        self.replace_checked(expected, replacement, accept_xai, read_file)
    }
    pub fn replace_openai(&self, expected: &[u8], replacement: &[u8]) -> Result<(), Error> {
        self.replace_checked(expected, replacement, accept_openai, read_openai_file)
    }
    fn replace_checked(
        &self,
        expected: &[u8],
        replacement: &[u8],
        accept: fn(&[u8]) -> Result<(), Error>,
        read: fn(&Path) -> Result<Vec<u8>, Error>,
    ) -> Result<(), Error> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        accept(replacement)?;
        if read(&self.path)? != expected {
            return Err(Error);
        }
        let mut name = self.path.as_os_str().to_os_string();
        name.push(".refresh");
        let temporary = Path::new(&name);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(temporary)
            .map_err(|_| Error)?;
        file.write_all(replacement).map_err(|_| Error)?;
        file.sync_all().map_err(|_| Error)?;
        if read(&self.path)? != expected {
            return Err(Error);
        }
        fs::rename(temporary, &self.path).map_err(|_| Error)?;
        fs::File::open(self.path.parent().ok_or(Error)?)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error)
    }
}

impl Drop for FileOwner {
    fn drop(&mut self) {
        // flock ownership follows the open file description. A forked child or
        // a duplicated descriptor can keep that description alive after this
        // guard is dropped, so release the lock explicitly before closing it.
        let _ = self._lock.unlock();
    }
}

#[cfg(test)]
#[test]
fn dropping_file_owner_unlocks_while_cloned_description_remains_open() {
    let directory = tempfile::tempdir().unwrap();
    let grant = directory.path().join("grant");
    let owner = FileOwner::acquire(&grant).unwrap();
    assert!(FileOwner::acquire(&grant).is_err());

    // try_clone duplicates the descriptor while retaining the same open file
    // description, as a forked process would. Dropping the owner must still
    // unlock it so another owner can proceed.
    let retained = owner._lock.try_clone().unwrap();
    drop(owner);
    assert!(retained.metadata().is_ok());
    let next_owner = FileOwner::acquire(&grant).unwrap();
    drop(next_owner);
    drop(retained);

    // A bounded wait succeeds once the holder releases, and still refuses
    // when the holder outlives the wait.
    let held = FileOwner::acquire(&grant).unwrap();
    let short = std::time::Duration::from_millis(50);
    assert!(FileOwner::acquire_within(&grant, short).is_err());
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(held);
    });
    assert!(FileOwner::acquire_within(&grant, std::time::Duration::from_secs(5)).is_ok());
    release.join().unwrap();
}
