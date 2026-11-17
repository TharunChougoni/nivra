//! Separately deployed registered-analysis worker. It never starts trading.
//! The model may only enqueue a reviewed job. This process supplies the host limits
//! and the bubblewrap runner. Host qualification stays a separate drill.
use nivra_research::{
    bubblewrap,
    worker::{self, DevelopmentSplit, Outcome, RegisteredRun, ResourceLimits},
};
use nivra_store::research_jobs::{self as jobs};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn args() -> Option<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let name = flag.strip_prefix("--")?;
        let value = raw.next()?;
        if name.is_empty() || value.is_empty() || values.insert(name.to_owned(), value).is_some() {
            return None;
        }
    }
    Some(values)
}

fn need<'a>(values: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    values.get(key).map(String::as_str)
}

fn absolute(values: &BTreeMap<String, String>, key: &str) -> Option<PathBuf> {
    let path = PathBuf::from(need(values, key)?);
    path.is_absolute().then_some(path)
}

fn read_bounded(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.len() == 0
        || meta.len() as usize > limit
    {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    (bytes.len() == meta.len() as usize).then_some(bytes)
}

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("drill") {
        let mut raw = std::env::args().skip(2);
        let flag = raw.next();
        let Some(root) = raw.next() else {
            return ExitCode::from(2);
        };
        if flag.as_deref() != Some("--root") {
            return ExitCode::from(2);
        }
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return ExitCode::from(2);
        }
        let extra: Vec<_> = raw.collect();
        let report = if extra.is_empty() {
            nivra_research::drill::run(&root)
        } else if extra.len() == 4 && extra[0] == "--runtime" && extra[2] == "--runtime-sha256" {
            let runtime = PathBuf::from(&extra[1]);
            if !runtime.is_absolute() {
                return ExitCode::from(2);
            }
            nivra_research::drill::run_with_runtime(&root, Some((&runtime, &extra[3])))
        } else {
            return ExitCode::from(2);
        };
        let ok = report.sandbox_experiment == "MEASURED";
        return match serde_json::to_string(&report) {
            Ok(text) => {
                println!("{text}");
                if ok {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(2)
                }
            }
            Err(_) => ExitCode::from(2),
        };
    }
    let Some(values) = args() else {
        return ExitCode::from(2);
    };
    let expected = [
        "queue",
        "root",
        "job",
        "lease",
        "code",
        "runtime",
        "input",
        "split",
        "cpu-millis",
        "tasks",
        "workspace-bytes",
        "file-bytes",
    ];
    if values.len() != expected.len() || expected.iter().any(|key| !values.contains_key(*key)) {
        return ExitCode::from(2);
    }
    let split = match need(&values, "split") {
        Some("training") => DevelopmentSplit::Training,
        Some("development") => DevelopmentSplit::Development,
        _ => return ExitCode::from(2),
    };
    let Some(queue) = absolute(&values, "queue") else {
        return ExitCode::from(2);
    };
    let Some(root) = absolute(&values, "root") else {
        return ExitCode::from(2);
    };
    let Some(code_path) = absolute(&values, "code") else {
        return ExitCode::from(2);
    };
    let Some(runtime_path) = absolute(&values, "runtime") else {
        return ExitCode::from(2);
    };
    let Some(input_path) = absolute(&values, "input") else {
        return ExitCode::from(2);
    };
    let Some(code) = read_bounded(&code_path, 262144) else {
        return ExitCode::from(2);
    };
    let Some(runtime) = read_bounded(&runtime_path, 32 * 1024 * 1024) else {
        return ExitCode::from(2);
    };
    let Some(input) = read_bounded(&input_path, 16 * 1024 * 1024) else {
        return ExitCode::from(2);
    };
    let limits = ResourceLimits {
        cpu_millis: need(&values, "cpu-millis")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        tasks: need(&values, "tasks")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        workspace_bytes: need(&values, "workspace-bytes")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        file_bytes: need(&values, "file-bytes")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    };
    let Ok(mut db) = Connection::open(&queue) else {
        return ExitCode::from(2);
    };
    if db.pragma_update(None, "synchronous", "FULL").is_err() {
        return ExitCode::from(2);
    }
    let Ok(job) = jobs::load(&mut db, need(&values, "job").unwrap_or("")) else {
        return ExitCode::from(2);
    };
    let wall_millis = job.spec.wall_millis;
    let registered = RegisteredRun {
        spec: job.spec,
        split,
        limits,
        code,
        runtime,
        input,
    };
    let mut runner = bubblewrap::Runner::new(root);
    let Some(lease) = need(&values, "lease") else {
        return ExitCode::from(2);
    };
    let Ok(mut worker) = worker::start_one(&mut db, &registered, lease, &mut runner, now_ms) else {
        return ExitCode::from(2);
    };
    let deadline = now_ms().saturating_add(i64::from(wall_millis) + 30_000);
    loop {
        if now_ms() > deadline {
            eprintln!("research_worker_ambiguous");
            return ExitCode::from(2);
        }
        match worker.poll(&mut db, &mut runner, now_ms, false) {
            Ok(Outcome::Running | Outcome::AwaitingTermination { .. }) => {
                thread::sleep(Duration::from_millis(50));
            }
            Ok(Outcome::Succeeded(result)) => {
                match serde_json::to_string(&result) {
                    Ok(text) => println!("{text}"),
                    Err(_) => return ExitCode::from(2),
                }
                return ExitCode::SUCCESS;
            }
            Ok(Outcome::Terminated) => {
                eprintln!("research_worker_terminated");
                return ExitCode::from(2);
            }
            Err(_) => return ExitCode::from(2),
        }
    }
}
