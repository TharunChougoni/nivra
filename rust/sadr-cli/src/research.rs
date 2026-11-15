//! Review-model enqueue and sandbox-measurement check.
//! The model names a catalog alias. This process never executes the analysis.
use rusqlite::Connection;
use sadr_research::{
    AnalysisArgs, AnalysisTool, Dataset, RegisteredAnalysis,
    worker::{self, HostObservation},
};
use sadr_store::research_jobs::{self as jobs};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    process::ExitCode,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    review_id: String,
    analyses: Vec<RegisteredAnalysis>,
    datasets: Vec<Dataset>,
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, ()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ())?;
    let meta = file.metadata().map_err(|_| ())?;
    if !meta.is_file() || meta.len() > limit {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > limit {
        return Err(());
    }
    Ok(bytes)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn enqueue(queue: &Path, catalog: &Path, request: &Path) -> ExitCode {
    let printed = (|| {
        if !queue.is_absolute()
            || queue
                .symlink_metadata()
                .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err(());
        }
        let catalog: Catalog =
            serde_json::from_slice(&read_bounded(catalog, 65_536)?).map_err(|_| ())?;
        let request: AnalysisArgs =
            serde_json::from_slice(&read_bounded(request, 4096)?).map_err(|_| ())?;
        let created = !queue.exists();
        let mut db = Connection::open(queue).map_err(|_| ())?;
        if created {
            fs::set_permissions(queue, fs::Permissions::from_mode(0o600)).map_err(|_| ())?;
        }
        db.pragma_update(None, "synchronous", "FULL")
            .map_err(|_| ())?;
        let tables: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| ())?;
        if tables == 0 {
            jobs::initialize(&mut db).map_err(|_| ())?;
        }
        for analysis in &catalog.analyses {
            jobs::register_reviewed(&mut db, &analysis.script_hash, &analysis.runtime_hash)
                .map_err(|_| ())?;
        }
        let tool = AnalysisTool::bind(
            catalog.review_id,
            Arc::new(std::sync::Mutex::new(db)),
            catalog.analyses,
            catalog.datasets,
            Arc::new(now_ms),
        )
        .map_err(|_| ())?;
        let queued = tool.enqueue(request).map_err(|_| ())?;
        if queued.execution_started || queued.promotion_authorized {
            return Err(());
        }
        serde_json::to_string(&queued).map_err(|_| ())
    })();
    match printed {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(()) => {
            eprintln!("research_enqueue_rejected");
            ExitCode::from(2)
        }
    }
}

pub(crate) fn accept(
    result: &Path,
    code: &Path,
    runtime: &Path,
    input: &Path,
    split: &str,
    observation: &Path,
) -> ExitCode {
    let printed = (|| {
        if !matches!(split, "training" | "development") {
            return Err(());
        }
        let result: worker::AnalysisResult =
            serde_json::from_slice(&read_bounded(result, 65_536)?).map_err(|_| ())?;
        let observed: HostObservation =
            serde_json::from_slice(&read_bounded(observation, 4096)?).map_err(|_| ())?;
        let report = worker::accept_measured_experiment(
            &result,
            &sha(&read_bounded(code, 262_144)?),
            &sha(&read_bounded(runtime, 32 * 1024 * 1024)?),
            &sha(&read_bounded(input, 16 * 1024 * 1024)?),
            &observed,
        );
        let measured = report.sandbox_experiment == "MEASURED";
        let json = serde_json::to_string(&report).map_err(|_| ())?;
        Ok((measured, json))
    })();
    match printed {
        Ok((true, json)) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Ok((false, json)) => {
            println!("{json}");
            ExitCode::from(2)
        }
        Err(()) => {
            eprintln!("research_accept_rejected");
            ExitCode::from(2)
        }
    }
}
