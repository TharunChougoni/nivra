//! Offline authority-marker repair and explicit halted legacy migration.
//!
//! Both commands change authority metadata only. They never write the book
//! state, halt, block, active plan, attempts, usage or financial rows. The
//! prior marker bytes and the proof used to replace them are kept in an
//! append-only private audit table.
use super::*;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MARKER_MAX_BYTES: usize = 4096;
const AUDIT_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS planner_authority_audit(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL,
  kind TEXT NOT NULL,
  body TEXT NOT NULL
);
CREATE TRIGGER IF NOT EXISTS planner_authority_audit_no_update
  BEFORE UPDATE ON planner_authority_audit
  BEGIN SELECT RAISE(ABORT,'planner_authority_audit_immutable'); END;
CREATE TRIGGER IF NOT EXISTS planner_authority_audit_no_delete
  BEFORE DELETE ON planner_authority_audit
  BEGIN SELECT RAISE(ABORT,'planner_authority_audit_immutable'); END;
CREATE TRIGGER IF NOT EXISTS planner_authority_audit_no_replace
  BEFORE INSERT ON planner_authority_audit
  WHEN EXISTS(SELECT 1 FROM planner_authority_audit WHERE id=NEW.id)
  BEGIN SELECT RAISE(ABORT,'planner_authority_audit_immutable'); END;
";

#[derive(Debug)]
pub(super) enum MarkerState {
    TableAbsent,
    RowMissing,
    Malformed {
        raw: Vec<u8>,
        prior_hash: Option<String>,
        prior_owner: Option<String>,
        prior_binding: Option<String>,
        prior_version: Option<i64>,
        error: &'static str,
    },
    Stale {
        raw: Vec<u8>,
        receipt: AuthorityOwnerReceipt,
    },
    Current {
        raw: Vec<u8>,
        receipt: AuthorityOwnerReceipt,
    },
}

#[derive(Default)]
struct MarkerHints {
    hash: Option<String>,
    owner: Option<String>,
    binding: Option<String>,
    version: Option<i64>,
}

impl MarkerHints {
    fn parse(raw: &[u8]) -> Self {
        let Ok(value) = serde_json::from_slice::<Value>(raw) else {
            return Self::default();
        };
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        Self {
            hash: object
                .get("config_hash")
                .and_then(Value::as_str)
                .map(str::to_owned),
            owner: object
                .get("owner")
                .and_then(Value::as_str)
                .map(str::to_owned),
            binding: object
                .get("binding")
                .and_then(Value::as_str)
                .map(str::to_owned),
            version: object.get("version").and_then(Value::as_i64),
        }
    }
}

pub(super) fn read_marker_state(db: &Connection, state: &Control) -> Result<MarkerState> {
    let present: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='planner_authority_owner')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        return Ok(MarkerState::TableAbsent);
    }
    let raw: Option<rusqlite::types::Value> = db
        .query_row(
            "SELECT body FROM planner_authority_owner WHERE id=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(MarkerState::RowMissing);
    };
    let raw = match raw {
        rusqlite::types::Value::Text(text) => text.into_bytes(),
        rusqlite::types::Value::Blob(bytes) => bytes,
        _ => Vec::new(),
    };
    let hints = MarkerHints::parse(&raw);
    if raw.len() > MARKER_MAX_BYTES {
        return Ok(MarkerState::Malformed {
            raw,
            prior_hash: hints.hash,
            prior_owner: hints.owner,
            prior_binding: hints.binding,
            prior_version: hints.version,
            error: "planner_authority_owner_invalid",
        });
    }
    let text = match std::str::from_utf8(&raw) {
        Ok(text) => text,
        Err(_) => {
            return Ok(MarkerState::Malformed {
                raw,
                prior_hash: hints.hash,
                prior_owner: hints.owner,
                prior_binding: hints.binding,
                prior_version: hints.version,
                error: "planner_authority_owner_invalid",
            });
        }
    };
    let receipt: AuthorityOwnerReceipt = match serde_json::from_str(text) {
        Ok(receipt) => receipt,
        Err(_) => {
            return Ok(MarkerState::Malformed {
                raw,
                prior_hash: hints.hash,
                prior_owner: hints.owner,
                prior_binding: hints.binding,
                prior_version: hints.version,
                error: "planner_authority_owner_invalid",
            });
        }
    };
    if receipt.version != 1 || receipt.binding != state.binding {
        return Ok(MarkerState::Malformed {
            raw,
            prior_hash: hints.hash,
            prior_owner: hints.owner,
            prior_binding: hints.binding,
            prior_version: hints.version,
            error: "planner_authority_owner_mismatch",
        });
    }
    if receipt.config_hash != state.config_hash {
        return Ok(MarkerState::Stale { raw, receipt });
    }
    Ok(MarkerState::Current { raw, receipt })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthorityTransitionStepV1 {
    pub event: String,
    pub tick_rowid: i64,
    pub body_sha256: String,
    pub prior_config_hash: String,
    pub current_config_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthorityTransitionV1 {
    pub version: u32,
    pub kind: String,
    pub prior_config_hash: String,
    pub current_config_hash: String,
    pub steps: Vec<AuthorityTransitionStepV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthorityRepairReceiptV1 {
    pub version: u32,
    pub kind: String,
    pub binding: String,
    pub expected_owner: AuthorityOwner,
    pub prior_marker_raw_hex: Option<String>,
    pub prior_marker_sha256: Option<String>,
    pub prior_marker_config_hash: Option<String>,
    pub prior_marker_owner: Option<String>,
    pub control_config_hash: String,
    pub current_config_hash: String,
    pub transition: Option<AuthorityTransitionV1>,
    pub portfolio_sha256_before: String,
    pub portfolio_sha256_after: String,
    pub control_sha256_before: String,
    pub control_sha256_after: String,
    pub attempts_sha256_before: String,
    pub attempts_sha256_after: String,
    pub repaired_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthorityMigrationReceiptV1 {
    pub version: u32,
    pub kind: String,
    pub binding: String,
    pub prior_marker_raw_hex: String,
    pub prior_marker_sha256: String,
    pub prior_marker_config_hash: String,
    pub prior_owner: AuthorityOwner,
    pub prior_control_config_hash: String,
    pub current_control_config_hash: String,
    pub current_config_hash: String,
    pub transition: Option<AuthorityTransitionV1>,
    pub prior_active: Option<String>,
    pub prior_active_sha256: Option<String>,
    pub prior_active_owner: Option<String>,
    pub prior_blocked: Option<String>,
    pub prior_pending_attempt: Option<String>,
    pub prior_prompt_sha256: Option<Value>,
    pub current_prompt_sha256: Option<Value>,
    pub portfolio_sha256_before: String,
    pub portfolio_sha256_after: String,
    pub control_sha256_before: String,
    pub control_sha256_after: String,
    pub attempts_sha256_before: String,
    pub attempts_sha256_after: String,
    pub migrated_at_ms: i64,
}

fn owner_name(owner: AuthorityOwner) -> &'static str {
    match owner {
        AuthorityOwner::Legacy => "legacy",
        AuthorityOwner::Orchestrator => "orchestrator",
    }
}

fn parse_owner(value: &str) -> Option<AuthorityOwner> {
    match value {
        "legacy" => Some(AuthorityOwner::Legacy),
        "orchestrator" => Some(AuthorityOwner::Orchestrator),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn portfolio_sha256(db: &Connection) -> Result<String> {
    let body: String = db.query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))?;
    Ok(digest(body.as_bytes()))
}

fn control_sha256(db: &Connection) -> Result<String> {
    let body: String = db.query_row(
        "SELECT body FROM planner_event_control WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    Ok(digest(body.as_bytes()))
}

/// Hash the exact stored attempt rows in primary-key order. The authority
/// commands never write this table; the receipt proves that without logging
/// any attempt body.
fn attempts_sha256(db: &Connection) -> Result<String> {
    use sha2::{Digest, Sha256};
    let present: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='planner_attempt_log')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        return Ok(digest(b"planner-authority-attempts-absent"));
    }
    let mut hash = Sha256::new();
    hash.update(b"planner-authority-attempts-v1");
    let mut count = 0_u64;
    let mut statement =
        db.prepare("SELECT id,event_id,body,state,result FROM planner_attempt_log ORDER BY id")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        count = count
            .checked_add(1)
            .ok_or("planner_authority_attempt_history_invalid")?;
        if count > 4096 {
            return Err("planner_authority_attempt_history_invalid".into());
        }
        for column in 0..5 {
            match row.get_ref(column)? {
                rusqlite::types::ValueRef::Null if column == 4 => hash.update([0]),
                rusqlite::types::ValueRef::Text(value) => {
                    hash.update([1]);
                    hash.update((value.len() as u64).to_be_bytes());
                    hash.update(value);
                }
                _ => return Err("planner_authority_attempt_history_invalid".into()),
            }
        }
    }
    hash.update(count.to_be_bytes());
    Ok(hex(hash.finalize().as_slice()))
}

fn ensure_audit(db: &Connection) -> Result<()> {
    db.execute_batch(AUDIT_SCHEMA)?;
    Ok(())
}

fn insert_audit(db: &Connection, at: i64, kind: &str, body: &str) -> Result<i64> {
    db.execute(
        "INSERT INTO planner_authority_audit(at,kind,body) VALUES(?,?,?)",
        params![at, kind, body],
    )?;
    Ok(db.last_insert_rowid())
}

fn transition_path(
    edges: &[AuthorityTransitionStepV1],
    prior: &str,
    current: &str,
) -> Option<Vec<AuthorityTransitionStepV1>> {
    use std::collections::BTreeMap;
    // Edges arrive in tick-rowid order. Extending only paths that already
    // exist keeps every proof step strictly chronological; a hash cycle in
    // nonchronological rows is not a transition.
    let mut paths: BTreeMap<&str, Vec<AuthorityTransitionStepV1>> = BTreeMap::new();
    paths.insert(prior, Vec::new());
    for edge in edges {
        let Some(path) = paths.get(edge.prior_config_hash.as_str()).cloned() else {
            continue;
        };
        paths
            .entry(edge.current_config_hash.as_str())
            .or_insert_with(|| {
                let mut next = path;
                next.push(edge.clone());
                next
            });
    }
    paths.get(current).cloned()
}

/// Prove `prior -> current` from audited apply/rebound ticks, or from an exact
/// archived policy that upgrades the old full-config hash to the current one.
fn transition_evidence(
    db: &Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    prior: &str,
    current: &str,
) -> Result<Option<AuthorityTransitionV1>> {
    if prior == current {
        return Ok(None);
    }
    let authority_rowid: i64 = db.query_row(
        "SELECT COALESCE(MAX(rowid),0) FROM ticks WHERE event='event_planner_authority'",
        [],
        |r| r.get(0),
    )?;
    let mut statement = db.prepare(
        "SELECT rowid,event,body FROM ticks WHERE rowid>? AND event IN ('operator_planner_config_applied','operator_planner_authority_rebound') ORDER BY rowid",
    )?;
    let mut rows = statement.query(params![authority_rowid])?;
    let mut edges = Vec::new();
    while let Some(row) = rows.next()? {
        let rowid: i64 = row.get(0)?;
        let event: String = row.get(1)?;
        let body: String = row.get(2)?;
        let value: Value = serde_json::from_str(&body)
            .map_err(|_| "planner_authority_repair_transition_evidence_invalid")?;
        let prior_hash = value
            .get("prior_config_hash")
            .and_then(Value::as_str)
            .ok_or("planner_authority_repair_transition_evidence_invalid")?;
        let current_hash = value
            .get("current_config_hash")
            .and_then(Value::as_str)
            .ok_or("planner_authority_repair_transition_evidence_invalid")?;
        edges.push(AuthorityTransitionStepV1 {
            event,
            tick_rowid: rowid,
            body_sha256: digest(body.as_bytes()),
            prior_config_hash: prior_hash.to_owned(),
            current_config_hash: current_hash.to_owned(),
        });
    }
    if let Some(steps) = transition_path(&edges, prior, current) {
        return Ok(Some(AuthorityTransitionV1 {
            version: 1,
            kind: "audited_ticks".into(),
            prior_config_hash: prior.to_owned(),
            current_config_hash: current.to_owned(),
            steps,
        }));
    }
    if upgrade_config_hash(c, cfg, prior)?
        .map(|(hash, _)| hash)
        .as_deref()
        == Some(current)
    {
        return Ok(Some(AuthorityTransitionV1 {
            version: 1,
            kind: "archived_policy".into(),
            prior_config_hash: prior.to_owned(),
            current_config_hash: current.to_owned(),
            steps: Vec::new(),
        }));
    }
    Ok(None)
}

fn active_plan_evidence(
    db: &Connection,
    state: &Control,
    invalid_code: &'static str,
) -> Result<Option<(String, String, Option<String>)>> {
    let Some(id) = &state.active else {
        return Ok(None);
    };
    let body: String = db.query_row("SELECT body FROM planner_plan WHERE id=?", [id], |r| {
        r.get(0)
    })?;
    let plan: Value = serde_json::from_str(&body).map_err(|_| invalid_code)?;
    if plan.get("decision_id").and_then(Value::as_str) != Some(id.as_str()) {
        return Err(invalid_code.into());
    }
    let owner = plan
        .get("decision_owner")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(Some((id.clone(), digest(body.as_bytes()), owner)))
}

fn check_repair_active_plan(
    active: Option<&(String, String, Option<String>)>,
    expected_owner: AuthorityOwner,
) -> Result<()> {
    let Some((id, _, owner)) = active else {
        return Ok(());
    };
    match (owner.as_deref(), expected_owner) {
        (Some("orchestrator"), AuthorityOwner::Orchestrator) if id.starts_with("owner-") => Ok(()),
        (Some("orchestrator"), AuthorityOwner::Orchestrator) => {
            Err("planner_authority_repair_active_plan_invalid".into())
        }
        (Some("orchestrator"), AuthorityOwner::Legacy) => {
            Err("planner_authority_repair_active_owner_plan".into())
        }
        (_, AuthorityOwner::Orchestrator) => {
            Err("planner_authority_repair_active_legacy_plan".into())
        }
        (_, AuthorityOwner::Legacy) => Ok(()),
    }
}

fn check_migration_active_plan(active: Option<&(String, String, Option<String>)>) -> Result<()> {
    if active
        .and_then(|(_, _, owner)| owner.as_deref())
        .is_some_and(|owner| owner == "orchestrator")
    {
        return Err("planner_authority_migrate_active_owner_plan".into());
    }
    Ok(())
}

pub(crate) fn repair_authority(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    expected_owner: AuthorityOwner,
) -> Result<AuthorityRepairReceiptV1> {
    let state = load(db)?.ok_or("planner_authority_repair_authority_missing")?;
    check(&state, c)?;
    if expected_owner == AuthorityOwner::Orchestrator {
        cfg.validate_decision_owner()?;
    }
    if AuthorityOwner::for_config(cfg) != expected_owner {
        return Err("planner_authority_repair_owner_mismatch".into());
    }
    let current_hash = config_hash(cfg)?;
    if state.config_hash != current_hash {
        return Err("planner_authority_repair_control_hash_mismatch".into());
    }
    let marker = read_marker_state(db, &state)?;
    let (kind, prior_raw, prior_hash, prior_owner, transition) = match &marker {
        MarkerState::Current { receipt, .. } => {
            if receipt.owner != expected_owner {
                return Err("planner_authority_repair_owner_mismatch".into());
            }
            return Err("planner_authority_repair_not_required".into());
        }
        MarkerState::TableAbsent => ("absent", None, None, None, None),
        MarkerState::RowMissing => ("missing", None, None, None, None),
        MarkerState::Malformed {
            raw,
            prior_hash,
            prior_owner,
            prior_binding,
            prior_version,
            ..
        } => {
            if raw.len() > MARKER_MAX_BYTES {
                return Err("planner_authority_repair_marker_bounds".into());
            }
            if prior_binding
                .as_deref()
                .is_some_and(|binding| binding != state.binding)
            {
                return Err("planner_authority_repair_binding_mismatch".into());
            }
            if prior_version.is_some_and(|version| version != 1) {
                return Err("planner_authority_repair_marker_version_unsupported".into());
            }
            if prior_owner
                .as_deref()
                .and_then(parse_owner)
                .is_some_and(|owner| owner != expected_owner)
            {
                return Err("planner_authority_repair_owner_mismatch".into());
            }
            (
                "malformed",
                Some(raw.clone()),
                prior_hash.clone(),
                prior_owner.clone(),
                None,
            )
        }
        MarkerState::Stale { raw, receipt } => {
            if receipt.owner != expected_owner {
                return Err("planner_authority_repair_owner_mismatch".into());
            }
            let transition =
                transition_evidence(db, c, cfg, &receipt.config_hash, &state.config_hash)?
                    .ok_or("planner_authority_repair_transition_unproven")?;
            (
                "stale",
                Some(raw.clone()),
                Some(receipt.config_hash.clone()),
                Some(owner_name(receipt.owner).to_owned()),
                Some(transition),
            )
        }
    };
    let active = active_plan_evidence(db, &state, "planner_authority_repair_active_plan_invalid")?;
    check_repair_active_plan(active.as_ref(), expected_owner)?;
    let portfolio_before = portfolio_sha256(db)?;
    let control_before = control_sha256(db)?;
    let attempts_before = attempts_sha256(db)?;
    let at = millis();
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_audit(&tx)?;
    save_authority_owner(
        &tx,
        &AuthorityOwnerReceipt {
            version: 1,
            binding: state.binding.clone(),
            config_hash: state.config_hash.clone(),
            owner: expected_owner,
        },
    )?;
    let portfolio_after = portfolio_sha256(&tx)?;
    let control_after = control_sha256(&tx)?;
    let attempts_after = attempts_sha256(&tx)?;
    if portfolio_after != portfolio_before
        || control_after != control_before
        || attempts_after != attempts_before
    {
        return Err("planner_authority_repair_evidence_mismatch".into());
    }
    let receipt = AuthorityRepairReceiptV1 {
        version: 1,
        kind: kind.into(),
        binding: state.binding.clone(),
        expected_owner,
        prior_marker_raw_hex: prior_raw.as_deref().map(hex),
        prior_marker_sha256: prior_raw.as_deref().map(digest),
        prior_marker_config_hash: prior_hash,
        prior_marker_owner: prior_owner,
        control_config_hash: state.config_hash.clone(),
        current_config_hash: current_hash,
        transition,
        portfolio_sha256_before: portfolio_before,
        portfolio_sha256_after: portfolio_after,
        control_sha256_before: control_before,
        control_sha256_after: control_after,
        attempts_sha256_before: attempts_before,
        attempts_sha256_after: attempts_after,
        repaired_at_ms: at,
    };
    let audit_id = insert_audit(&tx, at, "repair", &serde_json::to_string(&receipt)?)?;
    tx.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_authority_repaired')",
        params![
            at,
            json!({
                "audit_id":audit_id,
                "kind":receipt.kind.clone(),
                "prior_marker_sha256":receipt.prior_marker_sha256.clone(),
                "prior_marker_config_hash":receipt.prior_marker_config_hash.clone(),
                "control_config_hash":receipt.control_config_hash.clone(),
                "current_config_hash":receipt.current_config_hash.clone(),
                "expected_owner":owner_name(expected_owner),
                "transition":receipt.transition.as_ref().map(|transition| transition.kind.clone()),
                "portfolio_sha256":receipt.portfolio_sha256_after,
                "control_sha256_before":receipt.control_sha256_before,
                "control_sha256_after":receipt.control_sha256_after,
                "attempts_sha256_before":receipt.attempts_sha256_before,
                "attempts_sha256_after":receipt.attempts_sha256_after
            })
            .to_string(),
        ],
    )?;
    tx.commit()?;
    Ok(receipt)
}

pub(crate) fn migrate_legacy_authority(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
) -> Result<AuthorityMigrationReceiptV1> {
    if !cfg.orchestrator_owns_decisions() {
        return Err("planner_authority_migrate_owner_policy_required".into());
    }
    cfg.validate_decision_owner()?;
    if settings(cfg).is_none() {
        return Err("planner_authority_migrate_events_disabled".into());
    }
    if !p.halted {
        return Err("planner_authority_migrate_requires_halted_book".into());
    }
    let state = load(db)?.ok_or("planner_authority_migrate_authority_missing")?;
    check(&state, c)?;
    let marker = read_marker_state(db, &state)?;
    let (prior_raw, receipt, stale) = match marker {
        MarkerState::Current { raw, receipt } => (raw, receipt, false),
        MarkerState::Stale { raw, receipt } => (raw, receipt, true),
        MarkerState::TableAbsent | MarkerState::RowMissing | MarkerState::Malformed { .. } => {
            return Err("planner_authority_migrate_prior_owner_unproven".into());
        }
    };
    if receipt.owner != AuthorityOwner::Legacy {
        return Err("planner_authority_migrate_not_legacy".into());
    }
    let current_hash = config_hash(cfg)?;
    let (transition, update_control) = if stale {
        if state.config_hash != current_hash {
            return Err("planner_authority_migrate_control_unproven".into());
        }
        let transition = transition_evidence(db, c, cfg, &receipt.config_hash, &state.config_hash)?
            .ok_or("planner_authority_migrate_control_unproven")?;
        (Some(transition), false)
    } else {
        if state.config_hash != receipt.config_hash || state.config_hash == current_hash {
            return Err("planner_authority_migrate_control_unproven".into());
        }
        (None, true)
    };
    let active = active_plan_evidence(db, &state, "planner_authority_migrate_active_plan_invalid")?;
    check_migration_active_plan(active.as_ref())?;
    let (current_prompts, current_without_prompt) = if update_control {
        let (_, prompts, without_prompt) = config_identity(cfg)?;
        (prompts, without_prompt)
    } else {
        (
            state.prompt_sha256.clone(),
            state.config_without_prompt_sha256.clone(),
        )
    };
    let portfolio_before = portfolio_sha256(db)?;
    let control_before = control_sha256(db)?;
    let attempts_before = attempts_sha256(db)?;
    let at = millis();
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_audit(&tx)?;
    if update_control {
        let mut next = state.clone();
        next.config_hash = current_hash.clone();
        next.prompt_sha256 = current_prompts.clone();
        next.config_without_prompt_sha256 = current_without_prompt.clone();
        save(&tx, &next)?;
    }
    save_authority_owner(
        &tx,
        &AuthorityOwnerReceipt {
            version: 1,
            binding: state.binding.clone(),
            config_hash: current_hash.clone(),
            owner: AuthorityOwner::Orchestrator,
        },
    )?;
    let portfolio_after = portfolio_sha256(&tx)?;
    let control_after = control_sha256(&tx)?;
    let attempts_after = attempts_sha256(&tx)?;
    if portfolio_after != portfolio_before || attempts_after != attempts_before {
        return Err("planner_authority_migrate_evidence_mismatch".into());
    }
    let receipt = AuthorityMigrationReceiptV1 {
        version: 1,
        kind: "legacy_to_owner".into(),
        binding: state.binding.clone(),
        prior_marker_raw_hex: hex(&prior_raw),
        prior_marker_sha256: digest(&prior_raw),
        prior_marker_config_hash: receipt.config_hash.clone(),
        prior_owner: AuthorityOwner::Legacy,
        prior_control_config_hash: state.config_hash.clone(),
        current_control_config_hash: current_hash.clone(),
        current_config_hash: current_hash,
        transition,
        prior_active: active.as_ref().map(|(id, _, _)| id.clone()),
        prior_active_sha256: active.as_ref().map(|(_, sha, _)| sha.clone()),
        prior_active_owner: active.as_ref().and_then(|(_, _, owner)| owner.clone()),
        prior_blocked: state.blocked.clone(),
        prior_pending_attempt: state.pending.as_ref().map(|claim| claim.id.clone()),
        prior_prompt_sha256: state.prompt_sha256.clone(),
        current_prompt_sha256: current_prompts,
        portfolio_sha256_before: portfolio_before,
        portfolio_sha256_after: portfolio_after,
        control_sha256_before: control_before,
        control_sha256_after: control_after,
        attempts_sha256_before: attempts_before,
        attempts_sha256_after: attempts_after,
        migrated_at_ms: at,
    };
    let audit_id = insert_audit(&tx, at, "migrate", &serde_json::to_string(&receipt)?)?;
    tx.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_authority_migrated')",
        params![
            at,
            json!({
                "audit_id":audit_id,
                "prior_owner":"legacy",
                "prior_marker_sha256":receipt.prior_marker_sha256.clone(),
                "prior_marker_config_hash":receipt.prior_marker_config_hash.clone(),
                "prior_control_config_hash":receipt.prior_control_config_hash.clone(),
                "current_control_config_hash":receipt.current_control_config_hash.clone(),
                "current_config_hash":receipt.current_config_hash.clone(),
                "transition":receipt.transition.as_ref().map(|transition| transition.kind.clone()),
                "halted":p.halted,
                "stopped":p.stopped,
                "blocked":receipt.prior_blocked.clone(),
                "active":receipt.prior_active.clone(),
                "pending_attempt":receipt.prior_pending_attempt.clone(),
                "portfolio_sha256":receipt.portfolio_sha256_after,
                "control_sha256_before":receipt.control_sha256_before,
                "control_sha256_after":receipt.control_sha256_after,
                "attempts_sha256_before":receipt.attempts_sha256_before,
                "attempts_sha256_after":receipt.attempts_sha256_after
            })
            .to_string(),
        ],
    )?;
    tx.commit()?;
    Ok(receipt)
}
