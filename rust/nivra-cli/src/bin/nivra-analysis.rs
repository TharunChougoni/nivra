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
        let input: nivra_core::daily_research::Dataset =
            serde_json::from_slice(&raw).map_err(|_| ())?;
        let summary = nivra_core::daily_research::evaluate(&input).map_err(|_| ())?;
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
/// Trusted offline import: preserve source receipts and publish bounded dataset shards.
fn prepare() -> Result<(), ()> {
    use std::{
        collections::BTreeMap,
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 || args[0] != "--prepare-daily-history" {
        return Err(());
    }
    let output = Path::new(&args[2]);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(output)
        .map_err(|_| ())?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(output.join("receipts"))
        .map_err(|_| ())?;
    let mut instruments = BTreeMap::<String, Vec<serde_json::Value>>::new();
    let mut total = 0usize;
    for entry in fs::read_dir(&args[1]).map_err(|_| ())? {
        let path = entry.map_err(|_| ())?.path();
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".receipt.json"))
        {
            continue;
        }
        let raw = read(&path, 16 * 1024 * 1024)?;
        let value: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| ())?;
        if value["unit"] != "days" {
            continue;
        }
        total += raw.len();
        if total > 512 * 1024 * 1024 {
            return Err(());
        }
        let key = value["key"].as_str().ok_or(())?.to_owned();
        let range = format!(
            "{}:days:{}:{}",
            key,
            value["start"].as_str().ok_or(())?,
            value["end"].as_str().ok_or(())?
        );
        let expected = format!("{}.receipt.json", hash(range.as_bytes()));
        if path.file_name().and_then(|n| n.to_str()) != Some(expected.as_str()) {
            return Err(());
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(output.join("receipts").join(expected))
            .map_err(|_| ())?;
        file.write_all(&raw).map_err(|_| ())?;
        file.sync_all().map_err(|_| ())?;
        instruments.entry(key).or_default().push(value);
    }
    if instruments.is_empty() || instruments.len() > 6000 {
        return Err(());
    }
    for receipts in instruments.values_mut() {
        receipts.sort_by(|a, b| {
            a["start"]
                .as_str()
                .cmp(&b["start"].as_str())
                .then_with(|| a["end"].as_str().cmp(&b["end"].as_str()))
                .then_with(|| a["body_sha256"].as_str().cmp(&b["body_sha256"].as_str()))
        });
    }
    let groups = instruments.into_iter().collect::<Vec<_>>();
    let mut inventory = vec![];
    for (n, group) in groups.chunks(200).enumerate() {
        let dataset = nivra_core::daily_research::Dataset {
            schema: 1,
            receipts: group.iter().flat_map(|(_, r)| r.iter().cloned()).collect(),
        };
        let summary = nivra_core::daily_research::evaluate(&dataset).map_err(|_| ())?;
        let raw = serde_json::to_vec(&dataset).map_err(|_| ())?;
        if raw.len() > 16 * 1024 * 1024 {
            return Err(());
        }
        let name = format!("daily-{:02}.json", n + 1);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(output.join(&name))
            .map_err(|_| ())?;
        file.write_all(&raw).map_err(|_| ())?;
        file.sync_all().map_err(|_| ())?;
        inventory.push(serde_json::json!({"file":name,"input_sha256":hash(&raw),"instruments":summary.instruments,"bars":summary.bars,"first_day":summary.first_day,"last_day":summary.last_day,"instrument_keys":group.iter().map(|(k,_)|k).collect::<Vec<_>>()}));
    }
    let raw = serde_json::to_vec_pretty(
        &serde_json::json!({"schema":1,"source_directory":args[1],"shards":inventory}),
    )
    .map_err(|_| ())?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output.join("inventory.json"))
        .map_err(|_| ())?;
    file.write_all(&raw).map_err(|_| ())?;
    file.sync_all().map_err(|_| ())?;
    println!(
        "{}",
        serde_json::json!({"status":"daily_history_prepared","shards":inventory.len(),"source_bytes":total,"inventory_sha256":hash(&raw)})
    );
    Ok(())
}
fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--prepare-daily-history") {
        return if prepare().is_ok() {
            ExitCode::SUCCESS
        } else {
            eprintln!("daily_history_preparation_failed");
            ExitCode::from(2)
        };
    }
    if run().is_ok() {
        ExitCode::SUCCESS
    } else {
        eprintln!("registered_analysis_failed");
        ExitCode::from(2)
    }
}
