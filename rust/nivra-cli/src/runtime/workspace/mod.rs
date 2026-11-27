//! Bounded paper-book workspace for the decision-owning orchestrator (#211).
//!
//! Output is advisory evidence, never an order or execution authority. The
//! trading book and host journal are outside this tree and cannot be written.
use super::*;
use nivra_sandbox::ScorerSandboxConfig;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_RANKER_INPUT_BYTES: usize = 65_536;
const REFERENCE_STRATEGIES: &[(&str, &[u8])] =
    include!(concat!(env!("OUT_DIR"), "/reference_strategies.rs"));

const README: &str = r#"# Orchestrator workspace

This directory belongs to the persistent paper decision owner. Files here are
notes, skills, advisory scanner output and installed tools. They are untrusted
evidence, never prices, quotes, fills or execution authority.

Layout:

- `notes/` persistent notes across turns
- `skills/` standing procedures the owner writes for itself
- `tools/` files installed only when name and sha256 match the reviewed allowlist
- `reference/strategies/` shipped strategy sources, parameters and benchmark notes
- `scripts/` Python rankers written and run by the pi research lane
- `data/` bounded input JSON the owner prepares for a script
- `reports/` advisory output the owner chooses to keep
- `journal/` workspace notes, not the trading book journal

The decision owner can write under `notes/`, `skills/`, `reports/`, `data/` and
`journal/` and run reviewed scripts. The pi research lane can write notes,
skills and `scripts/<name>.py` and run its rankers with explicit inline features.
Its sandbox always disables network access. All scores are advisory.
"#;

fn default_root() -> PathBuf {
    PathBuf::from("workspace")
}
fn default_max_file_bytes() -> u64 {
    1_048_576
}
fn default_max_write_bytes() -> u64 {
    65_536
}
fn default_max_list_entries() -> usize {
    256
}
fn default_run_allowlist() -> Vec<String> {
    Vec::new()
}
fn default_cpu_seconds() -> u64 {
    30
}
fn default_memory_mb() -> u64 {
    512
}
fn default_wall_ms() -> u64 {
    30_000
}
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallAllow {
    pub name: String,
    pub sha256: String,
}

/// Research turns may only write notes/skills. Decision turns keep the existing
/// notes/skills/reports/data/journal scope. The pi research lane writes like a
/// research turn, can write Python rankers, may run rankers and allowlisted
/// scripts in the no-network sandbox, and may install hash-allowlisted files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceScope {
    Decision,
    Research,
    ResearchSandbox,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceSettings {
    pub enabled: bool,
    #[serde(default = "default_root")]
    pub root: PathBuf,
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,
    #[serde(default = "default_max_write_bytes")]
    pub max_write_bytes: u64,
    #[serde(default = "default_max_list_entries")]
    pub max_list_entries: usize,
    #[serde(default = "default_run_allowlist")]
    pub run_allowlist: Vec<String>,
    #[serde(default)]
    pub install_allowlist: Vec<InstallAllow>,
    #[serde(default = "default_cpu_seconds")]
    pub cpu_seconds: u64,
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u64,
    #[serde(default = "default_wall_ms")]
    pub wall_ms: u64,
    /// Explicit opt-in outbound network for allowlisted scanner scripts (#222).
    /// Default off and omitted from serialized policy when disabled. The scorer
    /// trigger never enables it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub network: bool,
    /// Optional memory cap (MiB) for `scripts/` rankers. When absent, stdlib-only
    /// rankers stay capped at 256 MiB and env-backed rankers use `memory_mb` (512 MiB default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranker_memory_mb: Option<u64>,
    /// Optional read-only Python environment directory; overrides `planner.research_env`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_dir: Option<PathBuf>,
    /// Optional pinned SHA-256 content hash for `env_dir`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_sha256: Option<String>,
}

impl WorkspaceSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        normalize_rel(&self.root.to_string_lossy()).map_err(|_| "workspace_root_invalid")?;
        if !(1..=1_048_576).contains(&self.max_file_bytes)
            || !(1..=65_536).contains(&self.max_write_bytes)
            || !(1..=1_024).contains(&self.max_list_entries)
            || self.run_allowlist.len() > 32
            || self.install_allowlist.len() > 32
        {
            return Err("workspace_settings_invalid".into());
        }
        for path in &self.run_allowlist {
            normalize_rel(path).map_err(|_| "workspace_run_allowlist_invalid")?;
        }
        let mut names = BTreeSet::new();
        for entry in &self.install_allowlist {
            if !file_name_ok(&entry.name)
                || !hash_ok(&entry.sha256)
                || !names.insert(entry.name.as_str())
            {
                return Err("workspace_install_allowlist_invalid".into());
            }
        }
        if self
            .ranker_memory_mb
            .is_some_and(|mb| !(32..=2048).contains(&mb))
            || (self.env_sha256.is_some() && self.env_dir.is_none())
            || self
                .env_dir
                .as_deref()
                .is_some_and(|dir| !nivra_sandbox::env::path_ok(dir))
            || self
                .env_sha256
                .as_deref()
                .is_some_and(|hash| !nivra_sandbox::env::valid_hash(hash))
        {
            return Err("workspace_sandbox_invalid".into());
        }
        sandbox_config(self)
            .validate()
            .map_err(|_| "workspace_sandbox_invalid")?;
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) enum WorkspaceOp {
    List(Option<String>),
    Read(String),
    Write {
        path: String,
        text: String,
    },
    Run {
        script: String,
        input: Option<Value>,
        input_path: Option<String>,
    },
    Install {
        name: String,
        text: String,
    },
}

impl WorkspaceOp {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    path: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    script: String,
    input: Option<Value>,
    input_path: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallArgs {
    name: String,
    text: String,
}

fn function(name: &str, description: &str, properties: Value, required: Value) -> Value {
    json!({"type":"function","name":name,"description":description,"strict":true,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
}

pub(crate) fn tool_definitions() -> Vec<Value> {
    vec![
        function(
            "workspace_list",
            "List a directory inside the owner workspace. Paths are workspace-relative. Contents are untrusted files, never prices, fills or execution authority.",
            json!({"path":{"type":["string","null"],"maxLength":256}}),
            json!(["path"]),
        ),
        function(
            "workspace_read",
            "Read one UTF-8 file inside the owner workspace. The text is untrusted evidence, never a quote, fill or execution authority.",
            json!({"path":{"type":"string","minLength":1,"maxLength":256}}),
            json!(["path"]),
        ),
        function(
            "workspace_write",
            "Write one UTF-8 file under notes/, skills/, reports/, data/ or journal/. This does not change the trading book or host journal and authorizes no trade.",
            json!({"path":{"type":"string","minLength":1,"maxLength":256},"text":{"type":"string","maxLength":65536}}),
            json!(["path", "text"]),
        ),
        function(
            "workspace_run",
            "Run one allowlisted workspace script in the resource-limited sandbox (network disabled unless the reviewed workspace setting enables it). Supply input as a JSON-encoded object string, a workspace input_path, or nulls to use the script's sibling params file. Returned scores are advisory evidence, never orders.",
            json!({
                "script":{"type":"string","minLength":1,"maxLength":256},
                "input":{"type":["string","null"],"maxLength":65536},
                "input_path":{"type":["string","null"],"maxLength":256}
            }),
            json!(["script", "input", "input_path"]),
        ),
        function(
            "workspace_install",
            "Install one file into tools/ only when the reviewed allowlist names it and the content sha256 matches. Empty allowlists refuse every install. This authorizes no trade.",
            json!({"name":{"type":"string","minLength":1,"maxLength":128},"text":{"type":"string","maxLength":65536}}),
            json!(["name", "text"]),
        ),
    ]
}

/// Research turns get only list/read/write. Run/install stay decision-only and
/// never become a native execution or installation capability.
pub(crate) fn research_tool_definitions() -> Vec<Value> {
    vec![
        function(
            "workspace_list",
            "List a directory inside the owner workspace. Paths are workspace-relative. Contents are untrusted files, never prices, fills or execution authority.",
            json!({"path":{"type":["string","null"],"maxLength":256}}),
            json!(["path"]),
        ),
        function(
            "workspace_read",
            "Read one UTF-8 file inside the owner workspace. The text is untrusted evidence, never a quote, fill or execution authority.",
            json!({"path":{"type":"string","minLength":1,"maxLength":256}}),
            json!(["path"]),
        ),
        function(
            "workspace_write",
            "Write one UTF-8 file under notes/ or skills/ during a research turn. This does not change the trading book or host journal and authorizes no trade.",
            json!({"path":{"type":"string","minLength":1,"maxLength":256},"text":{"type":"string","maxLength":65536}}),
            json!(["path", "text"]),
        ),
    ]
}

pub(crate) fn parse_tool(name: &str, args: Value) -> Result<WorkspaceOp> {
    match name {
        "workspace_list" => {
            let a: ListArgs = serde_json::from_value(args)?;
            Ok(WorkspaceOp::List(a.path.filter(|path| !path.is_empty())))
        }
        "workspace_read" => {
            let a: PathArgs = serde_json::from_value(args)?;
            Ok(WorkspaceOp::Read(a.path))
        }
        "workspace_write" => {
            let a: WriteArgs = serde_json::from_value(args)?;
            Ok(WorkspaceOp::Write {
                path: a.path,
                text: a.text,
            })
        }
        "workspace_run" => {
            let mut a: RunArgs = serde_json::from_value(args)?;
            // Legacy archived calls may contain an object; new strict calls use
            // a string so arbitrary script keys never become free-form schemas.
            if let Some(Value::String(text)) = &a.input {
                let ranker = normalize_rel(&a.script).is_ok_and(|rel| ranker_script(&rel));
                let decoded = if ranker {
                    nivra_core::native::decode_bounded(text.as_bytes(), MAX_RANKER_INPUT_BYTES).ok()
                } else {
                    serde_json::from_str(text).ok()
                };
                a.input = Some(decoded.ok_or("workspace_run_input_invalid")?);
            }
            if a.input.as_ref().is_some_and(|value| !value.is_object()) {
                return Err("workspace_run_input_invalid".into());
            }
            Ok(WorkspaceOp::Run {
                script: a.script,
                input: a.input,
                input_path: a.input_path.filter(|path| !path.is_empty()),
            })
        }
        "workspace_install" => {
            let a: InstallArgs = serde_json::from_value(args)?;
            Ok(WorkspaceOp::Install {
                name: a.name,
                text: a.text,
            })
        }
        _ => Err("harness_tool_not_allowed".into()),
    }
}

#[cfg(test)]
pub(crate) fn execute(book: &Path, settings: &WorkspaceSettings, op: WorkspaceOp) -> Result<Value> {
    execute_scoped(book, settings, WorkspaceScope::Decision, op)
}

#[cfg(test)]
pub(crate) fn execute_scoped(
    book: &Path,
    settings: &WorkspaceSettings,
    scope: WorkspaceScope,
    op: WorkspaceOp,
) -> Result<Value> {
    execute_scoped_with_env(book, settings, scope, None, op)
}

pub(crate) fn execute_scoped_with_env(
    book: &Path,
    settings: &WorkspaceSettings,
    scope: WorkspaceScope,
    env: Option<&super::research_env::ResearchEnvSettings>,
    op: WorkspaceOp,
) -> Result<Value> {
    settings.validate()?;
    if !settings.enabled {
        return Err("workspace_disabled".into());
    }
    if scope == WorkspaceScope::Research
        && matches!(op, WorkspaceOp::Run { .. } | WorkspaceOp::Install { .. })
    {
        return Err("workspace_research_scope_invalid".into());
    }
    let root = ensure(book, settings)?;
    match op {
        WorkspaceOp::List(path) => list(&root, settings, path.as_deref()),
        WorkspaceOp::Read(path) => read_file(&root, settings, &path),
        WorkspaceOp::Write { path, text } => write_file(&root, settings, scope, &path, &text),
        WorkspaceOp::Run {
            script,
            input,
            input_path,
        } => run_script(
            &root,
            settings,
            scope,
            env,
            &script,
            input,
            input_path.as_deref(),
        ),
        WorkspaceOp::Install { name, text } => install_tool(&root, settings, &name, &text),
    }
}

pub(crate) fn ensure(book: &Path, settings: &WorkspaceSettings) -> Result<PathBuf> {
    settings.validate()?;
    let root = root_dir(book, settings)?;
    ensure_private_root(book, &root)?;
    for name in ["notes", "skills", "tools", "data", "reports", "journal"] {
        ensure_private_dir(&root, &root.join(name))?;
    }
    write_if_missing(&root, &root.join("README.md"), README.as_bytes())?;
    seed_references(&root)?;
    Ok(root)
}

fn seed_references(root: &Path) -> Result<()> {
    let directory = root.join("reference/strategies");
    ensure_private_dir(root, &directory.join("costs"))?;
    for &(name, bytes) in REFERENCE_STRATEGIES {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_file() || meta.uid() != current_uid() => {
                return Err("workspace_seed_failed".into());
            }
            Ok(meta)
                if meta.mode() & 0o777 == 0o400
                    && meta.len() == bytes.len() as u64
                    && fs::read(&path).is_ok_and(|existing| existing == bytes) => {}
            Ok(_) => atomic_write_private(root, &path, bytes, 0o400)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                atomic_write_private(root, &path, bytes, 0o400)?;
            }
            Err(_) => return Err("workspace_seed_failed".into()),
        }
    }
    Ok(())
}

fn root_dir(book: &Path, settings: &WorkspaceSettings) -> Result<PathBuf> {
    let rel = normalize_rel(&settings.root.to_string_lossy())?;
    Ok(book.join(rel))
}

fn list(root: &Path, settings: &WorkspaceSettings, path: Option<&str>) -> Result<Value> {
    let rel = match path {
        None | Some("") | Some(".") => PathBuf::new(),
        Some(path) => normalize_rel(path)?,
    };
    let dir = resolve(root, &rel, false)?;
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("workspace_missing".into());
        }
        Err(_) => return Err("workspace_list_failed".into()),
    };
    let mut listed = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| "workspace_list_failed")?;
        let meta = match fs::symlink_metadata(entry.path()) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "workspace_list_failed")?;
        if !component_ok(&name) {
            continue;
        }
        listed.push((
            name,
            if meta.is_dir() { "directory" } else { "file" },
            meta.len(),
        ));
    }
    listed.sort_by(|a, b| a.0.cmp(&b.0));
    let truncated = listed.len() > settings.max_list_entries;
    listed.truncate(settings.max_list_entries);
    Ok(json!({
        "path": rel_display(&rel),
        "entries": listed.into_iter().map(|(name, kind, size)| {
            json!({"name":name,"kind":kind,"size_bytes":size})
        }).collect::<Vec<_>>(),
        "truncated": truncated,
        "execution_authorized": false,
        "evidence_only": true
    }))
}

fn read_file(root: &Path, settings: &WorkspaceSettings, path: &str) -> Result<Value> {
    let rel = normalize_rel(path)?;
    let file = resolve(root, &rel, true)?;
    let meta = fs::symlink_metadata(&file).map_err(|_| "workspace_missing")?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("workspace_not_file".into());
    }
    if meta.len() > settings.max_file_bytes {
        return Err("workspace_file_too_large".into());
    }
    let bytes = fs::read(&file).map_err(|_| "workspace_read_failed")?;
    let text = String::from_utf8(bytes).map_err(|_| "workspace_not_text")?;
    Ok(json!({
        "path": rel_display(&rel),
        "bytes": text.len(),
        "text": text,
        "sha256": digest(text.as_bytes()),
        "execution_authorized": false,
        "evidence_only": true
    }))
}

fn write_file(
    root: &Path,
    settings: &WorkspaceSettings,
    scope: WorkspaceScope,
    path: &str,
    text: &str,
) -> Result<Value> {
    put_file(root, settings, path, text, |rel| match scope {
        WorkspaceScope::Decision => write_allowed(rel),
        WorkspaceScope::Research => research_write_allowed(rel),
        WorkspaceScope::ResearchSandbox => research_write_allowed(rel) || ranker_script(rel),
    })
}

fn put_file(
    root: &Path,
    settings: &WorkspaceSettings,
    path: &str,
    text: &str,
    allowed: impl Fn(&Path) -> bool,
) -> Result<Value> {
    let rel = normalize_rel(path)?;
    if !allowed(&rel) {
        return Err("workspace_write_refused".into());
    }
    if text.len() as u64 > settings.max_write_bytes
        || text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
        || super::transcript::redact(&json!(text)).as_str() != Some(text)
    {
        return Err("workspace_write_invalid".into());
    }
    let dest = resolve(root, &rel, false)?;
    if let Some(parent) = dest.parent() {
        ensure_private_dir(root, parent)?;
    }
    atomic_write_private(root, &dest, text.as_bytes(), 0o600)?;
    Ok(json!({
        "path": rel_display(&rel),
        "bytes": text.len(),
        "sha256": digest(text.as_bytes()),
        "execution_authorized": false,
        "evidence_only": true
    }))
}

fn run_script(
    root: &Path,
    settings: &WorkspaceSettings,
    scope: WorkspaceScope,
    env: Option<&super::research_env::ResearchEnvSettings>,
    script: &str,
    input: Option<Value>,
    input_path: Option<&str>,
) -> Result<Value> {
    if input.is_some() && input_path.is_some() {
        return Err("workspace_run_input_ambiguous".into());
    }
    let rel = normalize_rel(script)?;
    let base = sandbox_config(settings);
    let mut sandbox = match env {
        Some(env) if base.env_dir.is_none() => env.apply_to(&base),
        _ => base,
    };
    if scope == WorkspaceScope::ResearchSandbox {
        sandbox.network = false;
    }
    if ranker_script(&rel) {
        if scope != WorkspaceScope::ResearchSandbox {
            return Err("workspace_run_not_allowlisted".into());
        }
        sandbox.memory_mb = match settings.ranker_memory_mb {
            Some(cap) => cap,
            None if sandbox.env_dir.is_some() => sandbox.memory_mb,
            None => sandbox.memory_mb.min(256),
        };
        return run_ranker(root, settings, &sandbox, &rel, input);
    }
    let allowed = settings.run_allowlist.iter().any(|entry| {
        normalize_rel(entry)
            .ok()
            .is_some_and(|allowed| allowed == rel)
    });
    if !allowed {
        return Err("workspace_run_not_allowlisted".into());
    }
    let script_path = resolve(root, &rel, true)?;
    let source = fs::read(&script_path).map_err(|_| "workspace_run_missing")?;
    if source.len() as u64 > settings.max_file_bytes {
        return Err("workspace_file_too_large".into());
    }
    let mut features = match input {
        Some(Value::Object(map)) => map_features(map),
        None => BTreeMap::new(),
        Some(_) => return Err("workspace_run_input_invalid".into()),
    };
    if let Some(path) = input_path {
        let input_rel = normalize_rel(path)?;
        let file = resolve(root, &input_rel, true)?;
        let bytes = fs::read(&file).map_err(|_| "workspace_run_input_missing")?;
        if bytes.len() as u64 > settings.max_file_bytes {
            return Err("workspace_file_too_large".into());
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "workspace_run_input_invalid")?;
        let Value::Object(map) = value else {
            return Err("workspace_run_input_invalid".into());
        };
        features = map_features(map);
    }
    let mut params_source = Value::Null;
    if !features.contains_key("params") {
        let params_rel = rel.with_extension("params.json");
        if let Ok(params_path) = resolve(root, &params_rel, true)
            && let Ok(bytes) = fs::read(&params_path)
            && bytes.len() as u64 <= settings.max_file_bytes
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        {
            features.insert("params".into(), value);
            params_source = json!(rel_display(&params_rel));
        }
    } else {
        params_source = json!("input");
    }
    score_script(&sandbox, &rel, &source, &features, params_source)
}

fn run_ranker(
    root: &Path,
    settings: &WorkspaceSettings,
    sandbox: &ScorerSandboxConfig,
    rel: &Path,
    input: Option<Value>,
) -> Result<Value> {
    let Some(Value::Object(rows)) = input else {
        return Err("workspace_run_ranker_input_required".into());
    };
    if rows.is_empty()
        || rows.iter().any(|(key, row)| {
            key == "params" || key.chars().any(char::is_control) || !row.is_object()
        })
    {
        return Err("workspace_run_input_invalid".into());
    }
    let features = map_features(rows);
    if serde_json::to_vec(&features)?.len() > MAX_RANKER_INPUT_BYTES {
        return Err("workspace_run_input_invalid".into());
    }

    let source = read_ranker(root, settings, rel)?;
    let mut result = score_script(sandbox, rel, &source, &features, Value::Null)?;
    result["source_sha256"] = json!(digest(&source));
    Ok(result)
}

fn read_ranker(root: &Path, settings: &WorkspaceSettings, rel: &Path) -> Result<Vec<u8>> {
    let path = resolve(root, rel, true)?;
    ensure_private_dir(root, &root.join("scripts"))?;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "workspace_run_missing")?;
    let meta = file.metadata().map_err(|_| "workspace_run_missing")?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err("workspace_not_file".into());
    }
    if meta.uid() != current_uid() || meta.mode() & 0o077 != 0 {
        return Err("workspace_permissions".into());
    }
    let limit = settings.max_file_bytes.min(settings.max_write_bytes);
    let mut source = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut source)
        .map_err(|_| "workspace_run_missing")?;
    if source.len() as u64 > limit {
        return Err("workspace_file_too_large".into());
    }
    Ok(source)
}

pub(crate) fn register_ranker(
    book: &Path,
    settings: &WorkspaceSettings,
    script: &str,
    expected_sha256: &str,
    author_model: &str,
    library_path: &Path,
) -> Result<nivra_sandbox::library::Version> {
    settings.validate()?;
    let rel = normalize_rel(script)?;
    if !settings.enabled || !ranker_script(&rel) || !hash_ok(expected_sha256) {
        return Err("workspace_ranker_registration_invalid".into());
    }
    let root = root_dir(book, settings)?;
    let source = read_ranker(&root, settings, &rel)?;
    if digest(&source) != expected_sha256 {
        return Err("workspace_ranker_hash_mismatch".into());
    }
    let body = std::str::from_utf8(&source).map_err(|_| "workspace_ranker_source_invalid")?;
    let name = rel
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("workspace_ranker_registration_invalid")?;
    let mut library = nivra_sandbox::library::Library::open(library_path)?;
    Ok(library.add_version(nivra_sandbox::library::NewVersion {
        name,
        file_body: body,
        author_model,
        intent: "operator-reviewed research ranker",
        inputs: &Value::Null,
        created_at_ms: millis(),
    })?)
}

fn score_script(
    sandbox: &ScorerSandboxConfig,
    rel: &Path,
    source: &[u8],
    features: &nivra_sandbox::Features,
    params_source: Value,
) -> Result<Value> {
    let env_sha256 = nivra_sandbox::env_sha256(sandbox).map_err(sandbox_error)?;
    let scores = nivra_sandbox::score_source(sandbox, source, features).map_err(sandbox_error)?;
    let mut out = json!({
        "script": rel_display(rel),
        "status": "ok",
        "scores": scores,
        "score_count": scores.len(),
        "params_source": params_source,
        "execution_authorized": false,
        "evidence_only": true,
        "authority": "advisory_scores_not_orders"
    });
    if let Some(hash) = env_sha256 {
        out["env_sha256"] = json!(hash);
    }
    Ok(out)
}

fn install_tool(
    root: &Path,
    settings: &WorkspaceSettings,
    name: &str,
    text: &str,
) -> Result<Value> {
    if !file_name_ok(name) {
        return Err("workspace_install_name_invalid".into());
    }
    let hash = digest(text.as_bytes());
    let allowed = settings
        .install_allowlist
        .iter()
        .any(|entry| entry.name == name && entry.sha256 == hash);
    if !allowed {
        return Err("workspace_install_not_allowlisted".into());
    }
    if text.len() as u64 > settings.max_write_bytes
        || text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
        || super::transcript::redact(&json!(text)).as_str() != Some(text)
    {
        return Err("workspace_install_invalid".into());
    }
    let rel = PathBuf::from("tools").join(name);
    put_file(root, settings, &rel_display(&rel), text, |path| path == rel)
}

fn map_features(map: Map<String, Value>) -> BTreeMap<String, Value> {
    map.into_iter().collect()
}

fn sandbox_config(settings: &WorkspaceSettings) -> ScorerSandboxConfig {
    ScorerSandboxConfig {
        cpu_seconds: settings.cpu_seconds,
        memory_mb: settings.memory_mb,
        wall_ms: settings.wall_ms,
        max_output_bytes: settings.max_write_bytes.min(262_144) as usize,
        network: settings.network,
        env_dir: settings.env_dir.clone(),
        env_sha256: settings.env_sha256.clone(),
        ..ScorerSandboxConfig::default()
    }
}

fn sandbox_error(failure: nivra_sandbox::Failure) -> Box<dyn std::error::Error> {
    match failure {
        nivra_sandbox::Failure::Config => "workspace_run_sandbox_config",
        nivra_sandbox::Failure::Input => "workspace_run_input_invalid",
        nivra_sandbox::Failure::Setup => "workspace_run_sandbox_setup",
        nivra_sandbox::Failure::Unavailable => "workspace_run_sandbox_unavailable",
        nivra_sandbox::Failure::Exit => "workspace_run_script_exit",
        nivra_sandbox::Failure::WallTimeout => "workspace_run_timeout",
        nivra_sandbox::Failure::OutputLimit => "workspace_run_output_limit",
        nivra_sandbox::Failure::OutputInvalid => "workspace_run_output_invalid",
    }
    .into()
}

fn write_allowed(rel: &Path) -> bool {
    let mut components = rel.components();
    let Some(Component::Normal(first)) = components.next() else {
        return false;
    };
    matches!(
        first.to_str(),
        Some("notes" | "skills" | "reports" | "data" | "journal")
    ) && components.next().is_some()
}

fn research_write_allowed(rel: &Path) -> bool {
    let mut components = rel.components();
    let Some(Component::Normal(first)) = components.next() else {
        return false;
    };
    matches!(first.to_str(), Some("notes" | "skills")) && components.next().is_some()
}

fn ranker_script(rel: &Path) -> bool {
    rel.parent() == Some(Path::new("scripts")) && rel.extension().is_some_and(|ext| ext == "py")
}

pub(crate) fn normalize_rel(path: &str) -> Result<PathBuf> {
    if path.is_empty()
        || path.len() > 256
        || path.contains('\0')
        || path.chars().any(|ch| ch.is_control())
    {
        return Err("workspace_path_invalid".into());
    }
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                return Err("workspace_path_absolute".into());
            }
            Component::ParentDir => return Err("workspace_path_escape".into()),
            Component::CurDir => {}
            Component::Normal(part) => {
                let name = part.to_str().ok_or("workspace_path_invalid")?;
                if !component_ok(name) {
                    return Err("workspace_path_invalid".into());
                }
                out.push(name);
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err("workspace_path_invalid".into());
    }
    Ok(out)
}

fn component_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn file_name_ok(name: &str) -> bool {
    !name.contains('/') && component_ok(name)
}

fn hash_ok(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn resolve(root: &Path, rel: &Path, must_exist: bool) -> Result<PathBuf> {
    let path = if rel.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    refuse_symlink(&path)?;
    if must_exist {
        let meta = fs::symlink_metadata(&path).map_err(|_| "workspace_missing")?;
        if meta.file_type().is_symlink() {
            return Err("workspace_symlink".into());
        }
    }
    Ok(path)
}

fn ensure_private_component(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err("workspace_symlink".into());
            }
            if meta.uid() != current_uid() || meta.mode() & 0o077 != 0 {
                return Err("workspace_permissions".into());
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|_| "workspace_create_failed")?;
            let meta = fs::symlink_metadata(path).map_err(|_| "workspace_create_failed")?;
            if meta.file_type().is_symlink()
                || !meta.is_dir()
                || meta.uid() != current_uid()
                || meta.mode() & 0o777 != 0o700
            {
                return Err("workspace_permissions".into());
            }
            Ok(())
        }
        Err(_) => Err("workspace_create_failed".into()),
    }
}

fn ensure_private_root(book: &Path, root: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(book).map_err(|_| "workspace_create_failed")?;
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != current_uid() {
        return Err("workspace_symlink".into());
    }
    let rel = root
        .strip_prefix(book)
        .map_err(|_| "workspace_root_invalid")?;
    let mut current = book.to_path_buf();
    for component in rel.components() {
        current.push(component);
        ensure_private_component(&current)?;
    }
    Ok(())
}

fn ensure_private_dir(root: &Path, path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(root).map_err(|_| "workspace_missing")?;
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != current_uid() {
        return Err("workspace_symlink".into());
    }
    if meta.mode() & 0o077 != 0 {
        return Err("workspace_permissions".into());
    }
    let rel = path
        .strip_prefix(root)
        .map_err(|_| "workspace_path_escape")?;
    let mut current = root.to_path_buf();
    for component in rel.components() {
        current.push(component);
        ensure_private_component(&current)?;
    }
    Ok(())
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "workspace_seed_failed")?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| "workspace_seed_failed")?;
    file.write_all(bytes).map_err(|_| "workspace_seed_failed")?;
    file.sync_all().map_err(|_| "workspace_seed_failed")?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| "workspace_seed_failed")?;
    }
    Ok(())
}

fn write_if_missing(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    refuse_symlink(path)?;
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err("workspace_symlink".into()),
        Ok(meta) if !meta.is_file() => Err("workspace_not_file".into()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                ensure_private_dir(root, parent)?;
            }
            write_new_private(path, bytes)
        }
        Err(_) => Err("workspace_seed_failed".into()),
    }
}

fn cleanup_temp(root: &Path, temp: &Path) {
    let Ok(meta) = fs::symlink_metadata(temp) else {
        return;
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.uid() != current_uid() {
        return;
    }
    let Some(parent) = temp.parent() else {
        return;
    };
    let Ok(root_canonical) = fs::canonicalize(root) else {
        return;
    };
    let Ok(parent_canonical) = fs::canonicalize(parent) else {
        return;
    };
    if parent_canonical.starts_with(&root_canonical) {
        let _ = fs::remove_file(temp);
    }
}

/// Replace the destination with a new private inode in one rename. The old
/// inode is never truncated or chmodded, so a reader holding it keeps the old
/// bytes and a hardlink target outside the workspace is untouched. The temp
/// file is created 0600 in the already-validated private parent, synced, and
/// renamed only after the destination is rechecked for symlinks.
fn atomic_write_private(root: &Path, dest: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = dest.parent().ok_or("workspace_write_failed")?;
    if !dest.starts_with(root) {
        return Err("workspace_path_escape".into());
    }
    refuse_symlink(dest)?;
    let root_canonical = fs::canonicalize(root).map_err(|_| "workspace_write_failed")?;
    let parent_canonical = fs::canonicalize(parent).map_err(|_| "workspace_write_failed")?;
    if !parent_canonical.starts_with(&root_canonical) {
        return Err("workspace_path_escape".into());
    }
    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".tmp-{}-{sequence}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temp)
            .map_err(|_| "workspace_write_failed")?;
        file.write_all(bytes)
            .map_err(|_| "workspace_write_failed")?;
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|_| "workspace_write_failed")?;
        file.sync_all().map_err(|_| "workspace_write_failed")?;
        drop(file);
        refuse_symlink(dest)?;
        fs::rename(&temp, dest).map_err(|_| "workspace_write_failed")?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| "workspace_write_failed")?;
        Ok(())
    })();
    if result.is_err() {
        cleanup_temp(root, &temp);
    }
    result
}

fn refuse_symlink(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => return Err("workspace_symlink".into()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("workspace_path_invalid".into()),
        }
    }
    Ok(())
}

fn rel_display(rel: &Path) -> String {
    if rel.as_os_str().is_empty() {
        ".".into()
    } else {
        rel.to_string_lossy().into_owned()
    }
}

fn current_uid() -> u32 {
    fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;
