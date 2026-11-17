mod budget;
mod bundle;
mod docs;
mod goldens;
mod progress;
mod release;
mod replay;
// Offline repository tooling; never loads application configuration or credentials.
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
        Task::CheckRelease { root } => {
            return match docs::check(&root) {
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
