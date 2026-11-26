mod bar_bench;
mod budget;
mod bundle;
mod docs;
mod goldens;
mod progress;
mod release;
mod replay;
mod retired;
// Offline repository tooling; never loads application configuration or credentials.
// `bar-bench` reads only the archive password file its operator names.
use clap::{Parser, Subcommand};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Task,
}
#[derive(Subcommand)]
enum Task {
    WatchProgress {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    CheckRelease {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    /// Print current retired-name counts in the allowance format.
    RetiredNames {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    ReplayFingerprint,
    Replay {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        events: PathBuf,
    },
    BundleCheck {
        root: PathBuf,
    },
    ReleaseArtifact {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        tag: String,
        #[arg(long)]
        binary: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    GoldenCheck {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    TestBudget {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    MigrationAudit {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    /// Backtest every bar strategy in a directory against an archive and write a leaderboard.
    BarBench {
        /// Archive URL without a password, as in `market_archive.url`.
        #[arg(long)]
        archive_url: String,
        /// File holding only the archive password.
        #[arg(long)]
        password_file: PathBuf,
        #[arg(long, default_value = "strategies")]
        strategies: PathBuf,
        /// Comma-separated strategy names; default is every bar strategy of the unit.
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        #[arg(long, value_parser = bar_bench::parse_unit)]
        unit: nivra_store::market_archive::BarUnit,
        /// First training date.
        #[arg(long)]
        from: chrono::NaiveDate,
        /// First test date; training ends the day before.
        #[arg(long)]
        test_from: chrono::NaiveDate,
        /// Last test date.
        #[arg(long)]
        to: chrono::NaiveDate,
        #[arg(long, default_value = "1000000")]
        capital: nivra_core::finance::Amount,
        /// A `paper_costs` fee schedule JSON file.
        #[arg(long)]
        costs: PathBuf,
        /// Wall budget per strategy in seconds (at most 600).
        #[arg(long, default_value_t = 600)]
        budget_secs: u64,
        #[arg(long, default_value_t = std::thread::available_parallelism().map_or(1, |n| n.get()))]
        jobs: usize,
        /// Markdown leaderboard path.
        #[arg(long)]
        output: PathBuf,
        /// Optional JSON path with every metric and the report hashes.
        #[arg(long)]
        json: Option<PathBuf>,
    },
}
fn main() -> ExitCode {
    let root = match Args::parse().command {
        Task::WatchProgress { root } => {
            return match progress::run(&root) {
                Ok(()) => ExitCode::SUCCESS,
                Err(code) => {
                    eprintln!("{code}");
                    ExitCode::FAILURE
                }
            };
        }
        Task::RetiredNames { root } => {
            return match retired::scan(&root) {
                Ok(found) => {
                    print!("{}", retired::render(&found));
                    ExitCode::SUCCESS
                }
                Err(code) => {
                    eprintln!("{code}");
                    ExitCode::FAILURE
                }
            };
        }
        Task::CheckRelease { root } => {
            let checked = match (docs::check(&root), retired::check(&root)) {
                (Ok(()), Ok(())) => Ok(()),
                (docs, names) => Err([docs.err(), names.err()]
                    .into_iter()
                    .flatten()
                    .flatten()
                    .collect::<Vec<_>>()),
            };
            return match checked {
                Ok(()) => {
                    println!("release_docs_ok");
                    ExitCode::SUCCESS
                }
                Err(errors) => {
                    for error in errors {
                        eprintln!("{error}");
                    }
                    ExitCode::FAILURE
                }
            };
        }
        Task::ReplayFingerprint => {
            println!("{}", replay::engine_fingerprint());
            return ExitCode::SUCCESS;
        }
        Task::Replay { manifest, events } => {
            return match replay::run(&manifest, &events) {
                Ok(()) => ExitCode::SUCCESS,
                Err(code) => {
                    eprintln!("{code}");
                    ExitCode::FAILURE
                }
            };
        }
        Task::BundleCheck { root } => {
            return match bundle::check(&root) {
                Ok(count) => {
                    println!(
                        "development_bundle_integrity_ok files={count} execution_authorized=false"
                    );
                    ExitCode::SUCCESS
                }
                Err(code) => {
                    eprintln!("{code}");
                    ExitCode::FAILURE
                }
            };
        }
        Task::ReleaseArtifact {
            root,
            tag,
            binary,
            output,
        } => {
            return match release::assemble(&root, &tag, &binary, &output) {
                Ok(()) => {
                    println!("release_artifact_assembled");
                    ExitCode::SUCCESS
                }
                Err(code) => {
                    eprintln!("{code}");
                    ExitCode::FAILURE
                }
            };
        }
        Task::MigrationAudit { root } => root,
        Task::BarBench {
            archive_url,
            password_file,
            strategies,
            only,
            unit,
            from,
            test_from,
            to,
            capital,
            costs,
            budget_secs,
            jobs,
            output,
            json,
        } => {
            let options = bar_bench::Options {
                archive_url,
                password_file,
                strategies,
                only,
                unit,
                from,
                test_from,
                to,
                capital,
                costs,
                budget: std::time::Duration::from_secs(budget_secs),
                jobs,
                output,
                json,
            };
            return match bar_bench::run(&options) {
                Ok(entries) => {
                    let failed = entries.iter().filter(|e| e.result.is_err()).count();
                    println!("bar_bench_ok strategies={} failed={failed}", entries.len());
                    ExitCode::SUCCESS
                }
                Err(code) => {
                    eprintln!("{code}");
                    ExitCode::FAILURE
                }
            };
        }
        Task::TestBudget { root } => {
            return match budget::check(&root) {
                Ok((count, ignored)) => {
                    println!(
                        "test_budget_ok count={count} limit={} ignored={ignored}",
                        budget::LIMIT
                    );
                    ExitCode::SUCCESS
                }
                Err(code) => {
                    eprintln!("{code}");
                    ExitCode::FAILURE
                }
            };
        }
        Task::GoldenCheck { root } => {
            return match goldens::check(&root) {
                Ok(count) => {
                    println!("frozen_goldens_ok files={count}");
                    ExitCode::SUCCESS
                }
                Err(code) => {
                    eprintln!("{code}");
                    ExitCode::FAILURE
                }
            };
        }
    };
    match audit_repository(&root) {
        Ok(()) => {
            println!("python_inventory_retired tracked_py=0 operational_acceptance=false");
            ExitCode::SUCCESS
        }
        Err(code) => {
            eprintln!("{code}");
            ExitCode::FAILURE
        }
    }
}
fn digest(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn safe_file(root: &Path, name: &str) -> Result<Vec<u8>, &'static str> {
    if name.is_empty()
        || !Path::new(name)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err("unsafe_inventory_path");
    }
    let mut path = root.to_path_buf();
    for component in Path::new(name).components() {
        path.push(component);
        if fs::symlink_metadata(&path)
            .map_err(|_| "inventory_file_missing")?
            .file_type()
            .is_symlink()
        {
            return Err("inventory_symlink_denied");
        }
    }
    fs::read(path).map_err(|_| "inventory_file_unreadable")
}
fn audit_repository(root: &Path) -> Result<(), &'static str> {
    // The Python file inventory and golden-parity manifest are not gates.
    // Fail only if a tracked .py file is reintroduced.
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", "*.py"])
        .current_dir(root)
        .output()
        .map_err(|_| "tracked_inventory_unavailable")?;
    if !output.status.success() {
        return Err("tracked_inventory_unavailable");
    }
    let files = std::str::from_utf8(&output.stdout).map_err(|_| "invalid_tracked_path")?;
    if files.split('\0').any(|name| !name.is_empty()) {
        return Err("python_source_present");
    }
    Ok(())
}
