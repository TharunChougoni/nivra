//! Opt-in model-archive retention, plus a default size bound for closed sibling
//! sessions. Pack and delete never open a book, journal or credential. The size
//! bound rewrites oversized `wide_quotes` bodies and large model artifacts in
//! past closed sessions only. It leaves the configured session directory, the
//! `state` book row, shared journals and failed artifacts unchanged.
use super::*;
use rusqlite::OptionalExtension;
use serde_json::Value;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

#[derive(Default, Serialize)]
pub struct Summary {
    dry_run: bool,
    packed: Vec<String>,
    deleted: Vec<String>,
    would_pack: Vec<String>,
    would_delete: Vec<String>,
    bytes_freed: u64,
    would_compact_quotes: usize,
    compacted_quotes: usize,
    would_compact_model_files: usize,
    compacted_model_files: usize,
}
pub fn entry(config: &Path, apply: bool) -> ExitCode {
    let result = (|| -> Result<()> {
        let c: Config = serde_json::from_slice(
            &crate::read_bounded(config, 16384).map_err(|_| "retention_config_invalid")?,
        )?;
        c.validate()?;
        let today = local(millis()).date_naive();
        println!("{}", serde_json::to_string(&run(&c, today, apply)?)?);
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => crate::fixed_error("retention_failed"),
    }
}
fn is_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}
fn closed_session(path: &Path, date: NaiveDate, c: &Config) -> bool {
    let Some(report) = crate::read_bounded(
        &path.join("report.json"),
        c.dashboard.history_read_max_bytes as u64,
    )
    .ok()
    .and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
        return false;
    };
    report["paper_only"] == true
        && report["live_enabled"] == false
        && report["broker"] == "upstox"
        && report["account"] == c.account
        && report["session"] == date.to_string()
        && report["portfolio"]["closed"] == true
        && report["portfolio"]["stopped"] == false
        && report["portfolio"]["halted"] == false
}
fn run_dirs(model: &Path) -> Result<Vec<PathBuf>> {
    if !is_dir(model) {
        return Ok(vec![]);
    }
    let mut runs = Vec::new();
    for entry in fs::read_dir(model)? {
        let path = entry?.path();
        if !is_dir(&path) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("retention_name_invalid")?;
        if !super::model_history::valid_id(name)
            || ["prompts", "transcripts", "usage"].contains(&name)
        {
            continue;
        }
        if name.starts_with("harness-")
            || (path.join("input.json").is_file()
                && ["output.json", "failure.json", "luna-review.json"]
                    .iter()
                    .any(|n| path.join(n).is_file()))
        {
            runs.push(path);
        }
    }
    runs.sort();
    Ok(runs)
}
// A minimal ustar writer avoids adding a dependency to the locked Cargo graph.
// The 512-byte blocks and octal field widths are format units, not runtime knobs.
struct Entry {
    source: PathBuf,
    name: String,
    meta: fs::Metadata,
}
fn collect(path: &Path, model: &Path, entries: &mut Vec<Entry>) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() && !meta.is_dir() {
        return Err("retention_special_file_refused".into());
    }
    let name = path
        .strip_prefix(model)?
        .to_str()
        .ok_or("retention_name_invalid")?
        .to_owned();
    if name
        .split('/')
        .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err("retention_name_invalid".into());
    }
    let directory = meta.is_dir();
    entries.push(Entry {
        source: path.to_owned(),
        name,
        meta,
    });
    if directory {
        let mut children: Vec<_> = fs::read_dir(path)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<_>>()?;
        children.sort();
        for child in children {
            collect(&child, model, entries)?;
        }
    }
    Ok(())
}
fn octal(field: &mut [u8], number: u64) -> Result<()> {
    let text = format!("{:0width$o}", number, width = field.len() - 1);
    if text.len() >= field.len() {
        return Err("retention_tar_range".into());
    }
    field.fill(0);
    field[..text.len()].copy_from_slice(text.as_bytes());
    Ok(())
}
fn header(entry: &Entry) -> Result<[u8; 512]> {
    let mut block = [0u8; 512];
    let (prefix, name) = if entry.name.len() <= 100 {
        ("", entry.name.as_str())
    } else {
        entry
            .name
            .match_indices('/')
            .rev()
            .find_map(|(at, _)| {
                (at <= 155 && entry.name.len() - at - 1 <= 100)
                    .then_some((&entry.name[..at], &entry.name[at + 1..]))
            })
            .ok_or("retention_tar_name_limit")?
    };
    block[..name.len()].copy_from_slice(name.as_bytes());
    block[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    octal(
        &mut block[100..108],
        u64::from(entry.meta.permissions().mode() & 0o777),
    )?;
    octal(&mut block[108..116], 0)?;
    octal(&mut block[116..124], 0)?;
    octal(
        &mut block[124..136],
        if entry.meta.is_file() {
            entry.meta.len()
        } else {
            0
        },
    )?;
    octal(
        &mut block[136..148],
        entry.meta.modified()?.duration_since(UNIX_EPOCH)?.as_secs(),
    )?;
    block[148..156].fill(b' ');
    block[156] = if entry.meta.is_dir() { b'5' } else { b'0' };
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    let sum: u64 = block.iter().map(|b| u64::from(*b)).sum();
    let checksum = format!("{sum:06o}\0 ");
    block[148..156].copy_from_slice(checksum.as_bytes());
    Ok(block)
}
fn pack(path: &Path, entries: &[Entry]) -> Result<()> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    for entry in entries {
        gz.write_all(&header(entry)?)?;
        if entry.meta.is_file() {
            let mut input = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&entry.source)?;
            if !input.metadata()?.is_file() {
                return Err("retention_source_changed".into());
            }
            let copied = std::io::copy(&mut input, &mut gz)?;
            if copied != entry.meta.len() {
                return Err("retention_source_changed".into());
            }
            let padding = (512 - copied % 512) % 512;
            gz.write_all(&[0u8; 512][..padding as usize])?;
        }
    }
    gz.write_all(&[0u8; 1024])?;
    gz.finish()?.sync_all()?;
    Ok(())
}
fn retain_transcripts(model: &Path, runs: &[PathBuf]) -> Result<u64> {
    let mut copied = 0u64;
    for run in runs {
        let source = run.join("transcript.ndjson");
        let Ok(meta) = fs::symlink_metadata(&source) else {
            continue;
        };
        if !meta.is_file() {
            return Err("retention_transcript_invalid".into());
        }
        let directory = model.join("transcripts");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        let name = run
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("retention_name_invalid")?;
        let target = directory.join(format!("{name}.ndjson"));
        nivra_store::private_path::file_path_ok(&target)?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&target)
        {
            Ok(mut output) => {
                let mut input = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&source)?;
                copied = copied.saturating_add(std::io::copy(&mut input, &mut output)?);
                output.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Preserve both artifacts if a previous copy was interrupted.
                let hash = |path: &Path| -> Result<Vec<u8>> {
                    let mut file = fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(path)?;
                    let mut digest = Sha256::new();
                    let mut buffer = [0u8; 8192];
                    loop {
                        let count = file.read(&mut buffer)?;
                        if count == 0 {
                            break;
                        }
                        digest.update(&buffer[..count]);
                    }
                    Ok(digest.finalize().to_vec())
                };
                if hash(&source)? != hash(&target)? {
                    return Err("retention_transcript_conflict".into());
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(copied)
}
pub fn run(c: &Config, today: NaiveDate, apply: bool) -> Result<Summary> {
    let mut summary = Summary {
        dry_run: !apply,
        ..Summary::default()
    };
    let retention = &c.dashboard.retention;
    if retention.pack_after_days.is_none() && retention.delete_after_days.is_none() {
        bound_closed_sessions(c, today, apply, &mut summary)?;
        return Ok(summary);
    }
    c.dashboard.validate()?;
    let root = c.directory.parent().ok_or("retention_root_invalid")?;
    if c.directory.file_name().and_then(|v| v.to_str()) != Some(c.session.to_string().as_str()) {
        return Err("retention_root_invalid".into());
    }
    nivra_store::private_path::file_path_ok(&root.join("retention-scope"))?;
    let mut sessions: Vec<_> = fs::read_dir(root)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    sessions.sort();
    for session in sessions {
        if !is_dir(&session) {
            continue;
        }
        let Some(date) = session
            .file_name()
            .and_then(|v| v.to_str())
            .and_then(|v| NaiveDate::parse_from_str(v, "%Y-%m-%d").ok())
        else {
            continue;
        };
        let age = today.signed_duration_since(date).num_days();
        if age <= 0 || !closed_session(&session, date, c) {
            continue;
        }
        let delete = retention
            .delete_after_days
            .is_some_and(|days| age > i64::from(days));
        let should_pack = retention
            .pack_after_days
            .is_some_and(|days| age > i64::from(days));
        if !delete && !should_pack {
            continue;
        }
        let model = session.join("model");
        let runs = run_dirs(&model)?;
        let archive_dir = session.join("model-archive");
        if archive_dir.exists() && !is_dir(&archive_dir) {
            return Err("retention_archive_directory_invalid".into());
        }
        let archive = archive_dir.join(format!("{date}.tar.gz"));
        let temp = archive_dir.join(format!("{date}.tar.gz.tmp"));
        if apply && let Ok(meta) = fs::symlink_metadata(&temp) {
            if !meta.is_file() && !meta.file_type().is_symlink() {
                return Err("retention_temp_invalid".into());
            }
            fs::remove_file(&temp)?;
            if meta.is_file() {
                summary.bytes_freed = summary.bytes_freed.saturating_add(meta.len());
            }
        }
        if delete {
            if runs.is_empty() && !archive.exists() {
                continue;
            }
            summary.would_delete.push(date.to_string());
            if !apply {
                continue;
            }
            let mut entries = Vec::new();
            for run in &runs {
                collect(run, &model, &mut entries)?;
            }
            let copied = retain_transcripts(&model, &runs)?;
            let bytes = entries
                .iter()
                .filter(|e| e.meta.is_file())
                .map(|e| e.meta.len())
                .sum::<u64>();
            if let Ok(meta) = fs::symlink_metadata(&archive) {
                if !meta.is_file() {
                    return Err("retention_archive_invalid".into());
                }
                summary.bytes_freed = summary.bytes_freed.saturating_add(meta.len());
                fs::remove_file(&archive)?;
            }
            for run in &runs {
                fs::remove_dir_all(run)?;
            }
            summary.bytes_freed = summary
                .bytes_freed
                .saturating_add(bytes.saturating_sub(copied));
            summary.deleted.push(date.to_string());
        } else if should_pack && !runs.is_empty() {
            summary.would_pack.push(date.to_string());
            if !apply {
                continue;
            }
            // Never replace a published archive or infer that it covers raw
            // directories left by an interrupted removal or added afterwards.
            if fs::symlink_metadata(&archive).is_ok() {
                return Err("retention_archive_conflict".into());
            }
            let mut entries = Vec::new();
            for run in &runs {
                collect(run, &model, &mut entries)?;
            }
            for entry in &entries {
                let _ = header(entry)?;
            }
            let copied = retain_transcripts(&model, &runs)?;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&archive_dir)?;
            nivra_store::private_path::file_path_ok(&archive)?;
            pack(&temp, &entries)?;
            fs::rename(&temp, &archive)?;
            fs::File::open(&archive_dir)?.sync_all()?;
            let raw_bytes: u64 = entries
                .iter()
                .filter(|e| e.meta.is_file())
                .map(|e| e.meta.len())
                .sum();
            let archive_bytes = fs::metadata(&archive)?.len();
            for run in &runs {
                fs::remove_dir_all(run)?;
            }
            summary.bytes_freed = summary
                .bytes_freed
                .saturating_add(raw_bytes.saturating_sub(archive_bytes.saturating_add(copied)));
            summary.packed.push(date.to_string());
        }
    }
    bound_closed_sessions(c, today, apply, &mut summary)?;
    Ok(summary)
}

/// Newest `keep_full` quote bodies stay intact. Older bodies over `max_body`
/// are the ones a closed session may replace with a receipt.
pub(crate) fn quote_rows_to_compact(
    rows: &[(i64, u64)],
    keep_full: usize,
    max_body: u64,
) -> Vec<i64> {
    let newest = rows.len().saturating_sub(keep_full);
    rows.iter()
        .take(newest)
        .filter(|(_, len)| *len > max_body)
        .map(|(id, _)| *id)
        .collect()
}

/// Only the large planner artifacts inside a run directory are replaceable.
/// Failure files and transcripts are not model-size ballast.
pub(crate) fn model_artifact_over_cap(name: &str, len: u64, max_bytes: u64) -> bool {
    len > max_bytes && matches!(name, "plan.json" | "output.json" | "input.json")
}

fn quote_receipt(original: &str) -> String {
    serde_json::json!({
        "kind": "compacted_quote_receipt",
        "sha256": super::digest(original.as_bytes()),
        "original_bytes": original.len(),
    })
    .to_string()
}

fn model_receipt(name: &str, original: &[u8]) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&serde_json::json!({
        "kind": "compacted_model_artifact",
        "name": name,
        "sha256": super::digest(original),
        "original_bytes": original.len(),
    }))?)
}

fn bound_closed_sessions(
    c: &Config,
    today: NaiveDate,
    apply: bool,
    summary: &mut Summary,
) -> Result<()> {
    if c.directory.file_name().and_then(|v| v.to_str()) != Some(c.session.to_string().as_str()) {
        return Ok(());
    }
    let Some(root) = c.directory.parent() else {
        return Ok(());
    };
    if nivra_store::private_path::file_path_ok(&root.join("retention-scope")).is_err() {
        return Ok(());
    }
    let mut sessions: Vec<_> = fs::read_dir(root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()?;
    sessions.sort();
    for session in sessions {
        if !is_dir(&session) {
            continue;
        }
        let Some(date) = session
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| NaiveDate::parse_from_str(name, "%Y-%m-%d").ok())
        else {
            continue;
        };
        // The running paper process keeps `c.directory` open across midnight.
        // Never rewrite that book. Sibling sessions are eligible once closed.
        if date == c.session || today.signed_duration_since(date).num_days() <= 0 {
            continue;
        }
        if !closed_session(&session, date, c) {
            continue;
        }
        compact_model_artifacts(&session, c, apply, summary)?;
        compact_quote_bodies(&session, c, apply, summary)?;
    }
    Ok(())
}

fn compact_model_artifacts(
    session: &Path,
    c: &Config,
    apply: bool,
    summary: &mut Summary,
) -> Result<()> {
    let max_bytes = c.dashboard.retention.max_model_file_bytes;
    for run in run_dirs(&session.join("model"))? {
        for name in ["plan.json", "output.json", "input.json"] {
            let path = run.join(name);
            let meta = match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err("retention_special_file_refused".into());
                }
                Ok(meta) if meta.is_file() => meta,
                Ok(_) => return Err("retention_special_file_refused".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !model_artifact_over_cap(name, meta.len(), max_bytes) {
                continue;
            }
            summary.would_compact_model_files = summary.would_compact_model_files.saturating_add(1);
            if !apply {
                continue;
            }
            let original = fs::read(&path)?;
            let receipt = model_receipt(name, &original)?;
            if receipt.len() as u64 >= meta.len() {
                continue;
            }
            replace_regular_file(&path, &receipt, meta.permissions().mode() & 0o777)?;
            summary.compacted_model_files = summary.compacted_model_files.saturating_add(1);
            summary.bytes_freed = summary
                .bytes_freed
                .saturating_add(meta.len().saturating_sub(receipt.len() as u64));
        }
    }
    Ok(())
}

fn replace_regular_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("retention_name_invalid")?;
    let tmp = path.with_file_name(format!(".{name}.retention-tmp"));
    nivra_store::private_path::file_path_ok(&tmp)?;
    if let Ok(meta) = fs::symlink_metadata(&tmp) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err("retention_temp_invalid".into());
        }
        fs::remove_file(&tmp)?;
    }
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn compact_quote_bodies(
    session: &Path,
    c: &Config,
    apply: bool,
    summary: &mut Summary,
) -> Result<()> {
    let db_path = session.join("paper.sqlite");
    let meta = match fs::symlink_metadata(&db_path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => meta,
        Ok(_) => return Err("retention_special_file_refused".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    // A leftover WAL means some process may still hold the database.
    for suffix in ["paper.sqlite-wal", "paper.sqlite-shm"] {
        if session.join(suffix).exists() {
            return Ok(());
        }
    }
    let flags = if apply {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let mut db = rusqlite::Connection::open_with_flags(&db_path, flags)?;
    if !sqlite_table(&db, "ticks")? {
        return Ok(());
    }
    let anchor_path = session.join("book-identity.json");
    let anchor = read_anchor(&anchor_path)?;
    let state = if sqlite_table(&db, "state")? {
        db.query_row("SELECT binding, body FROM state WHERE id=1", [], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()?
    } else {
        None
    };
    if let (Some(anchor), Some((binding, _))) = (&anchor, &state)
        && anchor["binding"].as_str() != Some(binding.as_str())
    {
        return Err("retention_book_identity_mismatch".into());
    }
    let mut rows = Vec::new();
    {
        let mut statement = db.prepare(
            "SELECT id, length(CAST(body AS BLOB)) FROM ticks \
             WHERE event='wide_quotes' AND body IS NOT NULL ORDER BY id",
        )?;
        let mut query = statement.query([])?;
        while let Some(row) = query.next()? {
            rows.push((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as u64));
        }
    }
    let policy = &c.dashboard.retention;
    let ids = quote_rows_to_compact(
        &rows,
        policy.keep_full_quote_bodies,
        policy.max_quote_body_bytes as u64,
    );
    summary.would_compact_quotes = summary.would_compact_quotes.saturating_add(ids.len());
    if !apply || ids.is_empty() {
        return Ok(());
    }
    let permissions = meta.permissions();
    let mut freed = 0u64;
    let tx = db.transaction()?;
    for id in &ids {
        let original: String = tx.query_row(
            "SELECT body FROM ticks WHERE id=?1 AND event='wide_quotes'",
            [id],
            |row| row.get(0),
        )?;
        let receipt = quote_receipt(&original);
        if receipt.len() >= original.len() {
            continue;
        }
        let updated = tx.execute(
            "UPDATE ticks SET body=?1 WHERE id=?2 AND event='wide_quotes'",
            rusqlite::params![receipt, id],
        )?;
        if updated != 1 {
            return Err("retention_quote_update_invalid".into());
        }
        freed = freed.saturating_add((original.len() - receipt.len()) as u64);
        summary.compacted_quotes = summary.compacted_quotes.saturating_add(1);
    }
    if let Some((binding, body)) = &state {
        let after: (String, String) =
            tx.query_row("SELECT binding, body FROM state WHERE id=1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        if after != (binding.clone(), body.clone()) {
            return Err("retention_book_changed".into());
        }
    }
    tx.commit()?;
    db.execute_batch("VACUUM")?;
    if let Some((binding, body)) = &state {
        let after: (String, String) =
            db.query_row("SELECT binding, body FROM state WHERE id=1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        if after != (binding.clone(), body.clone()) {
            return Err("retention_book_changed".into());
        }
    }
    fs::set_permissions(&db_path, permissions)?;
    if let Some(anchor) = anchor {
        let meta = fs::metadata(&db_path)?;
        let mut updated = anchor;
        updated["device"] = serde_json::json!(meta.dev());
        updated["inode"] = serde_json::json!(meta.ino());
        super::rollover::atomic(&anchor_path, &updated)?;
    }
    summary.bytes_freed = summary.bytes_freed.saturating_add(freed);
    Ok(())
}

fn sqlite_table(db: &rusqlite::Connection, name: &str) -> Result<bool> {
    let found: Option<String> = db
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

fn read_anchor(path: &Path) -> Result<Option<Value>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err("retention_book_identity_invalid".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture(root: &Path, date: &str, closed: bool, halted: bool) -> PathBuf {
        let session = root.join(date);
        let run = session.join("model/harness-digest-1000-fixture");
        fs::create_dir_all(&run).unwrap();
        fs::write(session.join("report.json"),serde_json::to_vec(&json!({"paper_only":true,"live_enabled":false,"broker":"upstox","account":"tharun-upstox","session":date,"portfolio":{"closed":closed,"halted":halted,"stopped":false}})).unwrap()).unwrap();
        fs::write(run.join("output.json"), "fixture output ".repeat(8000)).unwrap();
        fs::write(run.join("transcript.ndjson"), b"fixture-transcript\n").unwrap();
        for (name, bytes) in [
            ("runs.ndjson", b"fixture-index".as_slice()),
            ("live-stream.json", b"fixture-stream"),
        ] {
            fs::write(session.join("model").join(name), bytes).unwrap();
        }
        fs::create_dir(session.join("model/prompts")).unwrap();
        fs::write(session.join("model/prompts/prompt.md"), b"fixture-prompt").unwrap();
        // These are inert bytes, not a database or a hash-sealed application book.
        fs::write(session.join("untouched.sqlite"), b"opaque-fixture-sqlite").unwrap();
        fs::write(session.join("untouched-journal"), b"opaque-fixture-journal").unwrap();
        session
    }
    fn tar_files(path: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut data = Vec::new();
        flate2::read::GzDecoder::new(fs::File::open(path).unwrap())
            .read_to_end(&mut data)
            .unwrap();
        let text = |bytes: &[u8]| {
            String::from_utf8(bytes.iter().copied().take_while(|b| *b != 0).collect()).unwrap()
        };
        let mut files = std::collections::BTreeMap::new();
        let mut offset = 0;
        while data[offset..offset + 512].iter().any(|b| *b != 0) {
            let block = &data[offset..offset + 512];
            assert_eq!(&block[257..263], b"ustar\0");
            let expected = u64::from_str_radix(text(&block[148..154]).trim(), 8).unwrap();
            let actual: u64 = block
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    if (148..156).contains(&i) {
                        u64::from(b' ')
                    } else {
                        u64::from(*b)
                    }
                })
                .sum();
            assert_eq!(expected, actual);
            let prefix = text(&block[345..500]);
            let name = if prefix.is_empty() {
                text(&block[..100])
            } else {
                format!("{prefix}/{}", text(&block[..100]))
            };
            let size = usize::from_str_radix(text(&block[124..136]).trim(), 8).unwrap();
            offset += 512;
            if block[156] == b'0' {
                files.insert(name, data[offset..offset + size].to_vec());
            }
            offset += size.div_ceil(512) * 512;
        }
        files
    }
    #[test]
    fn opt_in_retention_is_scoped_closed_only_idempotent_and_preserves_display_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let old = fixture(&root, "2026-10-20", true, false);
        let ancient = fixture(&root, "2026-10-01", true, false);
        let current = fixture(&root, "2026-10-29", true, false);
        let open = fixture(&root, "2026-10-10", false, false);
        let halted = fixture(&root, "2026-10-12", true, true);
        let stopped = fixture(&root, "2026-10-13", true, false);
        let mut report: Value =
            serde_json::from_slice(&fs::read(stopped.join("report.json")).unwrap()).unwrap();
        report["portfolio"]["stopped"] = json!(true);
        fs::write(
            stopped.join("report.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        let legacy = old.join("model/legacy-audit");
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("input.json"), b"fixture-input").unwrap();
        fs::write(legacy.join("failure.json"), b"fixture-failure").unwrap();
        let mut c = super::super::tests::config(&current);
        c.session = "2026-10-29".parse().unwrap();
        let today = c.session;
        assert!(run(&c, today, true).unwrap().packed.is_empty());
        assert!(old.join("model/harness-digest-1000-fixture").exists());
        c.dashboard.retention = RetentionSettings {
            pack_after_days: Some(3),
            delete_after_days: Some(10),
            ..RetentionSettings::default()
        };
        fs::create_dir(old.join("model-archive")).unwrap();
        let temp = old.join("model-archive/2026-10-20.tar.gz.tmp");
        fs::write(&temp, b"interrupted-pack-fixture").unwrap();
        fs::create_dir(ancient.join("model-archive")).unwrap();
        fs::write(
            ancient.join("model-archive/2026-10-01.tar.gz"),
            b"deletion-fixture",
        )
        .unwrap();
        let preview = run(&c, today, false).unwrap();
        assert_eq!(preview.would_pack, vec!["2026-10-20"]);
        assert_eq!(preview.would_delete, vec!["2026-10-01"]);
        assert!(temp.exists());
        assert!(old.join("model/harness-digest-1000-fixture").exists());
        let summary = run(&c, today, true).unwrap();
        assert_eq!(summary.packed, vec!["2026-10-20"]);
        assert_eq!(summary.deleted, vec!["2026-10-01"]);
        assert!(summary.bytes_freed > 0);
        assert!(!temp.exists());
        assert!(!legacy.exists());
        assert!(!old.join("model/harness-digest-1000-fixture").exists());
        let files = tar_files(&old.join("model-archive/2026-10-20.tar.gz"));
        assert_eq!(files["legacy-audit/failure.json"], b"fixture-failure");
        assert_eq!(
            files["harness-digest-1000-fixture/transcript.ndjson"],
            b"fixture-transcript\n"
        );
        for session in [&old, &ancient] {
            assert_eq!(
                fs::read(session.join("model/transcripts/harness-digest-1000-fixture.ndjson"))
                    .unwrap(),
                b"fixture-transcript\n"
            );
            assert_eq!(
                fs::read(session.join("model/runs.ndjson")).unwrap(),
                b"fixture-index"
            );
            assert_eq!(
                fs::read(session.join("model/prompts/prompt.md")).unwrap(),
                b"fixture-prompt"
            );
            assert_eq!(
                fs::read(session.join("model/live-stream.json")).unwrap(),
                b"fixture-stream"
            );
            assert_eq!(
                fs::read(session.join("untouched.sqlite")).unwrap(),
                b"opaque-fixture-sqlite"
            );
            assert_eq!(
                fs::read(session.join("untouched-journal")).unwrap(),
                b"opaque-fixture-journal"
            );
        }
        assert!(!ancient.join("model-archive/2026-10-01.tar.gz").exists());
        for session in [&current, &open, &halted, &stopped] {
            assert!(
                session
                    .join("model/harness-digest-1000-fixture/output.json")
                    .exists()
            );
        }
        let repeated = run(&c, today, true).unwrap();
        assert!(repeated.packed.is_empty() && repeated.deleted.is_empty());
        assert_eq!(repeated.bytes_freed, 0);
    }
    #[test]
    fn refuses_symlinks_and_conflicting_archives_without_removing_originals() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let old = fixture(&root, "2026-10-20", true, false);
        let current = fixture(&root, "2026-10-29", true, false);
        let mut c = super::super::tests::config(&current);
        c.session = "2026-10-29".parse().unwrap();
        c.dashboard.retention.pack_after_days = Some(3);
        let external = dir.path().join("external");
        fs::write(&external, b"external-fixture").unwrap();
        let link = old.join("model/harness-digest-1000-fixture/link");
        std::os::unix::fs::symlink(&external, &link).unwrap();
        assert!(run(&c, c.session, true).is_err());
        assert_eq!(fs::read(&external).unwrap(), b"external-fixture");
        assert!(
            old.join("model/harness-digest-1000-fixture/output.json")
                .exists()
        );
        fs::remove_file(link).unwrap();
        fs::create_dir(old.join("model-archive")).unwrap();
        let archive = old.join("model-archive/2026-10-20.tar.gz");
        fs::write(&archive, b"preserve-original-archive").unwrap();
        assert!(run(&c, c.session, true).is_err());
        assert_eq!(fs::read(archive).unwrap(), b"preserve-original-archive");
        assert!(
            old.join("model/harness-digest-1000-fixture/output.json")
                .exists()
        );
    }

    #[test]
    fn closed_session_bound_keeps_book_journals_and_failures_and_stops_growth() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let old = root.join("2026-10-20");
        let current = root.join("2026-10-29");
        let open = root.join("2026-10-21");
        for path in [&old, &current, &open] {
            fs::create_dir_all(path).unwrap();
        }
        let report = |date: &str, closed: bool| json!({"paper_only":true,"live_enabled":false,"broker":"upstox","account":"tharun-upstox","session":date,"portfolio":{"closed":closed,"halted":false,"stopped":false}});
        fs::write(
            old.join("report.json"),
            serde_json::to_vec(&report("2026-10-20", true)).unwrap(),
        )
        .unwrap();
        fs::write(
            current.join("report.json"),
            serde_json::to_vec(&report("2026-10-29", true)).unwrap(),
        )
        .unwrap();
        fs::write(
            open.join("report.json"),
            serde_json::to_vec(&report("2026-10-21", false)).unwrap(),
        )
        .unwrap();
        let quote_len = 50_000usize;
        let quote = |id: i64| format!("{id:08}{}", "q".repeat(quote_len));
        let db_path = old.join("paper.sqlite");
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE state(id INTEGER PRIMARY KEY CHECK(id=1), binding TEXT NOT NULL, body TEXT NOT NULL);
             CREATE TABLE ticks(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, receipt INTEGER, body TEXT, event TEXT NOT NULL);",
        )
        .unwrap();
        let book = "ORIGINAL-BOOK";
        db.execute("INSERT INTO state VALUES(1, 'bind-1', ?1)", [book])
            .unwrap();
        let audit = "A".repeat(80_000);
        db.execute(
            "INSERT INTO ticks(id,at,receipt,body,event) VALUES(1000, 1, 1, ?1, 'audit')",
            [&audit],
        )
        .unwrap();
        for id in 1..=40 {
            db.execute(
                "INSERT INTO ticks(id,at,receipt,body,event) VALUES(?1, ?1, ?1, ?2, 'wide_quotes')",
                rusqlite::params![id, quote(id)],
            )
            .unwrap();
        }
        drop(db);
        let meta = fs::metadata(&db_path).unwrap();
        fs::write(
            old.join("book-identity.json"),
            serde_json::to_vec(&json!({"device":meta.dev(),"inode":meta.ino(),"binding":"bind-1"}))
                .unwrap(),
        )
        .unwrap();
        let run_dir = old.join("model/harness-digest-1000-fixture");
        fs::create_dir_all(&run_dir).unwrap();
        let plan = "p".repeat(200_000);
        fs::write(run_dir.join("plan.json"), &plan).unwrap();
        let failure = b"failure-artifact";
        let transcript = b"transcript-line\n";
        fs::write(run_dir.join("failure.json"), failure).unwrap();
        fs::write(run_dir.join("transcript.ndjson"), transcript).unwrap();
        let journal = b"shared-journal-bytes";
        let usage = b"usage-journal-bytes";
        let reads = b"read-journal-bytes";
        fs::write(old.join("untouched-journal"), journal).unwrap();
        fs::write(old.join("model-usage-shared.db"), usage).unwrap();
        fs::write(old.join("upstox-shared-reads.db"), reads).unwrap();
        let live_plan = "L".repeat(200_000);
        fs::create_dir_all(current.join("model/harness-digest-1000-live")).unwrap();
        fs::write(
            current.join("model/harness-digest-1000-live/plan.json"),
            &live_plan,
        )
        .unwrap();
        fs::create_dir_all(open.join("model/harness-digest-1000-open")).unwrap();
        fs::write(
            open.join("model/harness-digest-1000-open/plan.json"),
            &live_plan,
        )
        .unwrap();
        let mut c = super::super::tests::config(&current);
        c.session = "2026-10-29".parse().unwrap();
        let today = c.session;
        let preview = run(&c, today, false).unwrap();
        assert_eq!(preview.would_compact_quotes, 8);
        assert_eq!(preview.would_compact_model_files, 1);
        assert_eq!(preview.compacted_quotes, 0);
        assert_eq!(preview.compacted_model_files, 0);
        assert_eq!(
            fs::read(run_dir.join("plan.json")).unwrap(),
            plan.as_bytes()
        );
        let before_len = fs::metadata(&db_path).unwrap().len();
        let summary = run(&c, today, true).unwrap();
        assert_eq!(summary.compacted_quotes, 8);
        assert_eq!(summary.compacted_model_files, 1);
        assert!(summary.bytes_freed > 0);
        assert!(fs::metadata(&db_path).unwrap().len() < before_len);
        let db = rusqlite::Connection::open(&db_path).unwrap();
        let (binding, body): (String, String) = db
            .query_row("SELECT binding, body FROM state WHERE id=1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(binding, "bind-1");
        assert_eq!(body, book);
        let stored_audit: String = db
            .query_row("SELECT body FROM ticks WHERE event='audit'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(stored_audit, audit);
        let mut full = 0usize;
        let mut logical = 0u64;
        let mut statement = db
            .prepare("SELECT id, body FROM ticks WHERE event='wide_quotes' ORDER BY id")
            .unwrap();
        let rows: Vec<(i64, String)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        drop(statement);
        assert_eq!(rows.len(), 40);
        for (id, body) in &rows {
            logical += body.len() as u64;
            if body.len() > 4096 {
                full += 1;
                assert_eq!(body, &quote(*id));
                assert!(*id > 8);
            } else {
                assert_eq!(body, &quote_receipt(&quote(*id)));
                assert!(*id <= 8);
            }
        }
        assert_eq!(full, 32);
        assert!(logical < 32 * (quote_len as u64 + 16) + 8 * 512);
        drop(db);
        let anchor: Value =
            serde_json::from_slice(&fs::read(old.join("book-identity.json")).unwrap()).unwrap();
        let meta = fs::metadata(&db_path).unwrap();
        assert_eq!(anchor["binding"], "bind-1");
        assert_eq!(anchor["device"], json!(meta.dev()));
        assert_eq!(anchor["inode"], json!(meta.ino()));
        assert_eq!(fs::read(run_dir.join("failure.json")).unwrap(), failure);
        assert_eq!(
            fs::read(run_dir.join("transcript.ndjson")).unwrap(),
            transcript
        );
        assert_eq!(fs::read(old.join("untouched-journal")).unwrap(), journal);
        assert_eq!(fs::read(old.join("model-usage-shared.db")).unwrap(), usage);
        assert_eq!(fs::read(old.join("upstox-shared-reads.db")).unwrap(), reads);
        let stored: Value =
            serde_json::from_slice(&fs::read(run_dir.join("plan.json")).unwrap()).unwrap();
        assert_eq!(stored["kind"], "compacted_model_artifact");
        assert_eq!(stored["sha256"], super::super::digest(plan.as_bytes()));
        assert!(fs::metadata(run_dir.join("plan.json")).unwrap().len() < 512);
        assert_eq!(
            fs::read(current.join("model/harness-digest-1000-live/plan.json")).unwrap(),
            live_plan.as_bytes()
        );
        assert_eq!(
            fs::read(open.join("model/harness-digest-1000-open/plan.json")).unwrap(),
            live_plan.as_bytes()
        );
        let db = rusqlite::Connection::open(&db_path).unwrap();
        for id in 41..=56 {
            db.execute(
                "INSERT INTO ticks(id,at,receipt,body,event) VALUES(?1, ?1, ?1, ?2, 'wide_quotes')",
                rusqlite::params![id, quote(id)],
            )
            .unwrap();
        }
        // A research-observation receipt is a later session's original
        // evidence (#240): retention never compacts it, however large.
        db.execute(
            "INSERT INTO ticks(id,at,receipt,body,event) VALUES(57, 57, 57, ?1, 'research_quotes')",
            [quote(57)],
        )
        .unwrap();
        drop(db);
        let again = run(&c, today, true).unwrap();
        assert_eq!(again.compacted_quotes, 16);
        let db = rusqlite::Connection::open(&db_path).unwrap();
        let research: String = db
            .query_row("SELECT body FROM ticks WHERE id=57", [], |row| row.get(0))
            .unwrap();
        assert_eq!(research, quote(57));
        let full: i64 = db
            .query_row(
                "SELECT count(*) FROM ticks WHERE event='wide_quotes' AND length(CAST(body AS BLOB)) > 4096",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let total: i64 = db
            .query_row(
                "SELECT count(*) FROM ticks WHERE event='wide_quotes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total, 56);
        assert_eq!(full, 32);
        let (binding, body): (String, String) = db
            .query_row("SELECT binding, body FROM state WHERE id=1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!((binding.as_str(), body.as_str()), ("bind-1", book));
        let logical: i64 = db
            .query_row(
                "SELECT coalesce(sum(length(CAST(body AS BLOB))), 0) FROM ticks WHERE event='wide_quotes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(logical < 56 * (quote_len as i64 + 8) + 80_000);
        assert!(logical < 32 * (quote_len as i64 + 16) + 24 * 512 + 80_000);
        drop(db);
        let anchor: Value =
            serde_json::from_slice(&fs::read(old.join("book-identity.json")).unwrap()).unwrap();
        let meta = fs::metadata(&db_path).unwrap();
        assert_eq!(anchor["binding"], "bind-1");
        assert_eq!(anchor["inode"], json!(meta.ino()));
        assert_eq!(fs::read(run_dir.join("failure.json")).unwrap(), failure);
        assert_eq!(
            fs::read(run_dir.join("transcript.ndjson")).unwrap(),
            transcript
        );
        assert_eq!(fs::read(old.join("untouched-journal")).unwrap(), journal);
    }
}
