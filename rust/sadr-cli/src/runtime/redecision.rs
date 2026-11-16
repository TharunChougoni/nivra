//! Durable event admission for the opt-in planner. No model or market I/O.
//! The original book owns the active decision and every consumed attempt.
use super::orchestrator_model::Trigger;
use super::phases::PlannerLogPhase;
use super::planner::PlannerConfig;
use super::*;
use rusqlite::OptionalExtension;
use sadr_core::{
    execution::Portfolio,
    redecision::{Event, EventPolicy, EventState, MarketObservation, NewsObservation},
};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    pub enabled: bool,
    /// Independent run lifetime, measured from admission rather than the queued event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_timeout_ms: Option<i64>,
    /// IST minute for admitting decisions. Omission retains legacy pre-open planning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_start_minute: Option<u16>,
    pub policy: EventPolicy,
    pub cooldown_ms: i64,
    pub max_pending: usize,
    pub news_file: Option<PathBuf>,
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        self.policy.validate()?;
        if self
            .decision_timeout_ms
            .is_some_and(|n| !(30_000..=900_000).contains(&n))
        {
            return Err("planner_decision_timeout_invalid".into());
        }
        if self
            .decision_start_minute
            .is_some_and(|n| !(480..930).contains(&n))
            || !(0..=86_400_000).contains(&self.cooldown_ms)
            || !(1..=200).contains(&self.max_pending)
        {
            return Err("planner_event_settings_invalid".into());
        }
        Ok(())
    }
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Claim {
    pub id: String,
    pub event_id: String,
    pub trigger: Value,
    pub binding: String,
    pub portfolio_hash: String,
    pub started_ms: i64,
    pub deadline_ms: i64,
    pub predecessor: Option<String>,
    pub previous_attempts: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    binding: String,
    config_hash: String,
    /// Trigger prompt content hashes observed with `config_hash`. Absent in
    /// pre-#137 journals; absence degrades to a config-hash diagnosis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_sha256: Option<Value>,
    /// Policy hash with trigger `prompt_sha256` fields removed. Comparing this
    /// distinguishes a prompt edit from a non-prompt configuration change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config_without_prompt_sha256: Option<String>,
    detector: EventState,
    fill_count: usize,
    fill_hash: String,
    last_started_ms: Option<i64>,
    pending: Option<Claim>,
    active: Option<String>,
    blocked: Option<String>,
    /// Owner policy only: the last recoverable digest-pipeline failure. It is
    /// recorded for diagnosis and never refuses an owner decision built from an
    /// earlier valid digest (#221). Absent in older journals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    digest_failed: Option<String>,
    initial_available: bool,
    /// Owner-policy cadence facts already delivered to the orchestrator. A
    /// repeated cadence digest with the same host signature is coalesced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_cadence_facts_sha256: Option<String>,
    market_observations: u64,
    missing_volume: u64,
    missing_prior_close: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuthorityOwner {
    Legacy,
    Orchestrator,
}
impl AuthorityOwner {
    fn for_config(cfg: &planner::PlannerConfig) -> Self {
        if cfg.orchestrator_owns_decisions() {
            Self::Orchestrator
        } else {
            Self::Legacy
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityOwnerReceipt {
    version: u32,
    binding: String,
    config_hash: String,
    owner: AuthorityOwner,
}
fn authority_owner(db: &Connection, state: &Control) -> Result<Option<AuthorityOwnerReceipt>> {
    match authority::read_marker_state(db, state)? {
        authority::MarkerState::TableAbsent => Ok(None),
        authority::MarkerState::RowMissing => Err("planner_authority_owner_missing".into()),
        authority::MarkerState::Malformed { error, .. } => Err(error.into()),
        authority::MarkerState::Stale { .. } => Err("planner_authority_owner_mismatch".into()),
        authority::MarkerState::Current { receipt, .. } => Ok(Some(receipt)),
    }
}
fn save_authority_owner(db: &Connection, receipt: &AuthorityOwnerReceipt) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS planner_authority_owner(id INTEGER PRIMARY KEY CHECK(id=1),body TEXT NOT NULL);")?;
    db.execute(
        "INSERT INTO planner_authority_owner VALUES(1,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
        [serde_json::to_string(receipt)?],
    )?;
    Ok(())
}
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS planner_event_control(id INTEGER PRIMARY KEY CHECK(id=1),body TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS planner_event_log(id TEXT PRIMARY KEY,at INTEGER NOT NULL,body TEXT NOT NULL,state TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS planner_attempt_log(id TEXT PRIMARY KEY,event_id TEXT NOT NULL UNIQUE,body TEXT NOT NULL,state TEXT NOT NULL,result TEXT);
CREATE TABLE IF NOT EXISTS planner_plan(id TEXT PRIMARY KEY,body TEXT NOT NULL);
";
fn exists(db: &Connection) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='planner_event_control')", [], |r| r.get(0))?)
}
pub(crate) fn has_authority(db: &Connection) -> Result<bool> {
    if exists(db)? {
        return Ok(true);
    }
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM ticks WHERE event='event_planner_authority')",
        [],
        |r| r.get(0),
    )?)
}
fn load(db: &Connection) -> Result<Option<Control>> {
    if !exists(db)? {
        let ticks: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='ticks')",
            [],
            |r| r.get(0),
        )?;
        if ticks
            && db.query_row(
                "SELECT EXISTS(SELECT 1 FROM ticks WHERE event='event_planner_authority')",
                [],
                |r| r.get::<_, bool>(0),
            )?
        {
            return Err("planner_event_authority_missing".into());
        }
        return Ok(None);
    }
    let raw: Option<String> = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let raw = raw.ok_or("planner_event_state_missing")?;
    Some(raw)
        .map(|raw| {
            if raw.len() > 16_000_000 {
                return Err("planner_event_state_bounds".into());
            }
            serde_json::from_str(&raw).map_err(|_| "planner_event_state_invalid".into())
        })
        .transpose()
}
fn save(db: &Connection, state: &Control) -> Result<()> {
    let body = serde_json::to_string(state)?;
    if body.len() > 16_000_000 {
        return Err("planner_event_state_bounds".into());
    }
    db.execute("INSERT INTO planner_event_control VALUES(1,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body", [body])?;
    Ok(())
}
/// True when no authority exists yet or it already binds this planner config.
pub(crate) fn config_current(db: &Connection, cfg: &planner::PlannerConfig) -> Result<bool> {
    Ok(load(db)?.is_none_or(|state| config_hash(cfg).is_ok_and(|h| h == state.config_hash)))
}
fn config_hash(cfg: &planner::PlannerConfig) -> Result<String> {
    Ok(digest(&planner::policy_bytes(cfg)?))
}
/// Policy identity used to explain an authority mismatch. Prompt content is a
/// policy input, but keeping it separate makes the exact failed check auditable.
fn config_identity(
    cfg: &planner::PlannerConfig,
) -> Result<(String, Option<Value>, Option<String>)> {
    let bytes = planner::policy_bytes(cfg)?;
    let hash = digest(&bytes);
    let mut prompts = cfg
        .orchestrator
        .as_ref()
        .map(|settings| settings.prompt_hashes())
        .transpose()?;
    if cfg.file_prompts.is_some() {
        prompts = Some(json!({"orchestrator":prompts, "defaults":cfg.prompt_hashes()}));
    }
    let mut value: Value = serde_json::from_slice(&bytes)?;
    if let Some(value) = value.as_object_mut() {
        value.remove("default_prompt_sha256");
    }
    if let Some(triggers) = value
        .get_mut("orchestrator")
        .and_then(|settings| settings.get_mut("triggers"))
        .and_then(Value::as_object_mut)
    {
        for settings in triggers.values_mut() {
            if let Some(settings) = settings.as_object_mut() {
                settings.remove("prompt_sha256");
            }
        }
    }
    let without_prompt = digest(&serde_json::to_vec(&value)?);
    Ok((hash, prompts, Some(without_prompt)))
}
/// Read-only diagnosis attached to a planner-authority incident tick. It never
/// mutates the journal and never loads a prompt outside the normal policy path.
pub(crate) fn configuration_check(db: &Connection, c: &Config) -> Value {
    let Some(path) = c.planner_config.as_deref() else {
        return json!({"failed_check":"planner_config_missing"});
    };
    let cfg = match planner::PlannerConfig::load_for_session(path, c.session) {
        Ok(cfg) => cfg,
        Err(error) => {
            return json!({
                "failed_check":"planner_configuration_invalid",
                "config_error":error.to_string()
            });
        }
    };
    let state = match load(db) {
        Ok(state) => state,
        Err(error) => {
            return json!({
                "failed_check":"planner_authority_invalid",
                "authority_error":error.to_string()
            });
        }
    };
    let Some(state) = state else {
        return json!({"failed_check":"planner_authority_missing"});
    };
    let (current_hash, current_prompts, current_without) = match config_identity(&cfg) {
        Ok(identity) => identity,
        Err(error) => {
            return json!({
                "failed_check":"planner_configuration_invalid",
                "config_error":error.to_string(),
                "prior_config_hash":state.config_hash
            });
        }
    };
    let prompt_changed = state
        .prompt_sha256
        .as_ref()
        .zip(current_prompts.as_ref())
        .is_some_and(|(prior, current)| prior != current);
    let non_prompt_changed = state
        .config_without_prompt_sha256
        .as_ref()
        .zip(current_without.as_ref())
        .is_some_and(|(prior, current)| prior != current);
    let failed_check = if prompt_changed && !non_prompt_changed {
        "prompt_sha256"
    } else {
        "config_hash"
    };
    json!({
        "failed_check":failed_check,
        "prior_config_hash":state.config_hash,
        "current_config_hash":current_hash,
        "prior_prompt_sha256":state.prompt_sha256,
        "current_prompt_sha256":current_prompts,
        "prior_config_without_prompt_sha256":state.config_without_prompt_sha256,
        "current_config_without_prompt_sha256":current_without
    })
}
/// Upgrade a legacy full-config hash only using an exact matching archived policy.
/// The source names the exact proof so the startup audit does not call every
/// upgrade an exact pre-marker match.
fn upgrade_config_hash(
    c: &Config,
    cfg: &planner::PlannerConfig,
    old: &str,
) -> Result<Option<(String, &'static str)>> {
    if old == digest(&serde_json::to_vec(cfg)?) {
        return Ok(Some((config_hash(cfg)?, "current_config")));
    }
    if old == digest(&planner::legacy_policy_bytes(cfg)?) {
        return Ok(Some((config_hash(cfg)?, "legacy_policy")));
    }
    let root = c.directory.join("model");
    if !root.exists() {
        return Ok(None);
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("input.json");
        if !path.exists() {
            continue;
        }
        let value: Value = serde_json::from_slice(
            &crate::read_bounded(&path, 1_000_000).map_err(|_| "planner_config_audit_invalid")?,
        )?;
        let (Some(digest_role), Some(decide_role)) = (value.get("digest"), value.get("decide"))
        else {
            continue;
        };
        let mut prior = cfg.clone();
        prior.digest = serde_json::from_value(digest_role.clone())?;
        prior.decide = serde_json::from_value(decide_role.clone())?;
        if digest(&serde_json::to_vec(&prior)?) == old
            && planner::policy_bytes(&prior)? == planner::policy_bytes(cfg)?
        {
            return Ok(Some((config_hash(cfg)?, "archived_policy")));
        }
    }
    Ok(None)
}
fn settings_for(cfg: &planner::PlannerConfig, allow_legacy_decide: bool) -> Option<&Settings> {
    let decision_enabled = (allow_legacy_decide && cfg.role(Trigger::Decide).enabled)
        || cfg.orchestrator_owns_decisions();
    cfg.events
        .as_ref()
        .filter(|e| e.enabled && cfg.enabled && decision_enabled)
}
/// Unit mechanics keep the legacy Decide path available; production admits it
/// only through the reviewed migration and never as independent inference.
fn settings(cfg: &planner::PlannerConfig) -> Option<&Settings> {
    settings_for(cfg, cfg!(test))
}
/// Ignore quote marks: a protection tick may update marks while a model is running.
/// Cash, holdings, costs, exit instructions and the complete fill sequence must agree.
pub(crate) fn portfolio_hash(p: &Portfolio) -> Result<String> {
    let mut body = serde_json::to_value(p)?;
    if let Some(positions) = body["positions"].as_object_mut() {
        for position in positions.values_mut() {
            if let Some(position) = position.as_object_mut() {
                position.remove("mark");
                position.remove("marked_at");
            }
        }
    }
    if let Some(body) = body.as_object_mut() {
        body.remove("health");
        body.remove("high_water");
    }
    Ok(digest(&serde_json::to_vec(&body)?))
}
/// A transport, quota or provider-availability failure used no market data.
/// Its block clears only after a fully validated fresh quote, once every
/// attempt that overlapped the failure has settled. Malformed or
/// identity-invalid responses retain the sticky data block.
pub(crate) const READ_TRANSIENT_BLOCK: &str = "planner_read_unavailable_no_replay";
pub(crate) const DATA_BLOCK: &str = "planner_data_failed_no_replay";
/// Timing and model-output failures can be retried after a new observation.
/// Operator, configuration, data, news, and interrupted-attempt blocks cannot.
pub(crate) fn recoverable_block(reason: &str) -> bool {
    matches!(
        reason,
        "planner_event_observation_invalid"
            | "planner_event_queue_full"
            | "planner_failed_no_replay"
            | "planner_failed_no_new_entries"
            | "planner_cancelled_no_replay"
            | "planner_event_deadline_expired"
            | "planner_completion_expired_or_changed"
            | "planner_completion_outside_session"
            | "planner_budget_exhausted"
            | "planner_provider_failed"
            | "planner_worker_failed_no_replay"
            | "model_plan_unavailable_no_trades"
            | "planner_invalid_plan_no_entries"
    )
}
/// Blocks that must refuse an owner decision. Only a sticky block is
/// authoritative for the owner; a recoverable failure of the factual digest
/// pipeline is not (#221). The transient read block suspends execution in
/// `active_plan` instead, so a decision committed while it is set stays
/// suspended until a validated fresh quote clears it.
fn refuses_owner_decision(reason: &str) -> bool {
    !recoverable_block(reason) && reason != READ_TRANSIENT_BLOCK
}

/// A block that records an integrity or authority fact which startup must not
/// replace with a weaker pending/config signal.
fn preserves_through_startup(reason: &str) -> bool {
    !recoverable_block(reason)
}

/// Read failures that prove only that no market data arrived: HTTP/transport,
/// local quota admission and response-encoding limits. They are safe to clear
/// automatically after a later validated quote. Request, credential, journal
/// and clock failures stay sticky for review.
pub(crate) fn transient_read_failure(reason: &str) -> bool {
    reason.starts_with("http_")
        || matches!(
            reason,
            "read_only_http_failed"
                | "read_quota_wait_per_second"
                | "read_quota_wait_per_minute"
                | "read_quota_wait_per_half_hour"
                | "response_too_large"
                | "response_not_utf8"
        )
}
fn settled_failure(reason: &str) -> String {
    match reason {
        code @ ("planner_budget_exhausted"
        | "model_plan_unavailable_no_trades"
        | "planner_provider_failed"
        | "planner_invalid_plan_no_entries"
        | "planner_configuration_failed"
        | "planner_cancelled_no_replay"
        | "planner_event_deadline_expired"
        | "planner_event_configuration_changed"
        | "planner_event_fill_history_changed"
        | "planner_data_failed_no_replay"
        | "planner_news_evidence_invalid"
        | "planner_worker_failed_no_replay"
        | "planner_failed_no_new_entries"
        | "planner_failed_no_replay"
        | "planner_completion_expired_or_changed"
        | "planner_completion_outside_session"
        | "planner_result_binding_invalid"
        | "session_closed_no_replay"
        | "operator_stopped_no_replay"
        | "attempt_interrupted_no_replay"
        | "prior_attempt_incomplete_no_replay") => code.to_owned(),
        _ => "planner_failed_no_replay".to_owned(),
    }
}
fn check(state: &Control, c: &Config) -> Result<()> {
    if state.binding != book_binding(c)?
        || state.detector.binding() != state.binding
        || state.detector.session() != c.session.to_string()
    {
        return Err("planner_event_binding_changed".into());
    }
    Ok(())
}
/// Bootstrap only when the original event journal proves no provider could have run.
/// A pending, interrupted, ambiguous or successful attempt requires the original pin.
fn prepare_usage(db: &Connection, c: &Config) -> Result<()> {
    if !c.directory.join("model/usage-identity.json").exists() {
        let mut rows = db.prepare("SELECT state,result FROM planner_attempt_log")?;
        for row in rows.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })? {
            let (state, result) = row?;
            let result: Value = serde_json::from_str(result.as_deref().unwrap_or("null"))?;
            if state != "failed" || result["status"] != "model_plan_unavailable_no_trades" {
                return Err("planner_usage_identity_missing".into());
            }
        }
        if c.directory.join("model/attempt.json").exists()
            || c.directory.join("model/session-plan.json").exists()
            || c.directory.join("model/usage-reservations.json").exists()
        {
            return Err("planner_usage_identity_missing".into());
        }
    }
    planner::prepare_review_usage(c)
}

/// A pre-marker proof is safe only when the active plan is absent or is the
/// owner's own plan head. A legacy plan keeps the marker absent for migration.
fn active_plan_owner_or_absent(db: &Connection, state: &Control) -> Result<bool> {
    let Some(id) = &state.active else {
        return Ok(true);
    };
    let body: String = db.query_row("SELECT body FROM planner_plan WHERE id=?", [id], |r| {
        r.get(0)
    })?;
    let plan: Value = serde_json::from_str(&body)?;
    if plan["decision_id"] != *id {
        return Err("planner_saved_plan_invalid".into());
    }
    Ok(plan["decision_owner"] == "orchestrator" && id.starts_with("owner-"))
}
/// Called once under the book's controller lock. An abandoned claim is never replayed.
pub(crate) fn initialize(
    db: &mut Connection,
    c: &Config,
    p: &Portfolio,
    cfg: Option<&planner::PlannerConfig>,
    now: i64,
) -> Result<bool> {
    if closed_incident::closed_sealed(c, p) {
        return Ok(false);
    }
    // A closed book cannot trade or need planner protection. Skipping authority
    // initialization also keeps a config edit from halting it on restart.
    if p.closed {
        return Ok(false);
    }
    let enabled = cfg.and_then(settings).is_some();
    let previous = load(db)?;
    if previous.is_none() && !enabled {
        return Ok(false);
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(SCHEMA)?;
    if previous.is_none() {
        tx.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'event_planner_authority')",
            rusqlite::params![now, book_binding(c)?],
        )?;
    }
    let original_authority = previous.is_none();
    let mut state = match previous {
        Some(state) => state,
        None => {
            let cfg = cfg.ok_or("planner_event_configuration_required")?;
            let mut active = None;
            let legacy = c.directory.join("model/session-plan.json");
            if legacy.exists() && !cfg.orchestrator_owns_decisions() {
                let plan = planner::read_legacy_plan(c)?.ok_or("planner_saved_plan_invalid")?;
                planner::validate_saved(c, &plan)?;
                let id = plan["decision_id"]
                    .as_str()
                    .ok_or("planner_saved_plan_invalid")?;
                tx.execute(
                    "INSERT INTO planner_plan VALUES(?,?)",
                    rusqlite::params![id, serde_json::to_string(&plan)?],
                )?;
                active = Some(id.to_owned());
            }
            let attempted = c.directory.join("model/attempt.json").exists()
                || planner::usage(c)?["decide"].as_u64().unwrap_or(0) > 0
                || planner::usage(c)?["digest"].as_u64().unwrap_or(0) > 0;
            let (config_hash, prompt_sha256, config_without_prompt_sha256) = config_identity(cfg)?;
            Control {
                binding: book_binding(c)?,
                config_hash,
                prompt_sha256,
                config_without_prompt_sha256,
                detector: EventState::new(book_binding(c)?, c.session.to_string())?,
                fill_count: p.fills.len(),
                fill_hash: digest(&serde_json::to_vec(&p.fills)?),
                last_started_ms: None,
                pending: None,
                blocked: if attempted && active.is_none() {
                    Some("prior_attempt_incomplete_no_replay".into())
                } else {
                    None
                },
                initial_available: !attempted && active.is_none(),
                digest_failed: None,
                active,
                last_cadence_facts_sha256: None,
                market_observations: 0,
                missing_volume: 0,
                missing_prior_close: 0,
            }
        }
    };
    check(&state, c)?;
    let mut owner_receipt = if original_authority {
        if authority_owner(&tx, &state)?.is_some() {
            return Err("planner_authority_owner_without_authority".into());
        }
        Some(AuthorityOwnerReceipt {
            version: 1,
            binding: state.binding.clone(),
            config_hash: state.config_hash.clone(),
            owner: AuthorityOwner::for_config(cfg.ok_or("planner_event_configuration_required")?),
        })
    } else {
        authority_owner(&tx, &state)?
    };
    let current_identity = cfg.map(config_identity).transpose()?;
    let mut upgrade: Option<(String, &'static str)> = None;
    if let (Some(cfg), Some((hash, prompts, without_prompt))) = (cfg, current_identity.as_ref()) {
        if state.config_hash != *hash
            && let Some((upgraded, source)) = upgrade_config_hash(c, cfg, &state.config_hash)?
        {
            upgrade = Some((state.config_hash.clone(), source));
            state.config_hash = upgraded;
            if let Some(receipt) = &mut owner_receipt {
                receipt.config_hash = state.config_hash.clone();
            }
        }
        if state.config_hash == *hash {
            state.prompt_sha256 = prompts.clone();
            state.config_without_prompt_sha256 = without_prompt.clone();
        }
    }
    // A pre-marker owner journal proves the same owner from the exact current
    // policy hash. The active plan must be absent or demonstrably owner; a
    // legacy plan keeps the marker absent so startup names the migration.
    if !original_authority
        && owner_receipt.is_none()
        && let Some(cfg) = cfg
        && cfg.orchestrator_owns_decisions()
        && cfg.validate_decision_owner().is_ok()
        && state.config_hash == config_hash(cfg)?
        && active_plan_owner_or_absent(&tx, &state)?
    {
        owner_receipt = Some(AuthorityOwnerReceipt {
            version: 1,
            binding: state.binding.clone(),
            config_hash: state.config_hash.clone(),
            owner: AuthorityOwner::Orchestrator,
        });
        let (source, prior_config_hash, upgrade_source) = match &upgrade {
            Some((prior, source)) => (
                "pre_marker_hash_upgrade",
                Some(prior.clone()),
                Some(*source),
            ),
            None => ("pre_marker_exact", None, None),
        };
        tx.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_authority_proof_created')",
            rusqlite::params![
                now,
                json!({
                    "source":source,
                    "prior_config_hash":prior_config_hash,
                    "upgrade_source":upgrade_source,
                    "config_hash":state.config_hash.clone(),
                    "owner":"orchestrator",
                    "active":state.active.clone()
                })
                .to_string()
            ],
        )?;
    }
    if (!enabled
        || current_identity.as_ref().map(|(hash, _, _)| hash.as_str())
            != Some(state.config_hash.as_str()))
        && !state
            .blocked
            .as_deref()
            .is_some_and(preserves_through_startup)
    {
        state.blocked = Some("planner_event_configuration_changed".into());
    }
    if enabled && !p.stopped && !p.halted && !p.closed {
        prepare_usage(&tx, c)?;
    }
    if state.blocked.as_deref() == Some("planner_budget_exhausted")
        && let Some(cfg) = cfg
    {
        let digest_role = cfg.role(Trigger::Digest);
        let decide_role = cfg.role(Trigger::Decide);
        let usage = planner::usage(c)?;
        if (!decide_role.enabled
            || usage["decide"].as_u64().unwrap_or(0) < u64::from(decide_role.daily_call_budget))
            && (!digest_role.enabled
                || usage["digest"].as_u64().unwrap_or(0) < u64::from(digest_role.daily_call_budget))
        {
            state.blocked = None;
        }
    }
    if let Some(claim) = &state.pending {
        tx.execute(
            "UPDATE planner_attempt_log SET state=? WHERE id=? AND state=?",
            rusqlite::params![
                PlannerLogPhase::Interrupted.as_str(),
                &claim.id,
                PlannerLogPhase::Reserved.as_str()
            ],
        )?;
        tx.execute(
            "UPDATE planner_event_log SET state=? WHERE id=?",
            rusqlite::params![PlannerLogPhase::Interrupted.as_str(), &claim.event_id],
        )?;
        if !state
            .blocked
            .as_deref()
            .is_some_and(preserves_through_startup)
        {
            state.blocked = Some("attempt_interrupted_no_replay".into());
        }
    }
    if let Some(settings) = cfg.and_then(settings) {
        let stuck = state.blocked.as_deref() == Some("planner_event_queue_full");
        if stuck || state.blocked.is_none() {
            let pending = collapse_recheck_backlog(
                &tx,
                &state.binding,
                settings.max_pending as i64,
                now,
                stuck,
            )?;
            if stuck && pending <= settings.max_pending as i64 {
                state.blocked = None;
            }
        }
    }
    save(&tx, &state)?;
    if let Some(receipt) = owner_receipt {
        save_authority_owner(&tx, &receipt)?;
    }
    tx.commit()?;
    Ok(true)
}

/// A sealed financial book needs settled attempts, not current planner authority.
/// Hash exact stored bytes in primary-key order with length framing and a NULL tag.
/// Rows are consumed individually; no history-size cap or full-history allocation.
pub(crate) fn post_seal_incident_evidence(db: &Connection) -> Result<Value> {
    settled_attempt_evidence(db, "disposition_pending_attempt")
}

/// Same settled-attempt proof with a caller-specific pending-claim refusal code.
fn settled_attempt_evidence(db: &Connection, pending_code: &str) -> Result<Value> {
    use sha2::{Digest, Sha256};
    let state = load(db)?.ok_or("disposition_planner_authority_missing")?;
    if state.pending.is_some() {
        return Err(pending_code.into());
    }
    let raw: String = db.query_row(
        "SELECT body FROM planner_event_control WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    let mut hash = Sha256::new();
    hash.update(b"post-seal-attempts-v1");
    let mut count = 0_u64;
    let mut bytes = 0_u64;
    let mut statement =
        db.prepare("SELECT id,event_id,body,state,result FROM planner_attempt_log ORDER BY id")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let state = row.get_ref(3)?.as_str()?;
        if !matches!(state, "planned" | "failed" | "superseded" | "deferred") {
            return Err(pending_code.into());
        }
        count = count
            .checked_add(1)
            .ok_or("disposition_attempt_history_invalid")?;
        for column in 0..5 {
            match row.get_ref(column)? {
                rusqlite::types::ValueRef::Null if column == 4 => hash.update([0]),
                rusqlite::types::ValueRef::Text(value) => {
                    hash.update([1]);
                    hash.update((value.len() as u64).to_be_bytes());
                    hash.update(value);
                    if column == 2 || column == 4 {
                        bytes = bytes
                            .checked_add(value.len() as u64)
                            .ok_or("disposition_attempt_history_invalid")?;
                    }
                }
                _ => return Err("disposition_attempt_history_invalid".into()),
            }
        }
    }
    hash.update(count.to_be_bytes());
    hash.update(bytes.to_be_bytes());
    Ok(json!({"version":"post-seal-planner-evidence-v1",
        "control_sha256":digest(raw.as_bytes()),
        "attempts_sha256":hash.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>(),
        "attempt_count":count,"attempt_body_result_bytes":bytes}))
}

/// Carry acceptance cannot retire a data-integrity or interrupted-attempt block.
/// A recorded operator stop may itself have blocked further planner admission.
pub(crate) fn stopped_carry_evidence(db: &Connection, c: &Config) -> Result<Value> {
    let state = load(db)?.ok_or("stopped_carry_planner_authority_missing")?;
    check(&state, c)?;
    if state
        .blocked
        .as_deref()
        .is_some_and(|reason| reason != "operator_stopped_no_replay")
    {
        return Err("stopped_carry_unresolved_planner_block".into());
    }
    post_seal_incident_evidence(db)
}

/// Protection-only carry cannot retire an integrity block or an interrupted
/// attempt. Only settled attempts and the ordinary close/stop no-replay blocks
/// are eligible; the halt itself is preserved by the successor.
pub(crate) fn halted_carry_evidence(db: &Connection, c: &Config) -> Result<Value> {
    let state = load(db)?.ok_or("halted_carry_planner_authority_missing")?;
    check(&state, c)?;
    if state.blocked.as_deref().is_some_and(|reason| {
        !matches!(
            reason,
            "operator_stopped_no_replay" | "session_closed_no_replay"
        )
    }) {
        return Err("halted_carry_unresolved_planner_block".into());
    }
    settled_attempt_evidence(db, "halted_carry_pending_attempt")
}

/// Read-only admission proof: no in-flight claim and no reserved/invalid attempt
/// row. Unknown or unreadable authority defers (error), never activates.
pub(crate) fn pending_attempt(db: &Connection) -> Result<bool> {
    let table: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='planner_attempt_log')",
        [],
        |r| r.get(0),
    )?;
    if table
        && db.query_row(
            "SELECT EXISTS(SELECT 1 FROM planner_attempt_log WHERE state NOT IN ('planned','failed','superseded','deferred'))",
            [],
            |r| r.get::<_, bool>(0),
        )?
    {
        return Ok(true);
    }
    Ok(load(db)?.is_some_and(|state| state.pending.is_some()))
}

/// A pristine legacy flag can change only with settled, unchanged authority.
/// Never clear a block, replay an attempt or change the configuration hash here.
pub(crate) fn check_model_led_migration(db: &Connection, c: &Config) -> Result<()> {
    if let Some(state) = load(db)? {
        check(&state, c)?;
        if state.pending.is_some()
            || state.active.is_some()
            || state
                .blocked
                .as_deref()
                .is_some_and(|reason| !recoverable_block(reason))
        {
            return Err("resume_model_led_planner_authority_not_idle".into());
        }
    }
    Ok(())
}

/// A reviewed planner block shared by every recovery mode. An interrupted
/// claim is resolved by exact retirement; market and configuration blocks need
/// the caller's shared input proof before a flat recovery clears them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReviewedBlock {
    Data,
    ReadTransient,
    News,
    ConfigurationChanged,
    ConfigurationFailed,
    AttemptInterrupted,
}
impl ReviewedBlock {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Data => DATA_BLOCK,
            Self::ReadTransient => READ_TRANSIENT_BLOCK,
            Self::News => "planner_news_evidence_invalid",
            Self::ConfigurationChanged => "planner_event_configuration_changed",
            Self::ConfigurationFailed => "planner_configuration_failed",
            Self::AttemptInterrupted => "attempt_interrupted_no_replay",
        }
    }
    fn from_code(code: &str) -> Option<Self> {
        match code {
            DATA_BLOCK => Some(Self::Data),
            READ_TRANSIENT_BLOCK => Some(Self::ReadTransient),
            "planner_news_evidence_invalid" => Some(Self::News),
            "planner_event_configuration_changed" => Some(Self::ConfigurationChanged),
            "planner_configuration_failed" => Some(Self::ConfigurationFailed),
            "attempt_interrupted_no_replay" => Some(Self::AttemptInterrupted),
            _ => None,
        }
    }
    pub(crate) fn requires_input_revalidation(self) -> bool {
        matches!(self, Self::Data | Self::ReadTransient | Self::News)
    }
    /// Protection-only and closed recovery keep the halt/seal and cannot prove
    /// market-data input; they retain these blocks instead of clearing them.
    fn retained_by_protection(self) -> bool {
        matches!(self, Self::Data | Self::ReadTransient | Self::News)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryBlock {
    None,
    Recoverable,
    Lifecycle,
    Reviewed(ReviewedBlock),
}

/// How a recovery command resolves the reviewed block. `Reviewed` requires the
/// caller's shared input proof; `RetainIntegrity` keeps market-data blocks
/// because the halt or seal means no new decision can execute.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryClearance {
    Reviewed,
    RetainIntegrity,
}

fn recovery_block_for(db: &Connection, state: &Control) -> Result<RecoveryBlock> {
    // A reserved claim is interrupted by the controller lock the recovery
    // command holds; retirement still requires the exact stored claim body.
    let pending_id = if let Some(claim) = &state.pending {
        let attempt_state: Option<String> = db
            .query_row(
                "SELECT state FROM planner_attempt_log WHERE id=?",
                [&claim.id],
                |r| r.get(0),
            )
            .optional()?;
        if !matches!(attempt_state.as_deref(), Some("reserved" | "interrupted")) {
            return Err("planner_recover_pending_attempt".into());
        }
        Some(claim.id.clone())
    } else {
        None
    };
    let unsettled: i64 = if let Some(id) = &pending_id {
        db.query_row(
            "SELECT count(*) FROM planner_attempt_log WHERE state NOT IN ('planned','failed','superseded','deferred') AND id<>?",
            [id],
            |r| r.get(0),
        )?
    } else {
        db.query_row(
            "SELECT count(*) FROM planner_attempt_log WHERE state NOT IN ('planned','failed','superseded','deferred')",
            [],
            |r| r.get(0),
        )?
    };
    if unsettled != 0 {
        return Err("planner_recover_unsettled_attempt_history".into());
    }
    match state.blocked.as_deref() {
        None => Ok(RecoveryBlock::None),
        Some(reason) if recoverable_block(reason) => Ok(RecoveryBlock::Recoverable),
        Some("operator_stopped_no_replay" | "session_closed_no_replay") => {
            Ok(RecoveryBlock::Lifecycle)
        }
        Some(reason) => match ReviewedBlock::from_code(reason) {
            Some(ReviewedBlock::AttemptInterrupted) if pending_id.is_none() => {
                Err("planner_recover_stopped_block_not_eligible".into())
            }
            Some(block) => Ok(RecoveryBlock::Reviewed(block)),
            None => Err("planner_recover_stopped_block_not_eligible".into()),
        },
    }
}

pub(crate) fn recovery_block(db: &Connection, c: &Config) -> Result<RecoveryBlock> {
    let state = load(db)?.ok_or("planner_recover_authority_missing")?;
    check(&state, c)?;
    recovery_block_for(db, &state)
}

/// Only these stopped-book blocks can be reviewed after repairing their inputs.
pub(crate) fn reviewed_stopped_block(db: &Connection, c: &Config) -> Result<ReviewedBlock> {
    match recovery_block(db, c)? {
        RecoveryBlock::Reviewed(block) => Ok(block),
        _ => Err("planner_recover_stopped_block_not_eligible".into()),
    }
}
/// An attempt left pending by a stopped controller is retired, never replayed; charges stay.
fn retire_interrupted(db: &Connection, c: &Config, claim: Claim) -> Result<()> {
    planner::require_usage(c)?;
    let body: String = db.query_row(
        "SELECT body FROM planner_attempt_log WHERE id=? AND state IN ('reserved','interrupted')",
        [&claim.id],
        |r| r.get(0),
    )?;
    if serde_json::from_str::<Claim>(&body)? != claim {
        return Err("planner_recover_claim_binding_mismatch".into());
    }
    let event_state: Option<String> = db
        .query_row(
            "SELECT state FROM planner_event_log WHERE id=?",
            [&claim.event_id],
            |r| r.get(0),
        )
        .optional()?;
    if !matches!(
        event_state.as_deref(),
        Some("pending" | "reserved" | "interrupted")
    ) {
        return Err("planner_recover_event_binding_mismatch".into());
    }
    db.execute("UPDATE planner_attempt_log SET state='failed',result=? WHERE id=?", rusqlite::params![json!({"status":"operator_review_interrupted_no_replay","original_claim":claim,"charges_preserved":true}).to_string(),claim.id])?;
    db.execute(
        "UPDATE planner_event_log SET state='failed' WHERE id=?",
        [&claim.event_id],
    )?;
    db.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_pending_planner_attempt_retained')", rusqlite::params![millis(),json!({"attempt_id":claim.id,"no_replay":true,"charges_preserved":true}).to_string()])?;
    Ok(())
}
/// Called inside the operator recovery transaction; history and charges remain intact.
pub(crate) fn recover(
    db: &Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    clearance: RecoveryClearance,
) -> Result<()> {
    recover_for(db, c, cfg, cfg!(test), clearance)
}
/// Production recovery never infers a legacy Decide path. Unit mechanics keep
/// the legacy branch available only through an explicit test-only flag.
fn recover_for(
    db: &Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    allow_legacy_decide: bool,
    clearance: RecoveryClearance,
) -> Result<()> {
    let mut state = load(db)?.ok_or("planner_event_state_missing")?;
    check(&state, c)?;
    let reviewed = recovery_block_for(db, &state)?;
    let next_owner = AuthorityOwner::for_config(cfg);
    if next_owner == AuthorityOwner::Orchestrator {
        cfg.validate_decision_owner()?;
    }
    if settings_for(cfg, allow_legacy_decide).is_none() {
        return Err("planner_recover_events_disabled".into());
    }
    let mut owner_receipt = authority_owner(db, &state)?;
    if let Some(receipt) = &owner_receipt {
        if receipt.owner != next_owner {
            return Err("decision_owner_migration_required".into());
        }
    } else if next_owner == AuthorityOwner::Orchestrator {
        if state.config_hash != config_hash(cfg)? {
            return Err("planner_recover_owner_unproven_restore_original_config".into());
        }
        // A pre-marker proof cannot be inferred by clearing a legacy plan head.
        if !active_plan_owner_or_absent(db, &state)? {
            return Err("planner_recover_owner_active_plan_unproven".into());
        }
        owner_receipt = Some(AuthorityOwnerReceipt {
            version: 1,
            binding: state.binding.clone(),
            config_hash: state.config_hash.clone(),
            owner: next_owner,
        });
    }
    if let Some(claim) = state.pending.take() {
        retire_interrupted(db, c, claim)?;
    }
    let (current_hash, current_prompts, current_without_prompt) = config_identity(cfg)?;
    db.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_authority_rebound')",
        rusqlite::params![millis(), serde_json::to_string(&json!({"prior_config_hash":state.config_hash,"current_config_hash":current_hash,"prior_prompt_sha256":state.prompt_sha256,"current_prompt_sha256":current_prompts,"prior_block":state.blocked,"prior_active":state.active}))?])?;
    state.config_hash = current_hash;
    state.prompt_sha256 = current_prompts;
    state.config_without_prompt_sha256 = current_without_prompt;
    state.blocked = match clearance {
        RecoveryClearance::Reviewed => None,
        RecoveryClearance::RetainIntegrity => match reviewed {
            RecoveryBlock::Reviewed(block) if block.retained_by_protection() => {
                Some(block.code().to_owned())
            }
            _ => None,
        },
    };
    state.active = None;
    state.initial_available = true;
    save(db, &state)?;
    if let Some(mut receipt) = owner_receipt {
        receipt.config_hash = state.config_hash.clone();
        save_authority_owner(db, &receipt)?;
    }
    Ok(())
}
/// Explicit operator application. Never clears an existing block or refunds a call.
pub(crate) fn fail_pending(db: &mut Connection, c: &Config, reason: &str) -> Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut state = load(&tx)?.ok_or("planner_event_state_missing")?;
    check(&state, c)?;
    if let Some(claim) = state.pending.take() {
        tx.execute("UPDATE planner_attempt_log SET state='failed',result=? WHERE id=? AND state='reserved'", rusqlite::params![json!({"status":reason,"no_trade":true}).to_string(),claim.id])?;
        tx.execute(
            "UPDATE planner_event_log SET state='failed' WHERE id=?",
            [&claim.event_id],
        )?;
    }
    state.active = None;
    state.blocked = None;
    state.digest_failed = None;
    save(&tx, &state)?;
    tx.commit()?;
    Ok(())
}
pub(crate) fn apply_config(
    db: &Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
) -> Result<()> {
    let Some(mut state) = load(db)? else {
        return Ok(());
    };
    check(&state, c)?;
    authority_owner(db, &state)?;
    // config-apply holds the controller lock, so a pending claim was interrupted.
    if let Some(claim) = state.pending.take() {
        retire_interrupted(db, c, claim)?;
    }
    // The stop artifact only records that an in-flight attempt was abandoned; with the
    // attempt retired above, the explicit operator apply clears it. Other blocks stay.
    if state.blocked.as_deref() == Some("operator_stopped_no_replay") {
        db.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_stop_block_cleared')", rusqlite::params![millis(), json!({"prior_block":state.blocked,"charges_preserved":true}).to_string()])?;
        state.blocked = None;
    }
    // A detected config/prompt edit is exactly the transition this command
    // applies; clear only that block and leave every other integrity block.
    if state.blocked.as_deref() == Some("planner_event_configuration_changed") {
        db.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_config_block_cleared')", rusqlite::params![millis(), json!({"prior_block":state.blocked,"charges_preserved":true}).to_string()])?;
        state.blocked = None;
    }
    if state.blocked.is_some() {
        return Err("config_apply_existing_block_requires_planner_recover".into());
    }
    prepare_usage(db, c)?;
    let (next, next_prompts, next_without_prompt) = config_identity(cfg)?;
    db.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_config_applied')",
        rusqlite::params![millis(), serde_json::to_string(&json!({"prior_config_hash":state.config_hash,"current_config_hash":next,"prior_prompt_sha256":state.prompt_sha256,"current_prompt_sha256":next_prompts,"configuration_sha256":digest(&serde_json::to_vec(cfg)?),"prior_active":state.active,"usage_preserved":true}))?])?;
    if state.config_hash != next {
        state.active = None;
        state.initial_available = true;
    }
    state.config_hash = next;
    state.prompt_sha256 = next_prompts;
    state.config_without_prompt_sha256 = next_without_prompt;
    save(db, &state)?;
    save_authority_owner(
        db,
        &AuthorityOwnerReceipt {
            version: 1,
            binding: state.binding.clone(),
            config_hash: state.config_hash.clone(),
            owner: AuthorityOwner::for_config(cfg),
        },
    )
}
/// Controller-startup admission for an orchestrator-owned policy. The policy
/// shape is checked at load; this checks the durable authority belongs to that
/// exact policy. A legacy authority requires the reviewed config-apply
/// transition, never an implicit rewrite of its config hash or active plan.
pub(crate) fn owner_startup_check(
    db: &Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
) -> Result<()> {
    if !cfg.orchestrator_owns_decisions() {
        return Ok(());
    }
    cfg.validate_decision_owner()?;
    let Some(state) = load(db)? else {
        return Ok(());
    };
    check(&state, c)?;
    if state.config_hash != config_hash(cfg)?
        || state.blocked.as_deref() == Some("planner_event_configuration_changed")
    {
        return Err("decision_owner_migration_required".into());
    }
    if let Some(id) = &state.active {
        let body: String = db.query_row("SELECT body FROM planner_plan WHERE id=?", [id], |r| {
            r.get(0)
        })?;
        let plan: Value = serde_json::from_str(&body)?;
        if plan["decision_owner"] != "orchestrator" {
            return Err("decision_owner_legacy_plan_requires_migration".into());
        }
    }
    match authority_owner(db, &state)? {
        Some(receipt) if receipt.owner == AuthorityOwner::Orchestrator => Ok(()),
        Some(_) => Err("decision_owner_migration_required".into()),
        None => Err("planner_authority_owner_missing".into()),
    }
}
/// None means the journal suspended/superseded the old plan; never fall back to a file.
pub(crate) fn active_plan(db: &Connection, c: &Config) -> Result<Option<Option<Value>>> {
    let Some(state) = load(db)? else {
        return Ok(None);
    };
    check(&state, c)?;
    if state.blocked.is_some() {
        return Ok(Some(None));
    }
    let Some(id) = state.active else {
        return Ok(Some(None));
    };
    let body: String = db.query_row("SELECT body FROM planner_plan WHERE id=?", [&id], |r| {
        r.get(0)
    })?;
    if body.len() > 8_000_000 {
        return Err("planner_saved_plan_invalid".into());
    }
    let plan: Value = serde_json::from_str(&body)?;
    if plan["decision_id"] != id {
        return Err("planner_saved_plan_invalid".into());
    }
    planner::validate_saved(c, &plan)?;
    // A pending factual digest must not suspend a committed owner plan: only
    // activate_owner_plan's own decision id marks an executable owner plan.
    let owned = plan["decision_owner"] == "orchestrator"
        && plan["decision_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("owner-"));
    if state.pending.is_some() && !owned {
        return Ok(Some(None));
    }
    Ok(Some(Some(plan)))
}
/// The persistent decision owner may accept work only against the original
/// event authority and the exact planner policy that initialized it.
pub(crate) fn owner_ready(
    db: &Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
) -> Result<bool> {
    if !cfg.orchestrator_owns_decisions() || settings(cfg).is_none() {
        return Ok(false);
    }
    let Some(state) = load(db)? else {
        return Ok(false);
    };
    check(&state, c)?;
    if !authority_owner(db, &state)?
        .is_some_and(|receipt| receipt.owner == AuthorityOwner::Orchestrator)
    {
        return Ok(false);
    }
    // Consistent with `activate_owner_plan`: a recoverable digest failure is
    // recorded on its attempt and in `digest_failed`, never as a refusal, and
    // the transient read block suspends execution without refusing a decision.
    Ok(state.config_hash == config_hash(cfg)?
        && !state.blocked.as_deref().is_some_and(refuses_owner_decision))
}

/// Activate a supervisor-owned plan inside the caller's transaction. This
/// writes only the durable plan head; admission/usage and execution stay in
/// their existing deterministic paths.
pub(crate) fn activate_owner_plan(
    db: &Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    plan: &Value,
    now: i64,
) -> Result<()> {
    if !cfg.orchestrator_owns_decisions() || settings(cfg).is_none() {
        return Err("planner_decision_owner_policy_required".into());
    }
    let mut state = load(db)?.ok_or("planner_event_authority_missing")?;
    check(&state, c)?;
    match authority_owner(db, &state)? {
        Some(receipt) if receipt.owner == AuthorityOwner::Orchestrator => {}
        Some(_) => return Err("decision_owner_migration_required".into()),
        None => return Err("planner_authority_owner_missing".into()),
    }
    if state.config_hash != config_hash(cfg)? {
        return Err("planner_event_configuration_changed".into());
    }
    // Factual digest work may overlap a decision. A transient read block still
    // prevents execution; completing the owned receipt must not erase it. A
    // recoverable digest failure only invalidates that one digest, so an owner
    // decision built from an earlier valid digest still commits (#221).
    if state.blocked.as_deref().is_some_and(refuses_owner_decision) {
        return Err("planner_decision_owner_not_ready".into());
    }
    if p.stopped || p.halted || p.closed || !super::decision_owner::in_window(c, cfg, now) {
        return Err("planner_completion_outside_session".into());
    }
    planner::validate_saved(c, plan)?;
    if plan["portfolio_revision"] != portfolio_hash(p)?
        || plan["policy_sha256"] != digest(&planner::policy_bytes(cfg)?)
        || plan["profile_sha256"]
            != cfg
                .profile_sha256()
                .map(str::to_owned)
                .map_or(Value::Null, Value::String)
    {
        return Err("planner_saved_binding_invalid".into());
    }
    let id = plan["decision_id"]
        .as_str()
        .ok_or("planner_saved_plan_invalid")?;
    if let Some(prior) = state.active.as_ref() {
        super::decision_owner::supersede_plan_actions(db, c, prior)?;
    }
    db.execute(
        "INSERT INTO planner_plan VALUES(?,?)",
        rusqlite::params![id, serde_json::to_string(plan)?],
    )?;
    state.active = Some(id.to_owned());
    save(db, &state)
}

fn owner_digest_completion(plan: &Value) -> bool {
    plan["owner_digest"]["version"] == 1
        && !plan["owner_digest"]["digest"].is_null()
        && plan["owner_digest"]["tool_results"].is_array()
        && plan["owner_digest"]["eligible"].is_boolean()
        && plan["owner_digest"]["facts_sha256"]
            .as_str()
            .is_some_and(|sha| sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        && plan["luna_handoff_requested"] == false
}
fn append(db: &Connection, event: &Event) -> Result<()> {
    let body = serde_json::to_string(event)?;
    if let Some(existing) = db
        .query_row(
            "SELECT body FROM planner_event_log WHERE id=?",
            [&event.id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        if existing != body {
            return Err("planner_event_identity_changed".into());
        }
        return Ok(());
    }
    db.execute(
        "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
        rusqlite::params![event.id, event.observed_ms, body],
    )?;
    Ok(())
}

mod authority;
mod events;
mod lifecycle;
pub(crate) use authority::repair_authority;
pub(crate) use events::*;
pub(crate) use lifecycle::*;

#[cfg(test)]
mod tests;
