//! Explicit offline inputs only; does not discover credentials or datasets.
use std::{
    fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};
fn read(path: &Path, limit: usize) -> Result<Vec<u8>, ()> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ())?;
    if !file.metadata().map_err(|_| ())?.is_file() {
        return Err(());
    }
    let mut bytes = vec![];
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > limit {
        return Err(());
    }
    Ok(bytes)
}
pub fn entry(manifest: &Path, events: &Path) -> ExitCode {
    let result = (|| {
        let manifest = read(manifest, sadr_core::replay::MAX_MANIFEST)
            .map_err(|_| "backtest_input_unavailable".to_string())?;
        let events = read(events, sadr_core::replay::MAX_EVENTS)
            .map_err(|_| "backtest_input_unavailable".to_string())?;
        let start = Instant::now();
        sadr_core::replay::evaluate(&manifest, &events, || {
            start.elapsed() < Duration::from_secs(5)
        })
        .map_err(|e| e.to_string())
    })();
    match result {
        Ok(report) => match serde_json::to_string(&report) {
            Ok(s) => {
                println!("{s}");
                ExitCode::SUCCESS
            }
            Err(_) => {
                eprintln!("backtest_report_unavailable");
                ExitCode::from(2)
            }
        },
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
