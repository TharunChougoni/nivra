use super::*;

/// Legacy per-role blocks parsed from a planner configuration. `None` means the
/// block was absent, which is distinct from a present block holding defaults.
#[derive(Clone, Default)]
pub(crate) struct LegacyRoleBlocks {
    // Parse and validate independent roles during migration; retain them unchanged.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) digest: Option<ModelRole>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) decide: Option<ModelRole>,
    pub(crate) operations: Option<ModelRole>,
    pub(crate) strategy: Option<ModelRole>,
}

impl LegacyRoleBlocks {
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.digest.is_none()
            && self.decide.is_none()
            && self.operations.is_none()
            && self.strategy.is_none()
    }

    /// Roles that the orchestrator replaces. Digest and decide deliberately stay
    /// outside this set and may coexist with the orchestrator.
    pub(crate) fn has_orchestrator_roles(&self) -> bool {
        self.operations.is_some() || self.strategy.is_some()
    }

    /// Parse the role blocks from a planner-shaped JSON object. The role-specific
    /// timeout defaults match `PlannerConfig` deserialization.
    pub(crate) fn from_planner_value(value: &Value) -> Result<Self> {
        let post_close = value.get("post_close");
        Ok(Self {
            digest: value
                .get("digest")
                .map(|role| role_from_value(role, 20_000))
                .transpose()?,
            decide: value
                .get("decide")
                .map(|role| role_from_value(role, 150_000))
                .transpose()?,
            operations: post_close
                .and_then(|settings| settings.get("operations"))
                .map(|role| role_from_value(role, 20_000))
                .transpose()?,
            strategy: post_close
                .and_then(|settings| settings.get("strategy"))
                .map(|role| role_from_value(role, 20_000))
                .transpose()?,
        })
    }
}

pub(crate) fn role_from_value(value: &Value, timeout_ms: u64) -> Result<ModelRole> {
    let mut value = value.clone();
    let object = value
        .as_object_mut()
        .ok_or("orchestrator_legacy_role_invalid")?;
    object
        .entry("timeout_ms")
        .or_insert_with(|| timeout_ms.into());
    Ok(serde_json::from_value(value)?)
}

/// Derive the orchestrator settings from the legacy roles it replaces.
///
/// Digest and decide keep their own role blocks and may coexist with an enabled
/// orchestrator. Operations and strategy may not: silently choosing which model
/// receives those triggers could change the review authority.
pub(crate) fn migrate_legacy_roles(
    orchestrator: Option<&OrchestratorModelSettings>,
    legacy: &LegacyRoleBlocks,
) -> Result<OrchestratorModelSettings> {
    if orchestrator.is_some_and(|settings| settings.enabled) && legacy.has_orchestrator_roles() {
        return Err(ORCHESTRATOR_LEGACY_CONFLICT.into());
    }
    if let Some(settings) = orchestrator
        && (settings.enabled || !legacy.has_orchestrator_roles())
    {
        return Ok(settings.clone());
    }
    Ok(derive_from_legacy(legacy))
}

pub(crate) fn derive_from_legacy(legacy: &LegacyRoleBlocks) -> OrchestratorModelSettings {
    let mut settings = OrchestratorModelSettings::default();
    // The orchestrator replaces only operations and strategy. Their blocks are
    // the source of its model identity; digest and decide stay separate.
    if legacy.has_orchestrator_roles() {
        settings.triggers.remove(&Trigger::Operations);
        settings.triggers.remove(&Trigger::Strategy);
        if let Some(role) = legacy.operations.as_ref().or(legacy.strategy.as_ref()) {
            settings.model = model_defaults_from_role(role);
        }
        for (trigger, role) in [
            (Trigger::Operations, legacy.operations.as_ref()),
            (Trigger::Strategy, legacy.strategy.as_ref()),
        ] {
            if let Some(role) = role {
                settings
                    .triggers
                    .insert(trigger, trigger_settings(trigger, role));
            }
        }
        settings.enabled = [&legacy.operations, &legacy.strategy]
            .into_iter()
            .flatten()
            .any(|role| role.enabled);
        // Legacy review roles are not a scorer. Leaving the default trigger
        // enabled would schedule a run the old config never had and reject the
        // migrated file for a missing `after_minute`.
        if let Some(scorer) = settings.triggers.get_mut(&Trigger::Scorer) {
            scorer.enabled = false;
        }
    }
    settings
}

pub(crate) fn model_defaults_from_role(role: &ModelRole) -> ModelDefaults {
    ModelDefaults {
        model_config: role.model_config.clone(),
        model_id: role.model_id.clone(),
        transport: role.transport,
        tools: role.tools.clone(),
        timeout_ms: role.timeout_ms,
        run_timeout_ms: role.run_timeout_ms,
        output_cap: role.output_cap,
        daily_call_budget: role.daily_call_budget,
        budget_unit: role.budget_unit.clone(),
        max_turns: role.max_turns,
        max_read_tools: role.max_read_tools,
        max_analysis_tools: role.max_analysis_tools,
        minimum_turn_ms: role.minimum_turn_ms,
        max_failed_tools: role.max_failed_tools,
        fallback: role.fallback.clone(),
        approval_auto_renew: None,
    }
}

pub(crate) fn trigger_settings(trigger: Trigger, role: &ModelRole) -> TriggerSettings {
    TriggerSettings {
        enabled: role.enabled,
        prompt: PathBuf::from(trigger.default_prompt()),
        tools: Some(role.tools.clone()),
        max_turns: role.max_turns,
        daily_call_budget: Some(role.daily_call_budget),
        timeout_ms: Some(role.timeout_ms),
        run_timeout_ms: role.run_timeout_ms,
        output_cap: Some(role.output_cap),
        budget_unit: role.budget_unit.clone(),
        max_read_tools: role.max_read_tools,
        max_analysis_tools: Some(role.max_analysis_tools),
        minimum_turn_ms: role.minimum_turn_ms,
        max_failed_tools: role.max_failed_tools,
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

/// Migrate one planner configuration into the single-orchestrator shape. The
/// destination is always a new file: an existing file, or the input itself, is
/// refused so the original configuration is never rewritten in place.
pub(crate) fn migrate_config_file(config: &Path, out: &Path) -> Result<()> {
    let raw = crate::read_bounded(config, 65536).map_err(|_| "config_migrate_unreadable")?;
    let mut value: Value = serde_json::from_slice(&raw).map_err(|_| "config_migrate_invalid")?;
    if !value.is_object() {
        return Err("config_migrate_invalid".into());
    }
    if same_file(config, out) {
        return Err("config_migrate_same_path".into());
    }
    let legacy =
        LegacyRoleBlocks::from_planner_value(&value).map_err(|_| "config_migrate_invalid")?;
    let existing = value
        .get("orchestrator")
        .map(|settings| serde_json::from_value::<OrchestratorModelSettings>(settings.clone()))
        .transpose()
        .map_err(|_| "config_migrate_invalid")?;
    let settings = migrate_legacy_roles(existing.as_ref(), &legacy)?;
    let object = value.as_object_mut().ok_or("config_migrate_invalid")?;
    // Digest and decide keep their own sandbox, credential, approval and budget.
    // Only the post-close roles move into the orchestrator.
    if let Some(post_close) = object.get_mut("post_close").and_then(Value::as_object_mut) {
        post_close.remove("operations");
        post_close.remove("strategy");
    }
    object.insert(
        "orchestrator".into(),
        serde_json::to_value(settings).map_err(|_| "config_migrate_invalid")?,
    );
    let mut text = serde_json::to_string_pretty(&value).map_err(|_| "config_migrate_invalid")?;
    text.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => "config_migrate_out_exists",
            _ => "config_migrate_write_failed",
        })?;
    use std::io::Write;
    file.write_all(text.as_bytes())
        .map_err(|_| "config_migrate_write_failed")?;
    file.sync_all().map_err(|_| "config_migrate_write_failed")?;
    Ok(())
}

pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
