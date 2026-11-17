//! Exact configured role/profile admission before a real provider reservation.
//! Catalog membership and a structurally valid profile do not prove entitlement;
//! the model transport still validates the application grant.
use super::{Config, Result, model_selection, planner};
use nivra_cli::native_transport::Transport;
use std::path::Path;

pub(super) fn binding_shape(
    model: &model_selection::Binding,
) -> std::result::Result<(), &'static str> {
    if model.model_id.is_empty()
        || model.model_id.len() > 128
        || model
            .model_id
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        || !model.model_config.is_absolute()
    {
        return Err("model_selection_profile_invalid");
    }
    Ok(())
}

pub(super) fn transport_tools(
    transport: Transport,
    tools: &[String],
) -> std::result::Result<(), &'static str> {
    if transport == Transport::OpenaiCodex
        && tools
            .iter()
            .any(|tool| matches!(tool.as_str(), "web_search" | "x_search"))
    {
        return Err("model_selection_profile_tools_incompatible");
    }
    Ok(())
}

/// Named inline role blocks are profiles too: `digest` and historical review
/// blocks keep their exact config identity. The persistent owner uses only the
/// currently selected, policy-admitted orchestrator profile, never a pending one.
pub(super) fn validate(
    c: &Config,
    cfg: &planner::PlannerConfig,
    name: &str,
    role: &planner::ModelRole,
    credential: &Path,
) -> Result<String> {
    if !role.enabled {
        return Err("harness_role_disabled".into());
    }
    let (profile, expected) = match name {
        "digest" => ("digest".to_owned(), inline(cfg, &cfg.digest)?),
        "supervisor" if cfg.orchestrator.is_some() => {
            match model_selection::resolve(c, cfg).map_err(str::to_owned)? {
                Some(active) => (format!("orchestrator.{}", active.profile), active.model),
                None => (
                    "orchestrator.default".into(),
                    model_selection::binding(cfg, "default").map_err(str::to_owned)?,
                ),
            }
        }
        "scorer" if cfg.orchestrator.is_some() => (
            "orchestrator.default".into(),
            model_selection::binding(cfg, "default").map_err(str::to_owned)?,
        ),
        _ => return Err("harness_role_model_profile_required".into()),
    };
    binding_shape(&expected).map_err(str::to_owned)?;
    transport_tools(role.transport, &role.tools).map_err(str::to_owned)?;
    if expected.model_id != role.model_id
        || expected.transport != role.transport
        || expected.model_config != credential
    {
        return Err("harness_role_model_profile_mismatch".into());
    }
    Ok(profile)
}

fn inline(
    cfg: &planner::PlannerConfig,
    role: &planner::ModelRole,
) -> Result<model_selection::Binding> {
    if !role.enabled {
        return Err("harness_role_disabled".into());
    }
    Ok(model_selection::Binding {
        model_id: role.model_id.clone(),
        transport: role.transport,
        model_config: role
            .model_config
            .clone()
            .or_else(|| cfg.model_config.clone())
            .ok_or("harness_role_model_config_required")?,
    })
}
