//! Offline sandbox admission; never opens credentials, journals or books.
use chrono::NaiveDate;
use serde::Deserialize;
use std::{
    fs,
    os::unix::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
};
#[derive(Deserialize)]
struct Paths {
    directory: PathBuf,
    sessions_root: Option<PathBuf>,
    read_journal: PathBuf,
    #[serde(default)]
    planner_config: Option<PathBuf>,
    #[serde(default)]
    session: Option<NaiveDate>,
}
fn absolute(path: &Path) -> Result<PathBuf, &'static str> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("preflight_path_invalid");
    }
    fs::canonicalize(path).map_err(|_| "preflight_path_unavailable")
}
fn grants(text: &str, home: &str) -> Result<Vec<PathBuf>, &'static str> {
    text.split_whitespace()
        .map(|word| {
            let word = word.trim_start_matches(['-', '+']);
            let expanded = word.replace("%h", home);
            if expanded.contains('%') || expanded.contains(['"', '\'']) {
                return Err("preflight_unit_syntax_unsupported");
            }
            absolute(Path::new(&expanded))
        })
        .collect()
}
fn inspect(config: &Path, unit: Option<&Path>) -> Result<(), &'static str> {
    let paths: Paths =
        serde_json::from_slice(&fs::read(config).map_err(|_| "preflight_config_unreadable")?)
            .map_err(|_| "preflight_config_invalid")?;
    let home = std::env::var("HOME").map_err(|_| "preflight_home_unavailable")?;
    let root = paths
        .sessions_root
        .unwrap_or_else(|| PathBuf::from(&home).join("nivra-data/sessions"));
    let journal = paths
        .read_journal
        .parent()
        .ok_or("preflight_path_invalid")?;
    let required: [(&Path, &str); 3] = [
        (&paths.directory, "preflight_legacy_not_writable"),
        (&root, "preflight_sessions_not_writable"),
        (journal, "preflight_journal_dir_not_writable"),
    ];
    let mut resolved = Vec::new();
    for (path, code) in required {
        let path = absolute(path).map_err(|_| code)?;
        let probe = path.join(format!(".nivra-preflight-{}", std::process::id()));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&probe)
            .map_err(|_| code)?;
        drop(file);
        fs::remove_file(probe).map_err(|_| code)?;
        resolved.push(path);
    }
    if let Some(planner) = paths.planner_config.as_deref() {
        let planner = match paths.session {
            Some(session) => {
                crate::runtime::planner::PlannerConfig::load_for_session(planner, session)
            }
            None => crate::runtime::planner::PlannerConfig::load(planner),
        }
        .map_err(|_| "preflight_planner_config_invalid")?;
        if let Some(research) = planner.research_tools.as_ref() {
            // Registered research bytes are reviewed inputs. Check them before a
            // post-close review call instead of discovering a stale path later.
            crate::runtime::harness::validate_research_files(research)?;
        }
    }
    let coverage = if let Some(unit) = unit {
        let text = fs::read_to_string(unit).map_err(|_| "preflight_unit_unreadable")?;
        let mut service = false;
        let mut values = String::new();
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                service = line == "[Service]";
            }
            if service && let Some(value) = line.strip_prefix("ReadWritePaths=") {
                if value.is_empty() {
                    values.clear();
                } else {
                    values.push(' ');
                    values.push_str(value);
                }
            }
        }
        Some(values)
    } else if std::env::var_os("INVOCATION_ID").is_some() {
        let output = Command::new("systemctl")
            .args([
                "--user",
                "show",
                "nivra-paper.service",
                "--property=ReadWritePaths",
                "--value",
            ])
            .output()
            .map_err(|_| "preflight_unit_query_failed")?;
        if !output.status.success() {
            return Err("preflight_unit_query_failed");
        }
        Some(String::from_utf8(output.stdout).map_err(|_| "preflight_unit_query_failed")?)
    } else {
        None
    };
    if let Some(coverage) = coverage {
        let allowed = grants(&coverage, &home)?;
        if resolved
            .iter()
            .any(|p| !allowed.iter().any(|a| p.starts_with(a)))
        {
            return Err("preflight_unit_path_uncovered");
        }
    }
    Ok(())
}
pub fn entry(config: &Path, unit: Option<&Path>) -> ExitCode {
    match inspect(config, unit) {
        Ok(()) => {
            println!("preflight_ok");
            ExitCode::SUCCESS
        }
        Err(code) => {
            eprintln!("{code}");
            ExitCode::FAILURE
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_and_unit_coverage_fail_closed_without_touching_journal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let journal = root.join("journal");
        fs::write(&journal, "original").unwrap();
        let config = root.join("config.json");
        fs::write(
            &config,
            serde_json::to_vec(
                &serde_json::json!({"directory":root,"sessions_root":root,"read_journal":journal}),
            )
            .unwrap(),
        )
        .unwrap();
        let unit = root.join("paper.service");
        fs::write(
            &unit,
            format!("[Service]\nReadWritePaths={}\n", root.display()),
        )
        .unwrap();
        assert_eq!(inspect(&config, Some(&unit)), Ok(()));
        fs::write(&unit, "[Service]\nReadWritePaths=/tmp\nReadWritePaths=\n").unwrap();
        assert_eq!(
            inspect(&config, Some(&unit)),
            Err("preflight_unit_path_uncovered")
        );
        fs::write(&config, serde_json::to_vec(&serde_json::json!({"directory":journal,"sessions_root":root,"read_journal":journal})).unwrap()).unwrap();
        assert_eq!(
            inspect(&config, Some(&unit)),
            Err("preflight_legacy_not_writable")
        );
        for (field, code) in [
            ("sessions_root", "preflight_sessions_not_writable"),
            ("read_journal", "preflight_journal_dir_not_writable"),
        ] {
            let mut value =
                serde_json::json!({"directory":root,"sessions_root":root,"read_journal":journal});
            value[field] = serde_json::json!(root.join("missing").join("file"));
            fs::write(&config, serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(inspect(&config, Some(&unit)), Err(code));
        }
        fs::write(
            &unit,
            format!("[Service]\nReadWritePaths={}\n", root.display()),
        )
        .unwrap();
        let code = root.join("analysis.py");
        let runtime = root.join("nivra-analysis");
        let input = root.join("dataset.json");
        fs::write(&code, b"print('fixture')\n").unwrap();
        fs::write(&runtime, b"runtime-fixture").unwrap();
        fs::write(&input, b"input-fixture").unwrap();
        let sha = |bytes: &[u8]| {
            use sha2::{Digest, Sha256};
            Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let planner = root.join("planner.json");
        let write_planner = |runtime: &Path, runtime_sha: &str| {
            fs::write(
                &planner,
                serde_json::to_vec(&serde_json::json!({
                    "enabled":false,
                    "research_tools":{
                        "queue":root.join("research.sqlite"),
                        "stage_root":root.join("research-stage"),
                        "registrations":[{
                            "analysis_id":"shared_core_backtest",
                            "dataset_id":"synthetic_no_quotes_fixture",
                            "code_file":code,"runtime_file":runtime,"input_file":input,
                            "code_sha256":sha(b"print('fixture')\n"),
                            "runtime_sha256":runtime_sha,
                            "input_sha256":sha(b"input-fixture"),
                            "memory_bytes":67108864,"wall_millis":5000,
                            "limits":{"cpu_millis":1000,"tasks":2,"workspace_bytes":1048576,"file_bytes":65536}
                        }]
                    }
                }))
                .unwrap(),
            )
            .unwrap();
        };
        let with_planner = root.join("config-planner.json");
        fs::write(
            &with_planner,
            serde_json::to_vec(&serde_json::json!({
                "directory":root,"sessions_root":root,"read_journal":journal,
                "planner_config":planner.clone(),"session":"2026-10-16"
            }))
            .unwrap(),
        )
        .unwrap();
        write_planner(&runtime, &sha(b"runtime-fixture"));
        assert_eq!(inspect(&with_planner, Some(&unit)), Ok(()));
        write_planner(&root.join("stale-nivra-analysis"), &sha(b"runtime-fixture"));
        assert_eq!(
            inspect(&with_planner, Some(&unit)),
            Err("research_runtime_unreadable")
        );
        fs::write(root.join("stale-nivra-analysis"), b"replaced").unwrap();
        assert_eq!(
            inspect(&with_planner, Some(&unit)),
            Err("research_registered_bytes_changed")
        );
        assert_eq!(fs::read_to_string(journal).unwrap(), "original");
    }
}
