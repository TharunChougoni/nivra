//! Fixed reviewed sandbox runtime. No network adapters, credentials or order APIs.
use nivra_research::worker::{AnalysisResult, Launch, Metric};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    manifest: serde_json::Value,
    events: String,
}
fn read(path: &Path, cap: u64) -> Result<Vec<u8>, ()> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ())?;
    let m = file.metadata().map_err(|_| ())?;
    if !m.is_file() || m.len() > cap {
        return Err(());
    }
    let mut bytes = vec![];
    file.take(cap + 1).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() as u64 > cap {
        return Err(());
    }
    Ok(bytes)
}
fn hash(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn run() -> Result<(), ()> {
    if std::env::args().skip(1).collect::<Vec<_>>() != ["/inputs/code", "/inputs/input"] {
        return Err(());
    }
    let code = read(Path::new("/inputs/code"), 128)?;
    if code != b"shared_core_backtest_v1" && code != b"daily_history_summary_v1" {
        return Err(());
    }
    let raw = read(Path::new("/inputs/input"), 16 * 1024 * 1024)?;
    let launch: Launch =
        serde_json::from_slice(&read(Path::new("/inputs/launch.json"), 16384)?).map_err(|_| ())?;
    if hash(&raw) != launch.spec.input_hash || hash(&code) != launch.spec.script_hash {
        return Err(());
    }
    if code == b"daily_history_summary_v1" {
        use nivra_core::daily_research::{self, ArchiveDataset, Dataset};
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum HistoryInput {
            Archive(ArchiveDataset),
            Legacy(Dataset),
        }
        let summary = match serde_json::from_slice(&raw).map_err(|_| ())? {
            HistoryInput::Archive(input) => daily_research::evaluate_archive(&input),
            HistoryInput::Legacy(input) => daily_research::evaluate(&input),
        }
        .map_err(|_| ())?;
        let mut observations = vec![serde_json::json!({"instruments":summary.instruments,"bars":summary.bars,"first_day":summary.first_day,"last_day":summary.last_day}).to_string(), "Exploratory unadjusted historical close returns, not strategy P&L. No fills, fees, point-in-time universe or corporate-action adjustment inferred. Not out-of-sample evidence, strategy acceptance or promotion. No live authority.".into()];
        observations.extend(summary.ranked_close_returns.iter().map(ToString::to_string));
        let output = AnalysisResult {
            identity: launch.identity,
            script_hash: launch.spec.script_hash,
            runtime_hash: launch.spec.runtime_hash,
            input_hash: launch.spec.input_hash,
            metrics: vec![
                Metric {
                    name: "instruments".into(),
                    value: summary.instruments.to_string(),
                },
                Metric {
                    name: "daily_bars".into(),
                    value: summary.bars.to_string(),
                },
            ],
            observations,
        };
        return fs::write(
            "/outputs/result.json",
            serde_json::to_vec(&output).map_err(|_| ())?,
        )
        .map_err(|_| ());
    }
    let input: Input = serde_json::from_slice(&raw).map_err(|_| ())?;
    let manifest = serde_json::to_vec(&input.manifest).map_err(|_| ())?;
    let start = Instant::now();
    let result = nivra_core::replay::evaluate(&manifest, input.events.as_bytes(), || {
        start.elapsed() < Duration::from_secs(5)
    })
    .map_err(|_| ())?;
    let report = &result.report;
    let mut metrics = vec![
        Metric {
            name: "events".into(),
            value: report.events.to_string(),
        },
        Metric {
            name: "fills".into(),
            value: report.portfolio.fills.len().to_string(),
        },
        Metric {
            name: "fees_inr".into(),
            value: report.fees.to_string(),
        },
        Metric {
            name: "turnover_inr".into(),
            value: report.turnover.to_string(),
        },
        Metric {
            name: "maximum_drawdown_inr".into(),
            value: report.maximum_drawdown.to_string(),
        },
        Metric {
            name: "cash_inr".into(),
            value: report.portfolio.cash.to_string(),
        },
    ];
    if let Some(pnl) = report.realized_pnl_if_flat {
        metrics.push(Metric {
            name: "realized_pnl_if_flat_inr".into(),
            value: pnl.to_string(),
        });
    }
    let output = AnalysisResult {
        identity: launch.identity, script_hash: launch.spec.script_hash,
        runtime_hash: launch.spec.runtime_hash, input_hash: launch.spec.input_hash,
        metrics,
        observations: vec![
            "Exploratory shared-core replay; no strategy promotion or live authorization. Historical bars are not executable quotes.".into(),
            serde_json::json!({"session":report.session,"split":report.dataset_split,"receipt_clock":report.receipt_clock,"limitations":report.limitations}).to_string(),
        ],
    };
    let raw = serde_json::to_vec(&output).map_err(|_| ())?;
    if raw.len() > 65536 {
        return Err(());
    }
    fs::write("/outputs/result.json", raw).map_err(|_| ())?;
    Ok(())
}
fn main() -> ExitCode {
    if run().is_ok() {
        ExitCode::SUCCESS
    } else {
        eprintln!("registered_analysis_failed");
        ExitCode::from(2)
    }
}
