use super::*;

pub(crate) fn validate_prompt_bundle(
    bytes: &[u8],
) -> Result<std::collections::BTreeMap<String, String>> {
    if bytes.is_empty() || bytes.len() > 65_536 {
        return Err("release_prompts_invalid");
    }
    let prompts: std::collections::BTreeMap<String, String> =
        serde_json::from_slice(bytes).map_err(|_| "release_prompts_invalid")?;
    let expected = prompt_files();
    if prompts.len() != expected.len()
        || expected.iter().any(|name| {
            prompts
                .get(*name)
                .is_none_or(|text| text.trim().is_empty() || text.len() > 16_384)
        })
    {
        return Err("release_prompts_invalid");
    }
    Ok(prompts)
}
pub(crate) fn retain_prompt_bundle(stage: &Path, bundle: &[u8], checksum: &[u8]) -> Result<()> {
    for (name, bytes) in [
        (DEFAULT_PROMPTS_ASSET, bundle),
        ("default-prompts.sha256", checksum),
    ] {
        let path = stage.join(name);
        if exists(&path) {
            if read(&path, 65_536)? != bytes {
                return Err("installed_artifact_mismatch");
            }
        } else if let Err(error) = write_new(&path, bytes, false)
            && (!exists(&path) || read(&path, 65_536)? != bytes)
        {
            return Err(error);
        }
    }
    sync(stage)
}
pub(crate) fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}
pub(crate) fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "update_file_unavailable")?;
    if !file
        .metadata()
        .map_err(|_| "update_file_unavailable")?
        .is_file()
    {
        return Err("update_regular_file_required");
    }
    let mut bytes = vec![];
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "update_file_unavailable")?;
    if bytes.len() > limit {
        return Err("update_file_too_large");
    }
    Ok(bytes)
}
pub(crate) fn write_new(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(if executable { 0o755 } else { 0o600 })
        .open(path)
        .map_err(|_| "immutable_artifact_exists")?;
    file.set_permissions(fs::Permissions::from_mode(if executable {
        0o755
    } else {
        0o600
    }))
    .map_err(|_| "update_permissions_failed")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "update_write_failed")
}
pub(crate) fn sync(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| "update_sync_failed")
}
pub(crate) fn publish_noreplace(pending: &Path, path: &Path) -> Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        pending,
        rustix::fs::CWD,
        path,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|_| "immutable_artifact_exists")?;
    sync(path.parent().ok_or("update_parent")?)
}
pub(crate) fn secure_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = path
        .symlink_metadata()
        .map_err(|_| "update_directory_unavailable")?;
    let owner = fs::metadata("/proc/self")
        .map_err(|_| "update_owner_unavailable")?
        .uid();
    if !metadata.is_dir() || metadata.mode() & 0o077 != 0 || metadata.uid() != owner {
        return Err("private_update_directory_required");
    }
    Ok(())
}

pub(crate) fn release_metadata(dir: &Path, m: &Manifest) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let owner = fs::metadata("/proc/self")
        .map_err(|_| "update_owner_unavailable")?
        .uid();
    let meta = dir
        .symlink_metadata()
        .map_err(|_| "update_directory_unavailable")?;
    if !meta.is_dir() || meta.uid() != owner {
        return Err("release_directory_owner_mismatch");
    }
    for (name, bytes) in [
        ("VERSION", m.tag.as_bytes().to_vec()),
        ("SOURCE_COMMIT", m.source_commit.as_bytes().to_vec()),
        ("SHA256SUMS", format!("{}  nivra\n", m.sha256).into_bytes()),
    ] {
        let path = dir.join(name);
        if exists(&path) {
            let existing = read(&path, 16384)?;
            let text = std::str::from_utf8(&existing).map_err(|_| "installed_metadata_mismatch")?;
            // Bootstrap bundles retain newline-terminated identities and a
            // checksum list covering additional files. Preserve those bytes.
            let matches = if name == "SHA256SUMS" {
                text.lines()
                    .any(|line| line == format!("{}  nivra", m.sha256))
            } else {
                text.trim().as_bytes() == bytes
            };
            if !matches {
                return Err("installed_metadata_mismatch");
            }
        } else {
            write_new(&path, &bytes, false)?;
        }
    }
    for entry in fs::read_dir(dir).map_err(|_| "update_directory_unavailable")? {
        let entry = entry.map_err(|_| "update_directory_unavailable")?;
        let meta = entry
            .path()
            .symlink_metadata()
            .map_err(|_| "update_file_unavailable")?;
        if !meta.is_file() || meta.uid() != owner || meta.nlink() != 1 {
            return Err("release_regular_owned_files_required");
        }
        fs::set_permissions(
            entry.path(),
            fs::Permissions::from_mode(if entry.file_name() == "nivra" {
                0o755
            } else {
                0o600
            }),
        )
        .map_err(|_| "update_permissions_failed")?;
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|_| "update_permissions_failed")?;
    sync(dir)
}
fn release_complete(dir: &Path, m: &Manifest, manifest_bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let owner = fs::metadata("/proc/self")
        .map_err(|_| "update_owner_unavailable")?
        .uid();
    let meta = dir
        .symlink_metadata()
        .map_err(|_| "update_directory_unavailable")?;
    if !meta.is_dir() || meta.uid() != owner {
        return Err("release_directory_owner_mismatch");
    }
    for entry in fs::read_dir(dir).map_err(|_| "update_directory_unavailable")? {
        let entry = entry.map_err(|_| "update_directory_unavailable")?;
        let meta = entry
            .path()
            .symlink_metadata()
            .map_err(|_| "update_file_unavailable")?;
        if !meta.is_file()
            || meta.uid() != owner
            || meta.nlink() != 1
            || (entry.file_name() == "nivra" && meta.mode() & 0o100 == 0)
        {
            return Err("release_regular_owned_files_required");
        }
    }
    if read(&dir.join("release.json"), 16384)? != manifest_bytes
        || digest(&read(&dir.join("nivra"), 64 * 1024 * 1024)?) != m.sha256
    {
        return Err("installed_artifact_mismatch");
    }
    for (name, expected) in [("VERSION", &m.tag), ("SOURCE_COMMIT", &m.source_commit)] {
        let bytes = read(&dir.join(name), 16384)?;
        if std::str::from_utf8(&bytes)
            .map_err(|_| "installed_metadata_mismatch")?
            .trim()
            != expected
        {
            return Err("installed_metadata_mismatch");
        }
    }
    let checksums = read(&dir.join("SHA256SUMS"), 16384)?;
    if !std::str::from_utf8(&checksums)
        .map_err(|_| "installed_metadata_mismatch")?
        .lines()
        .any(|line| line == format!("{}  nivra", m.sha256))
    {
        return Err("installed_metadata_mismatch");
    }
    Ok(())
}
/// Write a verified release into its immutable `RELEASES/<tag>-<commit>`
/// directory, or prove an existing one holds exactly these bytes.
pub(crate) fn stage_release(
    releases: &Path,
    m: &Manifest,
    manifest_bytes: &[u8],
    binary: &[u8],
) -> Result<PathBuf> {
    let stage = releases.join(format!("{}-{}", m.tag, m.source_commit));
    if exists(&stage) {
        release_complete(&stage, m, manifest_bytes)?;
        sync(releases)?;
        return Ok(stage);
    }
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut pending = None;
    for _ in 0..128 {
        let path = releases.join(format!(
            ".{}.pending.{}.{}",
            stage
                .file_name()
                .ok_or("update_stage_failed")?
                .to_string_lossy(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {
                pending = Some(path);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("update_stage_failed"),
        }
    }
    let pending = pending.ok_or("update_stage_failed")?;
    for (name, data, exec) in [
        ("nivra", binary, true),
        ("release.json", manifest_bytes, false),
        ("SOURCE_COMMIT", m.source_commit.as_bytes(), false),
        ("VERSION", m.tag.as_bytes(), false),
    ] {
        write_new(&pending.join(name), data, exec)?;
    }
    write_new(
        &pending.join("SHA256SUMS"),
        format!("{}  nivra\n", m.sha256).as_bytes(),
        false,
    )?;
    release_complete(&pending, m, manifest_bytes)?;
    sync(&pending)?;
    sync(releases)?;
    if let Err(error) = publish_noreplace(&pending, &stage) {
        if !exists(&pending) || !exists(&stage) {
            return Err(error);
        }
        release_complete(&stage, m, manifest_bytes)?;
        sync(releases)?;
    }
    Ok(stage)
}
/// A release the operator fetched from `refs/release-assets/<tag>` by hand,
/// verified with the updater's own manifest and checksum rules. Only the
/// one-time host migration installs from a local directory.
pub(crate) struct StagedRelease {
    manifest: Manifest,
    manifest_bytes: Vec<u8>,
    binary: Vec<u8>,
}
impl StagedRelease {
    pub(crate) fn read(stage: &Path) -> Result<Self> {
        let manifest_bytes = read(&stage.join("release.json"), 16384)?;
        let manifest: Manifest =
            serde_json::from_slice(&manifest_bytes).map_err(|_| "invalid_release_manifest")?;
        Version::parse(&manifest.tag)?;
        if !hash(&manifest.source_commit, 40)
            || manifest.target != TARGET
            || !hash(&manifest.sha256, 64)
        {
            return Err("release_identity_mismatch");
        }
        let checksum = read(&stage.join(format!("{ASSET}.sha256")), 256)?;
        let binary = read(&stage.join(ASSET), 64 * 1024 * 1024)?;
        if checksum != format!("{}  {ASSET}\n", manifest.sha256).as_bytes()
            || digest(&binary) != manifest.sha256
        {
            return Err("release_checksum_mismatch");
        }
        Ok(Self {
            manifest,
            manifest_bytes,
            binary,
        })
    }
    pub(crate) fn tag(&self) -> &str {
        &self.manifest.tag
    }
    pub(crate) fn directory(&self, releases: &Path) -> PathBuf {
        releases.join(format!(
            "{}-{}",
            self.manifest.tag, self.manifest.source_commit
        ))
    }
    pub(crate) fn install(&self, releases: &Path) -> Result<PathBuf> {
        let stage = stage_release(releases, &self.manifest, &self.manifest_bytes, &self.binary)?;
        release_metadata(&stage, &self.manifest)?;
        Ok(stage)
    }
    /// True when `dir` already holds exactly this release.
    pub(crate) fn installed_at(&self, dir: &Path) -> bool {
        release_complete(dir, &self.manifest, &self.manifest_bytes).is_ok()
    }
}
pub(crate) fn installed_manifest(dir: &Path) -> Result<Option<Manifest>> {
    if !exists(&dir.join("release.json")) {
        return Ok(None);
    }
    let m: Manifest = serde_json::from_slice(&read(&dir.join("release.json"), 16384)?)
        .map_err(|_| "invalid_installed_manifest")?;
    Version::parse(&m.tag)?;
    if !hash(&m.source_commit, 40)
        || m.target != TARGET
        || !hash(&m.sha256, 64)
        || digest(&read(&dir.join("nivra"), 64 * 1024 * 1024)?) != m.sha256
    {
        return Err("installed_artifact_mismatch");
    }
    Ok(Some(m))
}
pub(crate) fn swap(current: &Path, target: &Path) -> Result<()> {
    let next = current.with_extension("next");
    std::os::unix::fs::symlink(target, &next).map_err(|_| "update_pointer_pending")?;
    fs::rename(next, current).map_err(|_| "update_swap_failed")?;
    sync(current.parent().ok_or("update_parent")?)
}
/// Replace a small bounded state record without following links or inheriting
/// the caller's umask. Records live only in the private updater state directory.
pub(crate) fn record_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    let next = path.with_extension(format!("{}.next", std::process::id()));
    write_new(
        &next,
        &serde_json::to_vec(value).map_err(|_| "update_record_failed")?,
        false,
    )?;
    fs::rename(&next, path).map_err(|_| "update_record_failed")?;
    if let Some(parent) = path.parent() {
        sync(parent)?;
    }
    Ok(())
}
pub(crate) fn read_record(path: &Path, limit: usize) -> Option<serde_json::Value> {
    serde_json::from_slice(&read(path, limit).ok()?).ok()
}
/// Only an explicit diagnostic code may leave the child. Preflight writes its
/// own reason code on stderr; anything else is replaced so config contents,
/// paths, tokens and account data never reach updater state, logs or alerts.
pub(crate) fn preflight_reason(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .rev()
        .map(str::trim)
        .find_map(|line| {
            // Accept only the updater's own lowercase snake_case failure codes.
            // Paths, URLs, tokens, config values and account data never match.
            let safe = (4..=64).contains(&line.len())
                && line.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && line.contains('_')
                && line
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                && [
                    "preflight_",
                    "research_",
                    "invalid_",
                    "config_",
                    "planner_",
                    "paper_",
                    "update_",
                    "rollover_",
                ]
                .iter()
                .any(|prefix| line.starts_with(prefix));
            safe.then(|| line.to_owned())
        })
        .unwrap_or_else(|| "preflight_failed_unspecified".into())
}
pub(crate) fn preflight_status(state: &Path) -> Option<serde_json::Value> {
    let record = read_record(&state.join("preflight-failure.json"), 4096)?;
    let reason = record["reason"].as_str()?;
    if reason.len() > 64
        || !reason
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return None;
    }
    let phase = record["phase"]
        .as_str()
        .filter(|p| *p == "candidate" || *p == "rollback")?;
    let version = record["version"]
        .as_str()
        .filter(|v| v.len() <= 32 && Version::parse(v).is_ok());
    Some(serde_json::json!({
        "reason":reason,
        "phase":phase,
        "version":version,
        "since":record["at"].as_str().filter(|a| a.len() <= 40),
    }))
}
pub(crate) fn record_preflight_failure(
    state: &Path,
    phase: &str,
    version: Option<&str>,
    stderr: &[u8],
) {
    let record = serde_json::json!({
        "reason":preflight_reason(stderr),
        "phase":phase,
        "version":version,
        "at":now_utc().to_rfc3339(),
    });
    let _ = record_json(&state.join("preflight-failure.json"), &record);
}
pub(crate) fn clear_preflight_failure(state: &Path) {
    let path = state.join("preflight-failure.json");
    if exists(&path) {
        let _ = fs::remove_file(path);
    }
}
/// Bounded dashboard projection: a reason code and timestamps, never config bytes.
pub(crate) fn dashboard_status() -> serde_json::Value {
    let Ok(home) = std::env::var("HOME") else {
        return serde_json::Value::Null;
    };
    let state = Path::new(&home).join(".local/state/nivra");
    let Some(preflight) = preflight_status(&state) else {
        return serde_json::Value::Null;
    };
    serde_json::json!({
        "status":"preflight_failed",
        "reason":preflight["reason"],
        "version":preflight["version"],
        "phase":preflight["phase"],
        "since":preflight["since"],
    })
}
/// Bounded recovery projection for the critical alert. No paths or config bytes.
pub(crate) fn recovery_status(state: &Path) -> Option<serde_json::Value> {
    let record = read_record(&state.join("update-recovery.json"), 4096)?;
    let kind = record["kind"].as_str()?;
    if !matches!(kind, "rollback_restart_failed" | "pointer_restore_failed") {
        return None;
    }
    Some(serde_json::json!({
        "kind":kind,
        "cause":record["cause"].as_str().filter(|c| c.len() <= 64),
        "pointer_restored":record["pointer_restored"].as_bool(),
        "restarted":record["restarted"].as_bool(),
        "restore_error":record["restore_error"].as_str().filter(|c| c.len() <= 64),
        "restart_error":record["restart_error"].as_str().filter(|c| c.len() <= 64),
        "at":record["at"].as_str().filter(|a| a.len() <= 40),
    }))
}
pub(crate) fn updater_failure_details(
    state: Option<&Path>,
    code: &'static str,
) -> serde_json::Value {
    let mut details = serde_json::json!({
        "rollback":matches!(
            code,
            "update_rolled_back"
                | "update_rollback_restart_failed"
                | "update_rollback_pointer_restore_failed"
        ),
        "reason":code,
    });
    if let Some(state) = state {
        if let Some(preflight) = preflight_status(state) {
            details["preflight_failure"] = preflight;
        }
        if let Some(recovery) = recovery_status(state) {
            details["rollback_restart_failed"] =
                serde_json::json!(recovery["kind"] == "rollback_restart_failed");
            details["recovery"] = recovery;
        }
    }
    details
}

/// Keep a failed recovery visible for this run only; later runs archive it so a
/// stale verdict cannot be attributed to a different failure.
pub(crate) fn archive_recovery(state: &Path) {
    let path = state.join("update-recovery.json");
    if exists(&path) {
        let stamp = now_utc().timestamp_millis();
        let _ = fs::rename(&path, state.join(format!("update-recovery.{stamp}.json")));
    }
}
pub(crate) enum Interrupted {
    Cleared,
    Review {
        reason: &'static str,
        previous: Option<String>,
        new: Option<String>,
    },
}
/// A stale transaction is never replayed. The live pointer distinguishes the
/// unambiguous pre-flip case (preserve evidence, then run a fresh transaction)
/// from a partially activated release, which needs operator review.
pub(crate) fn recover_interrupted(
    state: &Path,
    current: &Path,
    old: &Path,
    recover: bool,
) -> Result<Interrupted> {
    let pending = state.join("pending.json");
    let next = current.with_extension("next");
    let next_exists = exists(&next);
    if !exists(&pending) {
        if next_exists {
            if !recover {
                return Ok(Interrupted::Review {
                    reason: "interrupted_update_swap_pending_requires_review",
                    previous: None,
                    new: None,
                });
            }
            fs::remove_file(&next).map_err(|_| "update_record_failed")?;
            sync(current.parent().ok_or("update_parent")?)?;
        }
        return Ok(Interrupted::Cleared);
    }
    let Some(evidence) = read_record(&pending, 16384) else {
        return Ok(Interrupted::Review {
            reason: "interrupted_update_evidence_invalid_requires_review",
            previous: None,
            new: None,
        });
    };
    let (Some(previous), Some(new)) = (evidence["previous"].as_str(), evidence["new"].as_str())
    else {
        return Ok(Interrupted::Review {
            reason: "interrupted_update_evidence_invalid_requires_review",
            previous: None,
            new: None,
        });
    };
    if !Path::new(previous).is_absolute() || !Path::new(new).is_absolute() {
        return Ok(Interrupted::Review {
            reason: "interrupted_update_evidence_invalid_requires_review",
            previous: Some(previous.into()),
            new: Some(new.into()),
        });
    }
    let previous_path = fs::canonicalize(previous).ok();
    let new_path = fs::canonicalize(new).ok();
    if previous_path.as_deref() == Some(old) {
        if !recover {
            // Previews never move transaction evidence or stale pointers.
            return Ok(Interrupted::Review {
                reason: "interrupted_update_before_flip_requires_explicit_update",
                previous: Some(previous.into()),
                new: Some(new.into()),
            });
        }
        // The flip never moved past the running release. Keep the original
        // evidence and let this run repeat every check from scratch.
        let stamp = now_utc().timestamp_millis();
        let preserved = state.join(format!("interrupted-update.{stamp}.json"));
        fs::rename(&pending, preserved).map_err(|_| "update_record_failed")?;
        if next_exists {
            fs::remove_file(&next).map_err(|_| "update_record_failed")?;
        }
        sync(state)?;
        return Ok(Interrupted::Cleared);
    }
    Ok(Interrupted::Review {
        reason: if new_path == fs::canonicalize(current).ok() {
            "interrupted_update_new_release_active_requires_review"
        } else {
            "interrupted_update_pointer_unrecognized_requires_review"
        },
        previous: Some(previous.into()),
        new: Some(new.into()),
    })
}
pub(crate) fn write_pending_review(
    state: &Path,
    current: &Path,
    reason: &'static str,
    previous: Option<&str>,
    new: Option<&str>,
) {
    let record = serde_json::json!({
        "reason":reason,
        "at":now_utc().to_rfc3339(),
        "current":fs::read_link(current).ok().map(|p| p.to_string_lossy().to_string()),
        "previous":previous,
        "new":new,
        "swap_pending":exists(&current.with_extension("next")),
        "advice":"inspect the pointer, service state and preserved pending.json; never replay or delete evidence",
    });
    let _ = record_json(&state.join("pending-review.json"), &record);
}
pub(crate) fn finalize_pending(
    state: &Path,
    stage: &Path,
    old: &Path,
    key: &str,
    value: serde_json::Value,
) -> Result<()> {
    let pending = state.join("pending.json");
    let mut evidence = read_record(&pending, 16384).unwrap_or_else(
        || serde_json::json!({"previous":old.to_string_lossy(),"new":stage.to_string_lossy()}),
    );
    evidence[key] = value;
    let mut receipt = stage.join("failed-update.json");
    if exists(&receipt) {
        receipt = stage.join(format!(
            "failed-update.{}.json",
            now_utc().timestamp_millis()
        ));
    }
    record_json(&receipt, &evidence)?;
    if exists(&pending) {
        fs::remove_file(&pending).map_err(|_| "update_record_failed")?;
    }
    sync(state)
}
pub(crate) fn close_unwatched(state: &Path) -> Result<Option<serde_json::Value>> {
    let path = state.join("pending.json");
    let Some(mut record) = read_record(&path, 16384) else {
        return Ok(None);
    };
    if !record["unwatched"].is_object() {
        return Ok(None);
    }
    let started = record["unwatched"]["started_at"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    let stopped = now_utc();
    let duration = started.map(|s| (stopped - s.with_timezone(&Utc)).num_milliseconds().max(0));
    record["unwatched"]["stopped_at"] = serde_json::json!(stopped.to_rfc3339());
    record["unwatched"]["duration_ms"] = serde_json::json!(duration);
    record_json(&path, &record)?;
    Ok(Some(record["unwatched"].clone()))
}
/// One recovery path for every activation failure: restore the previous pointer,
/// try to restart the previous release, finalise the transaction as a failed
/// receipt and leave a bounded state record when the bot could not be restored.
#[allow(clippy::too_many_arguments)]
pub(crate) fn recover_failed_activation(
    state: &Path,
    stage: &Path,
    current: &Path,
    old: &Path,
    host: &mut impl Host,
    cause: &'static str,
    swapped: bool,
    keep_stopped: bool,
) -> Result<&'static str> {
    // Stop the candidate before touching the pointer; failure here is recorded,
    // not fatal to the rollback attempt.
    let _ = host.stop();
    let pointer = if swapped {
        host.swap(current, old)
    } else {
        Ok(())
    };
    let _ = fs::remove_file(current.with_extension("next"));
    let pointer_restored = pointer.is_ok();
    let restart = if keep_stopped || !pointer_restored {
        None
    } else {
        Some(host.restart().and_then(|_| host.healthy()))
    };
    let restarted = restart.as_ref().map(|r| r.is_ok());
    let window = close_unwatched(state).ok().flatten();
    if let Some(window) = &window {
        eprintln!("update_unwatched_window: {window}");
    }
    let evidence = serde_json::json!({
        "cause":cause,
        "at":now_utc().to_rfc3339(),
        "swapped":swapped,
        "pointer_restored":pointer_restored,
        "restart_requested":restart.is_some(),
        "restarted":restarted,
        "restore_error":pointer.as_ref().err(),
        "restart_error":restart.as_ref().and_then(|r| r.as_ref().err()),
        "unwatched":window,
    });
    let _ = finalize_pending(state, stage, old, "recovery", evidence.clone());
    let failure = if !pointer_restored {
        Some((
            "pointer_restore_failed",
            "update_rollback_pointer_restore_failed",
        ))
    } else if restarted == Some(false) {
        Some(("rollback_restart_failed", "update_rollback_restart_failed"))
    } else {
        None
    };
    if let Some((kind, code)) = failure {
        let record = serde_json::json!({
            "kind":kind,
            "cause":cause,
            "at":evidence["at"],
            "previous":old.to_string_lossy(),
            "new":stage.to_string_lossy(),
            "pointer_restored":pointer_restored,
            "restarted":restarted,
            "restore_error":evidence["restore_error"],
            "restart_error":evidence["restart_error"],
            "unwatched":evidence["unwatched"],
        });
        record_json(&state.join("update-recovery.json"), &record)?;
        return Err(code);
    }
    Err(cause)
}
#[cfg(test)]
pub(crate) fn apply(
    home: &Path,
    host: &mut impl Host,
    check: bool,
    dry: bool,
) -> Result<&'static str> {
    apply_selected(home, host, check, dry, false, &Selection::default())
}
/// Install the selected release.
pub(crate) fn apply_selected(
    home: &Path,
    host: &mut impl Host,
    check: bool,
    dry: bool,
    force: bool,
    target: &Selection,
) -> Result<&'static str> {
    let state = home.join(".local/state/nivra");
    let releases = home.join("nivra-releases");
    let current = home.join("nivra-current");
    let old = fs::canonicalize(&current).map_err(|_| "existing_binary_required")?;
    // A HOLD freezes the deployment, including transaction recovery.
    if exists(&home.join("nivra-deploy/HOLD")) {
        return Err("update_hold");
    }
    if !check && !dry {
        archive_recovery(&state);
    }
    match recover_interrupted(&state, &current, &old, !check && !dry)? {
        Interrupted::Cleared => {}
        Interrupted::Review {
            reason,
            previous,
            new,
        } => {
            write_pending_review(
                &state,
                &current,
                reason,
                previous.as_deref(),
                new.as_deref(),
            );
            eprintln!("update_interrupted_transaction_requires_review: {reason}");
            return Err(reason);
        }
    }
    let installed = installed_manifest(&old)?;
    // Query the symlink target, never the updater's own compiled version or
    // optional VERSION/SOURCE_COMMIT sidecars.
    let running = host.version(&old.join("nivra"))?;
    if installed.as_ref().is_some_and(|m| m.tag != running) {
        return Err("installed_release_identity_mismatch");
    }
    let catalog = host.fetch("tags", 1024 * 1024)?;
    let catalog = std::str::from_utf8(&catalog).map_err(|_| "invalid_tag_catalog")?;
    if let Some(m) = &installed {
        // A normal install must be published by the configured distribution with
        // exactly the recorded source commit.
        let published = select_target(catalog, &running, &Selection::new(Some(&m.tag), None)?);
        let identity_ok = matches!(published, Ok(Some((_, ref sha))) if *sha == m.source_commit);
        if !identity_ok {
            return Err("installed_release_identity_mismatch");
        }
        if !check && !dry {
            release_metadata(&old, m)?;
        }
    }
    let Some((tag, commit)) = select_target(catalog, &running, target)? else {
        if !check
            && !dry
            && let Some(m) = &installed
        {
            release_metadata(&old, m)?;
        }
        return Ok("already_current");
    };
    if check {
        return Ok(if running == tag {
            "already_current"
        } else {
            "available"
        });
    }
    let manifest_bytes = match host.fetch(&format!("{tag}/release.json"), 16384) {
        // The timer retries next run; an explicit selection reports the missing assets.
        Err("release_assets_pending") if target.tag.is_none() && target.sha.is_none() => {
            return Ok("deferred_release_assets_pending");
        }
        other => other?,
    };
    let m: Manifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| "invalid_release_manifest")?;
    if m.tag != tag || m.source_commit != commit || m.target != TARGET || !hash(&m.sha256, 64) {
        return Err("release_identity_mismatch");
    }
    let checksum = host.fetch(&format!("{tag}/{ASSET}.sha256"), 256)?;
    let binary = host.fetch(&format!("{tag}/{ASSET}"), 64 * 1024 * 1024)?;
    if checksum != format!("{}  {ASSET}\n", m.sha256).as_bytes() || digest(&binary) != m.sha256 {
        return Err("release_checksum_mismatch");
    }
    let prompt_bundle = if let Some(checksum_bytes) =
        host.fetch_optional(&format!("{tag}/default-prompts.sha256"), 256)?
    {
        let checksum =
            std::str::from_utf8(&checksum_bytes).map_err(|_| "release_prompts_checksum_invalid")?;
        let (expected, name) = checksum
            .strip_suffix('\n')
            .and_then(|s| s.split_once("  "))
            .ok_or("release_prompts_checksum_invalid")?;
        if name != DEFAULT_PROMPTS_ASSET || !hash(expected, 64) {
            return Err("release_prompts_checksum_invalid");
        }
        let bytes = host.fetch(&format!("{tag}/{DEFAULT_PROMPTS_ASSET}"), 65_536)?;
        if digest(&bytes) != expected {
            return Err("release_prompts_checksum_mismatch");
        }
        validate_prompt_bundle(&bytes)?;
        Some((bytes, checksum_bytes))
    } else {
        None
    };
    if running == m.tag && digest(&read(&old.join("nivra"), 64 * 1024 * 1024)?) == m.sha256 {
        if !check && !dry {
            if let Some((bundle, checksum)) = prompt_bundle.as_ref() {
                retain_prompt_bundle(&old, bundle, checksum)?;
            }
            release_metadata(&old, &m)?;
        }
        return Ok("already_current");
    }
    if dry {
        return Ok(match host.guard(force)? {
            Admission::Safe => "would_update",
            Admission::Forced(_) => "would_update_unwatched_window",
            Admission::Deferred(reason) => reason,
        });
    }
    let stage = stage_release(&releases, &m, &manifest_bytes, &binary)?;
    if let Some((bundle, checksum)) = prompt_bundle.as_ref() {
        retain_prompt_bundle(&stage, bundle, checksum)?;
    }
    release_metadata(&stage, &m)?;
    if exists(&stage.join("failed-update.json")) {
        return Err("failed_release_requires_review");
    }
    host.smoke(&stage.join("nivra"), &tag)?;
    if exists(&home.join("nivra-deploy/HOLD")) {
        return Err("update_hold");
    }
    let forced = match host.guard(force)? {
        Admission::Safe => None,
        Admission::Forced(reason) => Some(reason),
        Admission::Deferred(reason) => return Ok(reason),
    };
    let keep_stopped = host.keep_stopped();
    if forced.is_some() && keep_stopped {
        return Err("update_force_conflicts_operator_stopped");
    }
    // Activation failure restores `old`, so it must still run the reviewed
    // config. Refuse before pending, stop or swap rather than strand rollback.
    host.rollback_preflight(&old.join("nivra"))?;
    if !keep_stopped {
        host.plan_restart(&m.tag)?;
    }
    // Preserve transaction evidence across interruption. Never automatically replay it.
    let unwatched_start = forced.map(|_| now_utc());
    let mut record = serde_json::json!({"previous":old,"new":stage});
    if let (Some(reason), Some(start)) = (forced, &unwatched_start) {
        record["unwatched"] = serde_json::json!({
            "forced":true,
            "reason":reason,
            "started_at":start.to_rfc3339(),
        });
    }
    write_new(
        &state.join("pending.json"),
        &serde_json::to_vec(&record).map_err(|_| "update_record_failed")?,
        false,
    )?;
    sync(&state)?;
    // No symlink changes while the old controller is still running.
    if let Err(cause) = host.stop() {
        eprintln!("update_stop_failed: {cause}");
        return recover_failed_activation(
            &state,
            &stage,
            &current,
            &old,
            host,
            cause,
            false,
            keep_stopped,
        );
    }
    if let Err(cause) = host.swap(&current, &stage) {
        // A failed sync can leave the new pointer in place; detect what happened
        // instead of assuming the flip is atomic end-to-end.
        let swapped = fs::canonicalize(&current).ok().as_deref() == Some(stage.as_path());
        eprintln!("update_swap_failed: {cause}");
        return recover_failed_activation(
            &state,
            &stage,
            &current,
            &old,
            host,
            cause,
            swapped,
            keep_stopped,
        );
    }
    let activation = host.smoke(&stage.join("nivra"), &tag).and_then(|_| {
        if keep_stopped {
            Ok(())
        } else {
            host.restart().and_then(|_| host.healthy())
        }
    });
    if let Err(cause) = activation {
        eprintln!("update_activation_failed: {cause}");
        let recovered = recover_failed_activation(
            &state,
            &stage,
            &current,
            &old,
            host,
            cause,
            true,
            keep_stopped,
        );
        return match recovered {
            // A completed rollback keeps its established status; an unrecoverable
            // one keeps the actionable recovery code (and its critical alert).
            Err(code) if code.starts_with("update_rollback_") => Err(code),
            _ => Err("update_rolled_back"),
        };
    }
    if forced.is_some()
        && let Ok(Some(window)) = close_unwatched(&state)
    {
        eprintln!("update_unwatched_window: {window}");
    }
    let previous = home.join("nivra-previous");
    if exists(&previous) {
        fs::remove_file(&previous).map_err(|_| "update_record_failed")?;
    }
    std::os::unix::fs::symlink(&old, &previous).map_err(|_| "update_record_failed")?;
    // An explicit rollback can reactivate a previously installed release.
    // Keep its original installation receipt instead of replacing history.
    let mut receipt = stage.join("installed-update.json");
    if exists(&receipt) {
        receipt = stage.join(format!(
            "installed-update.{}.{}.json",
            now_utc()
                .timestamp_nanos_opt()
                .ok_or("update_record_failed")?,
            std::process::id()
        ));
        if exists(&receipt) {
            return Err("update_record_failed");
        }
    }
    fs::rename(state.join("pending.json"), receipt).map_err(|_| "update_record_failed")?;
    sync(&stage)?;
    sync(&state)?;
    // Retain all releases, including failed artifacts (at least three plus previous).
    Ok("updated")
}
