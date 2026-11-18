//! Operator-selected orchestrator model binding.
//!
//! The reviewed planner policy names exact model/credential profiles and the
//! profile names it admits. A selection is a separate, versioned runtime setting
//! so a model-only switch never rewrites the sealed policy, merges role charges
//! or resets the persistent session. Requests are durable pending/active
//! revisions; the controller promotes a pending revision only at a turn boundary.
use super::{Config, digest, planner};
use nivra_cli::native_transport::Transport;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub(crate) const DEFAULT_PROFILE: &str = "default";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 65_536;
const MAX_RECEIPTS: usize = 16;

type SelectionResult<T> = std::result::Result<T, &'static str>;

/// Exact model/credential binding. The credential path is application-owned and
/// never inferred from another role, environment or another application's cache.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub model_id: String,
    pub transport: Transport,
    pub model_config: PathBuf,
}

impl Binding {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Active {
    revision: u64,
    profile: String,
    model: Binding,
    policy_sha256: String,
    request_id: String,
    applied_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    revision: u64,
    profile: String,
    model: Binding,
    policy_sha256: String,
    request_id: String,
    requested_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    revision: u64,
    request_id: String,
    profile: String,
    model_id: String,
    transport: Transport,
    policy_sha256: String,
    /// requested | applied | refused | superseded
    state: String,
    #[serde(default)]
    reason: Option<String>,
    at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: u32,
    session: String,
    session_binding: String,
    revision: u64,
    #[serde(default)]
    active: Option<Active>,
    #[serde(default)]
    pending: Option<Pending>,
    #[serde(default)]
    receipts: Vec<Receipt>,
}

/// A validated active selection applied to one model turn.
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    pub(crate) revision: u64,
    #[cfg(test)]
    pub(crate) profile: String,
    #[cfg(test)]
    pub(crate) model: Binding,
}

impl Resolved {}

fn state_path(c: &Config) -> PathBuf {
    c.directory.join("model/model-selection.json")
}

fn fresh(c: &Config) -> SelectionResult<Selection> {
    Ok(Selection {
        version: VERSION,
        session: c.session.to_string(),
        session_binding: super::book_binding(c).map_err(|_| "model_selection_binding_invalid")?,
        revision: 0,
        active: None,
        pending: None,
        receipts: Vec::new(),
    })
}

fn load(c: &Config) -> SelectionResult<Selection> {
    let path = state_path(c);
    if !path.exists() {
        return fresh(c);
    }
    let bytes = crate::read_bounded(&path, MAX_BYTES).map_err(|_| "model_selection_unreadable")?;
    let state: Selection = serde_json::from_slice(&bytes).map_err(|_| "model_selection_invalid")?;
    if state.version != VERSION
        || state.session != c.session.to_string()
        || state.session_binding
            != super::book_binding(c).map_err(|_| "model_selection_binding_invalid")?
        || state.receipts.len() > MAX_RECEIPTS
    {
        return Err("model_selection_invalid");
    }
    Ok(state)
}

fn policy_sha256(cfg: &planner::PlannerConfig) -> SelectionResult<String> {
    planner::policy_bytes(cfg)
        .map(|bytes| digest(&bytes))
        .map_err(|_| "model_selection_policy_invalid")
}

/// Resolve a policy-admitted profile to its exact binding. `default` is the
/// configured orchestrator model and never needs a separate admission entry so
/// existing configurations keep their behavior.
pub(super) fn binding(cfg: &planner::PlannerConfig, name: &str) -> SelectionResult<Binding> {
    let orchestrator = cfg
        .orchestrator
        .as_ref()
        .ok_or("model_selection_orchestrator_missing")?;
    if !orchestrator.enabled {
        return Err("model_selection_orchestrator_disabled");
    }
    if name == DEFAULT_PROFILE {
        let model_id = orchestrator.model.model_id.clone();
        let model_config = cfg
            .model_config
            .clone()
            .or_else(|| orchestrator.model.model_config.clone())
            .ok_or("model_selection_default_config_missing")?;
        if model_id.is_empty() {
            return Err("model_selection_default_model_missing");
        }
        return validate_owner_binding(
            cfg,
            Binding {
                model_id,
                transport: orchestrator.model.transport,
                model_config,
            },
        );
    }
    if !orchestrator
        .admitted_models
        .iter()
        .any(|admitted| admitted == name)
    {
        return Err("model_selection_not_admitted");
    }
    let profile = orchestrator
        .model_profiles
        .get(name)
        .ok_or("model_selection_profile_missing")?;
    validate_owner_binding(
        cfg,
        Binding {
            model_id: profile.model_id.clone(),
            transport: profile.transport,
            model_config: profile.model_config.clone(),
        },
    )
}

fn validate_owner_binding(
    cfg: &planner::PlannerConfig,
    model: Binding,
) -> SelectionResult<Binding> {
    super::model_admission::binding_shape(&model)?;
    super::model_admission::transport_tools(
        model.transport,
        &cfg.role(super::orchestrator_model::Trigger::Supervisor)
            .tools,
    )?;
    Ok(model)
}

/// Resolve the active selection for a new turn. A selection invalidated by a
/// changed policy is an explicit failure, never a silent fallback to another
/// model. An absent selection keeps the configured orchestrator model.
pub(crate) fn resolve(
    c: &Config,
    cfg: &planner::PlannerConfig,
) -> SelectionResult<Option<Resolved>> {
    let state = load(c)?;
    let Some(active) = state.active else {
        return Ok(None);
    };
    if active.policy_sha256 != policy_sha256(cfg)? {
        return Err("model_selection_policy_changed");
    }
    let model = binding(cfg, &active.profile).map_err(|_| "model_selection_policy_changed")?;
    if model != active.model {
        return Err("model_selection_policy_changed");
    }
    Ok(Some(Resolved {
        revision: active.revision,
        #[cfg(test)]
        profile: active.profile,
        #[cfg(test)]
        model,
    }))
}
