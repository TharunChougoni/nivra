//! Hard ceiling on the number of Rust test functions.
//!
//! A fixed test count is not a quality gate, but unbounded growth makes the
//! local parallel gate slow and encourages duplicate micro-tests. Merge or
//! remove tests before raising this limit; do not raise it to land a test.
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const LIMIT: usize = 400;

pub fn check(root: &Path) -> Result<(usize, usize), String> {
    let rust = root.join("rust");
    if !rust.is_dir() {
        return Err("test_budget_rust_directory_missing".into());
    }
    let mut files = Vec::new();
    collect(&rust, &mut files)?;
    files.sort();
    let mut total = 0;
    let mut ignored = 0;
    for file in files {
        let text = fs::read_to_string(&file)
            .map_err(|_| format!("test_budget_unreadable {}", file.display()))?;
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if !(trimmed.starts_with("#[test]") || trimmed.starts_with("#[tokio::test]")) {
                continue;
            }
            total += 1;
            let window = &lines[index.saturating_sub(2)..(index + 3).min(lines.len())];
            if window
                .iter()
                .any(|line| line.trim_start().starts_with("#[ignore"))
            {
                ignored += 1;
            }
        }
    }
    if total > LIMIT {
        return Err(format!(
            "test_budget_exceeded count={total} limit={LIMIT}; merge or remove tests"
        ));
    }
    Ok((total, ignored))
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|_| format!("test_budget_unreadable {}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|_| "test_budget_entry_unreadable".to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if ["target", "node_modules", ".git"].contains(&name.as_ref()) {
                continue;
            }
            collect(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}
