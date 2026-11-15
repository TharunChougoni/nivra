//! Cooperative inference owner only. No grant access, network or invocation authority.
use std::{
    fmt, fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Busy,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "native_lease_invalid",
            Self::Busy => "native_lease_busy",
        })
    }
}
impl std::error::Error for Error {}
fn private(m: &fs::Metadata, owner: u32) -> bool {
    m.is_file() && m.uid() == owner && m.mode() & 0o077 == 0 && m.nlink() == 1 && m.len() == 0
}
fn identity(m: &fs::Metadata) -> (u64, u64, u32, u32, u64, u64) {
    (m.dev(), m.ino(), m.mode(), m.uid(), m.nlink(), m.len())
}
pub(crate) fn directory(path: &Path) -> Result<fs::File, Error> {
    crate::native_grant::path_ok(path).map_err(|_| Error::Invalid)?;
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut file = fs::File::from(
        rustix::fs::open("/", flags, rustix::fs::Mode::empty()).map_err(|_| Error::Invalid)?,
    );
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            file = fs::File::from(
                rustix::fs::openat(&file, name, flags, rustix::fs::Mode::empty())
                    .map_err(|_| Error::Invalid)?,
            );
        }
    }
    let m = file.metadata().map_err(|_| Error::Invalid)?;
    let owner = fs::metadata("/proc/self")
        .map_err(|_| Error::Invalid)?
        .uid();
    if !m.is_dir() || m.uid() != owner || m.mode() & 0o077 != 0 {
        return Err(Error::Invalid);
    }
    Ok(file)
}
pub struct Lease {
    file: fs::File,
    directory: PathBuf,
    parent: (u64, u64),
    original: (u64, u64, u32, u32, u64, u64),
}
impl fmt::Debug for Lease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeInferenceLease(redacted, no authority)")
    }
}
impl Lease {
    pub(crate) fn namespace(&self) -> &Path {
        &self.directory
    }
    /// Existing private application directory only; create a zero-byte lock if absent.
    /// Never creates a grant/directory, chmods, truncates, unlinks or repairs artifacts.
    pub fn acquire(path: &Path) -> Result<Self, Error> {
        let parent = directory(path)?;
        let metadata = parent.metadata().map_err(|_| Error::Invalid)?;
        let fd = rustix::fs::openat(
            &parent,
            "inference.lock",
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map_err(|_| Error::Invalid)?;
        let file = fs::File::from(fd);
        let held = file.metadata().map_err(|_| Error::Invalid)?;
        if !private(&held, metadata.uid()) {
            return Err(Error::Invalid);
        }
        file.try_lock().map_err(|e| match e {
            fs::TryLockError::WouldBlock => Error::Busy,
            _ => Error::Invalid,
        })?;
        let lease = Self {
            file,
            directory: path.into(),
            parent: (metadata.dev(), metadata.ino()),
            original: identity(&held),
        };
        lease.verify()?;
        Ok(lease)
    }
    pub fn verify(&self) -> Result<(), Error> {
        let parent = directory(&self.directory)?;
        let m = parent.metadata().map_err(|_| Error::Invalid)?;
        if (m.dev(), m.ino()) != self.parent {
            return Err(Error::Invalid);
        }
        let named = self
            .directory
            .join("inference.lock")
            .symlink_metadata()
            .map_err(|_| Error::Invalid)?;
        let held = self.file.metadata().map_err(|_| Error::Invalid)?;
        if !private(&named, m.uid())
            || identity(&named) != self.original
            || identity(&held) != self.original
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn invocation_authorized(&self) -> bool {
        false
    }
}
