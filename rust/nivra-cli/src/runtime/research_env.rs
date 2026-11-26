//! Read-only research Python environment builder and configuration (#research-env).
use super::{Config, Result, digest, planner::PlannerConfig};
use nivra_sandbox::SandboxConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
};

const PINNED_REQUIREMENTS: &str = include_str!("../../../../packaging/research/requirements.txt");

fn default_python_path() -> PathBuf {
    PathBuf::from("/usr/bin/python3")
}
fn default_memory_mb() -> u64 {
    512
}
fn default_cpu_seconds() -> u64 {
    10
}
fn default_wall_ms() -> u64 {
    15_000
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ResearchEnvSettings {
    pub env_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_sha256: Option<String>,
    #[serde(default = "default_python_path")]
    pub python_path: PathBuf,
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u64,
    #[serde(default = "default_cpu_seconds")]
    pub cpu_seconds: u64,
    #[serde(default = "default_wall_ms")]
    pub wall_ms: u64,
}

impl Default for ResearchEnvSettings {
    fn default() -> Self {
        Self {
            env_dir: PathBuf::new(),
            env_sha256: None,
            python_path: default_python_path(),
            memory_mb: default_memory_mb(),
            cpu_seconds: default_cpu_seconds(),
            wall_ms: default_wall_ms(),
        }
    }
}

impl ResearchEnvSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        if !nivra_sandbox::env::path_ok(&self.env_dir)
            || matches!(
                self.env_dir.to_str(),
                Some("/" | "/usr" | "/etc" | "/bin" | "/lib" | "/lib64")
            )
            || !self.python_path.is_absolute()
            || !(32..=2048).contains(&self.memory_mb)
            || !(1..=30).contains(&self.cpu_seconds)
            || !(1..=30_000).contains(&self.wall_ms)
            || self
                .env_sha256
                .as_deref()
                .is_some_and(|hash| !nivra_sandbox::env::valid_hash(hash))
        {
            return Err("research_env_settings_invalid".into());
        }
        Ok(())
    }

    pub(crate) fn apply_to(&self, base: &SandboxConfig) -> SandboxConfig {
        if base.env_dir.is_some() {
            return base.clone();
        }
        let mut out = base.clone();
        out.env_dir = Some(self.env_dir.clone());
        out.env_sha256 = self.env_sha256.clone();
        if out.python_path == Path::new("/usr/bin/python3") {
            out.python_path = self.python_path.clone();
        }
        out.memory_mb = out.memory_mb.max(self.memory_mb);
        out.cpu_seconds = out.cpu_seconds.max(self.cpu_seconds);
        out.wall_ms = out.wall_ms.max(self.wall_ms);
        out
    }
}

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    /// Build or rebuild the pinned read-only Python environment from requirements.
    Build {
        /// Paper config, planner config, or standalone research_env JSON.
        #[arg(long)]
        config: PathBuf,
        /// Optional custom hash-pinned requirements file (defaults to committed lockfile).
        #[arg(long)]
        requirements: Option<PathBuf>,
    },
    /// Verify the built Python environment and print its content SHA-256 hash.
    Verify {
        /// Paper config, planner config, or standalone research_env JSON.
        #[arg(long)]
        config: PathBuf,
    },
}

fn load_settings(path: &Path) -> Result<ResearchEnvSettings> {
    let raw = crate::read_bounded(path, 65_536).map_err(|_| "research_env_config_unreadable")?;
    let value = nivra_core::native::decode(&raw).map_err(|_| "research_env_config_invalid")?;
    let settings = if let Some(planner_path) = value.get("planner_config").and_then(Value::as_str) {
        let cfg = match value.get("session").and_then(Value::as_str) {
            Some(session) => {
                let day: chrono::NaiveDate =
                    session.parse().map_err(|_| "research_env_config_invalid")?;
                PlannerConfig::load_for_session(Path::new(planner_path), day)?
            }
            None => PlannerConfig::load(Path::new(planner_path))?,
        };
        cfg.research_env.ok_or("research_env_not_configured")?
    } else if value.get("account").is_some() {
        let paper: Config =
            serde_json::from_value(value).map_err(|_| "research_env_config_invalid")?;
        let planner_path = paper
            .planner_config
            .as_deref()
            .ok_or("planner_config_missing")?;
        PlannerConfig::load_for_session(planner_path, paper.session)?
            .research_env
            .ok_or("research_env_not_configured")?
    } else if value.get("research_env").is_some() || value.get("enabled").is_some() {
        let cfg = PlannerConfig::parse(&raw)?;
        cfg.research_env.ok_or("research_env_not_configured")?
    } else {
        serde_json::from_value(value).map_err(|_| "research_env_config_invalid")?
    };
    settings.validate()?;
    Ok(settings)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PackageInfo {
    name: String,
    version: String,
}

fn collect_packages(site_packages: &Path) -> Result<Vec<PackageInfo>> {
    let mut packages = Vec::new();
    for entry in fs::read_dir(site_packages)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.ends_with(".dist-info") {
            continue;
        }
        let metadata_path = entry.path().join("METADATA");
        let Ok(text) = fs::read_to_string(metadata_path) else {
            continue;
        };
        let mut pkg_name = None;
        let mut pkg_version = None;
        for line in text.lines() {
            if line.is_empty() {
                break;
            }
            if let Some(rest) = line.strip_prefix("Name: ") {
                pkg_name = Some(rest.trim().to_owned());
            } else if let Some(rest) = line.strip_prefix("Version: ") {
                pkg_version = Some(rest.trim().to_owned());
            }
        }
        if let (Some(name), Some(version)) = (pkg_name, pkg_version) {
            packages.push(PackageInfo { name, version });
        }
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packages)
}

struct StagingCleanup(Option<PathBuf>, Option<PathBuf>);
impl Drop for StagingCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_dir_all(path);
        }
        if let Some(path) = self.1.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn build_env(settings: &ResearchEnvSettings, requirements: Option<&Path>) -> Result<Value> {
    if !settings.python_path.is_file() {
        return Err("research_env_python_missing".into());
    }
    let req_bytes = match requirements {
        Some(path) => {
            crate::read_bounded(path, 2 * 1024 * 1024).map_err(|_| "research_env_req_unreadable")?
        }
        None => PINNED_REQUIREMENTS.as_bytes().to_vec(),
    };
    let requirements_sha256 = digest(&req_bytes);

    let parent = settings
        .env_dir
        .parent()
        .ok_or("research_env_dir_invalid")?;
    fs::create_dir_all(parent)?;
    let name = settings
        .env_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("research_env_dir_invalid")?;
    let pid = std::process::id();
    let req_path = parent.join(format!(".{name}.req.{pid}.txt"));
    fs::write(&req_path, &req_bytes)?;
    let staging = parent.join(format!(".{name}.staging.{pid}"));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir(&staging)?;
    let mut cleanup = StagingCleanup(Some(staging.clone()), Some(req_path.clone()));

    let output = ProcessCommand::new(&settings.python_path)
        .args([
            "-m",
            "pip",
            "install",
            "--require-hashes",
            "--only-binary=:all:",
            "--no-compile",
            "--ignore-installed",
            "--no-deps",
            "--no-input",
            "--disable-pip-version-check",
            "--target",
        ])
        .arg(&staging)
        .arg("-r")
        .arg(&req_path)
        .output()
        .map_err(|_| "research_env_pip_unavailable")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        for line in stderr
            .lines()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            eprintln!("pip: {line}");
        }
        return Err("research_env_pip_install_failed".into());
    }

    let env_sha256 =
        nivra_sandbox::env::compute_dir_hash(&staging).map_err(|_| "research_env_hash_failed")?;
    let packages = collect_packages(&staging)?;
    let manifest = json!({
        "version": 1,
        "env_sha256": env_sha256,
        "requirements_sha256": requirements_sha256,
        "python_path": settings.python_path,
        "packages": packages,
    });
    fs::write(
        staging.join(nivra_sandbox::env::ENV_MANIFEST_NAME),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    if let Ok(meta) = fs::symlink_metadata(&settings.env_dir) {
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("research_env_dir_invalid".into());
        }
        let backup = parent.join(format!(".{name}.old.{pid}"));
        if backup.exists() {
            fs::remove_dir_all(&backup)?;
        }
        fs::rename(&settings.env_dir, &backup)?;
        if let Err(err) = fs::rename(&staging, &settings.env_dir) {
            let _ = fs::rename(&backup, &settings.env_dir);
            return Err(err.into());
        }
        cleanup.0 = None;
        let _ = fs::remove_dir_all(&backup);
    } else {
        fs::rename(&staging, &settings.env_dir)?;
        cleanup.0 = None;
    }

    Ok(json!({
        "status": "ok",
        "env_dir": settings.env_dir,
        "env_sha256": env_sha256,
        "requirements_sha256": requirements_sha256,
        "python_path": settings.python_path,
        "packages": packages,
    }))
}

fn verify_env(settings: &ResearchEnvSettings) -> Result<Value> {
    let (site_packages, actual_sha256) = nivra_sandbox::env::verified_site_packages(
        &settings.env_dir,
        settings.env_sha256.as_deref(),
    )
    .map_err(|_| "research_env_verification_failed")?;
    let packages = collect_packages(&site_packages)?;
    Ok(json!({
        "status": "ok",
        "env_dir": settings.env_dir,
        "site_packages": site_packages,
        "env_sha256": actual_sha256,
        "pinned_sha256": settings.env_sha256,
        "packages": packages,
    }))
}

pub(crate) fn entry(command: Command) -> ExitCode {
    let result = match command {
        Command::Build {
            config,
            requirements,
        } => load_settings(&config).and_then(|s| build_env(&s, requirements.as_deref())),
        Command::Verify { config } => load_settings(&config).and_then(|s| verify_env(&s)),
    };
    match result {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "research_env_failed: {}",
                super::failure_code(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}
