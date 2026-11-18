//! The controller's only path from a completed pi decision to the paper head.
use super::Lane;
use crate::runtime::{
    Config, Connection, LIVE_ENABLED, Result, decision_owner, digest, planner, redecision,
};
use nivra_core::execution::{Instrument, Portfolio};
use serde_json::{Value, json};

pub(crate) fn initialize(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    now: i64,
) -> Result<()> {
    if LIVE_ENABLED || c.strategy != "model_planner_v1" || !cfg.uses_pi_paper() {
        return Err("agent_paper_book_required".into());
    }
    if redecision::initialize(db, c, p, Some(cfg), now)? {
        redecision::owner_startup_check(db, c, cfg)?;
    }
    Ok(())
}

fn current_policy(c: &Config, cfg: &planner::PlannerConfig) -> Result<()> {
    let path = c
        .planner_config
        .as_deref()
        .ok_or("agent_paper_policy_missing")?;
    let current = planner::PlannerConfig::load_for_session(path, c.session)
        .map_err(|_| "agent_paper_policy_unavailable")?;
    if !current.uses_pi_paper()
        || planner::policy_bytes(&current)? != planner::policy_bytes(cfg)?
        || current.profile_sha256() != cfg.profile_sha256()
    {
        return Err("agent_paper_policy_changed".into());
    }
    Ok(())
}

fn in_window(c: &Config, cfg: &planner::PlannerConfig, p: &Portfolio, now: i64) -> Result<()> {
    if LIVE_ENABLED
        || c.strategy != "model_planner_v1"
        || !cfg.uses_pi_paper()
        || !p.model_led
        || p.stopped
        || p.closed
        || p.halted
        || !decision_owner::in_window(c, cfg, now)
        || !c
            .calendar_at(c.session)?
            .is_open(nivra_core::time::Timestamp::from_epoch_millis(now)?)?
    {
        return Err("agent_paper_outside_session".into());
    }
    Ok(())
}

/// Reserve the exact evidence and model before sending the worker a decision.
pub(super) fn reserve(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    now: i64,
    input: &Value,
    triggered_by: Value,
) -> Result<Option<redecision::Claim>> {
    in_window(c, cfg, p, now)?;
    current_policy(c, cfg)?;
    let lane = cfg
        .agent
        .as_ref()
        .ok_or("agent_settings_missing")?
        .lane(Lane::Decision)?;
    let deadline = now
        .checked_add(i64::try_from(lane.turn_timeout_ms)?)
        .ok_or("agent_paper_deadline_invalid")?;
    let trigger = json!({
        "kind":"pi_decision",
        "observed_ms":now,
        "triggered_by":{
            "kind":triggered_by["kind"],
            "reason":triggered_by["reason"],
            "sha256":digest(&serde_json::to_vec(&triggered_by)?)
        },
        "policy_sha256":digest(&planner::policy_bytes(cfg)?),
        "profile_sha256":cfg.profile_sha256(),
        "input_sha256":digest(&serde_json::to_vec(input)?),
        "model":lane.model.id,
        "provider":lane.model.provider,
    });
    redecision::reserve_pi(db, c, cfg, p, now, deadline, trigger)
}

pub(super) fn turn_id(claim: &redecision::Claim) -> String {
    format!("decision-{}", claim.id)
}

pub(crate) fn refuse(
    db: &mut Connection,
    c: &Config,
    claim: &redecision::Claim,
    reason: &str,
) -> Result<()> {
    redecision::settle_pi(db, c, claim, Err(reason))
}

pub(super) fn failure_reason(error: &(dyn std::error::Error + 'static)) -> String {
    let reason = error.to_string();
    if reason.starts_with("agent_paper_")
        && reason.len() <= 96
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    {
        reason
    } else {
        "agent_paper_commit_refused".to_owned()
    }
}

pub(super) struct Completed<'a> {
    pub turn_id: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub proposal: &'a Value,
    pub input: &'a Value,
}

pub(super) fn commit(
    context: &mut decision_owner::Context<'_>,
    c: &Config,
    cfg: &planner::PlannerConfig,
    claim: &redecision::Claim,
    completed: Completed<'_>,
    now: i64,
) -> Result<Value> {
    in_window(c, cfg, context.portfolio, now)?;
    current_policy(c, cfg)?;
    if now < claim.started_ms || now > claim.deadline_ms {
        return Err("agent_paper_decision_expired".into());
    }
    if redecision::portfolio_hash(context.portfolio)? != claim.portfolio_hash {
        return Err("agent_paper_portfolio_changed".into());
    }
    let hash = digest(&serde_json::to_vec(completed.input)?);
    if completed.turn_id != turn_id(claim)
        || claim.trigger["provider"] != completed.provider
        || claim.trigger["model"] != completed.model
        || claim.trigger["input_sha256"] != hash
        || claim.trigger["policy_sha256"] != digest(&planner::policy_bytes(cfg)?)
        || claim.trigger["profile_sha256"] != json!(cfg.profile_sha256())
    {
        return Err("agent_paper_turn_binding_invalid".into());
    }
    let allowlist: Vec<Instrument> = serde_json::from_value(completed.input["allowlist"].clone())
        .map_err(|_| "agent_paper_instrument_revision_invalid")?;
    if allowlist
        .iter()
        .any(|instrument| !context.instruments.contains(instrument))
    {
        return Err("agent_paper_instrument_revision_changed".into());
    }
    if crate::runtime::transcript::redact(completed.proposal) != *completed.proposal {
        return Err("agent_paper_output_secret_refused".into());
    }
    let raw = completed.proposal.to_string();
    let verdicts = planner::validate(&raw, context.portfolio, &allowlist)?;
    let plan = json!({
        "decision_id":format!("owner-{}",completed.turn_id),
        "decision_turn_id":completed.turn_id,
        "decision_owner":"pi",
        "attempt_id":claim.id,
        "provider":completed.provider,
        "model_id":completed.model,
        "session_binding":claim.binding,
        "portfolio_revision":claim.portfolio_hash,
        "policy_sha256":claim.trigger["policy_sha256"],
        "profile_sha256":claim.trigger["profile_sha256"],
        "input_hash":hash,
        "input":completed.input,
        "parsed_plan":completed.proposal,
        "raw_output":raw,
        "latency_ms":now.saturating_sub(claim.started_ms),
        "valid_until_ms":claim.deadline_ms,
        "verdicts":verdicts,
        "fake":false,
        "dry_run":false,
    });
    redecision::settle_pi(
        context.db,
        c,
        claim,
        Ok((cfg, context.portfolio, &plan, now)),
    )?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{Quote, open_book, wide};
    use nivra_core::finance::Price;
    use std::collections::BTreeMap;

    #[test]
    fn pi_claim_activates_only_through_fresh_paper_quote_execution() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = crate::runtime::tests::config(dir.path());
        let mut settings = super::super::tests::settings(10, 10.0);
        settings.mode = super::super::AgentMode::Paper;
        let cfg = planner::PlannerConfig {
            enabled: true,
            agent: Some(settings),
            ..Default::default()
        };
        let path = dir.path().join("planner.json");
        std::fs::write(&path, serde_json::to_vec(&cfg).unwrap()).unwrap();
        c.planner_config = Some(path);
        let cfg = planner::PlannerConfig::load_for_session(
            c.planner_config.as_deref().unwrap(),
            c.session,
        )
        .unwrap();
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
            .unwrap()
            .timestamp_millis();
        let mut db = open_book(&c).unwrap();
        wide::ensure_actions(&db).unwrap();
        let mut p = wide::load_wide(&mut db, &c).unwrap();
        let instrument = Instrument {
            key: "NSE_EQ|INE000000001".into(),
            symbol: "PAPER".into(),
            tick: Price::parse("0.05").unwrap(),
            series: "EQ".into(),
        };
        let instruments = std::slice::from_ref(&instrument);
        let input = json!({"allowlist":instruments});
        let proposal = json!({
            "actions":[{
                "instrument_key":instrument.key,"side":"buy","units":2,
                "max_price":"101","rationale":"fixture",
                "exit_intent":{"target":"110","stop":"90"}
            }],
            "stance":"long",
            "notes":""
        });
        let model = &cfg
            .agent
            .as_ref()
            .unwrap()
            .lane(Lane::Decision)
            .unwrap()
            .model;
        initialize(&mut db, &c, &cfg, &p, at).unwrap();
        assert!(redecision::has_pi_authority(&db, &c).unwrap());
        let mut decision_id = String::new();
        for (offset, age, expected) in [
            (0, 2_001, "execution_gate_rejected"),
            (5_000, 2_000, "filled"),
        ] {
            let now = at + offset;
            let claim = reserve(&mut db, &c, &cfg, &p, now, &input, json!({}))
                .unwrap()
                .unwrap();
            assert!(redecision::pending_attempt(&db).unwrap());
            let turn_id = turn_id(&claim);
            let result = |model| Completed {
                turn_id: &turn_id,
                provider: &cfg
                    .agent
                    .as_ref()
                    .unwrap()
                    .lane(Lane::Decision)
                    .unwrap()
                    .model
                    .provider,
                model,
                proposal: &proposal,
                input: &input,
            };
            let mut context = decision_owner::Context {
                db: &mut db,
                portfolio: &p,
                instruments,
            };
            assert_eq!(
                commit(&mut context, &c, &cfg, &claim, result("foreign-model"), now)
                    .unwrap_err()
                    .to_string(),
                "agent_paper_turn_binding_invalid"
            );
            let committed = commit(&mut context, &c, &cfg, &claim, result(&model.id), now).unwrap();
            assert!(commit(&mut context, &c, &cfg, &claim, result(&model.id), now).is_err());
            assert!(!redecision::pending_attempt(&db).unwrap());
            assert!(p.fills.is_empty());
            decision_id = committed["decision_id"].as_str().unwrap().to_owned();
            assert_eq!(
                planner::saved_plan(&db, &c).unwrap().unwrap()["decision_id"],
                decision_id
            );
            let quote = Quote {
                bid: Price::parse("100").unwrap(),
                ask: Price::parse("100.05").unwrap(),
                bid_units: 100,
                ask_units: 100,
                source: now,
                receipt: now,
            };
            let quotes = BTreeMap::from([(instrument.key.clone(), quote)]);
            for _ in 0..2 {
                wide::commit_quotes(&mut db, &c, &mut p, &quotes, &[], now, "original", || {
                    now + age
                })
                .unwrap();
            }
            let verdict: String = db
                .query_row(
                    "SELECT verdict FROM planner_action WHERE decision=? AND idx=0",
                    [&decision_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(verdict, expected);
            let receipt: i64 = db
                .query_row(
                    "SELECT receipt FROM ticks WHERE event='wide_quotes' ORDER BY id DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(receipt, now);
        }
        assert_eq!(p.fills.len(), 1);
        assert_eq!(
            p.fills[0].decision_id.as_deref(),
            Some(decision_id.as_str())
        );
    }

    #[test]
    fn reviewed_pi_cutover_preserves_the_archived_head() {
        let dir = tempfile::tempdir().unwrap();
        let (c, mut cfg) = decision_owner::owner_fixture_config(dir.path());
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
            .unwrap()
            .timestamp_millis();
        let mut db = open_book(&c).unwrap();
        wide::ensure_actions(&db).unwrap();
        let p = wide::load_wide(&mut db, &c).unwrap();
        redecision::initialize(&mut db, &c, &p, Some(&cfg), at).unwrap();
        let input = json!({"allowlist":[]});
        let old = json!({
            "decision_id":"owner-archived",
            "decision_owner":"orchestrator",
            "session_binding":crate::runtime::book_binding(&c).unwrap(),
            "portfolio_revision":redecision::portfolio_hash(&p).unwrap(),
            "policy_sha256":digest(&planner::policy_bytes(&cfg).unwrap()),
            "profile_sha256":null,
            "input_hash":digest(&serde_json::to_vec(&input).unwrap()),
            "input":input,
            "verdicts":[],
            "fake":false,
            "dry_run":false
        });
        let tx = db.transaction().unwrap();
        redecision::activate_owner_plan(&tx, &c, &cfg, &p, &old, at).unwrap();
        tx.commit().unwrap();
        assert_eq!(planner::saved_plan(&db, &c).unwrap(), Some(old.clone()));

        let mut settings = super::super::tests::settings(10, 10.0);
        settings.mode = super::super::AgentMode::Paper;
        cfg.agent = Some(settings);
        cfg.digest = Default::default();
        cfg.orchestrator = None;
        cfg.events = None;
        assert_eq!(
            initialize(&mut db, &c, &cfg, &p, at + 1)
                .unwrap_err()
                .to_string(),
            "decision_owner_migration_required"
        );
        assert!(!redecision::has_pi_authority(&db, &c).unwrap());
        assert!(planner::saved_plan(&db, &c).unwrap().is_none());
        let tx = db.transaction().unwrap();
        redecision::apply_config(&tx, &c, &cfg).unwrap();
        tx.commit().unwrap();
        initialize(&mut db, &c, &cfg, &p, at + 2).unwrap();
        assert!(redecision::has_pi_authority(&db, &c).unwrap());
        assert!(planner::saved_plan(&db, &c).unwrap().is_none());
        let archived: String = db
            .query_row(
                "SELECT body FROM planner_plan WHERE id='owner-archived'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&archived).unwrap(), old);
    }
}
