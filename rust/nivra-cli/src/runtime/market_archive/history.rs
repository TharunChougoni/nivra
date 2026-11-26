use super::{Config, Path, Result, market_archive, normalize, open};
use crate::runtime::{digest, history_download};
use chrono::NaiveDate;
use nivra_core::daily_research::{ArchiveBar, ArchiveDataset, evaluate_archive};
use nivra_store::market_archive::{CandleUnit, Client};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
};

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct ImportSummary {
    pub(crate) receipts_inserted: usize,
    pub(crate) unavailable_inserted: usize,
}

#[derive(Deserialize)]
struct Unavailable {
    key: String,
    unit: String,
    start: NaiveDate,
    end: NaiveDate,
    recorded_ms: i64,
    status: String,
}

fn bind(name: &str, key: &str, unit: &str, start: NaiveDate, end: NaiveDate) -> Result<()> {
    if key.is_empty() || start > end || name != history_download::range_id(key, unit, start, end) {
        return Err("archive_history_import_binding".into());
    }
    Ok(())
}

pub(crate) fn import(db: &mut Client, directory: &Path) -> Result<ImportSummary> {
    let mut summary = ImportSummary::default();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let filename = entry.file_name();
        let Some((name, kind)) = filename.to_str().and_then(|name| {
            name.strip_suffix(".receipt.json")
                .map(|id| (id, "receipt"))
                .or_else(|| {
                    name.strip_suffix(".unavailable.json")
                        .map(|id| (id, "unavailable"))
                })
        }) else {
            continue;
        };
        let limit = if kind == "receipt" {
            history_download::MAX_RECEIPT_BYTES
        } else {
            4096
        };
        let bytes = crate::read_bounded(&entry.path(), limit)
            .map_err(|_| "archive_history_import_unreadable")?;
        if kind == "receipt" {
            let raw = std::str::from_utf8(&bytes).map_err(|_| "archive_history_import_invalid")?;
            let batch = normalize::candles(raw).map_err(|_| "archive_history_import_invalid")?;
            bind(
                name,
                &batch.instrument_key,
                history_download::unit_name(batch.unit),
                batch.start,
                batch.end,
            )?;
            summary.receipts_inserted +=
                usize::from(market_archive::import_candles(db, name, &batch)?);
        } else {
            let unavailable: Unavailable =
                serde_json::from_slice(&bytes).map_err(|_| "archive_history_import_invalid")?;
            let unit = match unavailable.unit.as_str() {
                "days" => CandleUnit::Daily,
                "minutes" => CandleUnit::Minute,
                _ => return Err("archive_history_import_invalid".into()),
            };
            if unavailable.status != "provider_http_400_unavailable"
                || unavailable.recorded_ms < 0
                || history_download::window_end(
                    unavailable.start,
                    unavailable.end,
                    &unavailable.unit,
                )? != unavailable.end
            {
                return Err("archive_history_import_invalid".into());
            }
            bind(
                name,
                &unavailable.key,
                &unavailable.unit,
                unavailable.start,
                unavailable.end,
            )?;
            summary.unavailable_inserted += usize::from(market_archive::import_unavailable(
                db,
                name,
                (&unavailable.key, unit, unavailable.start, unavailable.end),
                unavailable.recorded_ms,
                &bytes,
            )?);
        }
    }
    Ok(summary)
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(super) fn export(
    config: &Config,
    directory: &Path,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<()> {
    if end < start || end.signed_duration_since(start).num_days() >= 3660 {
        return Err("archive_history_export_range".into());
    }
    let mut db = open(config)?;
    let keys = market_archive::daily_instrument_keys(&mut db, start, end)?;
    if keys.is_empty() {
        return Err("archive_history_export_empty".into());
    }
    fs::DirBuilder::new().mode(0o700).create(directory)?;
    let mut inventory = Vec::new();
    for (index, keys) in keys.chunks(40).enumerate() {
        let requested: Vec<_> = keys.iter().map(String::as_str).collect();
        let bars = market_archive::daily_bars(&mut db, &requested, Some(start), Some(end))?;
        let dataset = ArchiveDataset {
            schema: 2,
            bars: bars
                .into_iter()
                .map(|bar| ArchiveBar {
                    key: bar.key,
                    day: bar.day,
                    close_paise: bar.close_paise,
                })
                .collect(),
        };
        let summary = evaluate_archive(&dataset)?;
        let bytes = serde_json::to_vec(&dataset)?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("archive_history_export_shard_limit".into());
        }
        let name = format!("daily-{:02}.json", index + 1);
        write(&directory.join(&name), &bytes)?;
        inventory.push(json!({"file":name,"input_sha256":digest(&bytes),
            "instruments":summary.instruments,"bars":summary.bars,
            "first_day":summary.first_day,"last_day":summary.last_day,"instrument_keys":keys}));
    }
    let bytes =
        serde_json::to_vec_pretty(&json!({"schema":2,"start":start,"end":end,"shards":inventory}))?;
    write(&directory.join("inventory.json"), &bytes)?;
    println!(
        "{}",
        json!({"status":"daily_history_prepared","shards":inventory.len(),"inventory_sha256":digest(&bytes)})
    );
    Ok(())
}
