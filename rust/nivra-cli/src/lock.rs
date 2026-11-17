//! Advisory lock lifetime guard.
//!
//! A `flock` is owned by the open file description, not by one descriptor.
//! Closing our descriptor is therefore not enough to release it: a child forked
//! before `exec` keeps the description (and the lock) alive until it exits.
//! Releasing explicitly when the owning operation ends makes a completed
//! operation's lock lifetime independent of unrelated process spawns (#122).
use std::{fs, ops::Deref};

/// An advisory-locked file that explicitly unlocks before closing.
pub struct AdvisoryLock(fs::File);

impl AdvisoryLock {
    /// Wrap a file whose lock was acquired successfully.
    pub fn new(file: fs::File) -> Self {
        Self(file)
    }
}

impl Deref for AdvisoryLock {
    type Target = fs::File;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for AdvisoryLock {
    fn drop(&mut self) {
        // Ignore "not locked": callers may have already unlocked explicitly.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

impl std::fmt::Debug for AdvisoryLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}
