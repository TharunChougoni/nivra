//! Immutable content-addressed blobs under `objects/sha256/<digest>` and the
//! consistent backup that copies the database with every object it names.
use super::{Error, Result, Store};
use nivra_core::autoresearch::sha256_ok;
use nivra_core::replay::digest;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const MAX_OBJECT_BYTES: u64 = 32 * 1024 * 1024;

pub(super) fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error("research_store_path_invalid"));
    }
    Ok(())
}

pub(super) fn object_path(root: &Path, sha256: &str) -> Result<PathBuf> {
    if !sha256_ok(sha256) {
        return Err(Error("research_object_digest_invalid"));
    }
    Ok(root.join("objects").join("sha256").join(sha256))
}

fn sync_dir(path: &Path) -> Result<()> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// Write `bytes` once under its digest, fsynced before the caller commits a
/// reference to it. An existing object must already hold these bytes.
pub(super) fn put(root: &Path, bytes: &[u8]) -> Result<String> {
    if bytes.len() as u64 > MAX_OBJECT_BYTES {
        return Err(Error("research_object_too_large"));
    }
    let sha256 = digest(bytes);
    let path = object_path(root, &sha256)?;
    let dir = path.parent().ok_or(Error("research_store_path_invalid"))?;
    private_dir(dir)?;
    if path.exists() {
        get(root, &sha256)?;
        return Ok(sha256);
    }
    let temp = dir.join(format!(".{sha256}.{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o400))?;
    fs::rename(&temp, &path)?;
    sync_dir(dir)?;
    Ok(sha256)
}

/// Read an object and verify its digest.
pub(super) fn get(root: &Path, sha256: &str) -> Result<Vec<u8>> {
    let path = object_path(root, sha256)?;
    let meta = fs::symlink_metadata(&path).map_err(|_| Error("research_object_missing"))?;
    if !meta.is_file() || meta.len() > MAX_OBJECT_BYTES {
        return Err(Error("research_object_invalid"));
    }
    let bytes = fs::read(&path)?;
    if digest(&bytes) != sha256 {
        return Err(Error("research_object_digest_mismatch"));
    }
    Ok(bytes)
}

/// What a backup or a verification covered.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct BackupReport {
    pub objects: usize,
    pub bytes: u64,
}

impl Store {
    /// Every digest the record references.
    pub(super) fn referenced(&self) -> Result<Vec<String>> {
        let mut statement = self.db.prepare(
            "SELECT source_blob FROM experiment UNION SELECT params_blob FROM experiment
             UNION SELECT manifest_blob FROM experiment
             UNION SELECT report_blob FROM experiment WHERE report_blob IS NOT NULL
             UNION SELECT manifest_digest FROM campaign
             UNION SELECT digest FROM skill_revision
             UNION SELECT evidence_digest FROM finding WHERE evidence_digest IS NOT NULL",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut digests = Vec::new();
        for row in rows {
            let value = row?;
            if sha256_ok(&value) {
                digests.push(value);
            }
        }
        digests.sort_unstable();
        digests.dedup();
        Ok(digests)
    }

    /// Take a consistent SQLite snapshot into `dest` and copy every object it
    /// references, then verify the copy. `dest` must not exist yet.
    pub fn backup(&self, dest: &Path) -> Result<BackupReport> {
        if dest.exists() {
            return Err(Error("research_backup_exists"));
        }
        private_dir(dest)?;
        let database = dest.join("research.sqlite");
        self.db.execute(
            "VACUUM INTO ?1",
            [database
                .to_str()
                .ok_or(Error("research_store_path_invalid"))?],
        )?;
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600))?;
        let snapshot = Store::open(dest)?;
        for sha256 in snapshot.referenced()? {
            if !object_path(dest, &sha256)?.exists() {
                put(dest, &get(&self.root, &sha256)?)?;
            }
        }
        verify(dest)
    }
}

/// Open the record at `root` and check that every referenced object is
/// present and matches its digest.
pub fn verify(root: &Path) -> Result<BackupReport> {
    let store = Store::open(root)?;
    let mut report = BackupReport {
        objects: 0,
        bytes: 0,
    };
    for sha256 in store.referenced()? {
        report.bytes += get(root, &sha256)?.len() as u64;
        report.objects += 1;
    }
    Ok(report)
}
