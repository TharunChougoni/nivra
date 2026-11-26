//! Opt-in model-archive retention, plus a default size bound for closed sibling
//! sessions. Pack and delete never open a book, journal or credential. The size
//! bound rewrites oversized `wide_quotes` bodies and large model artifacts in
//! past closed sessions only. It leaves the configured session directory, the
//! `state` book row, shared journals and failed artifacts unchanged.
use super::*;
use nivra_core::market::MAX_FINANCIAL_AGE_MS;
use nivra_store::market_archive::{ArchivedTickCheck, quote_reference};
use rusqlite::OptionalExtension;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
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
    would_rewrite_quotes: usize,
    rewritten_quotes: usize,
    kept_missing_archive_quotes: usize,
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
    let mut lookup = RealArchiveLookup {
        config: c,
        client: None,
    };
    bound_closed_sessions_with_lookup(c, today, apply, summary, &mut lookup)
}

fn bound_closed_sessions_with_lookup(
    c: &Config,
    today: NaiveDate,
    apply: bool,
    summary: &mut Summary,
    lookup: &mut impl ArchiveLookup,
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
        compact_quote_bodies(&session, c, apply, summary, lookup)?;
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

trait ArchiveLookup {
    fn verify_ticks(
        &mut self,
        binding: Option<&str>,
        ticks: &[ArchivedTickCheck],
    ) -> Result<Vec<bool>>;
}

struct RealArchiveLookup<'a> {
    config: &'a Config,
    client: Option<nivra_store::market_archive::ArchiveClient>,
}

impl ArchiveLookup for RealArchiveLookup<'_> {
    fn verify_ticks(
        &mut self,
        binding: Option<&str>,
        ticks: &[ArchivedTickCheck],
    ) -> Result<Vec<bool>> {
        if ticks.is_empty() {
            return Ok(Vec::new());
        }
        if self.client.is_none() {
            self.client = Some(super::market_archive::open(self.config)?);
        }
        let client = self.client.as_mut().expect("archive client");
        nivra_store::market_archive::verify_book_receipts(client, binding, ticks)
    }
}

#[derive(Deserialize)]
struct QuoteEnvelope {
    purpose: String,
    received_ms: i64,
    body: String,
}

#[derive(Default)]
struct ProtectedCitations {
    exact_tick_ats: BTreeSet<i64>,
    receipt_windows: Vec<(i64, i64, Option<String>)>,
    key_windows: BTreeMap<String, Vec<(i64, i64)>>,
}

impl ProtectedCitations {
    fn load(db: &rusqlite::Connection, state_body: Option<&str>) -> Result<Self> {
        let mut citations = Self::default();
        if let Some(body) = state_body
            && let Ok(portfolio) = serde_json::from_str::<nivra_core::execution::Portfolio>(body)
        {
            let mut fills: Vec<&nivra_core::execution::Fill> = portfolio.fills.iter().collect();
            fills.sort_by_key(|fill| fill.at);
            let mut open_units: BTreeMap<String, u64> = BTreeMap::new();
            let mut open_since: BTreeMap<String, i64> = BTreeMap::new();
            for fill in fills {
                citations.exact_tick_ats.insert(fill.at);
                citations.receipt_windows.push((
                    fill.at.saturating_sub(MAX_FINANCIAL_AGE_MS),
                    fill.at,
                    Some(fill.instrument.key.clone()),
                ));
                let key = &fill.instrument.key;
                let units = open_units.entry(key.clone()).or_insert(0);
                if fill.buy {
                    if *units == 0 {
                        open_since.insert(key.clone(), fill.at);
                    }
                    *units = units.saturating_add(fill.units);
                } else if *units == 0 {
                    citations
                        .key_windows
                        .entry(key.clone())
                        .or_default()
                        .push((i64::MIN, fill.at));
                } else {
                    *units = units.saturating_sub(fill.units);
                    if *units == 0
                        && let Some(start) = open_since.remove(key)
                    {
                        citations
                            .key_windows
                            .entry(key.clone())
                            .or_default()
                            .push((start, fill.at));
                    }
                }
            }
            for (key, start) in open_since {
                citations
                    .key_windows
                    .entry(key)
                    .or_default()
                    .push((start, i64::MAX));
            }
            for key in portfolio.positions.keys() {
                if open_units.get(key).is_none_or(|units| *units == 0) {
                    citations
                        .key_windows
                        .entry(key.clone())
                        .or_default()
                        .push((i64::MIN, i64::MAX));
                }
            }
        }
        if sqlite_table(db, "execution_refusal")? {
            let mut stmt =
                db.prepare("SELECT at, source, source_id, body FROM execution_refusal")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let at: i64 = row.get(0)?;
                let source: String = row.get(1)?;
                let source_id: String = row.get(2)?;
                let body: String = row.get(3)?;
                citations.exact_tick_ats.insert(at);
                if source == "strategy"
                    && let Some((_, observed)) = source_id.rsplit_once('@')
                    && let Ok(observed_at) = observed.parse::<i64>()
                {
                    let key = serde_json::from_str::<Value>(&body)
                        .ok()
                        .and_then(|v| v["action"]["instrument_key"].as_str().map(str::to_owned));
                    citations.receipt_windows.push((
                        observed_at.saturating_sub(MAX_FINANCIAL_AGE_MS),
                        observed_at,
                        key,
                    ));
                }
            }
        }
        if sqlite_table(db, "strategy_action")? {
            let mut stmt =
                db.prepare("SELECT at, observed_at, instrument_key FROM strategy_action")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let at: i64 = row.get(0)?;
                let observed_at: i64 = row.get(1)?;
                let key: Option<String> = row.get(2)?;
                citations.exact_tick_ats.insert(at);
                citations.receipt_windows.push((
                    observed_at.saturating_sub(MAX_FINANCIAL_AGE_MS),
                    observed_at,
                    key,
                ));
            }
        }
        if sqlite_table(db, "planner_plan")? {
            let mut stmt = db.prepare("SELECT body FROM planner_plan")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let body: String = row.get(0)?;
                let Ok(plan) = serde_json::from_str::<Value>(&body) else {
                    continue;
                };
                let start = plan["input"]["as_of_ms"].as_i64().unwrap_or(i64::MIN);
                let end = plan["valid_until_ms"].as_i64().unwrap_or(i64::MAX);
                for verdict in plan["verdicts"].as_array().into_iter().flatten() {
                    if let Some(key) = verdict["action"]["instrument_key"].as_str() {
                        citations
                            .key_windows
                            .entry(key.to_owned())
                            .or_default()
                            .push((start, end));
                    }
                }
            }
        }
        Ok(citations)
    }

    fn protects(&self, at: i64, receipt: Option<i64>, inner_body: &str) -> bool {
        if self.exact_tick_ats.contains(&at) {
            return true;
        }
        let mut candidate_keys: BTreeSet<&str> = BTreeSet::new();
        if let Some(receipt) = receipt {
            for (from, to, key) in &self.receipt_windows {
                if *from <= receipt && receipt <= *to {
                    match key.as_deref() {
                        Some(key) => {
                            candidate_keys.insert(key);
                        }
                        None => return true,
                    }
                }
            }
        }
        for (key, windows) in &self.key_windows {
            if windows.iter().any(|(from, to)| *from <= at && at <= *to) {
                candidate_keys.insert(key.as_str());
            }
        }
        if candidate_keys.is_empty() {
            return false;
        }
        let Ok(body) = serde_json::from_str::<Value>(inner_body) else {
            return false;
        };
        let Some(rows) = body["data"].as_object() else {
            return false;
        };
        rows.values().any(|row| {
            row["instrument_token"]
                .as_str()
                .is_some_and(|token| candidate_keys.contains(token))
        })
    }
}

fn compact_quote_bodies(
    session: &Path,
    c: &Config,
    apply: bool,
    summary: &mut Summary,
    lookup: &mut impl ArchiveLookup,
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
    if c.market_archive.is_some() {
        compact_quote_bodies_with_archive(
            &db_path,
            &anchor_path,
            meta,
            &mut db,
            anchor,
            state,
            apply,
            summary,
            lookup,
        )
    } else {
        compact_quote_bodies_offline(
            &db_path,
            &anchor_path,
            meta,
            &mut db,
            anchor,
            state,
            c,
            apply,
            summary,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn compact_quote_bodies_offline(
    db_path: &Path,
    anchor_path: &Path,
    meta: fs::Metadata,
    db: &mut rusqlite::Connection,
    anchor: Option<Value>,
    state: Option<(String, String)>,
    c: &Config,
    apply: bool,
    summary: &mut Summary,
) -> Result<()> {
    let mut rows = Vec::new();
    {
        let protected_filter = if sqlite_table(db, "archive_protected_receipts")? {
            "AND id NOT IN (SELECT tick_id FROM archive_protected_receipts)"
        } else {
            ""
        };
        let mut statement = db.prepare(&format!(
            "SELECT id, length(CAST(body AS BLOB)) FROM ticks
             WHERE event='wide_quotes' AND body IS NOT NULL
             AND body NOT LIKE '{{\"kind\":\"market_archive_quote\"%' {protected_filter} ORDER BY id",
        ))?;
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
    finish_quote_compaction(
        db_path,
        anchor_path,
        meta,
        db,
        anchor,
        state.as_ref(),
        summary,
        |tx, summary| {
            let mut freed = 0u64;
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
            Ok(freed)
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn compact_quote_bodies_with_archive(
    db_path: &Path,
    anchor_path: &Path,
    meta: fs::Metadata,
    db: &mut rusqlite::Connection,
    anchor: Option<Value>,
    state: Option<(String, String)>,
    apply: bool,
    summary: &mut Summary,
    lookup: &mut impl ArchiveLookup,
) -> Result<()> {
    if let Some((_, body)) = &state
        && let Ok(portfolio) = serde_json::from_str::<nivra_core::execution::Portfolio>(body)
        && !portfolio.closed
    {
        return Ok(());
    }
    let citations = ProtectedCitations::load(db, state.as_ref().map(|(_, body)| body.as_str()))?;
    let mut checks = Vec::new();
    let mut candidates = Vec::new();
    {
        let protected_filter = if sqlite_table(db, "archive_protected_receipts")? {
            "AND id NOT IN (SELECT tick_id FROM archive_protected_receipts)"
        } else {
            ""
        };
        let mut statement = db.prepare(&format!(
            "SELECT id, at, receipt, body, event FROM ticks
             WHERE event IN ('wide_quotes', 'scanner_metadata', 'research_quotes')
             AND body IS NOT NULL
             AND body NOT LIKE '{{\"kind\":\"market_archive_quote\"%'
             AND body NOT LIKE '{{\"kind\":\"compacted_quote_receipt\"%'
             {protected_filter}
             ORDER BY id",
        ))?;
        let mut query = statement.query([])?;
        while let Some(row) = query.next()? {
            let id: i64 = row.get(0)?;
            let at: i64 = row.get(1)?;
            let receipt: Option<i64> = row.get(2)?;
            let raw: String = row.get(3)?;
            let event: String = row.get(4)?;
            let Ok(envelope) = serde_json::from_str::<QuoteEnvelope>(&raw) else {
                summary.kept_missing_archive_quotes =
                    summary.kept_missing_archive_quotes.saturating_add(1);
                continue;
            };
            if !matches!(
                envelope.purpose.as_str(),
                "scanner" | "execution_or_protection" | "scanner_metadata" | "research_observation"
            ) {
                summary.kept_missing_archive_quotes =
                    summary.kept_missing_archive_quotes.saturating_add(1);
                continue;
            }
            if event == "wide_quotes"
                && matches!(
                    envelope.purpose.as_str(),
                    "scanner" | "execution_or_protection"
                )
                && citations.protects(at, receipt, &envelope.body)
            {
                continue;
            }
            let reference = quote_reference(&envelope.purpose, &raw);
            if reference.len() >= raw.len() {
                continue;
            }
            let Some(receipt_ms) = receipt.filter(|r| *r == envelope.received_ms) else {
                summary.kept_missing_archive_quotes =
                    summary.kept_missing_archive_quotes.saturating_add(1);
                continue;
            };
            let sha256: [u8; 32] = Sha256::digest(raw.as_bytes()).into();
            checks.push(ArchivedTickCheck {
                tick_id: id,
                event_ms: at,
                receipt_ms,
                sha256,
            });
            candidates.push((id, reference, raw.len()));
        }
    }
    if checks.is_empty() {
        return Ok(());
    }
    let binding = state
        .as_ref()
        .map(|(binding, _)| binding.as_str())
        .or_else(|| anchor.as_ref().and_then(|a| a["binding"].as_str()));
    let verified = lookup.verify_ticks(binding, &checks)?;
    if verified.len() != candidates.len() {
        return Err("retention_archive_verify_invalid".into());
    }
    let mut to_rewrite = Vec::new();
    for ((id, reference, original_len), ok) in candidates.into_iter().zip(verified) {
        if ok {
            to_rewrite.push((id, reference, original_len));
        } else {
            summary.kept_missing_archive_quotes =
                summary.kept_missing_archive_quotes.saturating_add(1);
        }
    }
    summary.would_rewrite_quotes = summary
        .would_rewrite_quotes
        .saturating_add(to_rewrite.len());
    if !apply || to_rewrite.is_empty() {
        return Ok(());
    }
    finish_quote_compaction(
        db_path,
        anchor_path,
        meta,
        db,
        anchor,
        state.as_ref(),
        summary,
        |tx, summary| {
            let mut freed = 0u64;
            for (id, reference, original_len) in to_rewrite {
                let updated = tx.execute(
                    "UPDATE ticks SET body=?1
                     WHERE id=?2 AND event IN ('wide_quotes', 'scanner_metadata', 'research_quotes')",
                    rusqlite::params![reference, id],
                )?;
                if updated != 1 {
                    return Err("retention_quote_update_invalid".into());
                }
                freed = freed.saturating_add((original_len - reference.len()) as u64);
                summary.rewritten_quotes = summary.rewritten_quotes.saturating_add(1);
            }
            Ok(freed)
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_quote_compaction(
    db_path: &Path,
    anchor_path: &Path,
    meta: fs::Metadata,
    db: &mut rusqlite::Connection,
    anchor: Option<Value>,
    state: Option<&(String, String)>,
    summary: &mut Summary,
    write: impl FnOnce(&rusqlite::Transaction<'_>, &mut Summary) -> Result<u64>,
) -> Result<()> {
    let permissions = meta.permissions();
    let tx = db.transaction()?;
    let freed = write(&tx, summary)?;
    if let Some((binding, body)) = state {
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
    if let Some((binding, body)) = state {
        let after: (String, String) =
            db.query_row("SELECT binding, body FROM state WHERE id=1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        if after != (binding.clone(), body.clone()) {
            return Err("retention_book_changed".into());
        }
    }
    fs::set_permissions(db_path, permissions)?;
    if let Some(anchor) = anchor {
        let meta = fs::metadata(db_path)?;
        let mut updated = anchor;
        updated["device"] = serde_json::json!(meta.dev());
        updated["inode"] = serde_json::json!(meta.ino());
        super::rollover::atomic(anchor_path, &updated)?;
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
             CREATE TABLE ticks(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, receipt INTEGER, body TEXT, event TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS ticks_event_receipt_idx ON ticks(event, receipt, id);
             CREATE INDEX IF NOT EXISTS ticks_event_at_idx ON ticks(event, at, id);",
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

    #[test]
    fn archive_retention_rewrites_verified_bulk_quotes_and_keeps_missing_and_cited() {
        use std::os::unix::fs::MetadataExt;
        struct FakeArchiveLookup {
            known: std::collections::BTreeSet<(String, i64, i64, i64, [u8; 32])>,
        }
        impl ArchiveLookup for FakeArchiveLookup {
            fn verify_ticks(
                &mut self,
                binding: Option<&str>,
                ticks: &[ArchivedTickCheck],
            ) -> Result<Vec<bool>> {
                let binding = binding.unwrap_or_default().to_owned();
                Ok(ticks
                    .iter()
                    .map(|tick| {
                        self.known.contains(&(
                            binding.clone(),
                            tick.tick_id,
                            tick.event_ms,
                            tick.receipt_ms,
                            tick.sha256,
                        ))
                    })
                    .collect())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let old = root.join("2026-10-20");
        let current = root.join("2026-10-29");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&current).unwrap();
        let report = |date: &str| {
            json!({
                "paper_only":true,"live_enabled":false,"broker":"upstox",
                "account":"tharun-upstox","session":date,
                "portfolio":{"closed":true,"halted":false,"stopped":false}
            })
        };
        fs::write(
            old.join("report.json"),
            serde_json::to_vec(&report("2026-10-20")).unwrap(),
        )
        .unwrap();
        fs::write(
            current.join("report.json"),
            serde_json::to_vec(&report("2026-10-29")).unwrap(),
        )
        .unwrap();

        let mut portfolio = super::super::tests::test_portfolio();
        portfolio.closed = true;
        portfolio.fills.push(nivra_core::execution::Fill {
            at: 6,
            instrument: nivra_core::execution::Instrument {
                key: "NSE_EQ|HELD".into(),
                symbol: "HELD".into(),
                series: "EQ".into(),
                tick: nivra_core::finance::Price::parse("0.05").unwrap(),
            },
            units: 10,
            buy: true,
            price: nivra_core::finance::Price::parse("100.00").unwrap(),
            fee: nivra_core::finance::Amount::ZERO,
            expected_price: None,
            slippage_bps: None,
            decision_id: None,
            costs: None,
        });
        portfolio.fills.push(nivra_core::execution::Fill {
            at: 8,
            instrument: nivra_core::execution::Instrument {
                key: "NSE_EQ|HELD".into(),
                symbol: "HELD".into(),
                series: "EQ".into(),
                tick: nivra_core::finance::Price::parse("0.05").unwrap(),
            },
            units: 10,
            buy: false,
            price: nivra_core::finance::Price::parse("101.00").unwrap(),
            fee: nivra_core::finance::Amount::ZERO,
            expected_price: None,
            slippage_bps: None,
            decision_id: None,
            costs: None,
        });
        let state_body = serde_json::to_string(&portfolio).unwrap();

        let envelope = |purpose: &str, receipt: i64, token: &str| {
            let padding = "x".repeat(4000);
            let inner = json!({
                "status": "success",
                "data": {
                    token: {
                        "instrument_token": token,
                        "pad": padding
                    }
                }
            })
            .to_string();
            json!({
                "source": "upstox-readonly-v1",
                "purpose": purpose,
                "requested_ms": receipt - 1,
                "received_ms": receipt,
                "body": inner
            })
            .to_string()
        };

        let db_path = old.join("paper.sqlite");
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE state(id INTEGER PRIMARY KEY CHECK(id=1), binding TEXT NOT NULL, body TEXT NOT NULL);
             CREATE TABLE ticks(id INTEGER PRIMARY KEY, at INTEGER NOT NULL, receipt INTEGER, body TEXT, event TEXT NOT NULL);
             CREATE TABLE planner_plan(id TEXT PRIMARY KEY, body TEXT NOT NULL);",
        )
        .unwrap();
        db.execute(
            "INSERT INTO state VALUES(1, 'bind-archive', ?1)",
            [&state_body],
        )
        .unwrap();
        db.execute(
            "INSERT INTO planner_plan VALUES('plan-1', ?1)",
            [json!({
                "decision_id": "plan-1",
                "input": {"as_of_ms": 10},
                "valid_until_ms": 20,
                "verdicts": [{"index": 0, "action": {"instrument_key": "NSE_EQ|PLAN"}}]
            })
            .to_string()],
        )
        .unwrap();

        let rows = [
            (
                1_i64,
                1_i64,
                "scanner_metadata",
                "scanner_metadata",
                "NSE_EQ|BULK",
                true,
            ),
            (
                2,
                2,
                "scanner_metadata",
                "scanner_metadata",
                "NSE_EQ|MISS",
                false,
            ),
            (3, 3, "wide_quotes", "scanner", "NSE_EQ|BULK", true),
            (4, 4, "wide_quotes", "scanner", "NSE_EQ|MISS", false),
            (
                5,
                5,
                "wide_quotes",
                "execution_or_protection",
                "NSE_EQ|BULK",
                true,
            ),
            (
                6,
                6,
                "wide_quotes",
                "execution_or_protection",
                "NSE_EQ|HELD",
                true,
            ),
            (
                7,
                7,
                "wide_quotes",
                "execution_or_protection",
                "NSE_EQ|HELD",
                true,
            ),
            (15, 15, "wide_quotes", "scanner", "NSE_EQ|PLAN", true),
            (
                20,
                20,
                "research_quotes",
                "research_observation",
                "NSE_EQ|RES",
                true,
            ),
        ];
        let mut known = std::collections::BTreeSet::new();
        let mut originals = std::collections::BTreeMap::new();
        for (id, at, event, purpose, token, in_archive) in rows {
            let body = envelope(purpose, at, token);
            db.execute(
                "INSERT INTO ticks(id, at, receipt, body, event) VALUES(?1, ?2, ?2, ?3, ?4)",
                rusqlite::params![id, at, body, event],
            )
            .unwrap();
            if in_archive {
                known.insert((
                    "bind-archive".to_owned(),
                    id,
                    at,
                    at,
                    Sha256::digest(body.as_bytes()).into(),
                ));
            }
            originals.insert(id, body);
        }
        drop(db);

        let meta = fs::metadata(&db_path).unwrap();
        fs::write(
            old.join("book-identity.json"),
            serde_json::to_vec(
                &json!({"device": meta.dev(), "inode": meta.ino(), "binding": "bind-archive"}),
            )
            .unwrap(),
        )
        .unwrap();

        let mut c = super::super::tests::config(&current);
        c.session = "2026-10-29".parse().unwrap();
        c.market_archive = Some(MarketArchiveSettings {
            url: "postgresql://nivra@127.0.0.1/nivra_market".into(),
            password_file: dir.path().join("unused-password"),
        });
        let today = c.session;
        let mut lookup = FakeArchiveLookup { known };

        let before_bytes = fs::read(&db_path).unwrap();
        let mut preview = Summary {
            dry_run: true,
            ..Summary::default()
        };
        bound_closed_sessions_with_lookup(&c, today, false, &mut preview, &mut lookup).unwrap();
        assert_eq!(preview.would_rewrite_quotes, 4);
        assert_eq!(preview.kept_missing_archive_quotes, 2);
        assert_eq!(preview.rewritten_quotes, 0);
        assert_eq!(preview.bytes_freed, 0);
        assert_eq!(fs::read(&db_path).unwrap(), before_bytes);

        // Leftover WAL skips rewrite cleanly.
        let wal_path = old.join("paper.sqlite-wal");
        fs::write(&wal_path, b"wal-in-use").unwrap();
        let mut wal_summary = Summary::default();
        bound_closed_sessions_with_lookup(&c, today, true, &mut wal_summary, &mut lookup).unwrap();
        assert_eq!(wal_summary.rewritten_quotes, 0);
        assert_eq!(wal_summary.kept_missing_archive_quotes, 0);
        fs::remove_file(&wal_path).unwrap();

        let mut applied = Summary::default();
        bound_closed_sessions_with_lookup(&c, today, true, &mut applied, &mut lookup).unwrap();
        assert_eq!(applied.would_rewrite_quotes, 4);
        assert_eq!(applied.rewritten_quotes, 4);
        assert_eq!(applied.kept_missing_archive_quotes, 2);
        assert!(applied.bytes_freed > 0);
        assert!(fs::metadata(&db_path).unwrap().len() < before_bytes.len() as u64);

        let db = rusqlite::Connection::open(&db_path).unwrap();
        for (id, purpose) in [
            (1_i64, "scanner_metadata"),
            (3, "scanner"),
            (5, "execution_or_protection"),
            (20, "research_observation"),
        ] {
            let body: String = db
                .query_row("SELECT body FROM ticks WHERE id=?1", [id], |r| r.get(0))
                .unwrap();
            assert_eq!(body, quote_reference(purpose, &originals[&id]));
        }
        // Missing from archive (2, 4) and cited executable receipts (6, 7, 15) stay untouched.
        for id in [2_i64, 4, 6, 7, 15] {
            let body: String = db
                .query_row("SELECT body FROM ticks WHERE id=?1", [id], |r| r.get(0))
                .unwrap();
            assert_eq!(body, originals[&id]);
        }
        let stored_state: String = db
            .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored_state, state_body);
        drop(db);

        let anchor: Value =
            serde_json::from_slice(&fs::read(old.join("book-identity.json")).unwrap()).unwrap();
        let meta = fs::metadata(&db_path).unwrap();
        assert_eq!(anchor["device"], json!(meta.dev()));
        assert_eq!(anchor["inode"], json!(meta.ino()));
    }

    #[test]
    #[ignore = "requires TimescaleDB at NIVRA_ARCHIVE_TEST_URL"]
    fn archive_import_and_retention_shrink_book_and_preserve_readers() -> Result<()> {
        use crate::runtime::{market_archive, paper_replay};
        use std::os::unix::fs::MetadataExt;

        let test_url = std::env::var("NIVRA_ARCHIVE_TEST_URL")?;
        let (scheme, auth_host) = test_url.split_once("://").ok_or("fixture_url")?;
        let (auth, host) = auth_host.rsplit_once('@').ok_or("fixture_url")?;
        let (user, password) = auth.split_once(':').ok_or("fixture_url")?;

        let tmp = tempfile::tempdir()?;
        let password_file = tmp.path().join("archive-password");
        fs::OpenOptions::new()
            .create_new(true)
            .mode(0o600)
            .write(true)
            .open(&password_file)?
            .write_all(password.as_bytes())?;
        let settings = MarketArchiveSettings {
            url: format!("{scheme}://{user}@{host}"),
            password_file,
        };
        let mut archive_db = market_archive::connect(&settings)?;
        nivra_store::market_archive::init(&mut archive_db)?;

        let sessions_root = tmp.path().join("sessions");
        let old_dir = sessions_root.join("2026-10-20");
        let current_dir = sessions_root.join("2026-10-29");
        fs::create_dir_all(&old_dir)?;
        fs::create_dir_all(&current_dir)?;

        let mut old_cfg = super::super::tests::config(&old_dir);
        old_cfg.session = "2026-10-20".parse()?;
        old_cfg.previous_session = "2026-10-19".parse()?;
        old_cfg.market_archive = Some(settings.clone());

        let mut current_cfg = super::super::tests::config(&current_dir);
        current_cfg.session = "2026-10-29".parse()?;
        current_cfg.previous_session = "2026-10-28".parse()?;
        current_cfg.market_archive = Some(settings.clone());

        let nonce = millis();
        let instruments = (0..40)
            .map(|i| {
                json!({
                    "key": format!("NSE_EQ|SYNTHETIC{nonce}-{i}"),
                    "symbol": format!("SYNTHETIC{i}"),
                    "series": "EQ",
                    "tick": "0.05"
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            old_dir.join("wide-universe.json"),
            json!({"session": old_cfg.session, "instruments": instruments}).to_string(),
        )?;

        let template: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/upstox_full_quote_doc_example.json"
        ))?;
        let quote_body = |count: usize, received: i64| {
            let data: serde_json::Map<String, Value> = instruments
                .iter()
                .take(count)
                .map(|instrument| {
                    let key = instrument["key"].as_str().unwrap();
                    let mut row = template["data"]["NSE_EQ:NHPC"].clone();
                    row["instrument_token"] = json!(key);
                    row["symbol"] = instrument["symbol"].clone();
                    row["timestamp"] = json!(received);
                    row["depth"]["sell"] = json!([{"price": 52.10, "quantity": 100}]);
                    (key.replace('|', ":"), row)
                })
                .collect();
            json!({"status": "success", "data": data}).to_string()
        };

        let base = old_cfg
            .session
            .and_hms_opt(4, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
            + (nonce % 1_000_000);
        let fill_at = base + 20 * 10_000 + 5_000;
        let fill_key = instruments[0]["key"].as_str().unwrap().to_owned();

        let mut portfolio = super::super::tests::test_portfolio();
        portfolio.closed = true;
        portfolio.fills.push(nivra_core::execution::Fill {
            at: fill_at,
            instrument: nivra_core::execution::Instrument {
                key: fill_key.clone(),
                symbol: "SYNTHETIC0".into(),
                series: "EQ".into(),
                tick: nivra_core::finance::Price::parse("0.05").unwrap(),
            },
            units: 5,
            buy: true,
            price: nivra_core::finance::Price::parse("52.10").unwrap(),
            fee: nivra_core::finance::Amount::ZERO,
            expected_price: None,
            slippage_bps: None,
            decision_id: None,
            costs: None,
        });
        portfolio.fills.push(nivra_core::execution::Fill {
            at: fill_at + 100,
            instrument: nivra_core::execution::Instrument {
                key: fill_key.clone(),
                symbol: "SYNTHETIC0".into(),
                series: "EQ".into(),
                tick: nivra_core::finance::Price::parse("0.05").unwrap(),
            },
            units: 5,
            buy: false,
            price: nivra_core::finance::Price::parse("52.20").unwrap(),
            fee: nivra_core::finance::Amount::ZERO,
            expected_price: None,
            slippage_bps: None,
            decision_id: None,
            costs: None,
        });
        let binding = super::super::book_binding(&old_cfg)?;
        let state_body = serde_json::to_string(&portfolio)?;

        let db = super::super::open_book(&old_cfg)?;
        db.execute(
            "INSERT INTO state VALUES(1, ?1, ?2)",
            rusqlite::params![binding, state_body],
        )?;
        let mut last = base;
        let mut total_quote_ticks = 0_i64;
        for i in 0..20_i64 {
            let receipt = base + i * 10_000;
            last = receipt + 2;
            let raw = json!({
                "source": "upstox-readonly-v1",
                "purpose": "scanner_metadata",
                "requested_ms": receipt - 1,
                "received_ms": receipt,
                "body": quote_body(40, receipt)
            })
            .to_string();
            db.execute(
                "INSERT INTO ticks(at, receipt, body, event) VALUES(?1, ?2, ?3, 'scanner_metadata')",
                rusqlite::params![last, receipt, raw],
            )?;
            total_quote_ticks += 1;

            let scan_receipt = receipt + 4_000;
            last = scan_receipt + 2;
            let scan_raw = json!({
                "source": "upstox-readonly-v1",
                "purpose": "scanner",
                "requested_ms": scan_receipt - 1,
                "received_ms": scan_receipt,
                "body": quote_body(25, scan_receipt)
            })
            .to_string();
            db.execute(
                "INSERT INTO ticks(at, receipt, body, event) VALUES(?1, ?2, ?3, 'wide_quotes')",
                rusqlite::params![last, scan_receipt, scan_raw],
            )?;
            total_quote_ticks += 1;
        }
        // One executable quote tick cited by the fill in `portfolio.fills`.
        let fill_raw = json!({
            "source": "upstox-readonly-v1",
            "purpose": "execution_or_protection",
            "requested_ms": fill_at - 1,
            "received_ms": fill_at,
            "body": quote_body(5, fill_at)
        })
        .to_string();
        db.execute(
            "INSERT INTO ticks(at, receipt, body, event) VALUES(?1, ?2, ?3, 'wide_quotes')",
            rusqlite::params![fill_at, fill_at, fill_raw],
        )?;
        let fill_tick_id = db.last_insert_rowid();
        total_quote_ticks += 1;
        last = last.max(fill_at + 200);

        // Research-observation ticks are bulk-archived too when market_archive is configured.
        for i in 0..2_i64 {
            let receipt = last + 1_000 + i * 1_000;
            last = receipt + 2;
            let res_raw = json!({
                "source": "upstox-readonly-v1",
                "purpose": "research_observation",
                "requested_ms": receipt - 1,
                "received_ms": receipt,
                "body": quote_body(10, receipt)
            })
            .to_string();
            db.execute(
                "INSERT INTO ticks(at, receipt, body, event) VALUES(?1, ?2, ?3, 'research_quotes')",
                rusqlite::params![last, receipt, res_raw],
            )?;
            total_quote_ticks += 1;
        }
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        drop(db);

        let db_path = old_dir.join("paper.sqlite");
        let meta = fs::metadata(&db_path)?;
        fs::write(
            old_dir.join("book-identity.json"),
            serde_json::to_vec(&json!({
                "device": meta.dev(),
                "inode": meta.ino(),
                "binding": binding
            }))?,
        )?;
        fs::write(
            old_dir.join("report.json"),
            serde_json::to_vec(&json!({
                "paper_only": true,
                "live_enabled": false,
                "broker": "upstox",
                "account": old_cfg.account,
                "session": old_cfg.session.to_string(),
                "portfolio": {"closed": true, "halted": false, "stopped": false}
            }))?,
        )?;
        fs::write(
            current_dir.join("report.json"),
            serde_json::to_vec(&json!({
                "paper_only": true,
                "live_enabled": false,
                "broker": "upstox",
                "account": current_cfg.account,
                "session": current_cfg.session.to_string(),
                "portfolio": {"closed": true, "halted": false, "stopped": false}
            }))?,
        )?;

        let before_len = fs::metadata(&db_path)?.len();
        let keys = BTreeSet::from([fill_key]);
        let before_candidates = paper_replay::candidate_quotes(
            paper_replay::QuoteSource::new(&old_dir, None),
            old_cfg.session,
            &keys,
            base,
            last,
            200,
            last,
        )?;
        let before_receipts = paper_replay::quote_receipts_bounded(
            paper_replay::QuoteSource::new(&old_dir, None),
            old_cfg.session,
            100,
            last,
        )?
        .receipts
        .into_iter()
        .map(|r| (r.at_ms, r.receipt_ms, r.kind, r.observations))
        .collect::<Vec<_>>();

        let imported = market_archive::import_book(&mut archive_db, &old_dir)?;
        assert_eq!(imported["inserted"], total_quote_ticks);

        let preview = run(&current_cfg, current_cfg.session, false)?;
        assert_eq!(
            preview.would_rewrite_quotes,
            (total_quote_ticks - 1) as usize
        );
        assert_eq!(preview.kept_missing_archive_quotes, 0);
        assert_eq!(preview.rewritten_quotes, 0);
        assert_eq!(fs::metadata(&db_path)?.len(), before_len);

        let applied = run(&current_cfg, current_cfg.session, true)?;
        assert_eq!(applied.rewritten_quotes, (total_quote_ticks - 1) as usize);
        assert_eq!(applied.kept_missing_archive_quotes, 0);
        assert!(applied.bytes_freed > 0);
        let after_len = fs::metadata(&db_path)?.len();
        assert!(after_len < before_len);
        println!(
            "fixture book size: before={before_len} bytes after={after_len} bytes rewritten={}",
            applied.rewritten_quotes
        );

        let db = rusqlite::Connection::open(&db_path)?;
        let kept_fill: String =
            db.query_row("SELECT body FROM ticks WHERE id=?1", [fill_tick_id], |r| {
                r.get(0)
            })?;
        assert_eq!(kept_fill, fill_raw);
        let inline_bulk: i64 = db.query_row(
            "SELECT count(*) FROM ticks
             WHERE event IN ('wide_quotes', 'scanner_metadata', 'research_quotes')
               AND id != ?1
               AND body NOT LIKE '{\"kind\":\"market_archive_quote\"%'",
            [fill_tick_id],
            |r| r.get(0),
        )?;
        assert_eq!(inline_bulk, 0);
        let (after_binding, after_state): (String, String) =
            db.query_row("SELECT binding, body FROM state WHERE id=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        assert_eq!(after_binding, binding);
        assert_eq!(after_state, state_body);
        drop(db);

        let anchor: Value = serde_json::from_slice(&fs::read(old_dir.join("book-identity.json"))?)?;
        let after_meta = fs::metadata(&db_path)?;
        assert_eq!(anchor["device"], json!(after_meta.dev()));
        assert_eq!(anchor["inode"], json!(after_meta.ino()));

        let after_candidates = paper_replay::candidate_quotes(
            paper_replay::QuoteSource::new(&old_dir, Some(&settings)),
            old_cfg.session,
            &keys,
            base,
            last,
            200,
            last,
        )?;
        assert_eq!(after_candidates, before_candidates);

        let after_receipts = paper_replay::quote_receipts_bounded(
            paper_replay::QuoteSource::new(&old_dir, Some(&settings)),
            old_cfg.session,
            100,
            last,
        )?
        .receipts
        .into_iter()
        .map(|r| (r.at_ms, r.receipt_ms, r.kind, r.observations))
        .collect::<Vec<_>>();
        assert_eq!(after_receipts, before_receipts);

        let reimported = market_archive::import_book(&mut archive_db, &old_dir)?;
        assert_eq!(reimported["inserted"], 0);
        assert_eq!(reimported["verified"], total_quote_ticks);

        let second = run(&current_cfg, current_cfg.session, true)?;
        assert_eq!(second.would_rewrite_quotes, 0);
        assert_eq!(second.rewritten_quotes, 0);
        assert_eq!(second.kept_missing_archive_quotes, 0);
        assert_eq!(second.bytes_freed, 0);
        Ok(())
    }
}
