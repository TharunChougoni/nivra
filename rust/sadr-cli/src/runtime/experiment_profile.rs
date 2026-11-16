//! Operator experiment profile: prompts and bounded runtime tunables outside the binary.
//!
//! Profile contents are deliberately excluded from `planner::policy_bytes`. Trading
//! policy (identities, allowlists, costs, exits and ceilings) stays in the dated
//! planner config; a profile can only tighten the runtime values it is given.
use super::planner::{ModelRole, PlannerConfig, ToolsSettings};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const PROFILE_BYTES: u64 = 262_144;

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Profile {
    version: u32,
    prompts: Prompts,
    tools: Option<ToolsSettings>,
    cadence: Cadence,
    roles: BTreeMap<String, RoleCaps>,
    /// Reserved for the intraday supervisor; validated by that issue.
    supervisor: Option<Value>,
    /// Reserved for the directive channel; validated by that issue.
    directives: Option<Value>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Prompts {
    objective: Option<PathBuf>,
    digest: Option<PathBuf>,
    supervisor: Option<PathBuf>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Cadence {
    idle_digest_interval_ms: Option<i64>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleCaps {
    max_turns: Option<usize>,
    max_read_tools: Option<usize>,
    run_timeout_ms: Option<u64>,
}
#[derive(Clone)]
struct PromptText {
    text: String,
    sha256: String,
}
impl PromptText {
    fn embedded(text: &'static str) -> Self {
        Self::from(text.to_owned())
    }
}
impl From<String> for PromptText {
    fn from(text: String) -> Self {
        Self {
            sha256: digest(text.as_bytes()),
            text,
        }
    }
}
/// Loaded prompt text and hashes. Paths are intentionally not retained.
#[derive(Clone)]
pub(crate) struct PromptSet {
    objective: PromptText,
    /// The factual digest prompt: the owner digest unless a profile overrides it.
    digest: PromptText,
    supervisor: Option<PromptText>,
    orchestrator: Option<PromptText>,
    scorer: Option<PromptText>,
    custom: bool,
}
impl Default for PromptSet {
    fn default() -> Self {
        Self {
            objective: PromptText::embedded(super::harness::OBJECTIVE.trim()),
            digest: PromptText::embedded(super::harness::DATA_OWNER_PROMPT),
            supervisor: None,
            orchestrator: None,
            scorer: None,
            custom: false,
        }
    }
}
impl PromptSet {
    /// A complete prompt directory is opt-in by presence. Missing directories
    /// preserve embedded defaults; incomplete or invalid installations refuse.
    pub(crate) fn load_directory(
        base: &Path,
        max: u64,
    ) -> std::result::Result<Option<Self>, String> {
        match fs::metadata(base) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("planner_prompts_unreadable".into()),
            Ok(meta) if !meta.is_dir() => return Err("planner_prompts_unreadable".into()),
            Ok(_) => {}
        }
        let read = |name: &str| -> std::result::Result<PromptText, String> {
            let prompt =
                read_prompt(base, &Some(name.into()), max)?.ok_or("planner_prompt_missing")?;
            if prompt.text.trim().is_empty() {
                return Err("planner_prompt_empty".into());
            }
            Ok(prompt)
        };
        let prompts = Self {
            objective: read("objective.md")?,
            digest: read("data_owner.md")?,
            supervisor: Some(read("supervisor.md")?),
            orchestrator: Some(read("orchestrator.md")?),
            scorer: Some(read("scorer.md")?),
            custom: true,
        };
        Ok(Some(prompts))
    }
    /// The persistent owner's standing prompt is the orchestrator prompt when
    /// one is loaded, then the supervisor prompt.
    pub(crate) fn role_for(&self, role: &str) -> Option<&str> {
        match role {
            "supervisor" => self
                .orchestrator
                .as_ref()
                .map(|p| p.text.as_str())
                .or_else(|| self.role(role)),
            _ => self.role(role),
        }
    }
    pub(crate) fn objective(&self) -> &str {
        &self.objective.text
    }
    pub(crate) fn role(&self, role: &str) -> Option<&str> {
        match role {
            "digest" => Some(&self.digest.text),
            "supervisor" => self.supervisor.as_ref().map(|p| p.text.as_str()),
            "scorer" => self.scorer.as_ref().map(|p| p.text.as_str()),
            _ => None,
        }
    }
    pub(crate) fn custom(&self) -> bool {
        self.custom
    }
    pub(crate) fn hashes(&self) -> Value {
        let mut hashes = BTreeMap::new();
        hashes.insert("digest", &self.digest.sha256);
        hashes.insert("objective", &self.objective.sha256);
        if let Some(supervisor) = &self.supervisor {
            hashes.insert("supervisor", &supervisor.sha256);
        }
        for (name, prompt) in [
            ("orchestrator", &self.orchestrator),
            ("scorer", &self.scorer),
        ] {
            if let Some(prompt) = prompt {
                hashes.insert(name, &prompt.sha256);
            }
        }
        json!(hashes)
    }
}
/// Profile values applied to one in-memory `PlannerConfig`.
#[derive(Clone)]
pub(crate) struct AppliedProfile {
    pub(crate) sha256: String,
    pub(crate) prompts: PromptSet,
    pub(crate) changed_keys: Vec<String>,
    raw: Profile,
    tools: Option<ToolsSettings>,
    idle: Option<i64>,
    digest_caps: Option<RoleCaps>,
    base_tools: ToolsSettings,
    base_digest: ModelRole,
    base_idle: Option<Option<i64>>,
}
impl PlannerConfig {
    /// Load and apply the configured profile. An invalid profile leaves the
    /// runtime values untouched and returns a bounded reason instead of halting.
    pub(crate) fn apply_experiment_profile(&mut self) -> std::result::Result<(), String> {
        let Some(path) = self.experiment_profile.clone() else {
            return Ok(());
        };
        match load_profile(
            &path,
            self.prompt_max_bytes,
            self.file_prompts.clone().unwrap_or_default(),
        ) {
            Ok((profile, prompts)) => {
                let applied = apply(self, profile, prompts)?;
                self.profile = Some(Box::new(applied));
                self.profile_error = None;
                Ok(())
            }
            Err(reason) => {
                self.profile_error = Some(reason.clone());
                Err(reason)
            }
        }
    }
    /// Re-apply the previous profile after a rejected reload, over new policy values.
    pub(crate) fn retain_previous_profile(&mut self, previous: &PlannerConfig) {
        if let Some(old) = &previous.profile
            && let Ok(applied) = apply(self, old.raw.clone(), old.prompts.clone())
        {
            self.profile = Some(Box::new(applied));
        }
    }
    pub(crate) fn profile_sha256(&self) -> Option<&str> {
        self.profile.as_ref().map(|p| p.sha256.as_str())
    }
    pub(crate) fn prompt_set(&self) -> Option<&PromptSet> {
        self.profile
            .as_ref()
            .map(|p| &p.prompts)
            .or(self.file_prompts.as_ref())
    }
    pub(crate) fn prompt_hashes(&self) -> Option<Value> {
        self.prompt_set().map(PromptSet::hashes)
    }
    pub(crate) fn profile_changed_keys(&self) -> Option<&[String]> {
        self.profile.as_ref().map(|p| p.changed_keys.as_slice())
    }
    pub(crate) fn profile_value(&self, key: &str) -> Option<&Value> {
        let raw = &self.profile.as_ref()?.raw;
        match key {
            "supervisor" => raw.supervisor.as_ref(),
            "directives" => raw.directives.as_ref(),
            _ => None,
        }
    }
    /// Policy representation: profile paths, prompt text and applied tuning removed.
    pub(crate) fn without_experiment_profile(&self) -> Self {
        let mut cfg = self.clone();
        cfg.experiment_profile = None;
        cfg.profile_error = None;
        cfg.profile = None;
        if let Some(profile) = &self.profile {
            cfg.tools = profile.base_tools.clone();
            cfg.digest = profile.base_digest.clone();
            if let Some(base) = profile.base_idle
                && let Some(discovery) = &mut cfg.discovery
            {
                discovery.idle_digest_interval_ms = base;
            }
        }
        cfg
    }
}
fn load_profile(
    path: &Path,
    max_prompt_bytes: u64,
    mut prompts: PromptSet,
) -> std::result::Result<(Profile, PromptSet), String> {
    let bytes =
        crate::read_bounded(path, PROFILE_BYTES).map_err(|_| "experiment_profile_unreadable")?;
    let profile: Profile =
        serde_json::from_slice(&bytes).map_err(|_| "experiment_profile_invalid")?;
    if profile.version != 1 {
        return Err("experiment_profile_version_invalid".into());
    }
    let base = path.parent().ok_or("experiment_profile_path_invalid")?;
    if let Some(text) = read_prompt(base, &profile.prompts.objective, max_prompt_bytes)? {
        prompts.objective = text;
    }
    if let Some(text) = read_prompt(base, &profile.prompts.digest, max_prompt_bytes)? {
        prompts.digest = text;
    }
    if let Some(prompt) = read_prompt(base, &profile.prompts.supervisor, max_prompt_bytes)? {
        prompts.supervisor = Some(prompt.clone());
        prompts.orchestrator = Some(prompt);
    }
    prompts.custom = prompts.hashes() != PromptSet::default().hashes();
    Ok((profile, prompts))
}
fn read_prompt(
    base: &Path,
    value: &Option<PathBuf>,
    max_prompt_bytes: u64,
) -> std::result::Result<Option<PromptText>, String> {
    let Some(path) = value else { return Ok(None) };
    let path = if path.is_absolute() {
        path.clone()
    } else {
        base.join(path)
    };
    let mut file = fs::File::open(path).map_err(|_| "experiment_profile_prompt_missing")?;
    if !file
        .metadata()
        .map_err(|_| "experiment_profile_prompt_unreadable")?
        .is_file()
    {
        return Err("experiment_profile_prompt_unreadable".into());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max_prompt_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| "experiment_profile_prompt_unreadable")?;
    if bytes.len() as u64 > max_prompt_bytes {
        return Err("experiment_profile_prompt_oversize".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "experiment_profile_prompt_invalid_utf8")?;
    Ok(Some(PromptText::from(text)))
}
fn apply(
    cfg: &mut PlannerConfig,
    profile: Profile,
    prompts: PromptSet,
) -> std::result::Result<AppliedProfile, String> {
    let base_tools = cfg.tools.clone();
    if let Some(tools) = &profile.tools {
        tools.validate().map_err(|error| error.to_string())?;
        if tools.scanner_limit > base_tools.scanner_limit
            || tools.ranking_limit > base_tools.ranking_limit
            || tools.history_limit > base_tools.history_limit
            || tools.max_instrument_keys > base_tools.max_instrument_keys
            || tools.fill_limit > base_tools.fill_limit
        {
            return Err("experiment_profile_tools_exceed_policy".into());
        }
    }
    if profile.cadence.idle_digest_interval_ms.is_some() && cfg.discovery.is_none() {
        return Err("experiment_profile_cadence_without_discovery".into());
    }
    if let Some(caps) = profile.roles.get("digest") {
        validate_caps(caps, &cfg.digest)?;
    }
    if profile.roles.keys().any(|role| role != "digest") {
        return Err("experiment_profile_role_invalid".into());
    }
    let mut changed_keys = Vec::new();
    if profile.prompts.objective.is_some()
        || profile.prompts.digest.is_some()
        || profile.prompts.supervisor.is_some()
    {
        changed_keys.push("prompts".into());
    }
    if profile.tools.is_some() {
        changed_keys.push("tools".into());
    }
    if profile.cadence.idle_digest_interval_ms.is_some() {
        changed_keys.push("cadence.idle_digest_interval_ms".into());
    }
    if !profile.roles.is_empty() {
        changed_keys.push("roles".into());
    }
    let sha256 = digest(
        serde_json::to_vec(&json!({
            "version":profile.version,
            "prompts":prompts.hashes(),
            "tools":profile.tools,
            "cadence":profile.cadence,
            "roles":profile.roles,
            "supervisor":profile.supervisor,
            "directives":profile.directives,
        }))
        .map_err(|_| "experiment_profile_invalid")?
        .as_slice(),
    );
    let applied = AppliedProfile {
        sha256,
        prompts,
        changed_keys,
        raw: profile.clone(),
        tools: profile.tools.clone(),
        idle: profile.cadence.idle_digest_interval_ms,
        digest_caps: profile.roles.get("digest").cloned(),
        base_tools,
        base_digest: cfg.digest.clone(),
        base_idle: cfg
            .discovery
            .as_ref()
            .map(|discovery| discovery.idle_digest_interval_ms),
    };
    if let Some(tools) = applied.tools.clone() {
        cfg.tools = tools;
    }
    if let Some(idle) = applied.idle
        && let Some(discovery) = &mut cfg.discovery
    {
        discovery.idle_digest_interval_ms = Some(idle);
    }
    if let Some(caps) = &applied.digest_caps {
        apply_caps(&mut cfg.digest, caps);
    }
    Ok(applied)
}
/// Profile caps may only tighten the digest role; its run ceiling is 120s.
fn validate_caps(caps: &RoleCaps, role: &ModelRole) -> std::result::Result<(), String> {
    if caps
        .max_turns
        .is_some_and(|n| n == 0 || n > role.max_turns.unwrap_or(12))
        || caps
            .max_read_tools
            .is_some_and(|n| n == 0 || n > role.max_read_tools.unwrap_or(16))
        || caps
            .run_timeout_ms
            .is_some_and(|n| n < role.timeout_ms || n > 120_000)
    {
        return Err("experiment_profile_role_exceeds_policy".into());
    }
    Ok(())
}
fn apply_caps(role: &mut ModelRole, caps: &RoleCaps) {
    if let Some(value) = caps.max_turns {
        role.max_turns = Some(value);
    }
    if let Some(value) = caps.max_read_tools {
        role.max_read_tools = Some(value);
    }
    if let Some(value) = caps.run_timeout_ms {
        role.run_timeout_ms = Some(value);
    }
}
fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
/// Offline `planner profile-check`: validate and print the applied hashes.
pub(crate) fn check(planner: &Path, profile: &Path) -> std::result::Result<Value, String> {
    let mut cfg =
        PlannerConfig::load(planner).map_err(|error| super::failure_code(error.as_ref()))?;
    cfg.experiment_profile = Some(profile.to_path_buf());
    cfg.apply_experiment_profile()?;
    let applied = cfg.profile.as_ref().ok_or("experiment_profile_missing")?;
    Ok(json!({
        "status":"experiment_profile_valid",
        "profile_sha256":applied.sha256,
        "prompt_sha256":applied.prompts.hashes(),
        "changed_keys":applied.changed_keys,
        "policy_sha256":digest(&super::planner::policy_bytes(&cfg).map_err(|error| super::failure_code(error.as_ref()))?),
        "paper_only":true,
        "model_calls":0
    }))
}
