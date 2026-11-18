//! Reviewed operator commands on the active paper book: planner recovery,
//! config apply and resume.
use super::*;

type BoxError = Box<dyn std::error::Error>;

/// The active session book an operator command acts on, held under the
/// sessions-root lock. Declare it before the session lock and the book so it
/// releases last.
struct Active {
    c: Config,
    _root_lock: rollover::ControllerLock,
}

impl Active {
    /// Shared preamble: take the sessions-root lock, resolve the same active book
    /// as the paper controller and refuse a live build with the command's code.
    /// The config directory can be a closed predecessor while
    /// sessions_root/current.json names today's book.
    fn open(c: &Config, live_enabled: impl Into<BoxError>) -> Result<Self> {
        let _root_lock = rollover::controller_lock(c)?;
        let c = rollover::current(c)?;
        c.validate()?;
        if LIVE_ENABLED {
            return Err(live_enabled.into());
        }
        Ok(Self { c, _root_lock })
    }

    fn book_present(&self) -> bool {
        self.c.directory.join("paper.sqlite").is_file()
            && self.c.directory.join("book-identity.json").is_file()
    }

    fn lock_file(&self) -> std::io::Result<fs::File> {
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.c.directory.join("controller.lock"))
    }

    fn load(&self) -> Result<(Connection, Portfolio)> {
        let mut db = open_book(&self.c)?;
        let p = load_wide(&mut db, &self.c)?;
        p.reconcile()?;
        Ok((db, p))
    }
}

/// Take the session controller lock, reporting `busy` when another holder has it.
fn hold(lock: fs::File, busy: impl Into<BoxError>) -> Result<rollover::ControllerLock> {
    lock.try_lock_exclusive().map_err(|_| busy.into())?;
    Ok(rollover::ControllerLock(lock))
}

pub(super) fn resume_preflight(p: &Portfolio) -> std::result::Result<(), &'static str> {
    if LIVE_ENABLED {
        return Err("resume_live_enabled");
    }
    if !p.stopped {
        return Err("resume_not_operator_stopped");
    }
    if p.halted {
        return Err("resume_risk_halted");
    }
    if p.closed {
        return Err("resume_session_closed");
    }
    // Model-led books continue with their holdings and exit intents under protection.
    if !p.positions.is_empty() && !p.model_led {
        return Err("resume_open_positions");
    }
    if pending_orders(p) != 0 {
        return Err("resume_pending_orders");
    }
    Ok(())
}
pub(in crate::runtime) fn planner_recover(c: &Config, confirm: bool) -> Result<()> {
    planner_recover_checked(c, confirm, false, |c, p, cfg| {
        let instruments =
            history_instruments(c).map_err(|_| "planner_recover_input_unavailable")?;
        if !planner::has_eligible_input(c, p, &instruments)? {
            return Err("planner_recover_no_eligible_input".into());
        }
        redecision::news(cfg, c, &instruments)?;
        planner::require_usage(c)
    })
}
/// Reviewed protection-only recovery. The halt stays set, so entries remain
/// blocked by `Portfolio::halted`; held positions continue under surgical exits.
pub(in crate::runtime) fn planner_recover_protection_only(c: &Config, confirm: bool) -> Result<()> {
    planner_recover_checked(c, confirm, true, |_, _, _| Ok(()))
}
pub(super) fn planner_recover_checked(
    c: &Config,
    confirm: bool,
    protection_only: bool,
    revalidate: impl FnOnce(&Config, &Portfolio, &planner::PlannerConfig) -> Result<()>,
) -> Result<()> {
    if !confirm {
        return Err("planner_recover_confirm_paper_review_required".into());
    }
    let active = Active::open(c, "planner_recover_live_enabled")?;
    let c = &active.c;
    if !active.book_present() {
        return Err("planner_recover_book_missing".into());
    }
    let _lock = hold(active.lock_file()?, "planner_recover_controller_busy")?;
    let cfg = planner::PlannerConfig::load_for_session(
        c.planner_config
            .as_deref()
            .ok_or("planner_config_required")?,
        c.session,
    )?;
    let recovery_role = cfg.role(Trigger::Supervisor);
    if !cfg.enabled || !recovery_role.enabled {
        return Err("planner_recover_planner_disabled".into());
    }
    let (mut db, mut p) = active.load()?;
    if p.closed {
        if protection_only {
            return Err("planner_recover_protection_only_requires_open_book".into());
        }
        if !p.halted {
            return Err("planner_recover_halt_not_eligible_book_closed_not_halted".into());
        }
        if !p.positions.is_empty() || pending_orders(&p) != 0 {
            return Err("planner_recover_book_not_flat".into());
        }
        // A carried protective halt may have reached the close flat before the
        // operator cleared it; the same verified evidence still gates the clear.
        let carried_protective = rollover::protective_carry_evidence(c, &db)?.is_some();
        let reviewed_halt = Some(last_halt_reason(&db)?);
        if let Some((reason, _)) = &reviewed_halt
            && reason != "planner_authority_invalid_recovery_required"
            && !(carried_protective && reason == rollover::PROTECTIVE_HEALTH)
        {
            return Err("planner_recover_halt_not_eligible_last_halt_not_planner_authority".into());
        }
        if !redecision::has_authority(&db)? {
            return Err("planner_recover_authority_missing".into());
        }
        let prior = p.clone();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        redecision::recover(&tx, c, &cfg, redecision::RecoveryClearance::RetainIntegrity)?;
        p.halted = false;
        p.health = "closed".into();
        let financial_unchanged = p.cash == prior.cash
            && p.positions == prior.positions
            && p.fills == prior.fills
            && p.high_water == prior.high_water;
        tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_recovery_confirmed_paper_review')",
            rusqlite::params![millis(), serde_json::to_string(&serde_json::json!({"mode":"closed_flat","prior_health":prior.health,"reviewed_halt":reviewed_halt,"financial_state_unchanged":financial_unchanged,"closed":true,"stopped":p.stopped,"halted":false,"carried_protective":carried_protective}))?])?;
        tx.execute(
            "UPDATE state SET body=? WHERE id=1",
            [serde_json::to_string(&p)?],
        )?;
        tx.commit()?;
        println!("planner_recovered_closed_flat_halt_cleared");
        if let Some((reason, source)) = reviewed_halt {
            println!("planner_recovery_reviewed_halt: reason={reason}, source={source}");
        }
        return Ok(());
    }
    if protection_only {
        if !p.halted {
            return Err("planner_recover_protection_only_requires_planner_halt".into());
        }
        let reviewed_halt = Some(last_halt_reason(&db)?);
        if let Some((reason, _)) = &reviewed_halt
            && reason != "planner_authority_invalid_recovery_required"
        {
            return Err("planner_recover_halt_not_eligible_last_halt_not_planner_authority".into());
        }
        if !p.model_led {
            return Err("planner_recover_protection_only_requires_model_led_book".into());
        }
        if p.positions.is_empty() {
            return Err("planner_recover_protection_only_requires_open_position".into());
        }
        if pending_orders(&p) != 0 {
            return Err("planner_recover_pending_orders".into());
        }
        if !redecision::has_authority(&db)? {
            return Err("planner_recover_authority_missing".into());
        }
        let prior = p.clone();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        redecision::recover(&tx, c, &cfg, redecision::RecoveryClearance::RetainIntegrity)?;
        // Halt stays set: entries remain blocked while held positions continue
        // under the frozen protection policy on the next controller start.
        p.stopped = false;
        p.health = "risk_halted_protective_exits_only".into();
        let financial_unchanged = p.cash == prior.cash
            && p.positions == prior.positions
            && p.fills == prior.fills
            && p.high_water == prior.high_water;
        tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_protection_only_confirmed_paper_review')",
            rusqlite::params![millis(), serde_json::to_string(&serde_json::json!({"mode":"protection_only","prior_health":prior.health,"reviewed_halt":reviewed_halt,"financial_state_unchanged":financial_unchanged,"open_positions":p.positions.len(),"entries_blocked":true,"halted":true,"stopped":false}))?])?;
        tx.execute(
            "UPDATE state SET body=? WHERE id=1",
            [serde_json::to_string(&p)?],
        )?;
        tx.commit()?;
        println!("planner_protection_only_active_halt_retained");
        if let Some((reason, source)) = reviewed_halt {
            println!("planner_recovery_reviewed_halt: reason={reason}, source={source}");
        }
        return Ok(());
    }
    // A reviewed halted carry starts a protection-only successor whose halt is
    // cleared only here, and only once the carried position is flat.
    let protective_flat = p.halted && rollover::protective_carry_evidence(c, &db)?.is_some();
    let reviewed_halt = if p.halted {
        Some(last_halt_reason(&db)?)
    } else {
        None
    };
    let reviewed_block = if let Some((reason, _)) = &reviewed_halt {
        if reason == "planner_authority_invalid_recovery_required"
            || (protective_flat && reason == rollover::PROTECTIVE_HEALTH)
        {
            match redecision::recovery_block(&db, c)? {
                redecision::RecoveryBlock::Reviewed(block) => Some(block),
                _ => None,
            }
        } else {
            return Err("planner_recover_halt_not_eligible_last_halt_not_planner_authority".into());
        }
    } else {
        if !p.stopped || p.health != "operator_stopped" {
            return Err("planner_recover_halt_not_eligible_requires_operator_stopped_book".into());
        }
        Some(redecision::reviewed_stopped_block(&db, c)?)
    };
    if !p.positions.is_empty() || pending_orders(&p) != 0 {
        return Err("planner_recover_book_not_flat".into());
    }
    if !redecision::has_authority(&db)? {
        return Err("planner_recover_authority_missing".into());
    }
    // A stopped flat book revalidates every reviewed block. A halted book
    // revalidates only market-data blocks; a configuration block is resolved by
    // the exact rebind and an interrupted claim by exact retirement.
    let inputs_revalidated = if reviewed_halt.is_some() {
        reviewed_block.is_some_and(|block| block.requires_input_revalidation())
    } else {
        reviewed_block.is_some()
    };
    if inputs_revalidated {
        revalidate(c, &p, &cfg)?;
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    redecision::recover(&tx, c, &cfg, redecision::RecoveryClearance::Reviewed)?;
    let prior_health = p.health.clone();
    p.halted = false;
    p.stopped = true;
    p.health = "operator_stopped".into();
    let mode = if protective_flat {
        "flat_carried_protective"
    } else {
        "flat_open"
    };
    let reviewed_block = reviewed_block.map(|block| block.code());
    tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_recovery_confirmed_paper_review')",
        rusqlite::params![millis(), serde_json::to_string(&serde_json::json!({"mode":mode,"prior_health":prior_health,"reviewed_halt":reviewed_halt,"reviewed_block":reviewed_block,"inputs_revalidated":inputs_revalidated,"stopped":true,"entries_blocked_before":protective_flat}))?])?;
    tx.execute(
        "UPDATE state SET body=? WHERE id=1",
        [serde_json::to_string(&p)?],
    )?;
    tx.commit()?;
    println!("planner_recovered_operator_stopped");
    if let Some((reason, source)) = reviewed_halt {
        println!("planner_recovery_reviewed_halt: reason={reason}, source={source}");
    }
    Ok(())
}
pub(in crate::runtime) fn config_apply(c: &Config, confirm: bool) -> Result<()> {
    config_apply_with(c, confirm, None, true)
}
/// Reviewed config transition. `context` is recorded on the audited transition
/// tick for an operator command that names the change beside the policy hashes;
/// `report` prints the CLI line unless the caller owns its own output.
pub(in crate::runtime) fn config_apply_with(
    c: &Config,
    confirm: bool,
    context: Option<serde_json::Value>,
    report: bool,
) -> Result<()> {
    if !confirm {
        return Err("config_apply_confirm_paper_review_required".into());
    }
    let active = Active::open(c, "config_apply_live_enabled")?;
    let c = &active.c;
    // Unlike the other commands, the session lock is taken before the book check.
    let _lock = hold(active.lock_file()?, "config_apply_controller_busy")?;
    if !active.book_present() {
        return Err("config_apply_book_missing".into());
    }
    let cfg = planner::PlannerConfig::load_for_session(
        c.planner_config
            .as_deref()
            .ok_or("planner_config_required")?,
        c.session,
    )?;
    let (mut db, mut p) = active.load()?;
    if p.halted {
        return Err("config_apply_halted_requires_planner_recover".into());
    }
    if p.closed {
        return Err("config_apply_book_closed".into());
    }
    let desired = cfg.model_led.unwrap_or(c.strategy == "model_planner_v1");
    // A continuing model-led book keeps its holdings, cash and exit intents across
    // a planner change; only the planner policy hash moves. Other books stay flat-only.
    let carried_ok = p.model_led && desired;
    if pending_orders(&p) != 0 || (!p.positions.is_empty() && !carried_ok) {
        return Err("config_apply_book_not_flat".into());
    }
    let before = serde_json::to_string(&p)?;
    if desired && c.strategy != "model_planner_v1" {
        return Err("config_apply_model_led_selector_mismatch".into());
    }
    if desired != p.model_led && (!p.fills.is_empty() || p.cash != c.allocation()?) {
        return Err("config_apply_model_led_requires_pristine_book".into());
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    redecision::apply_config(&tx, c, &cfg)?;
    p.model_led = desired;
    p.stopped = true;
    p.health = "operator_stopped".into();
    p.reconcile()?;
    let after = serde_json::to_string(&p)?;
    let mut audit = serde_json::json!({"prior_state_sha256":digest(before.as_bytes()),"current_state_sha256":digest(after.as_bytes()),"configuration_sha256":digest(&serde_json::to_vec(&cfg)?),"model_led":desired,"usage_preserved":true,"stopped":true,"live_enabled":false});
    if let Some(context) = context {
        audit["operator_command"] = context;
    }
    tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_config_apply_confirmed_paper_review')",
        rusqlite::params![millis(), audit.to_string()])?;
    tx.execute("UPDATE state SET body=? WHERE id=1", [&after])?;
    tx.commit()?;
    if report {
        println!("config_applied_operator_stopped_resume_required");
    }
    Ok(())
}
pub(in crate::runtime) fn resume(c: &Config, confirm: bool) -> Result<()> {
    if !confirm {
        return Err(Fail::new("resume_confirm_paper_review_required").into());
    }
    let active = Active::open(c, Fail::new("resume_live_enabled"))?;
    let c = &active.c;
    if !active.book_present() {
        return Err(Fail::new("resume_book_missing").into());
    }
    // A lock file that cannot be opened also reports busy for resume.
    let lock = active
        .lock_file()
        .map_err(|_| Fail::new("resume_controller_busy"))?;
    let _lock = hold(lock, Fail::new("resume_controller_busy"))?;
    let (mut db, mut p) = active.load()?;
    resume_preflight(&p).map_err(Fail::from)?;
    let before = serde_json::to_string(&p)?;
    let planner_cfg = c
        .planner_config
        .as_deref()
        .map(|path| planner::PlannerConfig::load_for_session(path, c.session))
        .transpose()?;
    // Starting with an unapplied planner config halts on planner authority; refuse here.
    if p.model_led
        && let Some(cfg) = &planner_cfg
        && !redecision::config_current(&db, cfg)?
    {
        return Err(Fail::new("resume_planner_config_not_applied").into());
    }
    let migrating = !p.model_led
        && c.strategy == "model_planner_v1"
        && planner_cfg
            .as_ref()
            .is_none_or(|cfg| cfg.model_led != Some(false));
    if migrating {
        if !p.fills.is_empty() || p.cash != c.allocation()? {
            return Err("resume_model_led_requires_pristine_book".into());
        }
        // Keep the serialized config and event authority hash unchanged. This is
        // an audited financial-state transition, not a config/journal replacement.
        redecision::check_model_led_migration(&db, c)?;
        p.model_led = true;
    }
    p.stopped = false;
    p.health = "waiting".into();
    p.reconcile()?;
    let after = serde_json::to_string(&p)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if migrating {
        tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_model_led_enabled_v1')",
            rusqlite::params![millis(), serde_json::json!({"prior_state":serde_json::from_str::<serde_json::Value>(&before)?,"prior_state_sha256":digest(before.as_bytes()),"new_state_sha256":digest(after.as_bytes()),"binding":book_binding(c)?,"financial_values_unchanged":true,"model_led":true}).to_string()])?;
    }
    tx.execute("UPDATE state SET body=? WHERE id=1", [&after])?;
    tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,NULL,'operator_resume_confirmed_paper_review')", [millis()])?;
    tx.commit()?;
    println!(
        "{}",
        serde_json::json!({
            "paper_only": true,
            "live_enabled": LIVE_ENABLED,
            "open_positions": p.positions.len(),
            "pending_orders": 0,
            "stopped": false,
            "halted": false,
            "resumed": true,
            "model_led": p.model_led,
            "audit_event": "operator_resume_confirmed_paper_review",
        })
    );
    Ok(())
}

/// Explicit operator stop. While a controller holds the sessions-root lock this
/// records a private request bound to the active account and session; the
/// controller consumes it on the next SIGTERM and stops the book. With no
/// controller running it stops the active book directly under both locks.
/// It never signals systemd itself.
pub(in crate::runtime) fn stop(c: &Config, confirm: bool, now: i64) -> Result<serde_json::Value> {
    if !confirm {
        return Err(Fail::new("stop_confirm_paper_review_required").into());
    }
    c.validate()?;
    let active = match Active::open(c, Fail::new("stop_live_enabled")) {
        Ok(active) => active,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock) =>
        {
            let current = rollover::current(c)?;
            let request = shutdown::write_request(&current, now)?;
            return Ok(serde_json::json!({
                "paper_only":true,
                "live_enabled":LIVE_ENABLED,
                "controller_running":true,
                "stop_requested":true,
                "account":request.account,
                "session":request.session,
                "requested_at_ms":request.requested_at_ms,
                "expires_at_ms":request.requested_at_ms + shutdown::STOP_REQUEST_TTL_MS,
                "next_step":"systemctl --user stop nivra-paper.service",
            }));
        }
        Err(error) => return Err(error),
    };
    let c = &active.c;
    if !active.book_present() {
        return Err(Fail::new("stop_book_missing").into());
    }
    let lock = active
        .lock_file()
        .map_err(|_| Fail::new("stop_controller_busy"))?;
    let _lock = hold(lock, Fail::new("stop_controller_busy"))?;
    let (mut db, mut p) = active.load()?;
    if p.closed {
        return Err(Fail::new("stop_session_closed").into());
    }
    let already = p.stopped;
    if !already {
        let intent = shutdown::Intent {
            kind: shutdown::ShutdownKind::Operator,
            request: serde_json::json!({"status":"direct","controller_running":false}),
            history: serde_json::Value::Null,
        };
        shutdown::settle(&mut db, c, &mut p, &intent, None, now)?;
    }
    Ok(serde_json::json!({
        "paper_only":true,
        "live_enabled":LIVE_ENABLED,
        "controller_running":false,
        "session":c.session,
        "stopped":true,
        "already_stopped":already,
        "halted":p.halted,
        "audit_event":"operator_stopped",
        "next_step":"nivra resume --config PAPER_CONFIG --confirm-paper-review",
    }))
}
