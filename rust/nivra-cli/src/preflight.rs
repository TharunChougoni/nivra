//! Offline sandbox admission; never opens credentials, journals or books.
//! Provider credential presence is asked of the pi agent, which reports only
//! whether each configured provider has one.
use crate::runtime::agent::AgentSettings;
use chrono::NaiveDate;
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
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
    let mut granted = Vec::new();
    for word in text.split_whitespace() {
        // systemd ignores a `-` entry whose path does not exist; it grants nothing.
        let optional = word.starts_with('-');
        let word = word.trim_start_matches(['-', '+']);
        let expanded = word.replace("%h", home);
        if expanded.contains('%') || expanded.contains(['"', '\'']) {
            return Err("preflight_unit_syntax_unsupported");
        }
        match absolute(Path::new(&expanded)) {
            Ok(path) => granted.push(path),
            Err("preflight_path_unavailable") if optional => {}
            Err(error) => return Err(error),
        }
    }
    Ok(granted)
}
/// Create and remove a private probe file; returns the canonical directory and
/// the probe's owner, which is the effective user.
fn writable(path: &Path, code: &'static str) -> Result<(PathBuf, u32), &'static str> {
    let path = absolute(path).map_err(|_| code)?;
    let probe = path.join(format!(".nivra-preflight-{}", std::process::id()));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&probe)
        .map_err(|_| code)?;
    let uid = file.metadata().map_err(|_| code)?.uid();
    drop(file);
    fs::remove_file(probe).map_err(|_| code)?;
    Ok((path, uid))
}

#[derive(Deserialize)]
struct AgentCheck {
    models: Vec<CheckedModel>,
}

#[derive(Deserialize)]
struct CheckedModel {
    known: bool,
    auth: bool,
}

/// The pi agent can start, write its own directory and reach every configured
/// lane model and fallback with stored or passed credentials.
fn agent(settings: &AgentSettings) -> Result<PathBuf, &'static str> {
    if !fs::metadata(&settings.node)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    {
        return Err("preflight_agent_node_missing");
    }
    if !settings.entry.is_file() {
        return Err("preflight_agent_entry_missing");
    }
    let package = settings
        .entry
        .ancestors()
        .find(|dir| dir.join("package.json").is_file());
    let installed = package.is_some_and(|package| {
        package
            .join("node_modules/@earendil-works/pi-coding-agent/package.json")
            .is_file()
    });
    if !installed {
        return Err("preflight_agent_modules_missing");
    }
    // The agent reads `<lane>.md` from `--prompts`, or from its package's
    // `prompts/`; a lane without one fails every turn.
    let prompts = settings
        .prompts
        .clone()
        .or_else(|| package.map(|package| package.join("prompts")));
    if !prompts.is_some_and(|dir| {
        settings
            .lanes
            .keys()
            .all(|lane| dir.join(format!("{}.md", lane.as_str())).is_file())
    }) {
        return Err("preflight_agent_prompts_missing");
    }
    let (dir, uid) = writable(&settings.agent_dir, "preflight_agent_dir_not_writable")?;
    if fs::metadata(&dir)
        .map_err(|_| "preflight_agent_dir_not_writable")?
        .uid()
        != uid
    {
        return Err("preflight_agent_dir_not_owned");
    }
    let models: BTreeSet<(&str, &str)> = settings
        .lanes
        .values()
        .flat_map(|lane| std::iter::once(&lane.model).chain(&lane.fallback))
        .map(|model| (model.provider.as_str(), model.id.as_str()))
        .collect();
    let query: Vec<_> = models
        .iter()
        .map(|(provider, id)| serde_json::json!({"provider":provider,"id":id}))
        .collect();
    let output = crate::runtime::agent::command(settings)
        .arg("--check")
        .arg(serde_json::Value::from(query).to_string())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|_| "preflight_agent_check_failed")?;
    let check: AgentCheck = serde_json::from_slice(&output.stdout)
        .ok()
        .filter(|check: &AgentCheck| output.status.success() && check.models.len() == models.len())
        .ok_or("preflight_agent_check_failed")?;
    if check.models.iter().any(|model| !model.known) {
        return Err("preflight_agent_model_unknown");
    }
    if check.models.iter().any(|model| !model.auth) {
        return Err("preflight_agent_auth_missing");
    }
    Ok(dir)
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
        resolved.push(writable(path, code)?.0);
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
        if let Some(env) = planner.research_env.as_ref() {
            nivra_sandbox::env::verified_site_packages(&env.env_dir, env.env_sha256.as_deref())
                .map_err(|_| "preflight_research_env_unavailable")?;
        }
        if let Some(settings) = planner.agent.as_ref() {
            resolved.push(agent(settings)?);
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
        // A missing optional entry grants nothing; a missing required one refuses.
        let missing = root.join("absent-research");
        fs::write(
            &unit,
            format!(
                "[Service]\nReadWritePaths={}\nReadWritePaths=-{}\n",
                root.display(),
                missing.display()
            ),
        )
        .unwrap();
        assert_eq!(inspect(&config, Some(&unit)), Ok(()));
        fs::write(
            &unit,
            format!(
                "[Service]\nReadWritePaths={} {}\n",
                root.display(),
                missing.display()
            ),
        )
        .unwrap();
        assert_eq!(
            inspect(&config, Some(&unit)),
            Err("preflight_path_unavailable")
        );
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

    #[test]
    fn pi_agent_install_directory_and_credentials_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let pi = tempfile::tempdir().unwrap();
        let (root, agent_dir) = (dir.path(), pi.path());
        let journal = root.join("journal");
        fs::write(&journal, "original").unwrap();
        let package = root.join("agent");
        let modules = package.join("node_modules/@earendil-works/pi-coding-agent");
        fs::create_dir_all(&modules).unwrap();
        fs::write(package.join("package.json"), "{}").unwrap();
        fs::write(modules.join("package.json"), "{}").unwrap();
        let prompts = package.join("prompts");
        fs::create_dir(&prompts).unwrap();
        fs::write(prompts.join("decision.md"), "plan").unwrap();
        fs::write(prompts.join("review.md"), "review").unwrap();
        // `sh` stands in for node; the entry records its query and replies.
        let entry = package.join("main.js");
        fs::write(
            &entry,
            "printf '%s' \"$4\" > \"$2/query.json\"; cat \"$2/check.json\"\n",
        )
        .unwrap();
        let planner = root.join("planner.json");
        let write_planner = |node: &Path| {
            let lane = serde_json::json!({
                "model":{"provider":"openai-codex","id":"gpt-6.1-sol"},
                "fallback":{"provider":"mistral","id":"deepseek-v4-1-flash-1017","thinking":"high"},
                "daily_calls":10,"daily_cost_usd":1.0
            });
            fs::write(
                &planner,
                serde_json::json!({"enabled":true,"agent":{"mode":"paper","node":node,"entry":entry,
                    "agent_dir":agent_dir,"lanes":{"decision":lane,"review":lane}}})
                .to_string(),
            )
            .unwrap();
        };
        write_planner(Path::new("/bin/sh"));
        let config = root.join("config.json");
        fs::write(
            &config,
            serde_json::json!({"directory":root,"sessions_root":root,"read_journal":journal,
                "planner_config":planner})
            .to_string(),
        )
        .unwrap();
        let unit = root.join("paper.service");
        let grant = |paths: &[&Path]| {
            let paths: Vec<_> = paths.iter().map(|p| p.display().to_string()).collect();
            fs::write(
                &unit,
                format!("[Service]\nReadWritePaths={}\n", paths.join(" ")),
            )
            .unwrap();
        };
        grant(&[root, agent_dir]);
        let reply = |known: bool, auth: bool| {
            fs::write(
                agent_dir.join("check.json"),
                serde_json::json!({"models":[{"known":true,"auth":true},{"known":known,"auth":auth}]})
                    .to_string(),
            )
            .unwrap();
        };
        reply(true, true);
        assert_eq!(inspect(&config, Some(&unit)), Ok(()));
        let query: serde_json::Value =
            serde_json::from_slice(&fs::read(agent_dir.join("query.json")).unwrap()).unwrap();
        assert_eq!(
            query,
            serde_json::json!([{"provider":"mistral","id":"deepseek-v4-1-flash-1017"},
                {"provider":"openai-codex","id":"gpt-6.1-sol"}])
        );
        grant(&[root]);
        assert_eq!(
            inspect(&config, Some(&unit)),
            Err("preflight_unit_path_uncovered")
        );
        grant(&[root, agent_dir]);
        reply(true, false);
        assert_eq!(
            inspect(&config, Some(&unit)),
            Err("preflight_agent_auth_missing")
        );
        reply(false, true);
        assert_eq!(
            inspect(&config, Some(&unit)),
            Err("preflight_agent_model_unknown")
        );
        fs::write(agent_dir.join("check.json"), "not json").unwrap();
        assert_eq!(
            inspect(&config, Some(&unit)),
            Err("preflight_agent_check_failed")
        );
        fs::set_permissions(agent_dir, fs::Permissions::from_mode(0o500)).unwrap();
        let readonly = inspect(&config, Some(&unit));
        fs::set_permissions(agent_dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(readonly, Err("preflight_agent_dir_not_writable"));
        fs::remove_file(prompts.join("review.md")).unwrap();
        assert_eq!(
            inspect(&config, Some(&unit)),
            Err("preflight_agent_prompts_missing")
        );
        fs::remove_file(modules.join("package.json")).unwrap();
        assert_eq!(
            inspect(&config, Some(&unit)),
            Err("preflight_agent_modules_missing")
        );
        fs::remove_file(&entry).unwrap();
        assert_eq!(
            inspect(&config, Some(&unit)),
            Err("preflight_agent_entry_missing")
        );
        write_planner(&root.join("missing-node"));
        assert_eq!(
            inspect(&config, Some(&unit)),
            Err("preflight_agent_node_missing")
        );
        assert_eq!(fs::read_to_string(journal).unwrap(), "original");
    }
}
