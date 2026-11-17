//! Public roadmap tracking and local Git history; no runtime/process discovery.
use std::{path::Path, process::Command};

pub fn run(root: &Path) -> Result<(), &'static str> {
    let output = Command::new("git")
        .args(["log", "--oneline", "-8"])
        .current_dir(root)
        .output()
        .map_err(|_| "progress_history_unavailable")?;
    let log = std::str::from_utf8(&output.stdout).map_err(|_| "progress_history_encoding")?;
    println!(
        "Nivra\nRoadmap: https://github.com/TharunChougoni/nivra/issues\n---\n{}",
        log.trim()
    );
    Ok(())
}
