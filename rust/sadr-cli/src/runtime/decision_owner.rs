//! Durable digest-to-orchestrator work. Models never write this ledger or fill.
//! The event journal owns original digest evidence; this ledger pins its exact
//! bytes and atomically joins a persistent turn to the existing paper plan head.
use super::*;
use rusqlite::OptionalExtension;
use sadr_core::execution::{Instrument, Portfolio};
use sadr_core::finance::{Amount, Price};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS owner_digest_work(
 id TEXT PRIMARY KEY,binding TEXT NOT NULL,digest_id TEXT NOT NULL,
 descriptor TEXT NOT NULL,state TEXT NOT NULL,turn_id TEXT UNIQUE,
 claim TEXT,result TEXT);";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Claim {
    pub version: u32,
    pub id: String,
    pub binding: String,
    pub digest_id: String,
    pub base_sha256: String,
    pub portfolio_revision: String,
    pub policy_sha256: String,
    pub profile_sha256: Option<String>,
    pub deadline_ms: i64,
    pub turn_id: String,
    pub model_id: String,
    pub started_ms: i64,
    /// This digest was scheduled by cadence rather than a material event.
    #[serde(default)]
    pub cadence: bool,
    /// Host signature of the facts this digest delivered. Used to coalesce a
    /// repeated cadence digest without inspecting model prose.
    #[serde(default)]
    pub facts_sha256: String,
    /// Exact terminal research receipts carried by the source event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub research_tasks: Vec<Value>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub research_tasks_truncated: bool,
}

pub(crate) struct Work {
    pub claim: Claim,
    pub facts: Value,
    pub bundle: Value,
}

pub(crate) struct Context<'a> {
    pub db: &'a mut Connection,
    pub portfolio: &'a Portfolio,
    pub instruments: &'a [Instrument],
}

fn exists(db: &Connection) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='owner_digest_work')", [], |r| r.get(0))?)
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn research_task_refs(source: &redecision::Claim) -> Result<(Vec<Value>, bool)> {
    let mut refs = Vec::new();
    let direct = match source.trigger.get("research_task") {
        Some(value) => Some(
            value
                .as_object()
                .ok_or("owner_research_task_receipt_invalid")?,
        ),
        None => None,
    };
    let collapsed_value = source.trigger.get("collapsed_research_tasks");
    let collapsed: &[Value] = match collapsed_value {
        Some(value) => value
            .as_array()
            .ok_or("owner_research_task_receipt_invalid")?,
        None => &[],
    };
    let mut truncated = match source.trigger.get("collapsed_research_tasks_truncated") {
        Some(value) => value
            .as_bool()
            .ok_or("owner_research_task_receipt_invalid")?,
        None => false,
    };
    let mut entries = direct
        .into_iter()
        .map(|task| {
            let observed_ms = source.trigger["observed_ms"]
                .as_i64()
                .ok_or("owner_research_task_receipt_invalid")?;
            if source.event_id.is_empty() || observed_ms < 0 {
                return Err("owner_research_task_receipt_invalid");
            }
            Ok((task, source.event_id.as_str(), observed_ms))
        })
        .collect::<std::result::Result<Vec<_>, &'static str>>()?;
    for task in collapsed {
        let task = task
            .as_object()
            .ok_or("owner_research_task_receipt_invalid")?;
        let event_id = task
            .get("source_event_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or("owner_research_task_receipt_invalid")?;
        let observed_ms = task
            .get("source_observed_ms")
            .and_then(Value::as_i64)
            .filter(|at| *at >= 0)
            .ok_or("owner_research_task_receipt_invalid")?;
        entries.push((task, event_id, observed_ms));
    }
    if entries.len() > 8 {
        truncated = true;
    }
    for (task, event_id, observed_ms) in entries.into_iter().take(8) {
        let task_id = task
            .get("task_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let revision = task
            .get("revision")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let status = task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let kind = match task.get("kind") {
            Some(value) => value
                .as_str()
                .filter(|kind| matches!(*kind, "registered" | "candidate"))
                .ok_or("owner_research_task_receipt_invalid")?,
            // Legacy events predate the explicit capability field.
            None => "registered",
        };
        let result_hash = task.get("result_sha256").cloned().unwrap_or(Value::Null);
        let valid_hash = result_hash.is_null()
            || result_hash.as_str().is_some_and(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            });
        if task_id.len() != 64
            || !task_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || revision == 0
            || revision > u64::from(u32::MAX)
            || !matches!(status, "succeeded" | "terminated" | "cancelled" | "refused")
            || (status == "succeeded") == result_hash.is_null()
            || !valid_hash
            || event_id.is_empty()
            || observed_ms < 0
        {
            return Err("owner_research_task_receipt_invalid".into());
        }
        refs.push(json!({"task_id":task_id,"revision":revision,"kind":kind,"status":status,
            "result_sha256":result_hash,"source_event_id":event_id,"source_observed_ms":observed_ms}));
    }
    Ok((refs, truncated))
}

/// Research dispatch yields to any durable owner work for the current book.
/// A session turn without a result is included so a controller restart cannot
/// launch a research worker while the model decision is unresolved.
pub(crate) fn research_priority_blocked(db: &Connection, c: &Config) -> Result<bool> {
    if exists(db)? {
        let pending: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_digest_work WHERE binding=? AND state IN ('pending','running'))",
            [book_binding(c)?],
            |row| row.get(0),
        )?;
        if pending {
            return Ok(true);
        }
    }
    Ok(
        super::session::SessionLog::new(&c.directory, &c.session.to_string())
            .pending()
            .is_some_and(|turn| turn.trigger == "decision"),
    )
}

pub(crate) fn is_cadence_trigger(trigger: &Value) -> bool {
    trigger["reason"] == "digest_cadence" || trigger["cause"] == "digest_cadence"
}

/// Called inside event settlement's transaction. No independent write or model
/// charge can leave a digest admitted without its durable decision wake.
pub(crate) fn enqueue_digest(
    db: &Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    source: &redecision::Claim,
    plan: &Value,
    now: i64,
    previous_cadence_facts: Option<&str>,
) -> Result<bool> {
    planner::validate_saved(c, plan)?;
    if !cfg.orchestrator_owns_decisions()
        || plan["owner_digest"]["version"] != 1
        || !plan["owner_digest"]["digest"].is_object()
        || !plan["owner_digest"]["tool_results"].is_array()
        || plan["luna_handoff_requested"] != false
        || plan["attempt_id"] != source.id
        || plan["portfolio_revision"] != source.portfolio_hash
        || source.binding != book_binding(c)?
        || now < source.started_ms
        || now > source.deadline_ms
    {
        return Err("owner_digest_binding_invalid".into());
    }
    let eligible = plan["owner_digest"]["eligible"]
        .as_bool()
        .ok_or("owner_digest_binding_invalid")?;
    if !eligible {
        // An empty or unusable feed never supersedes work that can still use
        // the previous facts.
        return Ok(false);
    }
    let facts_sha256 = plan["owner_digest"]["facts_sha256"]
        .as_str()
        .ok_or("owner_digest_binding_invalid")?;
    let cadence = is_cadence_trigger(&source.trigger);
    if cadence {
        let pending: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_digest_work WHERE binding=? AND state='pending')",
            [&source.binding],
            |r| r.get(0),
        )?;
        if pending || previous_cadence_facts == Some(facts_sha256) {
            // Material work owns the next turn; identical cadence prose would
            // otherwise wake the orchestrator for no new fact.
            return Ok(false);
        }
    }
    let (research_tasks, research_tasks_truncated) = research_task_refs(source)?;
    let claim = Claim {
        version: 1,
        id: source.id.clone(),
        binding: source.binding.clone(),
        digest_id: plan["digest_id"]
            .as_str()
            .ok_or("owner_digest_binding_invalid")?
            .into(),
        base_sha256: digest(serde_json::to_string(plan)?.as_bytes()),
        portfolio_revision: source.portfolio_hash.clone(),
        policy_sha256: digest(&planner::policy_bytes(cfg)?),
        profile_sha256: cfg.profile_sha256().map(str::to_owned),
        deadline_ms: source.deadline_ms,
        turn_id: String::new(),
        model_id: String::new(),
        started_ms: 0,
        cadence,
        facts_sha256: facts_sha256.to_owned(),
        research_tasks,
        research_tasks_truncated,
    };
    if plan["policy_sha256"] != claim.policy_sha256
        || plan["profile_sha256"] != json!(claim.profile_sha256)
    {
        return Err("owner_digest_policy_invalid".into());
    }
    db.execute_batch(SCHEMA)?;
    // Only obsolete work that has not started is coalesced. A slow in-flight
    // decision is revalidated financially, not starved by every newer digest.
    if !cadence {
        db.execute("UPDATE owner_digest_work SET state='superseded',result=? WHERE binding=? AND state='pending'",
            rusqlite::params![json!({"version":1,"status":"superseded","by_digest":claim.digest_id}).to_string(),claim.binding])?;
    }
    db.execute(
        "INSERT INTO owner_digest_work VALUES(?,?,?,?,'pending',NULL,NULL,NULL)",
        rusqlite::params![
            claim.id,
            claim.binding,
            claim.digest_id,
            serde_json::to_string(&claim)?
        ],
    )?;
    Ok(true)
}

pub(crate) fn selected_version(base: &Value) -> Value {
    if let Some(selection) = base["selection_provenance"]
        .as_object()
        .filter(|selection| {
            selection
                .get("source_hash")
                .and_then(Value::as_str)
                .is_some_and(|hash| hash.len() == 64)
        })
    {
        return json!({
            "source_hash": selection["source_hash"],
            "selection_reason": selection.get("selection_reason").unwrap_or(&Value::Null),
            "live_ranking": selection.get("live_ranking").unwrap_or(&Value::Null),
            "attribution_only": true,
            "activation_authority": false,
        });
    }
    if let Some(attribution) = base["strategy_attribution"].as_object() {
        let hash = attribution["spec_hash"]
            .as_str()
            .filter(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
        if hash.is_some() {
            return json!({
                "source_hash": hash,
                "selection_reason": attribution.get("selection_reason").unwrap_or(&Value::Null),
                "live_ranking": attribution.get("live_ranking").unwrap_or(&Value::Null),
                "attribution_only": true,
                "activation_authority": false,
            });
        }
    }
    Value::Null
}

fn base_plan(db: &Connection, c: &Config, claim: &Claim) -> Result<Value> {
    if claim.version != 1 || claim.binding != book_binding(c)? {
        return Err("owner_claim_binding_invalid".into());
    }
    let raw: String = db.query_row(
        "SELECT result FROM planner_attempt_log WHERE id=? AND state='deferred'",
        [&claim.id],
        |r| r.get(0),
    )?;
    if raw.len() > planner::SAVED_PLAN_MAX_BYTES || digest(raw.as_bytes()) != claim.base_sha256 {
        return Err("owner_digest_evidence_changed".into());
    }
    let plan = sadr_core::native::decode_bounded(raw.as_bytes(), planner::SAVED_PLAN_MAX_BYTES)
        .map_err(|_| "owner_digest_evidence_invalid")?;
    planner::validate_saved(c, &plan)?;
    if plan["digest_id"] != claim.digest_id
        || plan["portfolio_revision"] != claim.portfolio_revision
        || plan["owner_digest"]["version"] != 1
        || plan["owner_digest"]["eligible"] != true
        || plan["owner_digest"]["facts_sha256"]
            .as_str()
            .is_none_or(|sha| sha.len() != 64)
        || plan["owner_digest"]["facts_sha256"] != claim.facts_sha256
        || plan["attempt_id"] != claim.id
    {
        return Err("owner_digest_evidence_invalid".into());
    }
    Ok(plan)
}

pub(crate) fn in_window(c: &Config, cfg: &planner::PlannerConfig, now: i64) -> bool {
    let start = cfg
        .events
        .as_ref()
        .and_then(|s| s.decision_start_minute)
        .map(u32::from)
        .unwrap_or(c.execution.session_start_minute)
        .max(c.execution.session_start_minute);
    local(now).date_naive() == c.session
        && (start..c.execution.session_end_minute).contains(&minute(now))
}

fn current(
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    claim: &Claim,
    now: i64,
) -> Result<()> {
    if !cfg.orchestrator_owns_decisions() || claim.binding != book_binding(c)? || claim.version != 1
    {
        return Err("owner_claim_binding_invalid".into());
    }
    if p.stopped || p.halted || p.closed || !p.model_led || !in_window(c, cfg, now) {
        return Err("owner_decision_outside_session".into());
    }
    if now > claim.deadline_ms || (claim.started_ms != 0 && now < claim.started_ms) {
        return Err("owner_decision_expired".into());
    }
    if redecision::portfolio_hash(p)? != claim.portfolio_revision {
        return Err("owner_portfolio_revision_changed".into());
    }
    if digest(&planner::policy_bytes(cfg)?) != claim.policy_sha256
        || cfg.profile_sha256() != claim.profile_sha256.as_deref()
    {
        return Err("owner_policy_revision_changed".into());
    }
    Ok(())
}

pub(crate) fn queued(
    context: &mut Context<'_>,
    c: &Config,
    cfg: &planner::PlannerConfig,
    now: i64,
) -> Result<Option<Work>> {
    if !exists(context.db)? || !redecision::owner_ready(context.db, c, cfg)? {
        return Ok(None);
    }
    let running: bool = context.db.query_row(
        "SELECT EXISTS(SELECT 1 FROM owner_digest_work WHERE binding=? AND state='running')",
        [book_binding(c)?],
        |r| r.get(0),
    )?;
    if running {
        return Ok(None);
    }
    let raw: Option<String> = context.db.query_row("SELECT descriptor FROM owner_digest_work WHERE binding=? AND state='pending' ORDER BY rowid DESC LIMIT 1", [book_binding(c)?], |r| r.get(0)).optional()?;
    let Some(raw) = raw else { return Ok(None) };
    let claim: Claim = serde_json::from_str(&raw).map_err(|_| "owner_claim_invalid")?;
    if let Err(error) = current(c, cfg, context.portfolio, &claim, now) {
        // Refusal is durable and explicit; a silent None would hide work that
        // was dropped for a stale book, policy or session window.
        let reason = reason(error.as_ref());
        refuse(context.db, c, &claim, &reason)?;
        return Err(format!("owner_work_refused_{reason}").into());
    }
    let base = base_plan(context.db, c, &claim)?;
    // Luna's raw tool observations stay on the durable digest plan and are read
    // on demand with `get_luna_tool_results`; embedding them here made the owner
    // prompt grow with every digest tool call.
    let facts = json!({"digest":base["owner_digest"]["digest"],"digest_id":claim.digest_id,
        "facts_sha256":base["owner_digest"]["facts_sha256"],
        "digest_tool_result_count":base["owner_digest"]["tool_results"].as_array().map_or(0,Vec::len),
        "cash":base["input"]["cash"],
        "fees":base["input"]["fees"],"evidence_input_hash":base["input_hash"],
        "strategy_attribution":base["strategy_attribution"],
        "selection_provenance":selected_version(&base),
        "research_tasks":claim.research_tasks,
        "research_tasks_truncated":claim.research_tasks_truncated});
    Ok(Some(Work {
        claim,
        facts,
        bundle: base["input"].clone(),
    }))
}

pub(crate) fn reason(error: &(dyn std::error::Error + 'static)) -> String {
    let text = error.to_string();
    if text.len() <= 96
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && [
            "owner_",
            "planner_",
            "session_",
            "supervisor_",
            "model_",
            "harness_",
        ]
        .iter()
        .any(|prefix| text.starts_with(prefix))
    {
        text
    } else {
        "owner_operation_failed".into()
    }
}

fn decision_window_ms(cfg: &planner::PlannerConfig) -> Result<i64> {
    let events = cfg.events.as_ref().ok_or("owner_event_settings_missing")?;
    Ok(events
        .decision_timeout_ms
        .unwrap_or(events.policy.max_event_age_ms)
        .clamp(30_000, 900_000))
}

pub(crate) fn bind(
    context: &mut Context<'_>,
    c: &Config,
    cfg: &planner::PlannerConfig,
    claim: &mut Claim,
    now: i64,
) -> Result<()> {
    current(c, cfg, context.portfolio, claim, now)?;
    if claim.turn_id.is_empty() || claim.model_id.is_empty() || claim.started_ms != now {
        return Err("owner_turn_binding_invalid".into());
    }
    // The durable decision turn gets its own bounded lifetime. It must not
    // inherit the digest attempt's deadline: planning may start near that edge
    // and the model turn would otherwise commit an already-expired intent.
    claim.deadline_ms = now
        .checked_add(decision_window_ms(cfg)?)
        .ok_or("owner_decision_deadline_range")?;
    let tx = context
        .db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if !redecision::owner_ready(&tx, c, cfg)? {
        return Err("owner_authority_unavailable".into());
    }
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM owner_digest_work WHERE binding=? AND state='running'",
        [&claim.binding],
        |r| r.get(0),
    )?;
    if count != 0 {
        return Err("owner_decision_already_running".into());
    }
    base_plan(&tx, c, claim)?;
    let changed = tx.execute("UPDATE owner_digest_work SET state='running',turn_id=?,claim=? WHERE id=? AND binding=? AND state='pending'",
        rusqlite::params![claim.turn_id,serde_json::to_string(claim)?,claim.id,claim.binding])?;
    if changed != 1 {
        return Err("owner_digest_not_pending".into());
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn validate_running(
    context: &Context<'_>,
    c: &Config,
    cfg: &planner::PlannerConfig,
    claim: &Claim,
    now: i64,
) -> Result<()> {
    current(c, cfg, context.portfolio, claim, now)?;
    let raw: String = context.db.query_row("SELECT claim FROM owner_digest_work WHERE id=? AND binding=? AND turn_id=? AND state='running'",
        rusqlite::params![claim.id,claim.binding,claim.turn_id], |r|r.get(0))?;
    let saved: Claim = serde_json::from_str(&raw).map_err(|_| "owner_claim_invalid")?;
    if saved != *claim {
        return Err("owner_claim_changed".into());
    }
    base_plan(context.db, c, claim)?;
    Ok(())
}

pub(crate) fn refuse(db: &Connection, c: &Config, claim: &Claim, reason: &str) -> Result<()> {
    if claim.binding != book_binding(c)? {
        return Err("owner_claim_binding_invalid".into());
    }
    let safe = reason.len() <= 96
        && reason
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    let reason = if safe {
        reason
    } else {
        "owner_decision_refused"
    };
    let changed = db.execute("UPDATE owner_digest_work SET state='refused',result=? WHERE id=? AND binding=? AND state IN ('pending','running')",
        rusqlite::params![json!({"version":1,"status":"refused","reason":reason,"turn_id":claim.turn_id,"digest_id":claim.digest_id}).to_string(),claim.id,claim.binding])?;
    if changed == 0 {
        let state: Option<String> = db
            .query_row(
                "SELECT state FROM owner_digest_work WHERE id=? AND binding=?",
                rusqlite::params![claim.id, claim.binding],
                |r| r.get(0),
            )
            .optional()?;
        if !state
            .as_deref()
            .is_some_and(|v| matches!(v, "refused" | "superseded"))
        {
            return Err("owner_reconciliation_required".into());
        }
    }
    Ok(())
}

/// Both the plan head and its turn receipt commit in this transaction. A file
/// receipt is merely a projection; recovery reads this ledger before replay.
pub(crate) fn commit(
    context: &mut Context<'_>,
    c: &Config,
    cfg: &planner::PlannerConfig,
    claim: &Claim,
    raw: &str,
    now: i64,
) -> Result<Value> {
    validate_running(context, c, cfg, claim, now)?;
    if transcript::redact(&json!(raw)) != json!(raw) {
        return Err("owner_output_secret_refused".into());
    }
    let mut plan = base_plan(context.db, c, claim)?;
    let originals: Vec<Instrument> = serde_json::from_value(plan["input"]["allowlist"].clone())
        .map_err(|_| "owner_instrument_revision_invalid")?;
    if originals.iter().any(|instrument| {
        !context
            .instruments
            .iter()
            .any(|current| current == instrument)
    }) {
        return Err("owner_instrument_revision_changed".into());
    }
    let verdicts = planner::validate(raw, context.portfolio, &originals)?;
    let parsed = planner::plan_document(raw)?;
    plan.as_object_mut()
        .ok_or("owner_digest_evidence_invalid")?
        .remove("owner_digest");
    plan["decision_id"] = json!(format!("owner-{}", claim.turn_id));
    plan["decision_turn_id"] = json!(claim.turn_id);
    plan["decision_owner"] = json!("orchestrator");
    plan["digest_attempt_id"] = json!(claim.id);
    plan["attempt_id"] = Value::Null;
    plan["model_id"] = json!(claim.model_id);
    plan["luna_handoff_requested"] = Value::Null;
    plan["parsed_plan"] = parsed;
    plan["raw_output"] = json!(raw);
    plan["verdicts"] = json!(verdicts);
    plan["latency_ms"] = json!(now.saturating_sub(claim.started_ms));
    // Execution validity belongs to this decision turn, not the digest attempt
    // that woke it. A slow model turn must not produce a pre-expired intent.
    plan["valid_until_ms"] = json!(claim.deadline_ms);
    plan["digest_budget_used"] = plan["budget_used"].clone();
    plan["budget_used"] = Value::Null;
    plan["input"]["orchestrator_turn_id"] = json!(claim.turn_id);
    plan["input"]["digest_id"] = json!(claim.digest_id);
    plan["selection_provenance"] = selected_version(&plan);
    plan["input_hash"] = json!(digest(&serde_json::to_vec(&plan["input"])?));
    let tx = context
        .db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let saved: String = tx.query_row(
        "SELECT claim FROM owner_digest_work WHERE id=? AND turn_id=? AND state='running'",
        rusqlite::params![claim.id, claim.turn_id],
        |r| r.get(0),
    )?;
    if serde_json::from_str::<Claim>(&saved)? != *claim {
        return Err("owner_claim_changed".into());
    }
    redecision::activate_owner_plan(&tx, c, cfg, context.portfolio, &plan, now)?;
    let receipt = json!({"version":1,"status":"committed","decision_id":plan["decision_id"],"turn_id":claim.turn_id,"digest_id":claim.digest_id,"input_hash":plan["input_hash"]});
    if tx.execute("UPDATE owner_digest_work SET state='committed',result=? WHERE id=? AND turn_id=? AND state='running'", rusqlite::params![receipt.to_string(),claim.id,claim.turn_id])? != 1 {
        return Err("owner_commit_conflict".into());
    }
    tx.commit()?;
    Ok(plan)
}

pub(crate) fn committed(db: &Connection, c: &Config, claim: &Claim) -> Result<Option<Value>> {
    if !exists(db)? {
        return Ok(None);
    }
    let receipt: Option<String> = db.query_row("SELECT result FROM owner_digest_work WHERE id=? AND binding=? AND turn_id=? AND state='committed'", rusqlite::params![claim.id,book_binding(c)?,claim.turn_id], |r|r.get(0)).optional()?;
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    let receipt: Value = serde_json::from_str(&receipt).map_err(|_| "owner_receipt_invalid")?;
    let id = receipt["decision_id"]
        .as_str()
        .ok_or("owner_receipt_invalid")?;
    let saved: String = db.query_row(
        "SELECT claim FROM owner_digest_work WHERE id=? AND state='committed'",
        [&claim.id],
        |r| r.get(0),
    )?;
    if serde_json::from_str::<Claim>(&saved)? != *claim
        || receipt["version"] != 1
        || receipt["status"] != "committed"
        || receipt["turn_id"] != claim.turn_id
        || receipt["digest_id"] != claim.digest_id
        || id != format!("owner-{}", claim.turn_id)
    {
        return Err("owner_receipt_invalid".into());
    }
    base_plan(db, c, claim)?;
    let raw: String = db.query_row("SELECT body FROM planner_plan WHERE id=?", [id], |r| {
        r.get(0)
    })?;
    let plan: Value = serde_json::from_str(&raw).map_err(|_| "owner_receipt_invalid")?;
    planner::validate_saved(c, &plan)?;
    if plan["decision_turn_id"] != claim.turn_id
        || plan["digest_id"] != claim.digest_id
        || plan["input_hash"] != receipt["input_hash"]
        || plan["decision_owner"] != "orchestrator"
        || plan["digest_attempt_id"] != claim.id
        || plan["model_id"] != claim.model_id
        || plan["policy_sha256"] != claim.policy_sha256
        || plan["profile_sha256"] != json!(claim.profile_sha256)
        || plan["portfolio_revision"] != claim.portfolio_revision
    {
        return Err("owner_receipt_invalid".into());
    }
    let expected_provenance = selected_version(&plan);
    if plan["selection_provenance"] != expected_provenance {
        return Err("owner_receipt_invalid".into());
    }
    Ok(Some(plan))
}

pub(crate) fn supersede_plan_actions(db: &Connection, c: &Config, id: &str) -> Result<()> {
    let raw: String = db.query_row("SELECT body FROM planner_plan WHERE id=?", [id], |r| {
        r.get(0)
    })?;
    let plan: Value = serde_json::from_str(&raw)?;
    planner::validate_saved(c, &plan)?;
    if plan["decision_owner"] != "orchestrator" {
        return Err("owner_plan_identity_invalid".into());
    }
    let verdicts: Vec<planner::Verdict> = serde_json::from_value(plan["verdicts"].clone())?;
    for verdict in verdicts.into_iter().filter(|v| v.action.is_some()) {
        db.execute(
            "INSERT OR IGNORE INTO planner_action VALUES(?,?,'superseded_by_digest')",
            rusqlite::params![id, verdict.index as i64],
        )?;
    }
    Ok(())
}

pub(crate) fn actions_settled(db: &mut Connection, c: &Config, now: i64) -> Result<bool> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let Some(plan) = redecision::active_plan(&tx, c)?.flatten() else {
        return Ok(true);
    };
    if plan["decision_owner"] != "orchestrator" {
        return Ok(false);
    }
    let id = plan["decision_id"]
        .as_str()
        .ok_or("owner_plan_identity_invalid")?;
    let verdicts: Vec<planner::Verdict> = serde_json::from_value(plan["verdicts"].clone())?;
    let expired = plan["valid_until_ms"].as_i64().is_some_and(|at| now > at);
    let mut settled = true;
    for verdict in verdicts.into_iter().filter(|v| v.action.is_some()) {
        if expired {
            tx.execute(
                "INSERT OR IGNORE INTO planner_action VALUES(?,?,'plan_expired')",
                rusqlite::params![id, verdict.index as i64],
            )?;
        }
        let done: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM planner_action WHERE decision=? AND idx=?)",
            rusqlite::params![id, verdict.index as i64],
            |r| r.get(0),
        )?;
        settled &= done;
    }
    tx.commit()?;
    Ok(settled)
}

pub(crate) fn activity_report(
    db: &Connection,
    c: &Config,
    portfolio: &Portfolio,
) -> Result<activity_attribution::ActivityReport> {
    let session = c.session.to_string();
    let mut candidates_evaluated = 0u64;
    let mut decisions_considered = 0u64;
    let mut actions_proposed = 0u64;
    let mut actions_validated = 0u64;
    let mut abstentions = activity_attribution::AbstentionTaxonomy::default();
    let mut latencies = Vec::new();
    let mut attributions_by_source: BTreeMap<String, (Option<String>, Option<String>, u64)> =
        BTreeMap::new();
    let mut decision_to_source: BTreeMap<String, String> = BTreeMap::new();

    if exists(db)? {
        let binding = book_binding(c)?;
        let mut stmt = db.prepare(
            "SELECT id, state, result FROM owner_digest_work WHERE binding=? ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map([&binding], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;

        for row in rows {
            let (_id, state, result_raw) = row?;
            decisions_considered = decisions_considered.saturating_add(1);
            match state.as_str() {
                "refused" => {
                    let reason = result_raw
                        .as_deref()
                        .and_then(|r| serde_json::from_str::<Value>(r).ok())
                        .and_then(|v| v.get("reason").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_else(|| "owner_decision_refused".into());
                    abstentions.record(&reason);
                }
                "superseded" => {
                    abstentions.record("superseded");
                }
                "committed" => {
                    if let Some(result_json) = result_raw
                        .as_deref()
                        .and_then(|r| serde_json::from_str::<Value>(r).ok())
                        && let Some(decision_id) =
                            result_json.get("decision_id").and_then(Value::as_str)
                    {
                        let plan_raw: Option<String> = db
                            .query_row(
                                "SELECT body FROM planner_plan WHERE id=?",
                                [decision_id],
                                |r| r.get(0),
                            )
                            .optional()?;
                        if let Some(plan_raw) = plan_raw
                            && let Ok(plan) = serde_json::from_str::<Value>(&plan_raw)
                        {
                            if let Some(lat) = plan.get("latency_ms").and_then(Value::as_u64) {
                                latencies.push(lat);
                            }
                            if let Some(allowlist) = plan["input"]["allowlist"].as_array() {
                                candidates_evaluated =
                                    candidates_evaluated.saturating_add(allowlist.len() as u64);
                            }
                            if let Some(actions) = plan["parsed_plan"]["actions"].as_array() {
                                if actions.is_empty() {
                                    abstentions.record("model_abstain_wait");
                                } else {
                                    actions_proposed =
                                        actions_proposed.saturating_add(actions.len() as u64);
                                }
                            } else {
                                abstentions.record("model_abstain_wait");
                            }
                            if let Some(verdicts) = plan.get("verdicts").and_then(Value::as_array) {
                                for v in verdicts {
                                    let is_valid = v.as_str() == Some("valid")
                                        || v.get("verdict").and_then(Value::as_str)
                                            == Some("valid");
                                    if is_valid {
                                        actions_validated = actions_validated.saturating_add(1);
                                    }
                                }
                            }
                            let provenance = selected_version(&plan);
                            if let Some(source_hash) = provenance
                                .get("source_hash")
                                .and_then(Value::as_str)
                                .filter(|h| h.len() == 64)
                            {
                                let reason = provenance
                                    .get("selection_reason")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned);
                                let spec_id = plan["strategy_attribution"]["spec_id"]
                                    .as_str()
                                    .or_else(|| plan["selection_provenance"]["spec_id"].as_str())
                                    .map(str::to_owned);
                                let entry = attributions_by_source
                                    .entry(source_hash.to_string())
                                    .or_insert((spec_id.clone(), reason.clone(), 0));
                                if entry.0.is_none() && spec_id.is_some() {
                                    entry.0 = spec_id;
                                }
                                if entry.1.is_none() && reason.is_some() {
                                    entry.1 = reason;
                                }
                                entry.2 = entry.2.saturating_add(1);
                                decision_to_source
                                    .insert(decision_id.to_string(), source_hash.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    struct OpenLot {
        units: u64,
        price: Price,
        decision_id: Option<String>,
    }

    let fills_executed = portfolio.fills.len() as u64;
    let conversion = activity_attribution::CandidateConversion::new(
        candidates_evaluated,
        decisions_considered,
        actions_proposed,
        actions_validated,
        fills_executed,
    );

    let mut lots = BTreeMap::<String, std::collections::VecDeque<OpenLot>>::new();
    let mut gross_realized = Amount::ZERO;
    let mut total_notional = Amount::ZERO;
    let mut total_fees = Amount::ZERO;
    let mut total_slippage = Amount::ZERO;

    let mut notional_by_source: BTreeMap<String, Amount> = BTreeMap::new();
    let mut realized_by_source: BTreeMap<String, Amount> = BTreeMap::new();
    let mut fees_by_source: BTreeMap<String, Amount> = BTreeMap::new();
    let mut fills_by_source: BTreeMap<String, u64> = BTreeMap::new();

    let capital = portfolio.allocation().unwrap_or(portfolio.cash);
    let mut dd_points = Vec::new();
    dd_points.push((0u64, capital));
    let mut current_realized_cum = capital;

    for fill in &portfolio.fills {
        let notional = fill
            .price
            .amount()
            .checked_mul(Amount::from_unsigned(fill.units))
            .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
        total_notional = total_notional
            .checked_add(notional)
            .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
        total_fees = total_fees
            .checked_add(fill.fee)
            .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;

        if let Some(expected) = fill.expected_price {
            let adverse = if fill.buy {
                if fill.price.amount() > expected.amount() {
                    fill.price
                        .amount()
                        .checked_sub(expected.amount())
                        .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?
                } else {
                    Amount::ZERO
                }
            } else if expected.amount() > fill.price.amount() {
                expected
                    .amount()
                    .checked_sub(fill.price.amount())
                    .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?
            } else {
                Amount::ZERO
            };
            let slip_notional = adverse
                .checked_mul(Amount::from_unsigned(fill.units))
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            total_slippage = total_slippage
                .checked_add(slip_notional)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
        }

        let source_hash_opt = fill
            .decision_id
            .as_ref()
            .and_then(|id| decision_to_source.get(id));

        if let Some(source_hash) = source_hash_opt {
            *fills_by_source.entry(source_hash.clone()).or_insert(0) += 1;
            let n_entry = notional_by_source
                .entry(source_hash.clone())
                .or_insert(Amount::ZERO);
            *n_entry = n_entry
                .checked_add(notional)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            let f_entry = fees_by_source
                .entry(source_hash.clone())
                .or_insert(Amount::ZERO);
            *f_entry = f_entry
                .checked_add(fill.fee)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
        }

        let mut fill_realized_pnl = Amount::ZERO;
        let queue = lots.entry(fill.instrument.key.clone()).or_default();
        if fill.buy {
            queue.push_back(OpenLot {
                units: fill.units,
                price: fill.price,
                decision_id: fill.decision_id.clone(),
            });
        } else {
            let mut remaining = fill.units;
            let mut gross_buy_notional = Amount::ZERO;
            while remaining > 0 && !queue.is_empty() {
                let lot = queue.front_mut().unwrap();
                let taken = remaining.min(lot.units);
                let lot_notional = lot
                    .price
                    .amount()
                    .checked_mul(Amount::from_unsigned(taken))
                    .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                gross_buy_notional = gross_buy_notional
                    .checked_add(lot_notional)
                    .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                lot.units -= taken;
                remaining -= taken;
                if lot.units == 0 {
                    queue.pop_front();
                }
            }
            let sale_pnl = notional
                .checked_sub(gross_buy_notional)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            gross_realized = gross_realized
                .checked_add(sale_pnl)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            fill_realized_pnl = sale_pnl;

            if let Some(source_hash) = source_hash_opt {
                let r_entry = realized_by_source
                    .entry(source_hash.clone())
                    .or_insert(Amount::ZERO);
                *r_entry = r_entry
                    .checked_add(sale_pnl)
                    .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            }
        }
        current_realized_cum = current_realized_cum
            .checked_add(fill_realized_pnl)
            .and_then(|equity| equity.checked_sub(fill.fee))
            .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
        dd_points.push((fill.at.max(0) as u64, current_realized_cum));
    }

    let mut marked_pnl = Amount::ZERO;
    let mut marked_by_source: BTreeMap<String, Amount> = BTreeMap::new();
    let mut accounted_units: BTreeMap<String, u64> = BTreeMap::new();

    for (inst_key, queue) in lots {
        if let Some(pos) = portfolio.positions.get(&inst_key) {
            for lot in queue {
                if lot.units > 0 {
                    *accounted_units.entry(inst_key.clone()).or_insert(0) += lot.units;
                    let entry_val = lot
                        .price
                        .amount()
                        .checked_mul(Amount::from_unsigned(lot.units))
                        .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                    let mark_val = pos
                        .mark
                        .amount()
                        .checked_mul(Amount::from_unsigned(lot.units))
                        .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                    let lot_pnl = mark_val
                        .checked_sub(entry_val)
                        .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                    marked_pnl = marked_pnl
                        .checked_add(lot_pnl)
                        .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;

                    if let Some(source_hash) = lot
                        .decision_id
                        .as_ref()
                        .and_then(|id| decision_to_source.get(id))
                    {
                        let m_entry = marked_by_source
                            .entry(source_hash.clone())
                            .or_insert(Amount::ZERO);
                        *m_entry = m_entry
                            .checked_add(lot_pnl)
                            .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                    }
                }
            }
        }
    }

    for (key, pos) in &portfolio.positions {
        let accounted = accounted_units.get(key).copied().unwrap_or(0);
        if pos.units > accounted {
            let remaining_units = pos.units - accounted;
            let entry_val = pos
                .entry
                .amount()
                .checked_mul(Amount::from_unsigned(remaining_units))
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            let mark_val = pos
                .mark
                .amount()
                .checked_mul(Amount::from_unsigned(remaining_units))
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            let pos_pnl = mark_val
                .checked_sub(entry_val)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            marked_pnl = marked_pnl
                .checked_add(pos_pnl)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            if let Some(source_hash) = pos
                .decision_id
                .as_ref()
                .and_then(|id| decision_to_source.get(id))
            {
                let m_entry = marked_by_source
                    .entry(source_hash.clone())
                    .or_insert(Amount::ZERO);
                *m_entry = m_entry
                    .checked_add(pos_pnl)
                    .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            }
        }
    }

    if let Ok(current_eq) = portfolio.equity() {
        let last_time = portfolio
            .positions
            .values()
            .map(|position| position.marked_at)
            .filter(|at| *at > 0)
            .max()
            .or_else(|| portfolio.fills.last().map(|fill| fill.at))
            .unwrap_or(0)
            .max(0) as u64;
        dd_points.push((last_time, current_eq));
    }

    let financials = activity_attribution::FinancialAfterCost::calculate(
        gross_realized,
        marked_pnl,
        total_fees,
        total_slippage,
    )
    .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;

    let turnover = activity_attribution::TurnoverCostDrag::calculate(
        total_notional,
        capital,
        total_fees,
        total_slippage,
    )
    .map_err(|e| Fail::new(format!("turnover_calc_error: {e:?}")))?;

    let drawdown = activity_attribution::DrawdownTracker::from_series(&dd_points)
        .map_err(|e| Fail::new(format!("drawdown_calc_error: {e:?}")))?;

    let latency = activity_attribution::DecisionLatencyStats::from_latencies(&latencies);

    let trials = experiment_ledger::list(c).unwrap_or_default();
    let mut experiment_attributions = Vec::new();
    for (source_hash, (spec_id, selection_reason, decisions_count)) in attributions_by_source {
        let fills_count = fills_by_source.get(&source_hash).copied().unwrap_or(0);
        let notional_volume = notional_by_source
            .get(&source_hash)
            .copied()
            .unwrap_or(Amount::ZERO);
        let realized_pnl = realized_by_source
            .get(&source_hash)
            .copied()
            .unwrap_or(Amount::ZERO);
        let marked_pnl_attr = marked_by_source
            .get(&source_hash)
            .copied()
            .unwrap_or(Amount::ZERO);
        let fees_incurred = fees_by_source
            .get(&source_hash)
            .copied()
            .unwrap_or(Amount::ZERO);
        let trial_status = trials
            .iter()
            .find(|t| t.source_sha256 == source_hash)
            .map(|t| t.state.as_str().to_owned());
        let attr = activity_attribution::ExperimentAttribution::new(
            source_hash,
            spec_id,
            selection_reason,
            decisions_count,
            fills_count,
            notional_volume,
            realized_pnl,
            marked_pnl_attr,
            fees_incurred,
            trial_status,
        )
        .map_err(|e| Fail::new(format!("attr_calc_error: {e:?}")))?;
        experiment_attributions.push(attr);
    }

    Ok(activity_attribution::ActivityReport {
        session,
        conversion,
        abstentions,
        financials,
        turnover,
        drawdown,
        latency,
        experiment_attributions,
    })
}

pub(crate) fn activity_analytics(
    db: &Connection,
    c: &Config,
    portfolio: &Portfolio,
) -> Result<Value> {
    Ok(activity_report(db, c, portfolio)?.to_json())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::{
    exercise, exercise_instrument, interruption_fixture, interruption_switch_fixture,
    owner_fixture_config,
};
