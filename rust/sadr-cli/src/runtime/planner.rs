//! Opt-in planning. Models produce intents; the financial engine owns execution.
use super::orchestrator_model::Trigger;
use super::*;
use sadr_core::execution::{Instrument, Portfolio};
use sadr_core::finance::{Amount, Price};
use serde_json::{Value, json};
use std::collections::BTreeMap;
mod live;
mod turn;
pub(crate) use turn::produce;
pub(super) use turn::produce_clocked;
// Preserve the historical crate path for all internal consumers.
#[allow(unused_imports)]
pub(crate) use turn::produce_for;
#[cfg(test)]
use turn::{existing_digest, produce_with_backend};

mod config;
use config::role_invalid;
pub(crate) use config::{DEFAULT_TURN_MS, FallbackProfile, MAX_RUN_MS, ModelRole};
// Preserve the original crate paths for the moved constants, including test consumers.
#[allow(unused_imports)]
pub(crate) use config::{DEFAULT_MAX_FAILED_TOOLS, DEFAULT_RUN_MS, OUTPUT_BYTE_CAP_MULTIPLIER};

mod plan;
#[allow(unused_imports)] // re-exported for crate callers and tests
pub(crate) use plan::{
    Action, CandidateReason, PlanSide, TradePlan, Verdict, host_budget_abstain, plan_document,
    validate,
};
mod failure;
#[cfg(test)]
pub(crate) use failure::failure_record;
pub(crate) use failure::failure_record_with_provider;
#[allow(unused_imports)] // re-exported for crate callers and tests
pub(crate) use failure::{
    ProviderFailure, failure_evidence, logged_reason, provider_failure, provider_failure_evidence,
    reason,
};
mod usage;
#[cfg(test)]
pub(crate) use usage::call_guarded;
pub(crate) use usage::{
    CallGuard, attempt_failed, bucket_tokens, budget_status, call_guarded_as, checked_reservations,
    prepare_review_usage, preview_guard, require_usage, review_call, usage_directory, usage_lock,
    usage_path,
};
#[cfg(test)]
pub(crate) use usage::{
    UsageManifest, call, fixture_reservation, reservation_stamp, usage_lock_with,
};
/// How much evidence the model sees in its user message. `ToolsOnly` is the
/// default: the host keeps the full bundle for tools, validation and audits,
/// while the model receives only a trigger envelope and fetches facts itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContextMode {
    #[default]
    ToolsOnly,
    Projection,
}
impl ContextMode {
    fn is_tools_only(&self) -> bool {
        *self == Self::ToolsOnly
    }
}
/// Versioned decision-owner policy. New policies select the persistent owner.
/// Absent policies retain legacy identity for archive/migration decoding only;
/// the production runtime refuses legacy model work until reviewed migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct DecisionOwnerPolicy {
    pub version: u32,
    #[serde(default = "legacy_decision_owner")]
    pub owner: DecisionOwner,
}
fn legacy_decision_owner() -> DecisionOwner {
    DecisionOwner::Legacy
}
impl Default for DecisionOwnerPolicy {
    fn default() -> Self {
        Self {
            version: 1,
            owner: DecisionOwner::Orchestrator,
        }
    }
}
impl DecisionOwnerPolicy {
    fn two_role() -> Self {
        Self {
            version: 1,
            owner: DecisionOwner::Orchestrator,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DecisionOwner {
    /// Historical authority marker, retained for migration and archives only.
    Legacy,
    /// One persistent orchestrator owns paper decisions; decide is disabled.
    #[default]
    Orchestrator,
}
/// Bounds what each model turn re-sends. The host still holds the full evidence:
/// tools answer for every allowlisted key and validation uses the full allowlist.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContextSettings {
    /// Feature rows inlined: held and scanned names first, then by traded value.
    /// Ignored in `tools_only` mode.
    pub universe_rows: usize,
    /// Inline the full validated allowlist (thousands of identities) in prompts.
    /// Ignored in `tools_only` mode.
    #[serde(default)]
    pub include_allowlist: bool,
    /// Decimal places kept for inlined floating-point features and scanner changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u32>,
    /// `tools_only` (default) or `projection` for exact v2 rollback. The
    /// default is omitted so an existing config's serialized policy bytes do
    /// not change on upgrade.
    #[serde(default, skip_serializing_if = "ContextMode::is_tools_only")]
    pub mode: ContextMode,
}
impl ContextSettings {
    fn validate(&self) -> Result<()> {
        if !(1..=10_000).contains(&self.universe_rows) || self.decimals.is_some_and(|d| d > 12) {
            return Err("planner_context_settings_invalid".into());
        }
        Ok(())
    }
}
/// Call-time tool bounds. Defaults preserve the previous hard caps and are
/// omitted from serialized configuration so sealed policy bytes stay stable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ToolsSettings {
    pub scanner_limit: usize,
    pub ranking_limit: usize,
    pub history_limit: usize,
    pub max_instrument_keys: usize,
    pub fill_limit: usize,
}
impl Default for ToolsSettings {
    fn default() -> Self {
        Self {
            scanner_limit: 40,
            ranking_limit: 50,
            history_limit: 5,
            max_instrument_keys: super::harness::MAX_INSTRUMENT_KEYS,
            fill_limit: 20,
        }
    }
}
impl ToolsSettings {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=256).contains(&self.scanner_limit)
            || !(1..=256).contains(&self.ranking_limit)
            || !(1..=32).contains(&self.history_limit)
            || !(1..=super::harness::MAX_INSTRUMENT_KEYS).contains(&self.max_instrument_keys)
            || !(1..=200).contains(&self.fill_limit)
        {
            return Err("planner_tools_settings_invalid".into());
        }
        Ok(())
    }
}
fn rounded(value: &Value, decimals: Option<u32>) -> Value {
    match (value, decimals) {
        (Value::Number(n), Some(d)) if n.is_f64() => {
            let scale = 10f64.powi(d as i32);
            n.as_f64()
                .map(|f| json!((f * scale).round() / scale))
                .unwrap_or(Value::Null)
        }
        (Value::Array(items), Some(_)) => {
            Value::Array(items.iter().map(|v| rounded(v, decimals)).collect())
        }
        (Value::Object(map), Some(_)) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), rounded(v, decimals)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
/// Compact the per-turn model projection. Rows are `[key, symbol, ...]` arrays.
pub(super) fn compact_projection(projection: &mut Value, settings: &ContextSettings) {
    let mut priority: std::collections::BTreeSet<String> = projection["positions"]
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    for row in projection["discovery"]["prospects"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(key) = row["instrument_key"].as_str() {
            priority.insert(key.to_owned());
        }
    }
    if let Some(rows) = projection["universe_features"].as_array() {
        let total = rows.len();
        let (mut first, mut rest): (Vec<&Value>, Vec<&Value>) = rows
            .iter()
            .partition(|r| r[0].as_str().is_some_and(|k| priority.contains(k)));
        // Column 11 is traded-value liquidity; unknown sorts last.
        let liquidity = |r: &Value| {
            r[11]
                .as_f64()
                .or_else(|| r[11].as_str().and_then(|t| t.parse().ok()))
                .unwrap_or(f64::MIN)
        };
        rest.sort_by(|a, b| {
            liquidity(b)
                .total_cmp(&liquidity(a))
                .then_with(|| a[0].as_str().cmp(&b[0].as_str()))
        });
        first.extend(rest);
        first.truncate(settings.universe_rows.max(priority.len().min(total)));
        let kept: Vec<Value> = first
            .iter()
            .map(|r| rounded(r, settings.decimals))
            .collect();
        projection["universe_features_presented"] = json!({"rows":kept.len(),"of":total,"selection":"held and scanned names first, then descending traded value; get_instrument_features answers any allowlisted key"});
        projection["universe_features"] = Value::Array(kept);
    }
    if let Some(rows) = projection["discovery"]["prospects"].as_array() {
        let kept: Vec<Value> = rows.iter().map(|r| rounded(r, settings.decimals)).collect();
        projection["discovery"]["prospects"] = Value::Array(kept);
    }
    if !settings.include_allowlist
        && let Some(object) = projection.as_object_mut()
        && let Some(list) = object.remove("allowlist")
    {
        object.insert("allowlist_reference".into(), json!({"count":list.as_array().map(|a|a.len()),"omitted_from_prompt":true,"note":"The host enforces the full validated allowlist; any key shown in universe_features, discovery or positions is allowlisted, and tools answer for any allowlisted key."}));
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PlannerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[allow(dead_code)] // parse-compatible unused leftover from removed approval renewal
    pub approval_auto_renew: Option<crate::model::AutoRenewSettings>,
    pub enabled: bool,
    pub provider: String,
    /// Omission retains deployed selector semantics; explicit false disables planning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_led: Option<bool>,
    /// Session-local directory under model/. Existing inode pins cannot be moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_max_records: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_manifest_max_bytes: Option<u64>,
    pub model_config: Option<PathBuf>,
    pub shortlist_n: usize,
    #[serde(skip_serializing_if = "PlannerConfig::default_max_handoff_instruments")]
    pub max_handoff_instruments: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_digest: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_universe_features: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<super::discovery::Settings>,
    /// Model prompt size controls. Omission means `tools_only`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextSettings>,
    /// Call-time retrieval bounds. Defaults are omitted from serialized policy.
    #[serde(default, skip_serializing_if = "ToolsSettings::is_default")]
    pub tools: ToolsSettings,
    /// Deterministic gate on Luna's decide handoffs. Omission keeps Luna's choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_gate: Option<super::discovery::GateSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_from: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_config: Option<PathBuf>,
    #[serde(default = "default_digest", deserialize_with = "deserialize_digest")]
    pub digest: ModelRole,
    /// Parse-compatible leftover. Deployed planner JSON still contains
    /// `"decide": {"enabled": false}` and the schema denies unknown fields.
    /// Production inference never calls this role; owner policy requires it disabled.
    #[serde(default = "default_decide", deserialize_with = "deserialize_decide")]
    pub decide: ModelRole,
    pub events: Option<redecision::Settings>,
    pub post_close: Option<postclose::Settings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_tasks: Option<super::owner_tasks::OwnerTasksSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_tools: Option<super::harness::ResearchSettings>,
    /// Owner workspace under the paper book. Omission keeps deployed policy
    /// hashes unchanged and exposes no workspace tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<super::workspace::WorkspaceSettings>,
    /// Orchestrator web tools (#222). Omission keeps deployed policy hashes
    /// unchanged, exposes no web tool and makes no outbound call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_tools: Option<super::web_tools::WebToolsSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_evidence: Option<super::decision_evidence::Settings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator: Option<super::orchestrator_model::OrchestratorModelSettings>,
    /// Absent preserves legacy bytes/hashes for reviewed migration and archives;
    /// production model work requires explicit orchestrator authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_owner: Option<DecisionOwnerPolicy>,
    /// Frozen exit behaviour. Absent keeps the historical execute/liquidate path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_policy: Option<sadr_core::execution::ExitPolicy>,
    /// Operator experiment profile. The path and its contents are excluded from
    /// the frozen trading-policy hash; see `experiment_profile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_profile: Option<PathBuf>,
    /// Policy ceiling for one loaded profile prompt.
    #[serde(
        default = "default_prompt_max_bytes",
        skip_serializing_if = "is_default_prompt_max_bytes"
    )]
    pub prompt_max_bytes: u64,
    /// Default files loaded beside the planner config; content is sealed policy.
    #[serde(skip)]
    pub file_prompts: Option<super::experiment_profile::PromptSet>,
    /// Applied profile, excluded from serialization and policy bytes.
    #[serde(skip)]
    pub profile: Option<Box<super::experiment_profile::AppliedProfile>>,
    /// Non-fatal profile reload failure retained for operator logs.
    #[serde(skip)]
    pub profile_error: Option<String>,
}
fn deserialize_role<'de, D: serde::Deserializer<'de>>(
    d: D,
    timeout: u64,
) -> std::result::Result<ModelRole, D::Error> {
    let mut value = Value::deserialize(d)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| <D::Error as serde::de::Error>::custom("role must be an object"))?;
    object.entry("timeout_ms").or_insert(json!(timeout));
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}
fn deserialize_digest<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<ModelRole, D::Error> {
    deserialize_role(d, 20_000)
}
fn deserialize_decide<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<ModelRole, D::Error> {
    deserialize_role(d, 150_000)
}
fn default_digest() -> ModelRole {
    ModelRole {
        daily_call_budget: 0,
        ..Default::default()
    }
}
fn default_decide() -> ModelRole {
    ModelRole {
        daily_call_budget: 0,
        timeout_ms: 150_000,
        ..Default::default()
    }
}
fn default_prompt_max_bytes() -> u64 {
    65_536
}
fn is_default_prompt_max_bytes(value: &u64) -> bool {
    *value == default_prompt_max_bytes()
}
/// Operational allowances do not change the frozen trading policy. Shadow-only
/// selector diagnostics remain excluded; live ranking participates in the policy.
pub(super) fn policy_bytes(cfg: &PlannerConfig) -> Result<Vec<u8>> {
    let mut cfg = cfg.without_experiment_profile();
    if let Some(discovery) = &mut cfg.discovery
        && discovery
            .strategy_selection
            .as_ref()
            .is_none_or(|s| !s.mode.is_live())
    {
        discovery.strategy_selection = None;
    }
    let Some(orchestrator) = cfg.orchestrator.as_ref() else {
        return legacy_policy_bytes(&cfg);
    };
    let view = orchestrator.policy_value()?;
    cfg.digest.daily_call_budget = 0;
    cfg.decide.daily_call_budget = 0;
    let mut value = serde_json::to_value(&cfg)?;
    *value
        .get_mut("orchestrator")
        .ok_or("orchestrator_policy_invalid")? = view;
    super::owner_tasks::bind_policy(&cfg, &mut value);
    super::decision_evidence::bind_policy(&mut value);
    bind_file_prompts(&cfg, &mut value);
    Ok(serde_json::to_vec(&value)?)
}
/// Exact pre-shadow-policy representation for immutable legacy evidence checks.
pub(super) fn legacy_policy_bytes(cfg: &PlannerConfig) -> Result<Vec<u8>> {
    let mut cfg = cfg.without_experiment_profile();
    cfg.digest.daily_call_budget = 0;
    cfg.decide.daily_call_budget = 0;
    if cfg.decision_evidence.is_none() && cfg.file_prompts.is_none() {
        return Ok(serde_json::to_vec(&cfg)?);
    }
    let mut value = serde_json::to_value(&cfg)?;
    super::owner_tasks::bind_policy(&cfg, &mut value);
    super::decision_evidence::bind_policy(&mut value);
    bind_file_prompts(&cfg, &mut value);
    Ok(serde_json::to_vec(&value)?)
}
fn bind_file_prompts(cfg: &PlannerConfig, value: &mut Value) {
    if let Some(prompts) = &cfg.file_prompts {
        value["default_prompt_sha256"] = prompts.hashes_for(cfg.orchestrator_owns_decisions());
    }
}
impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "xai_subscription".into(),
            model_led: None,
            usage_namespace: None,
            usage_max_records: None,
            usage_manifest_max_bytes: None,
            model_config: None,
            shortlist_n: 200,
            max_handoff_instruments: 20,
            require_digest: None,
            operator_context: None,
            full_universe_features: None,
            discovery: None,
            context: None,
            tools: ToolsSettings::default(),
            handoff_gate: None,
            effective_from: None,
            previous_config: None,
            digest: default_digest(),
            decide: default_decide(),
            events: None,
            post_close: None,
            owner_tasks: None,
            approval_auto_renew: None,
            research_tools: None,
            workspace: None,
            web_tools: None,
            decision_evidence: None,
            orchestrator: None,
            decision_owner: Some(DecisionOwnerPolicy::default()),
            exit_policy: None,
            experiment_profile: None,
            prompt_max_bytes: default_prompt_max_bytes(),
            file_prompts: None,
            profile: None,
            profile_error: None,
        }
    }
}
impl PlannerConfig {
    /// Resolve role identity without merging historical charges. The Decide
    /// selector is retained for original journal/config accounting only;
    /// production inference admits digest and persistent orchestrator work.
    pub(crate) fn role(&self, trigger: Trigger) -> ModelRole {
        match trigger {
            Trigger::Digest => self.digest.clone(),
            Trigger::Decide => self.decide.clone(),
            Trigger::Operations | Trigger::Strategy | Trigger::Scorer | Trigger::Supervisor => {
                if let Some(o) = &self.orchestrator {
                    let mut role = o.role_for(trigger);
                    role.enabled = o.enabled
                        && o.triggers
                            .get(&trigger)
                            .is_some_and(|settings| settings.enabled);
                    role
                } else {
                    match trigger {
                        Trigger::Operations => self
                            .post_close
                            .as_ref()
                            .map(|settings| settings.operations.clone())
                            .unwrap_or_else(|| ModelRole {
                                enabled: false,
                                ..ModelRole::default()
                            }),
                        Trigger::Strategy => self
                            .post_close
                            .as_ref()
                            .map(|settings| settings.strategy.clone())
                            .unwrap_or_else(|| ModelRole {
                                enabled: false,
                                ..ModelRole::default()
                            }),
                        Trigger::Scorer | Trigger::Supervisor => ModelRole {
                            enabled: false,
                            ..ModelRole::default()
                        },
                        Trigger::Digest | Trigger::Decide => unreachable!(),
                    }
                }
            }
        }
    }
    fn default_max_handoff_instruments(v: &usize) -> bool {
        *v == 20
    }
    /// Preserve the deployed handoff-bound schemas when `planner.tools` is
    /// absent. An explicit tools limit wins; otherwise the old
    /// `max_handoff_instruments` bound still sizes instrument-key tools.
    pub(crate) fn effective_tools(&self) -> ToolsSettings {
        let mut tools = self.tools.clone();
        if tools.max_instrument_keys == super::harness::MAX_INSTRUMENT_KEYS
            && self.max_handoff_instruments != super::harness::MAX_INSTRUMENT_KEYS
        {
            tools.max_instrument_keys = self.max_handoff_instruments;
        }
        tools
    }
    fn validate_handoff_limit(&self) -> Result<()> {
        if !(1..=super::harness::MAX_INSTRUMENT_KEYS).contains(&self.max_handoff_instruments) {
            return Err("planner_handoff_limit_invalid".into());
        }
        Ok(())
    }
    /// Prerequisites for the versioned two-role decision policy. Empty for
    /// legacy/absent ownership. The same list feeds config validation and the
    /// read-only `agent policy preview` migration report.
    pub(crate) fn decision_owner_requirements(&self) -> Vec<&'static str> {
        let Some(policy) = self.decision_owner else {
            return Vec::new();
        };
        if policy.owner != DecisionOwner::Orchestrator || !self.enabled {
            return Vec::new();
        }
        let mut missing = Vec::new();
        if policy.version != 1 {
            missing.push("decision_owner_version_invalid");
        }
        let supervisor = self.orchestrator.as_ref().is_some_and(|orchestrator| {
            orchestrator.enabled
                && orchestrator
                    .triggers
                    .get(&Trigger::Supervisor)
                    .is_some_and(|trigger| trigger.enabled)
        });
        if !supervisor {
            missing.push("decision_owner_supervisor_required");
        }
        let supervisor_role = self.role(Trigger::Supervisor);
        if supervisor_role.model_id.is_empty() {
            missing.push("decision_owner_supervisor_model_required");
        }
        if self.orchestrator.as_ref().is_some_and(|orchestrator| {
            orchestrator
                .triggers
                .get(&Trigger::Operations)
                .is_some_and(|trigger| trigger.enabled)
        }) {
            missing.push("decision_owner_operations_trigger_must_be_disabled");
        }
        if self.orchestrator.as_ref().is_some_and(|orchestrator| {
            orchestrator
                .triggers
                .get(&Trigger::Strategy)
                .is_some_and(|trigger| trigger.enabled)
        }) {
            missing.push("decision_owner_strategy_trigger_must_be_disabled");
        }
        if self
            .post_close
            .as_ref()
            .is_some_and(|settings| settings.enabled)
        {
            missing.push("decision_owner_post_close_must_be_disabled");
        }
        if !self.digest.enabled {
            missing.push("decision_owner_digest_required");
        }
        if self.decide.enabled {
            missing.push("decision_owner_decide_must_be_disabled");
        }
        if self.handoff_gate.is_some() {
            missing.push("decision_owner_legacy_handoff_gate_refused");
        }
        if !self.events.as_ref().is_some_and(|events| events.enabled) {
            missing.push("decision_owner_events_required");
        }
        missing
    }
    pub(crate) fn validate_decision_owner(&self) -> Result<()> {
        if let Some(reason) = self.decision_owner_requirements().first() {
            return Err((*reason).into());
        }
        Ok(())
    }
    pub(crate) fn orchestrator_owns_decisions(&self) -> bool {
        self.decision_owner.is_some_and(|policy| {
            policy.version == 1 && policy.owner == DecisionOwner::Orchestrator
        })
    }
    /// Fail-closed runtime gate. An owner policy is admitted only after the
    /// reviewed migration prerequisites hold; the durable authority transition
    /// is checked separately at controller startup.
    pub(crate) fn runtime_decision_owner_gate(&self) -> Result<()> {
        if self.enabled && !self.orchestrator_owns_decisions() {
            return Err("decision_owner_migration_required".into());
        }
        if self.orchestrator_owns_decisions() {
            self.validate_decision_owner()?;
        }
        Ok(())
    }
    /// The proposed two-role policy for a read-only migration preview.
    pub(crate) fn proposed_two_role(&self) -> Self {
        let mut proposed = self.clone();
        proposed.decision_owner = Some(DecisionOwnerPolicy::two_role());
        proposed.decide.enabled = false;
        proposed.handoff_gate = None;
        if let Some(orchestrator) = proposed.orchestrator.as_mut() {
            for trigger in [Trigger::Operations, Trigger::Strategy] {
                if let Some(settings) = orchestrator.triggers.get_mut(&trigger) {
                    settings.enabled = false;
                }
            }
        }
        if let Some(post_close) = proposed.post_close.as_mut() {
            post_close.enabled = false;
        }
        proposed
    }
    /// Resolve dated configuration without changing a predecessor book's seal.
    pub fn load_for_session(path: &Path, session: NaiveDate) -> Result<Self> {
        let mut path = path.to_path_buf();
        let mut visited = std::collections::BTreeSet::new();
        for _ in 0..32 {
            if !visited.insert(path.clone()) {
                return Err("planner_config_schedule_cycle".into());
            }
            let cfg = Self::load(&path)?;
            if cfg.effective_from.is_none_or(|date| session >= date) {
                let mut cfg = cfg;
                // A bad profile is recorded and ignored; the policy remains usable.
                let _ = cfg.apply_experiment_profile();
                return Ok(cfg);
            }
            path = cfg
                .previous_config
                .ok_or("planner_config_previous_required")?;
        }
        Err("planner_config_schedule_too_deep".into())
    }
    pub fn load(path: &Path) -> Result<Self> {
        let raw = crate::read_bounded(path, 65536).map_err(|_| "planner_config_unreadable")?;
        let value = sadr_core::native::decode(&raw).map_err(|_| "planner_config_invalid")?;
        if value
            .get("orchestrator")
            .and_then(|settings| settings.get("enabled"))
            == Some(&Value::Bool(true))
            && super::orchestrator_model::LegacyRoleBlocks::from_planner_value(&value)
                .map_err(|_| "planner_config_invalid")?
                .has_orchestrator_roles()
        {
            return Err(super::orchestrator_model::ORCHESTRATOR_LEGACY_CONFLICT.into());
        }
        let mut c: Self = serde_json::from_value(value).map_err(|_| "planner_config_invalid")?;
        c.validate_handoff_limit()?;
        if let Some(settings) = &c.decision_evidence {
            settings.validate()?;
        }
        if c.effective_from.is_some() != c.previous_config.is_some()
            || c.previous_config.as_ref().is_some_and(|p| !p.is_absolute())
        {
            return Err("planner_config_schedule_invalid".into());
        }
        if c.experiment_profile
            .as_ref()
            .is_some_and(|p| !p.is_absolute())
        {
            return Err("planner_experiment_profile_invalid".into());
        }
        if !(1..=65_536).contains(&c.prompt_max_bytes) {
            return Err("planner_prompt_max_bytes_invalid".into());
        }
        c.file_prompts = super::experiment_profile::PromptSet::load_directory(
            &path
                .parent()
                .ok_or("planner_config_path_invalid")?
                .join("prompts"),
            c.prompt_max_bytes,
        )
        .map_err(|reason| format!("planner_default_{reason}"))?;
        if c.usage_max_records == Some(0) || c.usage_manifest_max_bytes == Some(0) {
            return Err("planner_usage_limits_invalid".into());
        }
        if c.operator_context
            .as_ref()
            .is_some_and(|s| s.len() > 8192 || s.chars().any(|c| c.is_control() && c != '\n'))
        {
            return Err("planner_operator_context_invalid".into());
        }
        if c.provider != "xai_subscription"
            || !(1..=1000).contains(&c.shortlist_n)
            || !c.digest.tools.is_empty()
            || (!c.decide.tools.is_empty()
                && !matches!(
                    c.decide.transport,
                    sadr_cli::native_transport::Transport::DirectOauth
                        | sadr_cli::native_transport::Transport::SubscriptionProxy
                ))
        {
            return Err("planner_config_invalid".into());
        }
        let orchestrator_owns_decisions = c.orchestrator_owns_decisions();
        if c.require_digest == Some(true)
            && (!c.digest.enabled || (!orchestrator_owns_decisions && !c.decide.enabled))
        {
            return Err("planner_initialization_roles_required".into());
        }
        if let Some(discovery) = &c.discovery {
            discovery.validate()?;
            let projection =
                c.context.as_ref().map(|x| x.mode).unwrap_or_default() == ContextMode::Projection;
            if !c.digest.enabled
                || (!orchestrator_owns_decisions && !c.decide.enabled)
                || (projection && c.full_universe_features != Some(true))
            {
                return Err("planner_discovery_roles_and_full_features_required".into());
            }
        }
        if let Some(context) = &c.context {
            context.validate()?;
        }
        c.tools.validate()?;
        if let Some(orchestrator) = &c.orchestrator {
            let scorer = orchestrator.triggers.get(&Trigger::Scorer);
            let scorer_after_minute = scorer
                .and_then(|trigger| trigger.after_minute)
                .or(orchestrator.scorer_after_minute);
            if orchestrator.enabled
                && scorer.is_some_and(|trigger| trigger.enabled)
                && scorer_after_minute.is_none()
            {
                return Err("planner_scorer_after_minute_required".into());
            }
            if orchestrator
                .scorer_after_minute
                .is_some_and(|minute| !(931..=1439).contains(&minute))
                || scorer
                    .and_then(|trigger| trigger.after_minute)
                    .is_some_and(|minute| !(931..=1439).contains(&minute))
            {
                return Err("planner_scorer_after_minute_invalid".into());
            }
            if let Some(trigger) = scorer
                && (trigger
                    .max_turns
                    .is_some_and(|turns| !(1..=64).contains(&turns))
                    || trigger
                        .replay
                        .as_ref()
                        .is_some_and(|replay| replay.validate().is_err()))
            {
                return Err("planner_scorer_replay_invalid".into());
            }
            orchestrator
                .validate_session_prompts()
                .map_err(|error| error.to_string())?;
            orchestrator
                .validate_models()
                .map_err(|error| error.to_string())?;
        }
        c.validate_decision_owner()?;
        if let Some(gate) = &c.handoff_gate {
            gate.validate()?;
        }
        if let Some(policy) = &c.exit_policy {
            policy.validate().map_err(|error| error.to_string())?;
        }
        if role_invalid(&c.digest, 120_000) || role_invalid(&c.decide, MAX_RUN_MS) {
            return Err("planner_role_invalid".into());
        }
        if c.usage_namespace.as_deref().is_some_and(|name| {
            name.is_empty()
                || name.len() > 64
                || name == "."
                || name == ".."
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        }) {
            return Err("planner_usage_namespace_invalid".into());
        }
        if let Some(review) = &c.post_close {
            review.validate()?;
        }
        if let Some(tasks) = &c.owner_tasks {
            tasks.validate(&c)?;
        }
        if let Some(research) = &c.research_tools {
            research.validate()?;
        }
        if let Some(workspace) = &c.workspace {
            workspace.validate()?;
        }
        if let Some(web_tools) = &c.web_tools {
            web_tools.validate()?;
        }
        if let Some(events) = &c.events {
            events.validate()?;
        }
        Ok(c)
    }
}
pub(crate) trait Backend {
    /// Real transports validate the configured profile before any reservation.
    /// Offline/fixture adapters have no provider credentials to admit.
    fn validate_role(
        &mut self,
        _c: &Config,
        _cfg: &PlannerConfig,
        _name: &str,
        _role: &ModelRole,
    ) -> Result<()> {
        Ok(())
    }
    fn call(&mut self, role: &ModelRole, prompt: &str) -> Result<String>;
    /// Optional immutable controller context; absence is an explicit unavailable tool result.
    fn intraday_context(&mut self, _keys: &[String], _at: i64) -> Option<Result<Value>> {
        None
    }
    /// Only an explicitly constructed offline replay backend overrides quote reads.
    fn snapshot(&mut self, _bundle: &Value, _keys: &[String], _at: i64) -> Option<Result<Value>> {
        None
    }
    fn provenance(&self) -> Value {
        Value::Null
    }
}
pub(super) struct Subscription<'a>(
    &'a Path,
    Value,
    Option<&'a Path>,
    Option<&'a super::decision_evidence::Snapshot>,
);
impl<'a> Subscription<'a> {
    pub(super) fn new(path: &'a Path) -> Self {
        Self(path, Value::Null, None, None)
    }
}
impl<'a> Subscription<'a> {
    pub(super) fn with_context(
        mut self,
        context: Option<&'a super::decision_evidence::Snapshot>,
    ) -> Self {
        self.3 = context;
        self
    }
    pub(super) fn with_alerts(mut self, webhook: Option<&'a Path>) -> Self {
        self.2 = webhook;
        self
    }
}
impl Backend for Subscription<'_> {
    fn validate_role(
        &mut self,
        c: &Config,
        cfg: &PlannerConfig,
        name: &str,
        role: &ModelRole,
    ) -> Result<()> {
        super::model_admission::validate(
            c,
            cfg,
            name,
            role,
            role.model_config.as_deref().unwrap_or(self.0),
        )?;
        Ok(())
    }
    fn call(&mut self, role: &ModelRole, prompt: &str) -> Result<String> {
        let started = std::time::Instant::now();
        let primary = crate::model::planner_response_with_alerts(
            role.model_config.as_deref().unwrap_or(self.0),
            role,
            prompt,
            &mut self.1,
            self.2,
        );
        let result = match (primary, role.fallback.as_ref()) {
            (Err(error), Some(fallback)) => match crate::model::limit_reason(&error) {
                Some(reason) => {
                    let remaining = role
                        .timeout_ms
                        .saturating_sub(started.elapsed().as_millis() as u64);
                    limit_fallback(
                        role,
                        fallback,
                        prompt,
                        reason,
                        remaining,
                        &mut self.1,
                        self.2,
                    )
                }
                None => Err(error),
            },
            (primary, _) => primary,
        }
        .map_err(|e| {
            provider_failure(&e)
                .map(|detail| detail.reason)
                .unwrap_or_else(|| "planner_provider_failed".to_owned())
                .into()
        });
        if result.is_err() {
            let frame: Option<super::harness::Frame> = serde_json::from_str(prompt).ok();
            if let Some(frame) = frame {
                sadr_cli::model_stream::publish(
                    &frame.role,
                    &role.model_id,
                    &digest(prompt.as_bytes()),
                    "request_failed",
                    self.1["stage"].as_str().unwrap_or("provider"),
                );
            }
        }
        result
    }
    fn intraday_context(&mut self, keys: &[String], at: i64) -> Option<Result<Value>> {
        self.3.map(|snapshot| snapshot.read(keys, at))
    }
    fn provenance(&self) -> Value {
        self.1.clone()
    }
}

/// Minimum time between two `model_fallback` alerts for one role. Every
/// switch is still logged and recorded in the call's transport evidence.
const FALLBACK_ALERT_INTERVAL: Duration = Duration::from_secs(1800);

/// Retry one call on the role's reviewed fallback after a primary usage, rate
/// or quota limit (#245). The switch is logged with its reason, recorded in the
/// transport evidence (so the usage journal and dashboard show it) and alerted
/// at most once per interval per role. Any other primary failure never gets here.
fn limit_fallback(
    role: &ModelRole,
    fallback: &FallbackProfile,
    prompt: &str,
    reason: &'static str,
    remaining_ms: u64,
    provenance: &mut Value,
    webhook: Option<&Path>,
) -> std::result::Result<String, String> {
    let primary = std::mem::take(provenance);
    let frame_role = serde_json::from_str::<super::harness::Frame>(prompt)
        .map(|frame| frame.role)
        .unwrap_or_else(|_| "unknown".into());
    eprintln!(
        "model_fallback role={frame_role} from={} to={} reason={reason} primary_status={}",
        role.model_id,
        fallback.model_id,
        primary["http_status"].as_u64().unwrap_or(0)
    );
    let result = crate::model::fallback_response(fallback, role, prompt, remaining_ms, provenance);
    let outcome = if result.is_ok() { "ok" } else { "failed" };
    provenance["fallback"] = json!({"from_model":role.model_id,"from_transport":role.transport,
        "to_model":fallback.model_id,"to_transport":fallback.transport,"reason":reason,
        "primary_http_status":primary["http_status"],"primary_stage":primary["stage"],
        "outcome":outcome});
    if let Err(error) = &result {
        eprintln!(
            "model_fallback_failed role={frame_role} to={} reason={}",
            fallback.model_id,
            provider_failure(error)
                .map(|detail| detail.reason)
                .unwrap_or_else(|| "planner_provider_failed".into())
        );
    }
    static LAST_ALERT: std::sync::Mutex<BTreeMap<String, std::time::Instant>> =
        std::sync::Mutex::new(BTreeMap::new());
    let due = LAST_ALERT.lock().is_ok_and(|mut last| {
        let now = std::time::Instant::now();
        let due = last
            .get(&frame_role)
            .is_none_or(|at| now.duration_since(*at) >= FALLBACK_ALERT_INTERVAL);
        if due {
            last.insert(frame_role.clone(), now);
        }
        due
    });
    if due {
        let alerts = crate::alert::Alerts::start(webhook, false);
        alerts.emit(
            crate::alert::Event::ModelFallback,
            chrono::DateTime::from_timestamp_millis(millis())
                .unwrap_or_default()
                .with_timezone(&chrono::FixedOffset::east_opt(19800).expect("IST"))
                .date_naive(),
            json!({"role":frame_role,"from_model":role.model_id,"to_model":fallback.model_id,
                "reason":reason,"outcome":outcome}),
        );
        alerts.finish();
    }
    result
}

pub(crate) const PROMPT_VERSION: &str = "planner-harness-v3";
/// No prospect and no managed position: the planner has nothing to decide. This
/// is a normal no-op, not a failed attempt, a block or an alert (#221).
pub(crate) const NO_DECISION_WORK: &str = "model_plan_no_prospects_no_positions";
/// Original durable plans include full-universe evidence, unlike provider replies.
pub(crate) const SAVED_PLAN_MAX_BYTES: usize = 8_000_000;
#[cfg(test)]
pub(crate) fn bundle(
    c: &Config,
    p: &Portfolio,
    instruments: &[Instrument],
    n: usize,
) -> Result<Value> {
    Ok(bundle_with_references(c, p, instruments, n)?.0)
}
fn load_daily(c: &Config) -> Result<BTreeMap<String, BTreeMap<NaiveDate, Value>>> {
    let mut daily = BTreeMap::<String, BTreeMap<NaiveDate, Value>>::new();
    let Some(dir) = &c.history_directory else {
        return Ok(daily);
    };
    for item in fs::read_dir(dir)? {
        let item = item?;
        if !item
            .file_name()
            .to_string_lossy()
            .ends_with(".receipt.json")
        {
            continue;
        }
        let r = history_download::read_receipt(&item.path()).map_err(|error| match error {
            "history_receipt_unreadable" => "planner_history_unreadable",
            _ => "planner_history_invalid",
        })?;
        if r.unit != "days" {
            continue;
        }
        if item.file_name().to_string_lossy()
            != format!(
                "{}.receipt.json",
                history_download::range_id(&r.key, &r.unit, r.start, r.end)
            )
        {
            return Err("planner_history_binding".into());
        }
        for (day, row) in history_download::candles(&r)? {
            if day >= c.session {
                continue;
            }
            let retained = daily.entry(r.key.clone()).or_default();
            if let Some(old) = retained.insert(day, row.clone())
                && old != row
            {
                return Err("planner_history_conflict".into());
            }
            // Retain the configured warmup window; feature lookbacks keep their schema. Bound resident memory
            // even when the research archive contains an entire broad-universe year.
            while retained.len() > c.daily_history_sessions {
                retained.pop_first();
            }
        }
    }
    Ok(daily)
}
// Newest daily bar strictly before the session. A name can miss previous_session
// after a holiday and still have an earlier dated close. Bars on the session
// itself are already dropped by load_daily.
fn latest_daily_closes(c: &Config) -> Result<BTreeMap<String, (NaiveDate, Price)>> {
    let mut closes = BTreeMap::new();
    for (key, rows) in load_daily(c)? {
        let Some((day, row)) = rows.iter().next_back() else {
            continue;
        };
        closes.insert(key, (*day, Price::parse(&row[4].to_string())?));
    }
    Ok(closes)
}
fn reference_document(c: &Config, closes: &BTreeMap<String, (NaiveDate, Price)>) -> Value {
    let prices = closes
        .iter()
        .map(|(key, (_, price))| (key.clone(), *price))
        .collect::<BTreeMap<_, _>>();
    let dates = closes
        .iter()
        .map(|(key, (day, _))| (key.clone(), day.to_string()))
        .collect::<BTreeMap<_, _>>();
    json!({
        "session": c.session,
        "as_of": c.previous_session,
        "close_basis": "latest_daily_before_session",
        "closes": prices,
        "close_dates": dates,
    })
}
fn verify_session_references(c: &Config, references: &Value) -> Result<()> {
    let closes = references["closes"].as_object();
    let dates = references["close_dates"].as_object();
    if references["session"] != c.session.to_string()
        || references["as_of"] != c.previous_session.to_string()
        || references["close_basis"] != "latest_daily_before_session"
        || closes.is_none_or(|m| m.len() > 20000)
        || dates.is_none_or(|m| m.len() > 20000)
        || closes.map(|m| m.len()) != dates.map(|m| m.len())
    {
        return Err("planner_saved_references_invalid".into());
    }
    let closes = closes.ok_or("planner_saved_references_invalid")?;
    let dates = dates.ok_or("planner_saved_references_invalid")?;
    for (key, close) in closes {
        let day = dates
            .get(key)
            .and_then(|value| value.as_str())
            .ok_or("planner_saved_references_invalid")?
            .parse::<NaiveDate>()
            .map_err(|_| "planner_saved_references_invalid")?;
        if day >= c.session {
            return Err("planner_saved_references_invalid".into());
        }
        Price::parse(close.as_str().ok_or("planner_saved_references_invalid")?)
            .map_err(|_| "planner_saved_references_invalid")?;
    }
    Ok(())
}
fn persist_session_references(c: &Config, references: &Value) -> Result<()> {
    verify_session_references(c, references)?;
    fs::create_dir_all(c.directory.join("model"))?;
    rollover::atomic(
        &c.directory.join("model/market-references.json"),
        references,
    )
}
pub(crate) fn session_references(c: &Config) -> Result<Value> {
    let path = c.directory.join("model/market-references.json");
    if path.exists() {
        let bytes = crate::read_bounded(&path, 8_000_000)
            .map_err(|_| "planner_saved_references_invalid")?;
        if let Ok(references) = serde_json::from_slice::<Value>(&bytes)
            && verify_session_references(c, &references).is_ok()
        {
            return Ok(references);
        }
    }
    // An older exact-date file has no close_basis. Rebuild it once, then reuse it.
    let references = reference_document(c, &latest_daily_closes(c)?);
    persist_session_references(c, &references)?;
    Ok(references)
}
pub(crate) fn close_from_references(references: &Value, key: &str) -> Option<String> {
    references["closes"][key]
        .as_str()
        .map(str::to_owned)
        .or_else(|| references["closes"][key].as_number().map(|n| n.to_string()))
}
fn bundle_with_references(
    c: &Config,
    p: &Portfolio,
    instruments: &[Instrument],
    n: usize,
) -> Result<(Value, Value)> {
    let daily = load_daily(c)?;
    let mut features = Vec::new();
    let mut historical_features = Vec::new();
    let mut reference_closes = BTreeMap::<String, (NaiveDate, Price)>::new();
    for i in instruments {
        let Some(rows) = daily.get(&i.key) else {
            continue;
        };
        if let Some((day, row)) = rows.iter().next_back() {
            reference_closes.insert(i.key.clone(), (*day, Price::parse(&row[4].to_string())?));
        }
        let bars = rows.iter().collect::<Vec<_>>();
        if bars.len() < c.daily_history_sessions.max(21) {
            continue;
        }
        let as_of = *bars.last().unwrap().0;

        let bars = &bars[bars.len() - 21..];
        let num = |v: &Value| -> Result<f64> {
            let n = v.as_f64().ok_or("planner_history_number")?;
            if !n.is_finite() || n < 0.0 {
                return Err("planner_history_number".into());
            }
            Ok(n)
        };
        let closes = bars
            .iter()
            .map(|(_, v)| num(&v[4]))
            .collect::<Result<Vec<_>>>()?;
        if closes.iter().any(|v| *v <= 0.0) {
            return Err("planner_history_close".into());
        }
        let volumes = bars
            .iter()
            .map(|(_, v)| num(&v[5]))
            .collect::<Result<Vec<_>>>()?;
        let returns = closes
            .windows(2)
            .map(|w| w[1] / w[0] - 1.0)
            .collect::<Vec<_>>();
        let mean = returns.iter().sum::<f64>() / 20.0;
        let volatility = (returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / 20.0).sqrt();
        let vm = volumes[..20].iter().sum::<f64>() / 20.0;
        if vm <= 0.0 || !volatility.is_finite() {
            continue;
        }
        let vs = (volumes[..20].iter().map(|v| (v - vm).powi(2)).sum::<f64>() / 20.0).sqrt();
        let prior_close = Price::parse(&bars[20].1[4].to_string())?;
        let row = json!({"instrument":i,"last_close":closes[20],"prior_close":prior_close,"as_of":as_of,"return_1d":closes[20]/closes[19]-1.0,"return_5d":closes[20]/closes[15]-1.0,"return_20d":closes[20]/closes[0]-1.0,"volatility_20d":volatility,"volume_z_score":if vs>0.0 {Some((volumes[20]-vm)/vs)}else{None},"gap":num(&bars[20].1[1])?/closes[19]-1.0,"gap_as_of":bars[20].0,"liquidity":vm*closes[20],"history_as_of":as_of,"history_current":as_of==c.previous_session});
        historical_features.push(row.clone());
        if as_of == c.previous_session {
            features.push(row);
        }
    }
    features.sort_by(|a, b| {
        b["liquidity"]
            .as_f64()
            .partial_cmp(&a["liquidity"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a["volatility_20d"]
                    .as_f64()
                    .partial_cmp(&b["volatility_20d"].as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                a["instrument"]["key"]
                    .as_str()
                    .cmp(&b["instrument"]["key"].as_str())
            })
    });
    let eligible_feature_count = features.len();
    features.truncate(n);
    Ok((
        json!({"session":c.session,"account":c.account,"cash":p.cash,"positions":p.positions,"allowlist":instruments,"shortlist":features,"historical_features":historical_features,"fees":p.costs.clone().ok_or("paper_costs_required")?,"data_selection":"descending trailing traded value, then ascending volatility; not a strategy","history_missing":daily.is_empty(),"eligible_feature_count":eligible_feature_count,"presented_feature_count":features.len(),"feature_coverage_partial":features.len()<eligible_feature_count}),
        reference_document(c, &reference_closes),
    ))
}

pub(crate) fn read_legacy_plan(c: &Config) -> Result<Option<Value>> {
    let path = c.directory.join("model/session-plan.json");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = crate::read_bounded(&path, SAVED_PLAN_MAX_BYTES as u64)
        .map_err(|_| "planner_saved_plan_invalid")?;
    Ok(Some(
        sadr_core::native::decode_bounded(&bytes, SAVED_PLAN_MAX_BYTES)
            .map_err(|_| "planner_saved_plan_invalid")?,
    ))
}
fn is_saved_hash(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}
pub(crate) fn validate_saved(c: &Config, plan: &Value) -> Result<()> {
    let id = plan["decision_id"]
        .as_str()
        .ok_or("planner_saved_plan_invalid")?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        || plan["session_binding"] != book_binding(c)?
        || plan["input_hash"] != digest(&serde_json::to_vec(&plan["input"])?)
        || plan["fake"] != false
        || plan["dry_run"] != false
        || plan["verdicts"].as_array().is_none_or(|v| v.len() > 200)
    {
        return Err("planner_saved_binding_invalid".into());
    }
    // A1 intent binding. A present revision must be a well-formed digest; a
    // missing value keeps old archived plans readable and executable.
    for key in [
        "portfolio_revision",
        "policy_sha256",
        "profile_sha256",
        "digest_id",
    ] {
        if let Some(value) = plan.get(key)
            && !value.is_null()
            && !is_saved_hash(value)
        {
            return Err("planner_saved_binding_invalid".into());
        }
    }
    if let Some(references) = plan.get("market_references")
        && (plan["market_references_hash"] != digest(&serde_json::to_vec(references)?)
            || references["session"] != c.session.to_string()
            || references["as_of"] != c.previous_session.to_string()
            || references["closes"]
                .as_object()
                .is_none_or(|m| m.len() > 20000))
    {
        return Err("planner_saved_references_invalid".into());
    }
    Ok(())
}
/// Admission for initial planning does not require an existing model plan.
/// Empty feature bundles must not burn the one-shot legacy attempt.
pub(super) fn has_eligible_input(
    c: &Config,
    p: &Portfolio,
    instruments: &[Instrument],
) -> Result<bool> {
    let cfg = c
        .planner_config
        .as_deref()
        .map(|path| PlannerConfig::load_for_session(path, c.session))
        .transpose()?;
    if let Some(cfg) = cfg.as_ref()
        && cfg.orchestrator_owns_decisions()
        && !p.positions.is_empty()
    {
        // A held book is evidence enough to compile a fresh factual digest,
        // even when the scanner has no new prospect.
        return Ok(true);
    }
    if let Some(settings) = cfg.as_ref().and_then(|cfg| cfg.discovery.as_ref()) {
        let snapshot = super::discovery::snapshot(c, instruments, settings, millis())?;
        let fresh = snapshot["fresh_count"].as_u64().unwrap_or(0) > 0
            || snapshot["eligible_count"].as_u64().unwrap_or(0) > 0
            || snapshot["prospects"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty());
        return Ok(
            if cfg
                .as_ref()
                .is_some_and(PlannerConfig::orchestrator_owns_decisions)
            {
                fresh
            } else {
                snapshot["prospects"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
            },
        );
    }
    let (input, _) = bundle_with_references(c, p, instruments, 1)?;
    Ok(input["shortlist"].as_array().is_some_and(|a| !a.is_empty())
        || p.positions
            .values()
            .any(|p| instruments.contains(&p.instrument)))
}

pub(crate) fn saved_plan(db: &Connection, c: &Config) -> Result<Option<Value>> {
    let plan = match redecision::active_plan(db, c)? {
        Some(plan) => plan,
        None => read_legacy_plan(c)?,
    };
    if let Some(plan) = &plan {
        validate_saved(c, plan)?;
    }
    Ok(plan)
}
pub(crate) fn usage(c: &Config) -> Result<Value> {
    let path = usage_path(c)?;
    if c.directory.join("model/usage-identity.json").exists() {
        usage_directory(c, true)?;
        let _lock = usage_lock(c, false)?;
        let journal = usage_directory(c, true)?;
        let records = checked_reservations(c, &journal)?;
        let mut counts = serde_json::Map::new();
        for role in [
            "digest",
            "decide",
            "operations",
            "strategy",
            "scorer",
            "supervisor",
            "research",
        ] {
            let mut charged = std::collections::BTreeSet::new();
            for name in records
                .keys()
                .filter(|n| n.starts_with(&format!("{role}-")))
            {
                if attempt_failed(&journal, name)? {
                    continue;
                }
                let prior: Value = serde_json::from_slice(
                    &crate::read_bounded(&journal.join(name), 4096)
                        .map_err(|_| "planner_usage_reservation_invalid")?,
                )?;
                charged.insert(prior["budget_event"].as_str().unwrap_or(name).to_owned());
            }
            counts.insert(role.into(), json!(charged.len()));
        }
        return Ok(Value::Object(counts));
    }
    let mut counts = BTreeMap::from([
        ("digest", 0usize),
        ("decide", 0usize),
        ("operations", 0usize),
        ("strategy", 0usize),
        ("scorer", 0usize),
        ("supervisor", 0usize),
        ("research", 0usize),
    ]);
    if path.exists() {
        for item in fs::read_dir(&path)? {
            let name = item?.file_name().to_string_lossy().into_owned();
            if name.ends_with(".attempt.json") && !attempt_failed(&path, &name)? {
                for (role, count) in &mut counts {
                    if name.starts_with(&format!("{role}-")) {
                        *count += 1;
                    }
                }
            }
        }
    }
    Ok(serde_json::to_value(counts)?)
}
pub(crate) fn status(plan: &Value) -> Value {
    json!({"status":"validated_plan","decision_id":plan["decision_id"],"model_id":plan["model_id"],"provider":plan["provider"],"actions":plan["verdicts"],"budget_used":plan["budget_used"],"budgets":plan["budgets"]})
}
#[derive(clap::Subcommand)]
pub enum Command {
    /// Validate and describe a dated planner config without credentials or model calls.
    ConfigCheck {
        #[arg(long)]
        planner: PathBuf,
        #[arg(long)]
        session: NaiveDate,
    },
    /// Validate an experiment profile against a planner policy without model calls.
    ProfileCheck {
        #[arg(long)]
        profile: PathBuf,
        #[arg(long)]
        planner: PathBuf,
    },
    /// Genuine three-role protocol check with synthetic evidence; no financial book.
    HarnessCheck {
        #[arg(long)]
        planner: PathBuf,
        /// New, private diagnostic authority; retain it and all consumed attempts.
        #[arg(long)]
        directory: PathBuf,
        /// Test one role, retaining all reservations in this diagnostic authority.
        #[arg(long, value_parser = ["digest", "strategy", "operations"])]
        role: Option<String>,
        /// Explicit new diagnostic after a code fix; never refunds or replays calls.
        #[arg(long)]
        continue_after_fix: bool,
        /// Charge the existing closed book's original allowance, never a new one.
        #[arg(long)]
        authority_config: Option<PathBuf>,
    },
    /// Advisory review of a verified historical close snapshot; never advances a book.
    ReviewPreview {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        planner: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// One-shot current-market diagnostic; never opens a book or executes trades.
    LivePreview {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        planner: PathBuf,
        /// New private diagnostic directory, separate from all financial books.
        #[arg(long)]
        output: PathBuf,
        /// Exact symbols resolved through the newly validated dated universe.
        #[arg(long, value_delimiter = ',')]
        symbols: Vec<String>,
    },
    DryRun {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        planner: PathBuf,
        #[arg(long)]
        fake_plan: Option<PathBuf>,
    },
}
pub fn entry(cmd: Command) -> ExitCode {
    if let Command::ProfileCheck { profile, planner } = &cmd {
        return match super::experiment_profile::check(planner, profile) {
            Ok(value) => {
                println!("{value}");
                ExitCode::SUCCESS
            }
            Err(code) => {
                eprintln!("experiment_profile_refused: {code}");
                ExitCode::FAILURE
            }
        };
    }
    if let Command::ConfigCheck { planner, session } = &cmd {
        return match PlannerConfig::load_for_session(planner, *session) {
            Ok(cfg) => {
                let digest_role = cfg.role(Trigger::Digest);
                let decide_role = cfg.role(Trigger::Decide);
                println!(
                    "{}",
                    json!({"status":"planner_config_valid","session":session,"policy_sha256":digest(&policy_bytes(&cfg).expect("serializable")),"enabled":cfg.enabled,"model_led":cfg.model_led,"discovery":cfg.discovery,"digest_model":digest_role.model_id,"decide_model":decide_role.model_id,"digest_daily_allowance":digest_role.daily_call_budget,"decide_daily_allowance":decide_role.daily_call_budget,"effective_from":cfg.effective_from,"paper_only":true,"model_calls":0})
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("planner_config_refused: {}", failure_code(error.as_ref()));
                ExitCode::FAILURE
            }
        };
    }
    if let Command::HarnessCheck {
        planner,
        directory,
        role,
        continue_after_fix,
        authority_config,
    } = &cmd
    {
        return super::harness::diagnostic_check(
            planner,
            directory,
            role.as_deref(),
            *continue_after_fix,
            authority_config.as_deref(),
        );
    }
    if let Command::ReviewPreview {
        config,
        planner,
        output,
    } = &cmd
    {
        return super::harness::review_preview(config, planner, output);
    }
    if let Command::LivePreview {
        config,
        planner,
        output,
        symbols,
    } = &cmd
    {
        return live::entry(config, planner, output, symbols);
    }
    let result = (|| -> Result<Value> {
        let Command::DryRun {
            config,
            planner,
            fake_plan,
        } = cmd
        else {
            unreachable!()
        };
        let c: Config = serde_json::from_slice(
            &crate::read_bounded(&config, 16384).map_err(|_| "paper_config_unreadable")?,
        )?;
        c.validate()?;
        let c = rollover::current(&c)?;
        let cfg = PlannerConfig::load_for_session(&planner, c.session)?;
        let instruments = wide::history_instruments(&c)?;
        // Read-only: a dry run never creates/changes a financial book.
        let p = if c.directory.join("paper.sqlite").exists() {
            let (_, body) = rollover::bound_state(&c)?;
            serde_json::from_str::<Portfolio>(&body)?
        } else {
            let mut p = Portfolio::with_capital(c.allocation()?);
            p.costs = c.paper_costs.clone();
            p
        };
        p.reconcile()?;
        let value = produce(&c, &cfg, &p, &instruments, fake_plan.as_deref(), true)?;
        Ok(json!({"paper_only":true,"dry_run":true,"traded":false,"result":value}))
    })();
    match result {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}", reason(error.as_ref()));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod remaining_settings_contract;
