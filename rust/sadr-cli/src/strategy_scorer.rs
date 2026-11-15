//! Offline Tier 2 entry points. Never constructs providers or a paper writer.
use clap::{Args, Subcommand, ValueEnum};
use sadr_research::strategy_sandbox::{SandboxConfig, Snapshot, Source, score};
use sadr_store::strategy_results::{Evidence, EvidenceKind, ResultsDb};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Python,
    Wasm,
}
#[derive(Args)]
pub struct ScorerArgs {
    /// SandboxConfig JSON. Execution is disabled unless enabled explicitly here.
    #[arg(long)]
    pub config: PathBuf,
    #[arg(long)]
    pub source: PathBuf,
    #[arg(long, value_enum)]
    pub language: Language,
    #[arg(long)]
    pub input: PathBuf,
    #[arg(long)]
    pub scorer_id: String,
    #[arg(long)]
    pub version: u32,
    #[arg(long)]
    pub run: String,
    /// Separate research DB only; no paper book is accepted.
    #[arg(long)]
    pub database: Option<PathBuf>,
}
#[derive(Subcommand)]
pub enum Commands {
    /// Score one read-only snapshot. Failed invocations are retained as failed ticks.
    Scores {
        #[command(flatten)]
        args: ScorerArgs,
        #[arg(long)]
        observed_ms: i64,
    },
    /// Replay a scorer over timestamped recorded snapshots, without simulated orders.
    ReplayScorer {
        #[command(flatten)]
        args: ScorerArgs,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedTick {
    pub observed_ms: i64,
    pub snapshot: Snapshot,
}
#[derive(Serialize)]
pub struct TickResult {
    pub observed_ms: i64,
    pub scores: Option<BTreeMap<String, f64>>,
    pub failure: Option<String>,
}
#[derive(Serialize)]
pub struct Report {
    pub scorer_id: String,
    pub version: u32,
    pub language: Language,
    pub source_sha256: String,
    pub scorer_spec_sha256: String,
    pub input_sha256: String,
    pub failed_ticks: usize,
    pub ticks: Vec<TickResult>,
    pub interpretation: &'static str,
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn read(path: &Path, limit: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        return Err("strategy_input_too_large".into());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("strategy_input_too_large".into());
    }
    Ok(bytes)
}
fn execute(args: ScorerArgs, observed: Option<i64>) -> Result<(), Box<dyn Error>> {
    if args.scorer_id.is_empty()
        || args.scorer_id.len() > 128
        || !args
            .scorer_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        || args.version == 0
        || args.run.is_empty()
        || args.run.len() > 128
        || args.run.contains('\0')
    {
        return Err("strategy_scorer_identity_invalid".into());
    }
    let config: SandboxConfig = serde_json::from_slice(&read(&args.config, 8192)?)?;
    config.validate()?;
    if !config.enabled {
        return Err("strategy_sandbox_disabled".into());
    }
    let source = read(&args.source, config.max_source_bytes)?;
    let input = read(&args.input, 16 * 1024 * 1024)?;
    let ticks: Vec<RecordedTick> = match observed {
        Some(observed_ms) => vec![RecordedTick {
            observed_ms,
            snapshot: serde_json::from_slice(&input)?,
        }],
        None => serde_json::from_slice(&input)?,
    };
    if ticks.is_empty()
        || ticks.len() > 10000
        || ticks
            .windows(2)
            .any(|w| w[0].observed_ms >= w[1].observed_ms)
    {
        return Err("strategy_replay_input_invalid".into());
    }
    let source_sha256 = hash(&source);
    // This is the complete scores-only definition, including resource settings.
    // Reusing a display ID/version with changed code or limits cannot conflate runs.
    let scorer_spec = serde_json::json!({
        "scorer_id": args.scorer_id,
        "version": args.version,
        "language": args.language,
        "source_sha256": source_sha256,
        "config": config,
        "abi": "ordered_numeric_rows_v1",
    });
    let mut report = Report {
        scorer_id: args.scorer_id,
        version: args.version,
        language: args.language,
        source_sha256,
        scorer_spec_sha256: hash(&serde_json::to_vec(&scorer_spec)?),
        input_sha256: hash(&input),
        failed_ticks: 0,
        ticks: Vec::new(),
        interpretation: "recorded_scores_only_no_executable_fills_or_profitability_claim",
    };
    let scorer_source = match args.language {
        Language::Python => Source::Python(std::str::from_utf8(&source)?),
        Language::Wasm => Source::Wasm(&source),
    };
    for tick in ticks {
        let source = match &scorer_source {
            Source::Python(s) => Source::Python(s),
            Source::Wasm(s) => Source::Wasm(s),
        };
        let result = match score(&config, source, &tick.snapshot) {
            Ok(scores) => TickResult {
                observed_ms: tick.observed_ms,
                scores: Some(scores),
                failure: None,
            },
            Err(reason) => {
                report.failed_ticks += 1;
                TickResult {
                    observed_ms: tick.observed_ms,
                    scores: None,
                    failure: Some(format!("{reason:?}")),
                }
            }
        };
        report.ticks.push(result);
    }
    if let Some(path) = args.database {
        let db = ResultsDb::open(&path)?;
        let spec_id = format!(
            "{}:{}:{}",
            report.scorer_id, report.version, report.scorer_spec_sha256
        );
        // Hex chunks retain exact executable bytes without exceeding the evidence
        // body bound for large modules or heavily escaped Python source.
        let source_chunks: Vec<String> = source
            .chunks(32 * 1024)
            .map(|chunk| chunk.iter().map(|b| format!("{b:02x}")).collect())
            .collect();
        let mut rows = vec![Evidence {
            id: format!("{}:source", args.run),
            kind: EvidenceKind::Scorecard,
            observed_ms: report.ticks[0].observed_ms,
            spec_id: spec_id.clone(),
            scan_id: None,
            digest_id: None,
            decide_id: None,
            body: serde_json::json!({"scorer_spec": scorer_spec, "scorer_spec_sha256": report.scorer_spec_sha256, "source_sha256": report.source_sha256, "source_encoding": "hex_chunks", "source_chunks": source_chunks.len(), "source_bytes": source.len(), "input_sha256": report.input_sha256, "failed_ticks": report.failed_ticks, "ticks": report.ticks.len()}),
        }];
        for (index, bytes_hex) in source_chunks.into_iter().enumerate() {
            rows.push(Evidence {
                id: format!("{}:source:{index}", args.run),
                kind: EvidenceKind::Scorecard,
                observed_ms: report.ticks[0].observed_ms,
                spec_id: spec_id.clone(),
                scan_id: None,
                digest_id: None,
                decide_id: None,
                body: serde_json::json!({"source_sha256": report.source_sha256, "part": index, "bytes_hex": bytes_hex}),
            });
        }
        for (index, tick) in report.ticks.iter().enumerate() {
            rows.push(Evidence {
                id: format!("{}:{index}", args.run),
                kind: if tick.failure.is_some() {
                    EvidenceKind::Failure
                } else {
                    EvidenceKind::Replay
                },
                observed_ms: tick.observed_ms,
                spec_id: spec_id.clone(),
                scan_id: None,
                digest_id: None,
                decide_id: None,
                body: serde_json::to_value(tick)?,
            });
        }
        db.record_batch(&rows)?;
    }
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
pub fn run(command: Commands) -> Result<(), Box<dyn Error>> {
    match command {
        Commands::Scores { args, observed_ms } => execute(args, Some(observed_ms)),
        Commands::ReplayScorer { args } => execute(args, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sadr_store::strategy_results::Query;

    #[test]
    fn scores_retain_large_source_and_exact_definition_without_overwriting_runs() {
        if !cfg!(all(target_os = "linux", target_arch = "x86_64"))
            || !Path::new("/usr/bin/python3").is_file()
            || !Path::new("/usr/bin/prlimit").is_file()
        {
            return;
        }
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("sadr-scorer-{}-{unique}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let source = format!(
            "def score(data):\n return {{'NSE:fixture': 1}}\n#{}",
            "x".repeat(200_000)
        );
        std::fs::write(dir.join("source.py"), &source).unwrap();
        std::fs::write(dir.join("input.json"), br#"{"NSE:fixture":[1]}"#).unwrap();
        let mut config = SandboxConfig {
            enabled: true,
            wall_millis: 2000,
            max_source_bytes: 262144,
            ..Default::default()
        };
        let args = |run: &str| ScorerArgs {
            config: dir.join("config.json"),
            source: dir.join("source.py"),
            language: Language::Python,
            input: dir.join("input.json"),
            scorer_id: "fixture".into(),
            version: 1,
            run: run.into(),
            database: Some(dir.join("results.sqlite")),
        };
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        execute(args("trial-1"), Some(1000)).unwrap();
        assert!(execute(args("trial-1"), Some(1000)).is_err());
        config.wall_millis = 2500;
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        execute(args("trial-2"), Some(1000)).unwrap();
        let db = ResultsDb::open_read_only(&dir.join("results.sqlite")).unwrap();
        let rows = db
            .query(&Query {
                kind: EvidenceKind::Scorecard,
                from_ms: 1000,
                to_ms: 1000,
                after_id: None,
                limit: 100,
                spec_id: None,
                instrument_key: None,
            })
            .unwrap();
        let manifests: Vec<_> = rows
            .iter()
            .filter(|r| r.body.get("scorer_spec").is_some())
            .collect();
        assert_eq!(manifests.len(), 2);
        assert_ne!(manifests[0].spec_id, manifests[1].spec_id);
        for manifest in manifests {
            let expected_hash = hash(&serde_json::to_vec(&manifest.body["scorer_spec"]).unwrap());
            assert_eq!(manifest.body["scorer_spec_sha256"], expected_hash);
            assert!(manifest.spec_id.ends_with(&expected_hash));
            assert_eq!(manifest.body["source_sha256"], hash(source.as_bytes()));
            let mut parts: Vec<_> = rows
                .iter()
                .filter(|r| r.spec_id == manifest.spec_id && r.body.get("part").is_some())
                .collect();
            parts.sort_by_key(|r| r.body["part"].as_u64().unwrap());
            let encoded: String = parts
                .iter()
                .map(|r| r.body["bytes_hex"].as_str().unwrap())
                .collect();
            let expected: String = source.bytes().map(|b| format!("{b:02x}")).collect();
            assert_eq!(encoded, expected);
        }
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
