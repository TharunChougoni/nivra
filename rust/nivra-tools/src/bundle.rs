//! Read-only validation of explicitly supplied development-data bundles.
//! Does not download data, mount a sandbox or certify provenance/containment.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path},
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    schema: u32,
    split: Split,
    contract: Contract,
    files: Vec<Input>,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Split {
    Development,
    Training,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    instruments: Vec<String>,
    start: String,
    end: String,
    warmup_start: String,
    frequency: String,
    fields: Vec<String>,
    maximum_bytes: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    role: Role,
    sha256: String,
    bytes: u64,
    source: String,
    acquired_at: String,
    availability_semantics: String,
}
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Role {
    Prices,
    Instruments,
    Calendar,
    Costs,
    Strategy,
    Runtime,
}
fn date(text: &str) -> bool {
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok() && text.len() == 10
}
fn collect(root: &Path, current: &Path, found: &mut BTreeSet<String>) -> Result<(), &'static str> {
    if found.len() > 512 {
        return Err("bundle_file_limit");
    }
    for entry in fs::read_dir(current).map_err(|_| "bundle_unreadable")? {
        let entry = entry.map_err(|_| "bundle_unreadable")?;
        let path = entry.path();
        let metadata = path.symlink_metadata().map_err(|_| "bundle_unreadable")?;
        if metadata.file_type().is_symlink() {
            return Err("bundle_symlink_denied");
        }
        if metadata.is_dir() {
            return Err("bundle_flat_layout_required");
        }
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err("bundle_regular_single_link_required");
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "bundle_path_invalid")?
            .to_str()
            .ok_or("bundle_path_invalid")?
            .to_owned();
        found.insert(relative);
        if found.len() > 512 {
            return Err("bundle_file_limit");
        }
    }
    Ok(())
}
pub fn check(root: &Path) -> Result<usize, &'static str> {
    if !root
        .symlink_metadata()
        .map_err(|_| "bundle_unreadable")?
        .is_dir()
    {
        return Err("bundle_directory_required");
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join("bundle.json"))
        .map_err(|_| "bundle_unreadable")?;
    if !file.metadata().map_err(|_| "bundle_unreadable")?.is_file() {
        return Err("bundle_regular_single_link_required");
    }
    let mut bytes = vec![];
    file.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| "bundle_unreadable")?;
    if bytes.len() > 65536 {
        return Err("bundle_manifest_limit");
    }
    let bundle: Bundle = serde_json::from_slice(&bytes).map_err(|_| "invalid_bundle_manifest")?;
    let contract = bundle.contract;
    if bundle.schema != 1
        || contract.instruments.is_empty()
        || contract.instruments.len() > 10000
        || contract
            .instruments
            .iter()
            .any(|s| s.is_empty() || s.len() > 128)
        || contract.instruments.iter().collect::<BTreeSet<_>>().len() != contract.instruments.len()
        || !date(&contract.start)
        || !date(&contract.end)
        || !date(&contract.warmup_start)
        || contract.warmup_start > contract.start
        || contract.start > contract.end
        || !["daily", "minute", "quote"].contains(&contract.frequency.as_str())
        || contract.fields.is_empty()
        || contract.fields.len() > 32
        || contract.fields.iter().any(|s| s.is_empty() || s.len() > 64)
        || contract.maximum_bytes == 0
        || contract.maximum_bytes > 2 * 1024 * 1024 * 1024
        || bundle.files.is_empty()
        || bundle.files.len() > 511
    {
        return Err("data_not_ready_contract");
    }
    // Both accepted splits are non-holdout. A holdout value fails deserialization.
    match bundle.split {
        Split::Development | Split::Training => {}
    }
    let mut expected = BTreeSet::from(["bundle.json".to_owned()]);
    let mut roles = BTreeSet::new();
    let mut total = 0_u64;
    for file in &bundle.files {
        if Path::new(&file.path).components().count() != 1
            || !Path::new(&file.path)
                .components()
                .all(|p| matches!(p, Component::Normal(_)))
            || !expected.insert(file.path.clone())
        {
            return Err("bundle_path_invalid");
        }
        if file.source.trim().is_empty()
            || file.source.len() > 512
            || file.availability_semantics.trim().is_empty()
            || file.availability_semantics.len() > 512
            || chrono::DateTime::parse_from_rfc3339(&file.acquired_at).is_err()
            || file.bytes == 0
            || file.sha256.len() != 64
        {
            return Err("data_not_ready_provenance");
        }
        roles.insert(file.role);
        total = total.checked_add(file.bytes).ok_or("bundle_size_limit")?;
        if total > contract.maximum_bytes {
            return Err("bundle_size_limit");
        }
        let path = root.join(&file.path);
        let metadata = path
            .symlink_metadata()
            .map_err(|_| "data_not_ready_missing_file")?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err("bundle_regular_single_link_required");
        }
        let mut input = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "bundle_unreadable")?;
        let opened = input.metadata().map_err(|_| "bundle_unreadable")?;
        if opened.ino() != metadata.ino()
            || opened.dev() != metadata.dev()
            || opened.len() != file.bytes
        {
            return Err("bundle_file_changed");
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 65536];
        let mut consumed = 0_u64;
        loop {
            let n = input.read(&mut buffer).map_err(|_| "bundle_unreadable")?;
            if n == 0 {
                break;
            }
            consumed += n as u64;
            if consumed > file.bytes {
                return Err("bundle_file_changed");
            }
            hasher.update(&buffer[..n]);
        }
        let digest: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if consumed != file.bytes || digest != file.sha256 {
            return Err("bundle_hash_mismatch");
        }
    }
    if roles
        != BTreeSet::from([
            Role::Prices,
            Role::Instruments,
            Role::Calendar,
            Role::Costs,
            Role::Strategy,
            Role::Runtime,
        ])
    {
        return Err("data_not_ready_roles");
    }
    let mut found = BTreeSet::new();
    collect(root, root, &mut found)?;
    if found != expected {
        return Err("bundle_unlisted_input");
    }
    Ok(bundle.files.len())
}
