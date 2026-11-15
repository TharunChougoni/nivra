//! Bounded JSON manifest + streaming NDJSON quote events. Offline only.
use sadr_core::historical::{Event, Manifest, Runner};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
fn open(path: &Path) -> Result<fs::File, &'static str> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "replay_input_unavailable")?;
    if !file
        .metadata()
        .map_err(|_| "replay_input_unavailable")?
        .is_file()
    {
        return Err("replay_regular_input_required");
    }
    Ok(file)
}
pub fn engine_fingerprint() -> String {
    sadr_core::replay::engine_fingerprint()
}
pub fn run(manifest: &Path, events: &Path) -> Result<(), &'static str> {
    let mut bytes = vec![];
    open(manifest)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "replay_manifest_unreadable")?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("replay_manifest_limit");
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| "replay_manifest_invalid")?;
    if manifest.bindings.engine != engine_fingerprint() {
        return Err("replay_engine_binding_mismatch");
    }
    let declared_data = manifest.bindings.data.clone();
    let mut input_hash = Sha256::new();
    let mut runner = Runner::new(manifest).map_err(|_| "replay_contract_rejected")?;
    let mut reader = BufReader::new(open(events)?);
    let mut row = vec![];
    loop {
        row.clear();
        let mut bounded = reader.by_ref().take(128 * 1024 + 1);
        let count = bounded
            .read_until(b'\n', &mut row)
            .map_err(|_| "replay_event_unreadable")?;
        if count == 0 {
            break;
        }
        if count > 128 * 1024 {
            return Err("replay_event_limit");
        }
        input_hash.update(&row);
        let event: Event = serde_json::from_slice(&row).map_err(|_| "replay_event_invalid")?;
        runner.apply(event).map_err(|_| "replay_event_rejected")?;
    }
    let actual_data: String = input_hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual_data != declared_data {
        return Err("replay_data_binding_mismatch");
    }
    let report = runner.finish().map_err(|_| "replay_report_unavailable")?;
    println!(
        "{}",
        serde_json::to_string(&report).map_err(|_| "replay_report_unavailable")?
    );
    Ok(())
}
