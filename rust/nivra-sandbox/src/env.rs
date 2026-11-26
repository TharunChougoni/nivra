//! Read-only research Python environment resolution and content-hash verification.
use crate::Failure;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::{Mutex, OnceLock},
};

pub const ENV_MANIFEST_NAME: &str = ".nivra-env.json";
const MAX_CACHED_ENVS: usize = 16;

static VERIFIED_CACHE: OnceLock<Mutex<BTreeMap<PathBuf, (String, String)>>> = OnceLock::new();

pub fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn path_ok(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

fn non_symlink_dir(path: &Path) -> Result<(), Failure> {
    let meta = fs::symlink_metadata(path).map_err(|_| Failure::Setup)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Failure::Setup);
    }
    Ok(())
}

/// Resolve either a `--target` directory or a `venv`'s `site-packages` directory.
/// Symlinks along the environment layout are refused.
pub fn resolve_site_packages(env_dir: &Path) -> Result<PathBuf, Failure> {
    if !path_ok(env_dir) {
        return Err(Failure::Config);
    }
    non_symlink_dir(env_dir)?;
    let pyvenv = env_dir.join("pyvenv.cfg");
    let resolved = if pyvenv.exists() {
        let pyvenv_meta = fs::symlink_metadata(&pyvenv).map_err(|_| Failure::Setup)?;
        if !pyvenv_meta.is_file() || pyvenv_meta.file_type().is_symlink() {
            return Err(Failure::Setup);
        }
        let lib = env_dir.join("lib");
        non_symlink_dir(&lib)?;
        let mut found = Vec::new();
        for entry in fs::read_dir(&lib).map_err(|_| Failure::Setup)? {
            let entry = entry.map_err(|_| Failure::Setup)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with("python3.") {
                continue;
            }
            let py_dir = entry.path();
            non_symlink_dir(&py_dir)?;
            let site = py_dir.join("site-packages");
            if site.exists() {
                non_symlink_dir(&site)?;
                found.push(site);
            }
        }
        if found.len() != 1 {
            return Err(Failure::Setup);
        }
        found.pop().ok_or(Failure::Setup)?
    } else {
        env_dir.to_path_buf()
    };
    fs::canonicalize(&resolved).map_err(|_| Failure::Setup)
}

fn collect_sorted_entries(
    root: &Path,
    dir: &Path,
    rel_prefix: &str,
    out: &mut Vec<(String, PathBuf, fs::Metadata)>,
) -> Result<(), Failure> {
    let mut children = Vec::new();
    for entry in fs::read_dir(dir).map_err(|_| Failure::Setup)? {
        let entry = entry.map_err(|_| Failure::Setup)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Failure::Setup)?;
        children.push((name, entry.path()));
    }
    children.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, path) in children {
        let rel = if rel_prefix.is_empty() {
            name
        } else {
            format!("{rel_prefix}/{name}")
        };
        if rel == ENV_MANIFEST_NAME {
            continue;
        }
        let meta = fs::symlink_metadata(&path).map_err(|_| Failure::Setup)?;
        if meta.file_type().is_symlink() {
            return Err(Failure::Setup);
        }
        if meta.is_dir() {
            collect_sorted_entries(root, &path, &rel, out)?;
        } else if meta.is_file() {
            out.push((rel, path, meta));
        } else {
            return Err(Failure::Setup);
        }
    }
    let _ = root;
    Ok(())
}

fn hex_digest(hasher: Sha256) -> String {
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn stat_fingerprint(entries: &[(String, PathBuf, fs::Metadata)]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"nivra-research-env-stat-v1\0");
    for (rel, _, meta) in entries {
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update(meta.len().to_le_bytes());
        hasher.update(meta.ino().to_le_bytes());
        hasher.update(meta.mtime().to_le_bytes());
        hasher.update(meta.mtime_nsec().to_le_bytes());
        hasher.update(meta.ctime().to_le_bytes());
        hasher.update(meta.ctime_nsec().to_le_bytes());
        hasher.update(meta.mode().to_le_bytes());
    }
    hex_digest(hasher)
}

fn hash_entries(entries: &[(String, PathBuf, fs::Metadata)]) -> Result<String, Failure> {
    let mut hasher = Sha256::new();
    hasher.update(b"nivra-research-env-v1\0");
    let mut buf = [0u8; 65_536];
    for (rel, path, meta) in entries {
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update(meta.len().to_le_bytes());
        let executable = u8::from(meta.permissions().mode() & 0o111 != 0);
        hasher.update([executable]);
        let mut file = fs::File::open(path).map_err(|_| Failure::Setup)?;
        let mut read_total = 0u64;
        loop {
            let n = file.read(&mut buf).map_err(|_| Failure::Setup)?;
            if n == 0 {
                break;
            }
            read_total = read_total.saturating_add(n as u64);
            hasher.update(&buf[..n]);
        }
        if read_total != meta.len() {
            return Err(Failure::Setup);
        }
    }
    Ok(hex_digest(hasher))
}

/// Compute the deterministic SHA-256 content hash of a resolved site-packages directory.
pub fn compute_dir_hash(site_packages: &Path) -> Result<String, Failure> {
    non_symlink_dir(site_packages)?;
    let mut entries = Vec::new();
    collect_sorted_entries(site_packages, site_packages, "", &mut entries)?;
    hash_entries(&entries)
}

/// Resolve and verify an environment directory, returning `(site_packages, env_sha256)`.
/// Uses a stat-fingerprint cache so repeated ticks in one process do not re-hash
/// hundreds of megabytes unless any file's size, inode, mode, or timestamps change.
pub fn verified_site_packages(
    env_dir: &Path,
    expected_sha256: Option<&str>,
) -> Result<(PathBuf, String), Failure> {
    if let Some(expected) = expected_sha256
        && !valid_hash(expected)
    {
        return Err(Failure::Config);
    }
    let site_packages = resolve_site_packages(env_dir)?;
    let mut entries = Vec::new();
    collect_sorted_entries(&site_packages, &site_packages, "", &mut entries)?;
    let fp = stat_fingerprint(&entries);
    let cache = VERIFIED_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let cached = cache
        .lock()
        .ok()
        .and_then(|guard| guard.get(&site_packages).cloned())
        .filter(|(cached_fp, _)| *cached_fp == fp)
        .map(|(_, hash)| hash);
    let hash = match cached {
        Some(hash) => hash,
        None => {
            let hash = hash_entries(&entries)?;
            if let Ok(mut guard) = cache.lock() {
                if guard.len() >= MAX_CACHED_ENVS {
                    guard.clear();
                }
                guard.insert(site_packages.clone(), (fp, hash.clone()));
            }
            hash
        }
    };
    if let Some(expected) = expected_sha256
        && hash != expected
    {
        return Err(Failure::Setup);
    }
    Ok((site_packages, hash))
}
