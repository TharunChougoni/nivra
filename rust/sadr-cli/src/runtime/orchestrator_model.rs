//! Orchestrator review configuration, prompt loading and legacy-role migration.
//! Digest and decide retain their independent planner identities.

use super::Result;
use super::planner::ModelRole;
use crate::model::AutoRenewSettings;
use sadr_cli::native_transport::Transport;
use sadr_cli::scorer_compare::ScorerReplaySettings;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

pub(crate) const DEFAULT_MODEL_TIMEOUT_MS: u64 = 120_000;
pub(crate) const DEFAULT_MODEL_OUTPUT_CAP: u32 = 8000;
pub(crate) const DEFAULT_DAILY_CALL_BUDGET: u32 = 200;
pub(crate) const DEFAULT_MAX_PROMPT_BYTES: u64 = 64 * 1024;
pub(crate) const ORCHESTRATOR_LEGACY_CONFLICT: &str = "orchestrator_and_legacy_roles_conflict";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LoadedPrompt {
    pub(crate) text: String,
    pub(crate) sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Trigger {
    /// Legacy selector retained for old configs. Digest always runs through its
    /// own planner role and is never an orchestrator trigger.
    Digest,
    /// Legacy selector retained for old configs. Decide always runs through its
    /// own planner role and is never an orchestrator trigger.
    Decide,
    Operations,
    Strategy,
    Scorer,
    /// Intraday supervisor. Not part of `ALL`, so existing orchestrator hashes
    /// stay unchanged until a config explicitly adds the trigger.
    Supervisor,
}

impl Trigger {
    /// Configured orchestrator triggers. Digest and decide remain separate model
    /// roles with their own sandbox, credential and budget.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 3] = [Self::Operations, Self::Strategy, Self::Scorer];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Digest => "digest",
            Self::Decide => "decide",
            Self::Operations => "operations",
            Self::Strategy => "strategy",
            Self::Scorer => "scorer",
            Self::Supervisor => "supervisor",
        }
    }

    pub(crate) const fn default_prompt(self) -> &'static str {
        match self {
            Self::Digest => "prompts/data.md",
            Self::Decide => "prompts/decision.md",
            Self::Operations => "prompts/operations.md",
            Self::Strategy => "prompts/strategy.md",
            Self::Scorer => "prompts/scorer.md",
            Self::Supervisor => "prompts/supervisor.md",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ModelDefaults {
    pub model_config: Option<PathBuf>,
    pub model_id: String,
    pub transport: Transport,
    pub tools: Vec<String>,
    pub timeout_ms: u64,
    pub run_timeout_ms: Option<u64>,
    pub output_cap: u32,
    pub daily_call_budget: u32,
    pub budget_unit: Option<String>,
    pub max_turns: Option<usize>,
    pub max_read_tools: Option<usize>,
    pub max_analysis_tools: usize,
    pub minimum_turn_ms: Option<u64>,
    pub max_failed_tools: Option<usize>,
    #[allow(dead_code)] // parse-compatible unused leftover from removed approval renewal
    pub approval_auto_renew: Option<AutoRenewSettings>,
    /// Explicit limit fallback (#245); omitted when absent so policy bytes
    /// of every existing orchestrator config are unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<super::planner::FallbackProfile>,
}

impl Default for ModelDefaults {
    fn default() -> Self {
        Self {
            model_config: None,
            model_id: String::new(),
            transport: Transport::default(),
            tools: Vec::new(),
            timeout_ms: DEFAULT_MODEL_TIMEOUT_MS,
            run_timeout_ms: None,
            output_cap: DEFAULT_MODEL_OUTPUT_CAP,
            daily_call_budget: 0,
            budget_unit: None,
            max_turns: None,
            max_read_tools: None,
            max_analysis_tools: 4,
            minimum_turn_ms: None,
            max_failed_tools: None,
            approval_auto_renew: None,
            fallback: None,
        }
    }
}

impl ModelDefaults {
    fn to_role(&self) -> ModelRole {
        ModelRole {
            model_config: self.model_config.clone(),
            model_id: self.model_id.clone(),
            transport: self.transport,
            enabled: false,
            tools: self.tools.clone(),
            timeout_ms: self.timeout_ms,
            run_timeout_ms: self.run_timeout_ms,
            output_cap: self.output_cap,
            daily_call_budget: self.daily_call_budget,
            budget_unit: self.budget_unit.clone(),
            max_turns: self.max_turns,
            max_read_tools: self.max_read_tools,
            max_analysis_tools: self.max_analysis_tools,
            minimum_turn_ms: self.minimum_turn_ms,
            max_failed_tools: self.max_failed_tools,
            fallback: self.fallback.clone(),
        }
    }
}

/// An exact, reviewed model/credential binding the operator may select for the
/// orchestrator session. A profile changes only model identity; tools, budgets,
/// timeouts and sandbox stay with the policy trigger. It cannot invent a model
/// or credential: the policy must name both the profile and the admitted switch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelProfile {
    pub model_id: String,
    pub transport: Transport,
    /// Application-owned credential/model configuration; never another app's cache.
    pub model_config: PathBuf,
}

fn enabled_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// Startup ordering for the supervisor. These are cadence/profile knobs, not
/// trading policy: they only decide when the advisory run happens and how long
/// Luna may be held before it fails open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RunFirstSettings {
    pub on_session_start: bool,
    pub on_resume: bool,
    pub on_change: bool,
    pub hold_luna_ms: i64,
}

impl Default for RunFirstSettings {
    fn default() -> Self {
        Self {
            on_session_start: true,
            on_resume: true,
            on_change: true,
            hold_luna_ms: 120_000,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct TriggerSettings {
    /// A disabled legacy role migrates to a disabled trigger. True is omitted
    /// from serialization so existing configuration hashes do not change.
    #[serde(default = "enabled_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    pub prompt: PathBuf,
    pub tools: Option<Vec<String>>,
    pub max_turns: Option<usize>,
    pub daily_call_budget: Option<u32>,
    pub timeout_ms: Option<u64>,
    pub run_timeout_ms: Option<u64>,
    pub output_cap: Option<u32>,
    pub budget_unit: Option<String>,
    pub max_read_tools: Option<usize>,
    pub max_analysis_tools: Option<usize>,
    pub minimum_turn_ms: Option<u64>,
    pub max_failed_tools: Option<usize>,
    /// Post-close-only scorer due time in minutes after midnight IST. Absent
    /// means the trigger has no scheduled run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_minute: Option<u32>,
    /// Scorer replay qualification. Absent keeps the documented defaults and
    /// existing serialized configuration hashes unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<ScorerReplaySettings>,
    /// Supervisor wake knobs. Absent means the documented defaults. They are
    /// excluded from the trading-policy hash so a profile can retune them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_interval_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_fill: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_decide: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub luna_no_decide_streak: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_streak: Option<u32>,
    /// Optional owner failure backoff. Absent keeps the historical wake-only
    /// failure streak and leaves existing serialized configuration unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_backoff_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_open_minute: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_close_minute: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_interval_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_luna_floor_ms: Option<i64>,
    /// Run before Luna's startup digest and on applied profile/config/version
    /// changes. Absent keeps the documented defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_first: Option<RunFirstSettings>,
    /// Minimum gap between distinct change wakes. Absent keeps the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_min_gap_ms: Option<i64>,
}

impl Default for TriggerSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            prompt: PathBuf::new(),
            tools: None,
            max_turns: None,
            daily_call_budget: None,
            timeout_ms: None,
            run_timeout_ms: None,
            output_cap: None,
            budget_unit: None,
            max_read_tools: None,
            max_analysis_tools: None,
            minimum_turn_ms: None,
            max_failed_tools: None,
            after_minute: None,
            replay: None,
            idle_interval_ms: None,
            after_fill: None,
            after_decide: None,
            luna_no_decide_streak: None,
            failure_streak: None,
            failure_backoff_ms: None,
            session_open_minute: None,
            pre_close_minute: None,
            min_interval_ms: None,
            wake_luna_floor_ms: None,
            run_first: None,
            change_min_gap_ms: None,
        }
    }
}

impl TriggerSettings {
    fn new(prompt: &str, tools: &[&str], max_turns: usize, daily_call_budget: u32) -> Self {
        Self {
            prompt: PathBuf::from(prompt),
            tools: Some(tools.iter().map(|tool| (*tool).to_owned()).collect()),
            max_turns: Some(max_turns),
            daily_call_budget: Some(daily_call_budget),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SandboxSettings {
    pub cpu_seconds: u32,
    pub memory_mb: u32,
    pub wall_ms: u64,
    pub network: bool,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            cpu_seconds: 30,
            memory_mb: 512,
            wall_ms: 60_000,
            network: false,
        }
    }
}

/// Single-orchestrator-model settings. `scorer_after_minute` is the optional
/// post-close-only scorer due time, in minutes after midnight IST; it is absent
/// from existing serialized configs so their bytes and policy hash are unchanged.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OrchestratorModelSettings {
    pub enabled: bool,
    pub model: ModelDefaults,
    pub daily_call_budget: u32,
    pub max_prompt_bytes: u64,
    pub sandbox: SandboxSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scorer_after_minute: Option<u32>,
    /// USD per million tokens for audit cost estimates; absent means 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_million_tokens: Option<f64>,
    pub triggers: BTreeMap<Trigger, TriggerSettings>,
    /// Standing session role, rules and tools. Absent uses the configured
    /// supervisor trigger prompt, so existing configurations and hashes are
    /// byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<PathBuf>,
    /// Short user-turn templates per session trigger. Absent uses the compiled
    /// default for that trigger. Templates may only use the trigger's known
    /// placeholders; validation rejects anything else.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub trigger_prompts: BTreeMap<SessionTrigger, String>,
    /// Reviewed, operator-selectable model/credential bindings. Empty keeps old
    /// policy bytes and hashes unchanged; a selection is only possible after the
    /// operator admits a name here and applies the reviewed policy.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_profiles: BTreeMap<String, ModelProfile>,
    /// Profile names the policy permits selecting. `default` is always the
    /// configured `model`; a named profile must appear here to be selectable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admitted_models: Vec<String>,
}

impl Default for OrchestratorModelSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            model: ModelDefaults::default(),
            daily_call_budget: DEFAULT_DAILY_CALL_BUDGET,
            max_prompt_bytes: DEFAULT_MAX_PROMPT_BYTES,
            sandbox: SandboxSettings::default(),
            scorer_after_minute: None,
            cost_per_million_tokens: None,
            system_prompt: None,
            trigger_prompts: BTreeMap::new(),
            model_profiles: BTreeMap::new(),
            admitted_models: Vec::new(),
            triggers: BTreeMap::from([
                (
                    Trigger::Operations,
                    TriggerSettings::new("prompts/operations.md", &[], 2, 1),
                ),
                (
                    Trigger::Strategy,
                    TriggerSettings::new("prompts/strategy.md", &["web_search", "x_search"], 8, 2),
                ),
                (
                    Trigger::Scorer,
                    TriggerSettings::new("prompts/scorer.md", &["sandbox_write", "replay"], 10, 4),
                ),
            ]),
        }
    }
}

impl OrchestratorModelSettings {
    pub(crate) fn role_for(&self, trigger: Trigger) -> ModelRole {
        let mut role = self.model.to_role();
        role.enabled = self.enabled;
        let Some(settings) = self.triggers.get(&trigger) else {
            return role;
        };
        if !settings.enabled {
            role.enabled = false;
        }
        if let Some(tools) = &settings.tools {
            role.tools = tools.clone();
        }
        if let Some(max_turns) = settings.max_turns {
            role.max_turns = Some(max_turns);
        }
        if let Some(daily_call_budget) = settings.daily_call_budget {
            role.daily_call_budget = daily_call_budget;
        }
        if let Some(timeout_ms) = settings.timeout_ms {
            role.timeout_ms = timeout_ms;
        }
        if let Some(run_timeout_ms) = settings.run_timeout_ms {
            role.run_timeout_ms = Some(run_timeout_ms);
        }
        if let Some(output_cap) = settings.output_cap {
            role.output_cap = output_cap;
        }
        if let Some(budget_unit) = &settings.budget_unit {
            role.budget_unit = Some(budget_unit.clone());
        }
        if let Some(max_read_tools) = settings.max_read_tools {
            role.max_read_tools = Some(max_read_tools);
        }
        if let Some(max_analysis_tools) = settings.max_analysis_tools {
            role.max_analysis_tools = max_analysis_tools;
        }
        if let Some(minimum_turn_ms) = settings.minimum_turn_ms {
            role.minimum_turn_ms = Some(minimum_turn_ms);
        }
        if let Some(max_failed_tools) = settings.max_failed_tools {
            role.max_failed_tools = Some(max_failed_tools);
        }
        role
    }

    /// Read this trigger's configured template. A missing template is a
    /// validation failure, never a silent fallback to embedded text.
    pub(crate) fn load_prompt(&self, trigger: Trigger) -> Result<LoadedPrompt> {
        let settings = self
            .triggers
            .get(&trigger)
            .ok_or("orchestrator_prompt_missing")?;
        load_prompt_file(&settings.prompt, self.max_prompt_bytes)
    }

    /// Exact prompt-content identity for every configured trigger. Used to
    /// distinguish a prompt edit from a non-prompt configuration change when a
    /// planner-authority halt is reviewed.
    pub(crate) fn prompt_hashes(&self) -> Result<Value> {
        let mut hashes = serde_json::Map::new();
        for trigger in self
            .triggers
            .keys()
            .filter(|trigger| **trigger != Trigger::Supervisor)
        {
            hashes.insert(
                trigger.as_str().into(),
                Value::from(self.load_prompt(*trigger)?.sha256),
            );
        }
        if self.system_prompt.is_some() {
            hashes.insert(
                "session_system".into(),
                Value::from(self.session_prompts()?.0.sha256),
            );
        }
        if !self.trigger_prompts.is_empty() {
            for (trigger, template) in self.session_prompts()?.1 {
                if self.trigger_prompts.contains_key(&trigger) {
                    hashes.insert(
                        format!("trigger_{}", trigger.as_str()),
                        Value::from(digest_text(&template)),
                    );
                }
            }
        }
        Ok(Value::Object(hashes))
    }

    /// Policy view of the orchestrator block. Trigger prompt paths become content
    /// hashes and operational call allowances are excluded, so the planner policy
    /// binds behaviour without binding operational budget.
    pub(crate) fn policy_value(&self) -> Result<Value> {
        let mut value = serde_json::to_value(self)?;
        let object = value.as_object_mut().ok_or("orchestrator_policy_invalid")?;
        object.insert("daily_call_budget".into(), Value::from(0));
        let triggers = object
            .get_mut("triggers")
            .and_then(Value::as_object_mut)
            .ok_or("orchestrator_policy_invalid")?;
        for trigger in self.triggers.keys() {
            let settings = triggers
                .get_mut(trigger.as_str())
                .and_then(Value::as_object_mut)
                .ok_or("orchestrator_policy_invalid")?;
            settings.remove("prompt");
            if *trigger == Trigger::Supervisor {
                // Prompt text and wake cadence are experiment-profile concerns.
                // The policy binds only that the supervisor exists and its ceiling.
                for key in [
                    "idle_interval_ms",
                    "after_fill",
                    "after_decide",
                    "luna_no_decide_streak",
                    "failure_streak",
                    "failure_backoff_ms",
                    "session_open_minute",
                    "pre_close_minute",
                    "min_interval_ms",
                    "wake_luna_floor_ms",
                    "run_first",
                    "change_min_gap_ms",
                    "timeout_ms",
                    "run_timeout_ms",
                    "max_turns",
                    "max_read_tools",
                ] {
                    settings.remove(key);
                }
                continue;
            }
            let sha256 = self.load_prompt(*trigger)?.sha256;
            settings.insert("prompt_sha256".into(), Value::from(sha256));
            settings.insert("daily_call_budget".into(), Value::Null);
        }
        // Session prompts are policy only when configured. Leaving the fields
        // absent for existing configurations keeps their serialized bytes and
        // authority hashes unchanged.
        if self.system_prompt.is_some() || !self.trigger_prompts.is_empty() {
            let (system, templates) = self.session_prompts()?;
            object.insert("system_prompt".into(), Value::Null);
            object.insert(
                "system_prompt_sha256".into(),
                Value::from(system.sha256.clone()),
            );
            let configured: BTreeMap<String, String> = self
                .trigger_prompts
                .keys()
                .filter_map(|trigger| {
                    templates
                        .get(trigger)
                        .map(|template| (trigger.as_str().to_owned(), digest_text(template)))
                })
                .collect();
            object.insert("trigger_prompts".into(), serde_json::to_value(configured)?);
        }
        Ok(value)
    }

    /// Validate the configured session prompts without changing serialization.
    pub(crate) fn validate_session_prompts(&self) -> Result<()> {
        if self.system_prompt.is_some()
            || !self.trigger_prompts.is_empty()
            || self.triggers.contains_key(&Trigger::Supervisor)
        {
            self.session_prompts()?;
        }
        Ok(())
    }

    /// Validate reviewed model profiles and the admitted switch scope. A profile
    /// carries an exact application credential path, so it is never inferred
    /// from another role or the environment.
    pub(crate) fn validate_models(&self) -> Result<()> {
        if self.model_profiles.len() > 8 {
            return Err("orchestrator_model_profiles_exceed_limit".into());
        }
        for (name, profile) in &self.model_profiles {
            let shape = !name.is_empty()
                && name.len() <= 64
                && name != "default"
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
            if !shape {
                return Err("orchestrator_model_profile_name_invalid".into());
            }
            if profile.model_id.is_empty()
                || profile.model_id.len() > 128
                || profile
                    .model_id
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace())
                || profile.model_config.as_os_str().is_empty()
                || !profile.model_config.is_absolute()
            {
                return Err("orchestrator_model_profile_invalid".into());
            }
        }
        if self
            .model
            .fallback
            .as_ref()
            .is_some_and(super::planner::FallbackProfile::invalid)
        {
            return Err("orchestrator_model_fallback_invalid".into());
        }
        if self.admitted_models.len() > 8 {
            return Err("orchestrator_admitted_models_exceed_limit".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for name in &self.admitted_models {
            if !seen.insert(name)
                || name.is_empty()
                || name.len() > 64
                || (name != "default" && !self.model_profiles.contains_key(name))
            {
                return Err("orchestrator_admitted_model_invalid".into());
            }
        }
        Ok(())
    }
}

mod migrate;
mod prompts;
pub(crate) use migrate::*;
pub(crate) use prompts::*;

#[cfg(test)]
mod tests;
