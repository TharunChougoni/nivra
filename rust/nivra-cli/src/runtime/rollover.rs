//! Model-led paper session rollover with explicit original-config migration.
use super::phases::{IdleStatus, PreviewStatus, SealVerdict};
use super::*;
use chrono::Datelike;
use nivra_cli::lock::AdvisoryLock;
use nivra_core::book_phase::BookPhase;
use nivra_core::execution::Portfolio;
use serde_json::{Value, json};

pub(super) fn root(c: &Config) -> Result<PathBuf> {
    match &c.sessions_root {
        Some(p) if p.is_absolute() => Ok(p.clone()),
        Some(_) => Err(Fail::new("sessions_root_must_be_absolute").into()),
        None => Ok(PathBuf::from(std::env::var("HOME")?).join("nivra-data/sessions")),
    }
}
pub(super) fn atomic(path: &Path, value: &Value) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    std::io::Write::write_all(&mut f, &serde_json::to_vec_pretty(value)?)?;
    f.sync_all()?;
    fs::rename(tmp, path)?;
    fs::File::open(path.parent().ok_or("session_parent")?)?.sync_all()?;
    Ok(())
}
pub(super) fn current(c: &Config) -> Result<Config> {
    let pointer = root(c)?.join("current.json");
    if !pointer.exists() {
        return Ok(c.clone());
    }
    let v: Value = serde_json::from_slice(&fs::read(pointer)?)?;
    let date: NaiveDate = serde_json::from_value(v["session"].clone())?;
    if v["account"] != c.account || date < c.session {
        return Err(Fail::new("session_pointer_identity").into());
    }
    let mut next = c.clone();
    next.legacy_directory = Some(
        c.legacy_directory
            .clone()
            .unwrap_or_else(|| c.directory.clone()),
    );
    next.session = date;
    next.previous_session = c.next_sessions(date)?.1;
    next.directory = root(c)?.join(date.to_string());
    let saved: Value = serde_json::from_slice(
        &crate::read_bounded(&next.directory.join("strategy-decision.json"), 1_048_576)
            .map_err(|_| Fail::new("session_decision_missing"))?,
    )?;
    verify_decision(&next, &saved)?;
    Ok(next)
}
// Explicit migration: the previous config is evidence, never a replacement binding.
pub(super) fn original_config(c: &Config) -> Result<Config> {
    if let Some(transition) = c
        .paper_cost_transition
        .as_ref()
        .filter(|t| t.source_session == c.session)
    {
        if c.prior_config.is_some() {
            return Err("paper_cost_transition_conflicting_prior_config".into());
        }
        let old: Config = serde_json::from_slice(
            &crate::read_bounded(&transition.prior_config, 65536)
                .map_err(|_| "prior_config_unreadable")?,
        )?;
        let mut old_identity = serde_json::to_value(&old)?;
        let mut new_identity = serde_json::to_value(c)?;
        old_identity
            .as_object_mut()
            .ok_or("paper_cost_transition_invalid")?
            .remove("paper_costs");
        new_identity
            .as_object_mut()
            .ok_or("paper_cost_transition_invalid")?
            .remove("paper_costs");
        if old_identity != new_identity
            || old.paper_costs == c.paper_costs
            || c.paper_costs.is_none()
        {
            return Err("paper_cost_transition_not_cost_only".into());
        }
        let journal = fs::symlink_metadata(&c.read_journal)?;
        if !journal.is_file()
            || journal.dev() != c.read_journal_device
            || journal.ino() != c.read_journal_inode
        {
            return Err("prior_config_journal_identity_mismatch".into());
        }
        let db = Connection::open_with_flags(
            c.directory.join("paper.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let body: String = db.query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))?;
        let portfolio: Portfolio = serde_json::from_str(&body)?;
        portfolio.reconcile()?;
        if !portfolio.closed
            || portfolio.stopped
            || portfolio.halted
            || !portfolio.model_led
            || portfolio.costs != old.paper_costs
        {
            return Err("paper_cost_transition_requires_closed_healthy_source".into());
        }
        return Ok(old);
    }
    let Some(path) = &c.prior_config else {
        return Ok(c.clone());
    };
    // Only the original book may use this evidence; dated successors use their own config.
    if c.legacy_directory
        .as_ref()
        .is_some_and(|d| d != &c.directory)
    {
        return Ok(c.clone());
    }
    let old: Config = serde_json::from_slice(
        &crate::read_bounded(path, 65536).map_err(|_| Fail::new("prior_config_unreadable"))?,
    )?;
    if old.account != c.account
        || old.session != c.session
        || old.directory != c.directory
        || old.previous_session != c.previous_session
        || old.calendar_source != c.calendar_source
        || old.read_journal != c.read_journal
        || old.read_scope != c.read_scope
        || old.read_journal_device != c.read_journal_device
        || old.read_journal_inode != c.read_journal_inode
        || old.token_file != c.token_file
        || old.dashboard_token_file != c.dashboard_token_file
    {
        return Err(Fail::new("prior_config_identity_mismatch").into());
    }
    // The saved config must still refer to the original shared read journal.
    let journal = fs::symlink_metadata(&c.read_journal)?;
    if !journal.is_file()
        || journal.dev() != c.read_journal_device
        || journal.ino() != c.read_journal_inode
    {
        return Err(Fail::new("prior_config_journal_identity_mismatch").into());
    }
    let db = Connection::open_with_flags(
        c.directory.join("paper.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let body: String = db.query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))?;
    let (open, _, halted, closed) = book_flags(&body)?;
    if open != 0 || halted || !closed {
        return Err(Fail::new("migration_requires_closed_flat_non_halted_book").into());
    }
    Ok(old)
}
fn migration_evidence(c: &Config, day: NaiveDate) -> Result<Value> {
    let cost_transition = c
        .paper_cost_transition
        .as_ref()
        .is_some_and(|t| t.source_session == c.session);
    if c.prior_config.is_none() && !cost_transition {
        return Ok(Value::Null);
    }
    let old = original_config(c)?;
    let mut successor = c.clone();
    successor.session = day;
    successor.previous_session = c.next_sessions(day)?.1;
    successor.directory = root(c)?.join(day.to_string());
    let kind = if cost_transition {
        "explicit_paper_cost_transition"
    } else {
        "explicit_prior_config"
    };
    let result = json!({
        "kind": kind,
        "old_binding": digest(&serde_json::to_vec(&old)?),
        "new_binding": digest(&serde_json::to_vec(&successor)?),
        "original_state_preserved": true
    });
    Ok(result)
}
pub(super) fn bound_state(c: &Config) -> Result<(Connection, String)> {
    let path = c.directory.join("paper.sqlite");
    let m = fs::symlink_metadata(&path)?;
    let anchor: Value = serde_json::from_slice(&fs::read(c.directory.join("book-identity.json"))?)?;
    let original = original_config(c)?;
    let binding = digest(&serde_json::to_vec(&original)?);
    if !m.is_file()
        || anchor["device"] != m.dev()
        || anchor["inode"] != m.ino()
        || anchor["binding"] != binding
    {
        return Err(Fail::new("rollover_book_identity_mismatch").into());
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let (bound, body): (String, String) =
        db.query_row("SELECT binding,body FROM state WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    if bound != binding {
        return Err(Fail::new("rollover_state_identity_mismatch").into());
    }
    Ok((db, body))
}
/// Predecessor state read for carry verification. The source book is CLOSED;
/// opening it with `immutable=1` avoids creating or touching its `-wal`/`-shm`
/// sidecars. A non-empty WAL is refused by the immutable opener.
pub(super) fn bound_state_immutable(c: &Config) -> Result<(Connection, String)> {
    let path = c.directory.join("paper.sqlite");
    let m = fs::symlink_metadata(&path)?;
    let anchor: Value = serde_json::from_slice(&fs::read(c.directory.join("book-identity.json"))?)?;
    let original = original_config(c)?;
    let binding = digest(&serde_json::to_vec(&original)?);
    if !m.is_file()
        || anchor["device"] != m.dev()
        || anchor["inode"] != m.ino()
        || anchor["binding"] != binding
    {
        return Err(Fail::new("rollover_book_identity_mismatch").into());
    }
    let db = super::open_book_immutable(&path)?;
    let (bound, body): (String, String) =
        db.query_row("SELECT binding,body FROM state WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    if bound != binding {
        return Err(Fail::new("rollover_state_identity_mismatch").into());
    }
    Ok((db, body))
}

fn book_flags(body: &str) -> Result<(u64, bool, bool, bool)> {
    let flags = if let Ok(p) = serde_json::from_str::<Portfolio>(body) {
        p.reconcile()?;
        (p.positions.len() as u64, p.stopped, p.halted, p.closed)
    } else {
        let b: Book = serde_json::from_str(body)?;
        b.reconcile()?;
        (b.units, b.stopped, b.halted, b.closed)
    };
    Ok(flags)
}
const STOPPED_CARRY_FILE: &str = "stopped-carry-acceptance.json";
const STOPPED_CARRY_REASON: &str = "operator_accepted_closed_stopped_carry";

fn stopped_carry_evidence(c: &Config, db: &Connection, body: &str) -> Result<Value> {
    let p: Portfolio = serde_json::from_str(body)?;
    p.reconcile()?;
    if LIVE_ENABLED
        || c.strategy != "model_planner_v1"
        || !p.model_led
        || !p.closed
        || !p.stopped
        || p.halted
        || p.positions.is_empty()
    {
        return Err("stopped_carry_requires_closed_stopped_non_halted_model_led_positions".into());
    }
    let operator_stop: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM ticks WHERE event='operator_stopped')",
        [],
        |r| r.get(0),
    )?;
    if !operator_stop {
        return Err("stopped_carry_operator_stop_evidence_missing".into());
    }
    let report = matching_report(c, body)?;
    let reported: Value = serde_json::from_slice(&report)?;
    if reported["live_enabled"] != false {
        return Err("stopped_carry_requires_paper_report".into());
    }
    let seal: Value = serde_json::from_slice(
        &crate::read_bounded(&c.directory.join("review.json"), 1_048_576)
            .map_err(|_| "stopped_carry_review_unavailable")?,
    )?;
    if seal["session"] != json!(c.session)
        || seal["closed"] != true
        || seal["state_hash"] != digest(body.as_bytes())
        || seal["report_hash"] != digest(&report)
    {
        return Err("stopped_carry_review_binding_changed".into());
    }
    let planner = redecision::stopped_carry_evidence(db, c)?;
    Ok(json!({"binding":book_binding(c)?,"session":c.session,
        "state_hash":digest(body.as_bytes()),"report_hash":digest(&report),
        "planner":planner,"live_enabled":false}))
}
fn accepted_stopped_carry(c: &Config, db: &Connection, body: &str) -> Result<Option<Value>> {
    let path = c.directory.join(STOPPED_CARRY_FILE);
    if !path.try_exists()? {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(&private(&path)?)?;
    if value["reason"] != STOPPED_CARRY_REASON
        || value["accepted_at_ms"].as_i64().is_none()
        || value["evidence"] != stopped_carry_evidence(c, db, body)?
    {
        return Err("stopped_carry_acceptance_binding_changed".into());
    }
    Ok(Some(value))
}
fn accept_stopped_carry(c: &Config, confirmed: bool, now: i64) -> Result<Value> {
    if !confirmed {
        return Err("stopped_carry_confirm_paper_review_required".into());
    }
    c.validate()?;
    let _root_lock = controller_lock(c)?;
    let c = current(c)?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(c.directory.join("controller.lock"))?;
    lock.try_lock_exclusive()?;
    let _lock = AdvisoryLock::new(lock);
    let (db, body) = bound_state(&c)?;
    if let Some(previous) = accepted_stopped_carry(&c, &db, &body)? {
        return Ok(previous);
    }
    let value = json!({"reason":STOPPED_CARRY_REASON,"accepted_at_ms":now,
        "evidence":stopped_carry_evidence(&c, &db, &body)?,"original_state_preserved":true});
    // A single append-only audit receipt: never truncate or replace an acceptance.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(c.directory.join(STOPPED_CARRY_FILE))?;
    std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(&value)?)?;
    file.sync_all()?;
    fs::File::open(&c.directory)?.sync_all()?;
    Ok(value)
}
pub fn accept_stopped_carry_entry(path: &Path, confirmed: bool) -> ExitCode {
    let result = (|| -> Result<Value> {
        let c: Config = serde_json::from_slice(&fs::read(path)?)?;
        accept_stopped_carry(&c, confirmed, millis())
    })();
    match result {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "stopped_carry_refused_original_state_preserved: {}",
                failure_code(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}
const HALTED_CARRY_FILE: &str = "halted-carry-acceptance.json";
const HALTED_CARRY_REASON: &str = "operator_accepted_closed_halted_protective_carry";
/// Protections-only halt health carried into the successor book.
pub(super) const PROTECTIVE_HEALTH: &str = "risk_halted_protective_exits_only";
const PROTECTIVE_CARRY_TICK: &str = "operator_protective_carry_adopted";

/// A closed, halted, non-flat model-led book may reach the next session only
/// through this reviewed acceptance. The halt, positions and original evidence
/// stay untouched; the successor runs protection-only with entries blocked.
fn halted_carry_evidence(c: &Config, db: &Connection, body: &str) -> Result<Value> {
    let p: Portfolio = serde_json::from_str(body)?;
    p.reconcile()?;
    if LIVE_ENABLED
        || c.strategy != "model_planner_v1"
        || !p.model_led
        || !p.closed
        || !p.halted
        || p.stopped
        || p.positions.is_empty()
    {
        return Err("halted_carry_requires_closed_halted_non_stopped_model_led_positions".into());
    }
    let (halt_reason, halt_source) = wide::last_halt_reason(db)?;
    let report = matching_report(c, body)?;
    let reported: Value = serde_json::from_slice(&report)?;
    if reported["live_enabled"] != json!(false) {
        return Err("halted_carry_requires_paper_report".into());
    }
    let seal: Value = serde_json::from_slice(
        &crate::read_bounded(&c.directory.join("review.json"), 1_048_576)
            .map_err(|_| "halted_carry_review_unavailable")?,
    )?;
    if seal["session"] != json!(c.session)
        || seal["closed"] != true
        || seal["state_hash"] != digest(body.as_bytes())
        || seal["report_hash"] != digest(&report)
    {
        return Err("halted_carry_review_binding_changed".into());
    }
    let planner = redecision::halted_carry_evidence(db, c)?;
    Ok(json!({"binding":book_binding(c)?,"session":c.session,
        "state_hash":digest(body.as_bytes()),"report_hash":digest(&report),
        "halt_reason":halt_reason,"halt_source":halt_source,"health":p.health,
        "open_positions":p.positions.len(),"planner":planner,"live_enabled":false}))
}
fn accepted_halted_carry(c: &Config, db: &Connection, body: &str) -> Result<Option<Value>> {
    let path = c.directory.join(HALTED_CARRY_FILE);
    if !path.try_exists()? {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(&private(&path)?)?;
    if value["reason"] != HALTED_CARRY_REASON
        || value["accepted_at_ms"].as_i64().is_none()
        || value["evidence"] != halted_carry_evidence(c, db, body)?
    {
        return Err("halted_carry_acceptance_binding_changed".into());
    }
    Ok(Some(value))
}
fn accept_halted_carry(c: &Config, confirmed: bool, now: i64) -> Result<Value> {
    if !confirmed {
        return Err("halted_carry_confirm_paper_review_required".into());
    }
    c.validate()?;
    let _root_lock = controller_lock(c)?;
    let c = current(c)?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(c.directory.join("controller.lock"))?;
    lock.try_lock_exclusive()?;
    let _lock = AdvisoryLock::new(lock);
    let (db, body) = bound_state(&c)?;
    if let Some(previous) = accepted_halted_carry(&c, &db, &body)? {
        return Ok(previous);
    }
    let value = json!({"reason":HALTED_CARRY_REASON,"accepted_at_ms":now,
        "evidence":halted_carry_evidence(&c, &db, &body)?,"original_state_preserved":true});
    // A single append-only audit receipt: never truncate or replace an acceptance.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(c.directory.join(HALTED_CARRY_FILE))?;
    std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(&value)?)?;
    file.sync_all()?;
    fs::File::open(&c.directory)?.sync_all()?;
    Ok(value)
}
pub fn accept_halted_carry_entry(path: &Path, confirmed: bool) -> ExitCode {
    let result = (|| -> Result<Value> {
        let c: Config = serde_json::from_slice(&fs::read(path)?)?;
        accept_halted_carry(&c, confirmed, millis())
    })();
    match result {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "halted_carry_refused_original_state_preserved: {}",
                failure_code(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}
/// Exact successor evidence for the accepted halt. Recomputed on verification.
fn halted_carry_over(receipt: &Value) -> Result<Value> {
    Ok(json!({"reason":HALTED_CARRY_REASON,
        "acceptance_sha256":digest(&serde_json::to_vec(receipt)?),
        "halt_reason":receipt["evidence"]["halt_reason"],
        "halt_source":receipt["evidence"]["halt_source"],
        "health":PROTECTIVE_HEALTH,"halted":true,"entries_blocked":true}))
}
/// Record the adopted protection-only carry once, under the controller lock.
pub(super) fn record_protective_carry(db: &mut Connection, carry: &Value, at: i64) -> Result<()> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM ticks WHERE event=?)",
        [PROTECTIVE_CARRY_TICK],
        |r| r.get(0),
    )?;
    if exists {
        return Ok(());
    }
    let evidence = json!({"mode":"protection_only","source_session":carry["source_session"],
        "source_state_sha256":carry["source_state_hash"],
        "acceptance_sha256":carry["halted_carry"]["acceptance_sha256"],
        "halt_reason":carry["halted_carry"]["halt_reason"],
        "halt_source":carry["halted_carry"]["halt_source"],
        "entries_blocked":true,"halted":true,"paper_only":true,"live_enabled":false});
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,?)",
        rusqlite::params![at, serde_json::to_string(&evidence)?, PROTECTIVE_CARRY_TICK],
    )?;
    Ok(())
}
/// Read-only proof for the reviewed clear of a carried protective halt: the
/// successor carry survives revalidation and its adoption tick matches exactly.
pub(super) fn protective_carry_evidence(c: &Config, db: &Connection) -> Result<Option<Value>> {
    let path = c.directory.join("carry-over.json");
    if !path.try_exists()? {
        return Ok(None);
    }
    let value: Value = serde_json::from_slice(
        &crate::read_bounded(&path, 16_000_000).map_err(|_| "protective_carry_unreadable")?,
    )?;
    let Some(halted) = value.get("halted_carry").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    verify_carry(c, &value)?;
    use rusqlite::OptionalExtension;
    let recorded: Option<String> = db
        .query_row(
            "SELECT body FROM ticks WHERE event=? ORDER BY id DESC LIMIT 1",
            [PROTECTIVE_CARRY_TICK],
            |r| r.get(0),
        )
        .optional()?;
    let Some(recorded) = recorded else {
        return Ok(None);
    };
    let recorded: Value = serde_json::from_str(&recorded)?;
    if recorded["source_session"] != value["source_session"]
        || recorded["source_state_sha256"] != value["source_state_hash"]
        || recorded["acceptance_sha256"] != halted["acceptance_sha256"]
        || recorded["entries_blocked"] != true
        || recorded["halted"] != true
    {
        return Err("protective_carry_evidence_changed".into());
    }
    Ok(Some(halted.clone()))
}
pub(super) fn review(c: &Config) -> Result<Value> {
    let (db, body) = bound_state(c)?;
    let (open, stopped, halted, closed) = book_flags(&body)?;
    if closed
        && open == 0
        && !halted
        && c.directory.join("review.json").exists()
        && let Some(ack) = acknowledged_review(c, &body)?
    {
        return Ok(ack);
    }
    let acceptance = accepted_stopped_carry(c, &db, &body)?;
    let halted_acceptance = accepted_halted_carry(c, &db, &body)?;
    let halted_admitted = halted_acceptance.is_some();
    // The sealed pre-stop report must prove the same already-closed flat book.
    // Accepting rollover never clears stopped or reopens this original state.
    let accepted_stop =
        stopped && closed && open == 0 && !halted && sealed_before_stop(c, &body).unwrap_or(false);
    let model_led = serde_json::from_str::<Portfolio>(&body).is_ok_and(|p| p.model_led);
    // A halted carry keeps the halt: only an explicitly reviewed acceptance can
    // admit a closed non-flat halted book, and a stopped halted book never can.
    let phase = BookPhase::from_flags(stopped, halted, closed);
    let carry = c.strategy == "model_planner_v1"
        && (!phase.stopped() || acceptance.is_some())
        && (!phase.halted() || halted_admitted)
        && !phase.carry_blocked()
        && phase.closed()
        && model_led;
    let verdict = SealVerdict::from_review(
        open,
        carry,
        phase.halted() && !halted_admitted,
        phase.stopped() && !accepted_stop && acceptance.is_none(),
    );
    let report = c.directory.join("report.json");
    let mut result = json!({"status":verdict.as_str(),"session":c.session,"open_unresolved":if carry {0}else{open},"carried_positions":if carry {open}else{0},"model_led":model_led,"carry_admitted":carry,"stopped":stopped,"halted":halted,"closed":closed,"state_hash":digest(body.as_bytes()),"report_hash":if report.exists(){Some(digest(&fs::read(report)?))}else{None}});
    if let Some(acceptance) = acceptance {
        result["reason"] = json!(STOPPED_CARRY_REASON);
        result["acceptance_hash"] = json!(digest(&serde_json::to_vec(&acceptance)?));
    }
    if let Some(acceptance) = halted_acceptance {
        result["reason"] = json!(HALTED_CARRY_REASON);
        result["halted_carry_admitted"] = json!(true);
        result["halt_reason"] = acceptance["evidence"]["halt_reason"].clone();
        result["acceptance_hash"] = json!(digest(&serde_json::to_vec(&acceptance)?));
    }
    if accepted_stop {
        use rusqlite::OptionalExtension;
        let stop: Option<(i64, String)> = db.query_row(
            "SELECT at,event FROM ticks WHERE event IN ('operator_stopped','stopped') ORDER BY id DESC LIMIT 1",
            [], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        let state: Value = serde_json::from_str(&body)?;
        result["reason"] = json!("stop_after_close_flat_book_accepted");
        result["stop_reason"] = stop
            .as_ref()
            .map(|(_, reason)| json!(reason))
            .unwrap_or_else(|| state["health"].clone());
        result["stop_at_ms"] = json!(stop.map(|(at, _)| at));
    }
    Ok(result)
}
fn sealed_before_stop(c: &Config, body: &str) -> Result<bool> {
    let bytes = crate::read_bounded(&c.directory.join("report.json"), 16_000_000)
        .map_err(|_| "report_unreadable")?;
    let seal: Value = serde_json::from_slice(&fs::read(c.directory.join("review.json"))?)?;
    if seal["session"] != json!(c.session)
        || seal["closed"] != true
        || seal["open_unresolved"] != 0
        || seal["report_hash"] != digest(&bytes)
    {
        return Ok(false);
    }
    let report: Value = serde_json::from_slice(&bytes)?;
    let mut prior = report
        .get("portfolio")
        .or_else(|| report.get("book"))
        .ok_or("report_book_missing")?
        .clone();
    if report["account"] != c.account
        || report["session"] != json!(c.session)
        || report["paper_only"] != true
        || prior["closed"] != true
        || prior["halted"] != false
        || prior["stopped"] != false
    {
        return Ok(false);
    }
    let mut current: Value = serde_json::from_str(body)?;
    for value in [&mut prior, &mut current] {
        let object = value.as_object_mut().ok_or("report_book_invalid")?;
        object.remove("stopped");
        object.remove("health");
    }
    Ok(prior == current)
}
const ACK_REASON: &str = "operator_acknowledged_closed_stopped_book";
fn acknowledged_body(body: &str, ack: &Value) -> Result<String> {
    if ack["state_hash"] == digest(body.as_bytes()) {
        return Ok(body.to_owned());
    }
    Err(Fail::new("acknowledgment_binding_changed").into())
}
fn matching_report(c: &Config, body: &str) -> Result<Vec<u8>> {
    let bytes = crate::read_bounded(&c.directory.join("report.json"), 16_000_000)
        .map_err(|_| Fail::new("acknowledge_report_unavailable"))?;
    let report: Value = serde_json::from_slice(&bytes)?;
    let state: Value = serde_json::from_str(body)?;
    let reported = report.get("portfolio").or_else(|| report.get("book"));
    if report["paper_only"] != true
        || report["account"] != c.account
        || report["session"] != json!(c.session)
        || reported != Some(&state)
    {
        return Err(Fail::new("acknowledge_report_state_mismatch").into());
    }
    Ok(bytes)
}
fn acknowledged_review(c: &Config, body: &str) -> Result<Option<Value>> {
    let bytes = crate::read_bounded(&c.directory.join("review.json"), 1_048_576)
        .map_err(|_| "acknowledgment_unavailable")?;
    let ack: Value = serde_json::from_slice(&bytes)?;
    if ack["reason"] != ACK_REASON {
        return Ok(None);
    }
    let sealed_body = acknowledged_body(body, &ack)?;
    if ack["status"] != SealVerdict::EligibleToSeal.as_str()
        || ack["session"] != json!(c.session)
        || ack["closed"] != true
        || ack["open_unresolved"] != 0
        || ack["halted"] != false
        || ack["acknowledged_at_ms"].as_i64().is_none()
        || ack["state_hash"] != digest(sealed_body.as_bytes())
        || ack["report_hash"] != digest(&matching_report(c, &sealed_body)?)
    {
        return Err(Fail::new("acknowledgment_binding_changed").into());
    }
    Ok(Some(ack))
}
fn acknowledge(c: &Config, confirmed: bool, now: i64) -> Result<Value> {
    if !confirmed {
        return Err(Fail::new("acknowledge_confirm_paper_review_required").into());
    }
    let mut mandate = c.clone();
    if mandate.strategy == "deterministic_momentum_v1" {
        mandate.strategy = "model_abstain".into();
    }
    mandate.validate()?;
    // Serialize with rollover and with writers/resume of the selected original book.
    let _root_lock = controller_lock(c)?;
    let c = current(c)?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(c.directory.join("controller.lock"))?;
    lock.try_lock_exclusive()?;
    let _lock = AdvisoryLock::new(lock);
    let (_db, body) = bound_state(&c)?;
    let (open, stopped, halted, closed) = book_flags(&body)?;
    if LIVE_ENABLED {
        return Err(Fail::new("acknowledge_live_enabled").into());
    }
    if !closed {
        return Err(Fail::new("acknowledge_book_not_closed").into());
    }
    if open != 0 {
        return Err(Fail::new("acknowledge_book_has_open_positions").into());
    }
    if halted {
        return Err(Fail::new("acknowledge_book_halted").into());
    }
    let report = matching_report(&c, &body)?;
    let path = c.directory.join("review.json");
    if path.exists() {
        // Preserve the first acknowledgment timestamp. Changed bindings require inspection.
        if let Some(previous) = acknowledged_review(&c, &body)? {
            return Ok(previous);
        }
    }
    let state: Value = serde_json::from_str(&body)?;
    let ack = json!({"status":SealVerdict::EligibleToSeal.as_str(),"reason":ACK_REASON,"session":c.session,
        "closed":closed,"open_unresolved":open,"stopped":stopped,"halted":halted,
        "acknowledged_at_ms":now,"report_hash":digest(&report),"state_hash":digest(body.as_bytes()),
        "stop_health":state["health"],"model_led":state["model_led"].as_bool().unwrap_or(false),"carry_admitted":false,"live_enabled":false});
    atomic(&path, &ack)?;
    Ok(ack)
}
pub fn acknowledge_entry(path: &Path, confirmed: bool) -> ExitCode {
    let result = (|| -> Result<Value> {
        let bytes = fs::read(path).map_err(|_| Fail::new("config_unreadable"))?;
        let c: Config = serde_json::from_slice(&bytes).map_err(|_| Fail::new("config_invalid"))?;
        acknowledge(&c, confirmed, millis())
    })();
    match result {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "closed_book_acknowledgment_refused_original_state_preserved: {}",
                failure_code(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}
fn verify_decision(c: &Config, v: &Value) -> Result<()> {
    if v["reviewer"] != "none_model_led"
        || v["live_enabled"] != false
        || v["chosen"]["strategy_id"] != "model_led_v1"
        || v["chosen"]["execution_selector"] != c.strategy
        || v["capital"] != json!(c.allocation()?)
        || v["chosen"]["capital"] != v["capital"]
    {
        return Err(Fail::new("session_decision_model_led_mismatch").into());
    }
    Ok(())
}
fn decision(c: &Config, old: &Value, day: NaiveDate) -> Result<Value> {
    Ok(json!({"reviewer":"none_model_led","live_enabled":false,
        "chosen":{"strategy_id":"model_led_v1","execution_selector":c.strategy,"capital":c.allocation()?},
        "capital":c.allocation()?,"carry_required":old.get("carry_admitted").unwrap_or(&old["model_led_account"])==true,"status":"model_plan_unavailable_no_trades",
        "prior_review_hash":digest(&serde_json::to_vec(old)?),
        "migration":migration_evidence(c, day)?}))
}

pub(super) fn preview(c: &Config, now: i64) -> Result<Value> {
    let day = local(now).date_naive();
    if !c.is_trading_day(day)? {
        return Ok(
            json!({"status":PreviewStatus::from_calendar(day.weekday().number_from_monday()>5).as_str(),"live_enabled":false}),
        );
    }
    if day <= c.session {
        return Ok(json!({"status":PreviewStatus::CurrentSession.as_str(),"session":c.session}));
    }
    let old = review(c)?;
    Ok(
        json!({"status":PreviewStatus::from_review(old["status"].as_str().and_then(|status| SealVerdict::parse(status).ok()), minute(now)).as_str(),"session":day,"previous_session":c.next_sessions(day)?.1,"session_dir":root(c)?.join(day.to_string()),"review":old,"strategy_decision":decision(c,&old,day)?,"live_enabled":false}),
    )
}
/// Idle/blocked status shown when no rollover advance happened. A passed
/// pre-open window is a normal wait for the next session, not a review blocker
/// (#159): the dashboard must say so instead of demanding inspection.
pub(super) fn idle_status(c: &Config, day: NaiveDate, now: i64) -> Result<&'static str> {
    if day.weekday().number_from_monday() > 5 {
        return Ok(IdleStatus::WeekendIdle.as_str());
    }
    if !c.is_trading_day(day)? {
        return Ok(IdleStatus::HolidayIdle.as_str());
    }
    if preview(c, now).ok().is_some_and(|value| {
        value["status"]
            .as_str()
            .and_then(|status| PreviewStatus::parse(status).ok())
            == Some(PreviewStatus::MissedPreopenIdle)
    }) {
        return Ok(IdleStatus::MissedPreopenIdle.as_str());
    }
    Ok(IdleStatus::RolloverBlockedInspectReviewHumanRequired.as_str())
}

/// Carry evidence binds the untouched predecessor body, not a new allocation.
pub(super) fn verify_carry(c: &Config, value: &Value) -> Result<()> {
    verify_carry_with(c, value, false)
}

/// Strict predecessor verification for cross-session continuation. A source
/// book with a non-empty WAL is refused: an immutable read would be stale and a
/// normal WAL read would create predecessor sidecars.
pub(super) fn verify_carry_immutable(c: &Config, value: &Value) -> Result<()> {
    verify_carry_with(c, value, true)
}

fn verify_carry_with(c: &Config, value: &Value, immutable: bool) -> Result<()> {
    if value["destination_binding"] != book_binding(c)? {
        return Err("carry_destination_binding_changed".into());
    }
    let source: NaiveDate = serde_json::from_value(value["source_session"].clone())?;
    if source >= c.session {
        return Err("carry_source_session".into());
    }
    let mut prior = c.clone();
    prior.session = source;
    prior.previous_session = serde_json::from_value(value["source_previous_session"].clone())?;
    prior.directory = if let Some(legacy) = &c.legacy_directory {
        let saved: Option<Value> = fs::read(legacy.join("report.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        if saved
            .as_ref()
            .is_some_and(|v| v["session"] == source.to_string())
        {
            legacy.clone()
        } else {
            root(c)?.join(source.to_string())
        }
    } else {
        root(c)?.join(source.to_string())
    };
    let (db, body) = if immutable {
        bound_state_immutable(&prior)?
    } else {
        // A live/uncheckpointed source WAL OR SHM must be read through SQLite.
        // A clean closed source is read immutably so no `-wal`/`-shm` sidecar is
        // made. This is the non-immutable carry-verification path; the strict
        // predecessor reader still refuses a non-empty sidecar.
        let book = prior.directory.join("paper.sqlite");
        let sidecar = ["-wal", "-shm"].iter().any(|suffix| {
            let path = PathBuf::from(format!("{}{suffix}", book.display()));
            fs::symlink_metadata(&path)
                .map(|metadata| metadata.len() > 0)
                .unwrap_or(false)
        });
        if sidecar {
            bound_state(&prior)?
        } else {
            bound_state_immutable(&prior)?
        }
    };
    if value["source_binding"] != book_binding(&prior)?
        || value["source_state_hash"] != digest(body.as_bytes())
    {
        return Err("carry_source_changed".into());
    }
    let mut p: Portfolio = serde_json::from_str(&body)?;
    if let Some(receipt) = accepted_halted_carry(&prior, &db, &body)? {
        // Halt stays sticky: only the reviewed protection-only successor is admitted.
        if p.stopped || !p.halted || !p.closed || !p.model_led || p.positions.is_empty() {
            return Err("carry_source_terminal".into());
        }
        p.stopped = false;
        p.closed = false;
        p.health = PROTECTIVE_HEALTH.into();
        if value["portfolio"] != serde_json::to_value(&p)? {
            return Err("carry_state_changed".into());
        }
        if value["halted_carry"] != halted_carry_over(&receipt)? {
            return Err("carry_state_changed".into());
        }
        return Ok(());
    }
    let accepted = accepted_stopped_carry(&prior, &db, &body)?.is_some();
    if (p.stopped && !accepted) || p.halted || !p.closed || !p.model_led {
        return Err("carry_source_terminal".into());
    }
    if !value["halted_carry"].is_null() {
        return Err("carry_state_changed".into());
    }
    p.stopped = false;
    p.closed = false;
    p.health = "carried_waiting_for_fresh_quotes".into();
    if value["portfolio"] != serde_json::to_value(p)? {
        return Err("carry_state_changed".into());
    }
    Ok(())
}
pub(super) fn advance(c: &Config, now: i64) -> Result<Option<Config>> {
    advance_checked(c, now, false)
}

pub(super) fn advance_pi(c: &Config, now: i64) -> Result<Option<Config>> {
    advance_checked(c, now, true)
}

fn configured_for_pi(c: &Config, session: NaiveDate) -> bool {
    c.planner_config
        .as_deref()
        .and_then(|path| planner::PlannerConfig::load_for_session(path, session).ok())
        .is_some_and(|cfg| cfg.uses_pi_paper())
}

fn advance_checked(c: &Config, now: i64, require_pi: bool) -> Result<Option<Config>> {
    let mut plan = preview(c, now)?;
    let source = if plan["status"] == "rollover_ready" {
        // Check the dated successor before changing the original review, the
        // successor directory or the current pointer. Read the original policy
        // receipt as well because the selected planner file can change.
        let (db, body) = bound_state(c)?;
        let target = serde_json::from_value(plan["session"].clone())?;
        if (require_pi || redecision::has_pi_authority(&db, c)? || configured_for_pi(c, c.session))
            && !configured_for_pi(c, target)
        {
            return Err(Fail::new("rollover_pi_paper_config_required").into());
        }
        Some((db, body))
    } else {
        None
    };
    if let Some(old) = plan.get("review")
        && old["reason"] != STOPPED_CARRY_REASON
        && old["reason"] != HALTED_CARRY_REASON
        && old["reason"] != ACK_REASON
    {
        atomic(&c.directory.join("review.json"), old)?;
    }
    let Some((source_db, body)) = source else {
        return Ok(None);
    };
    // The controller must persist its closed state and report before this commit.
    if plan["review"]["closed"] != true || plan["review"]["report_hash"].is_null() {
        return Err(Fail::new("rollover_requires_sealed_report").into());
    }
    let mut next = c.clone();
    next.legacy_directory = Some(
        c.legacy_directory
            .clone()
            .unwrap_or_else(|| c.directory.clone()),
    );
    next.session = serde_json::from_value(plan["session"].clone())?;
    next.previous_session = serde_json::from_value(plan["previous_session"].clone())?;
    next.directory = root(c)?.join(next.session.to_string());
    fs::create_dir_all(root(c)?)?;
    if next.directory.exists() {
        // A crash may leave only the decision before the pointer commit. Anything
        // else must be inspected, never adopted as a second primary book.
        let names: Vec<_> = fs::read_dir(&next.directory)?.collect::<std::io::Result<Vec<_>>>()?;
        if names.iter().any(|e| {
            e.file_name() != "carry-over.json"
                && e.file_name() != "strategy-decision.json"
                && !(e.file_name() == "model-review-attempt.json"
                    && next.directory.join("strategy-decision.json").is_file())
        }) {
            return Err(Fail::new("rollover_existing_book_requires_review").into());
        }
    } else {
        fs::create_dir(&next.directory)?;
    }
    fs::set_permissions(&next.directory, fs::Permissions::from_mode(0o700))?;
    let saved = next.directory.join("strategy-decision.json");
    if saved.exists() {
        plan["strategy_decision"] = serde_json::from_slice(
            &crate::read_bounded(&saved, 1_048_576).map_err(|_| "saved_decision_unreadable")?,
        )?;
    }
    verify_decision(&next, &plan["strategy_decision"])?;
    atomic(
        &next.directory.join("strategy-decision.json"),
        &plan["strategy_decision"],
    )?;
    if let Ok(mut p) = serde_json::from_str::<Portfolio>(&body)
        && p.model_led
    {
        let halted_acceptance = accepted_halted_carry(c, &source_db, &body)?;
        if halted_acceptance.is_some() {
            // Carried as halted protection-only: the halt, positions and source
            // evidence are preserved; entries stay blocked until planner-recover.
            if p.stopped || !p.halted || !p.closed || p.positions.is_empty() {
                return Err("carry_terminal_state".into());
            }
            p.health = PROTECTIVE_HEALTH.into();
        } else if (p.stopped && accepted_stopped_carry(c, &source_db, &body)?.is_none())
            || p.halted
            || !p.closed
        {
            return Err("carry_terminal_state".into());
        }
        p.stopped = false;
        p.closed = false;
        if halted_acceptance.is_none() {
            p.health = "carried_waiting_for_fresh_quotes".into();
        }
        let mut carry = json!({"source_session":c.session,"source_previous_session":c.previous_session,"source_binding":book_binding(c)?,"source_state_hash":digest(body.as_bytes()),"destination_binding":book_binding(&next)?,"portfolio":p});
        if let Some(receipt) = &halted_acceptance {
            carry["halted_carry"] = halted_carry_over(receipt)?;
        }
        let path = next.directory.join("carry-over.json");
        if path.exists() {
            let existing: Value = serde_json::from_slice(
                &crate::read_bounded(&path, 16_000_000).map_err(|_| "carry_unreadable")?,
            )?;
            if existing != carry {
                return Err("carry_changed".into());
            }
        } else {
            atomic(&path, &carry)?;
        }
    }
    atomic(
        &root(c)?.join("current.json"),
        &json!({"account":c.account,"session":next.session}),
    )?;
    Ok(Some(next))
}
fn offline_migration(c: &Config, target: NaiveDate) -> Value {
    if let Ok(evidence) = migration_evidence(c, target) {
        return evidence;
    }
    let kind = if c
        .paper_cost_transition
        .as_ref()
        .is_some_and(|t| t.source_session == c.session)
    {
        "explicit_paper_cost_transition"
    } else if c.prior_config.is_some() {
        "explicit_prior_config"
    } else {
        return Value::Null;
    };
    json!({"kind":kind,"original_state_preserved":true})
}
/// Advance an already-validated rollover while the controller is stopped.
/// The root lock is always taken before the selected source book lock. The
/// committed pointer, not the config's `session`, selects the source: a
/// stopped controller can leave the reviewed config naming the original
/// session after later sessions have already started. The requested `--now`
/// session guards the transition; the command never advances backwards or
/// replays a transition that the pointer already contains.
fn advance_offline(c: &Config, now: i64, confirmed: bool) -> Result<Value> {
    if !confirmed {
        return Err(Fail::new("rollover_advance_confirm_paper_review_required").into());
    }
    c.validate()?;
    if LIVE_ENABLED {
        return Err(Fail::new("rollover_advance_live_enabled").into());
    }
    let _root_lock = controller_lock(c)?;
    let selected = current(c)?;
    let source_lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(selected.directory.join("controller.lock"))?;
    source_lock.try_lock_exclusive()?;
    let _source_lock = AdvisoryLock::new(source_lock);
    if selected.session >= local(now).date_naive() {
        // Preserve the first transition. The pointer already contains the
        // requested target (or a stale --now is behind it); change nothing.
        let migration = offline_migration(c, selected.session);
        return Ok(json!({
            "status":"already_advanced",
            "source_session":c.session,
            "target_session":selected.session,
            "source_binding":book_binding(c)?,
            "target_binding":book_binding(&selected)?,
            "migration":migration,
            "migration_kind":migration.get("kind").cloned().unwrap_or(Value::Null),
            "source_unchanged":true,
            "live_enabled":false
        }));
    }
    let plan = preview(&selected, now)?;
    if plan["status"] != "rollover_ready" {
        return Err(Fail::new("rollover_advance_not_ready").into());
    }
    // The existing transition/closed-book/carry checks run here exactly once.
    let next = advance(&selected, now)?.ok_or_else(|| Fail::new("rollover_advance_not_ready"))?;
    let migration = plan["strategy_decision"]["migration"].clone();
    Ok(json!({
        "status":"advanced",
        "preview_status":"rollover_ready",
        "source_session":selected.session,
        "target_session":next.session,
        "source_binding":book_binding(&selected)?,
        "target_binding":book_binding(&next)?,
        "migration":migration,
        "migration_kind":migration.get("kind").cloned().unwrap_or(Value::Null),
        "source_unchanged":true,
        "live_enabled":false
    }))
}
pub fn advance_entry(path: &Path, now: Option<DateTime<FixedOffset>>, confirmed: bool) -> ExitCode {
    let result = (|| -> Result<Value> {
        let now = now
            .ok_or_else(|| Fail::new("rollover_advance_now_required"))?
            .timestamp_millis();
        let bytes = fs::read(path).map_err(|_| Fail::new("config_unreadable"))?;
        let c: Config = serde_json::from_slice(&bytes).map_err(|_| Fail::new("config_invalid"))?;
        advance_offline(&c, now, confirmed)
    })();
    match result {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "rollover_advance_refused_original_state_preserved: {}",
                failure_code(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}
pub fn entry(path: &Path, now: i64) -> ExitCode {
    let result = (|| -> Result<Value> {
        let c: Config = serde_json::from_slice(&fs::read(path)?)?;
        c.validate()?;
        preview(&current(&c)?, now)
    })();
    match result {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "rollover_preview_unavailable_inspect_original_book: {}",
                failure_code(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}

pub(super) fn published_decision(c: &Config) -> Value {
    fs::read(c.directory.join("strategy-decision.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}
/// Release the flock when its owning operation ends, even if a concurrent
/// fork still holds the open file description until exec closes CLOEXEC fds.
pub(super) struct ControllerLock(pub(super) fs::File);

impl Drop for ControllerLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

pub(super) fn controller_lock(c: &Config) -> Result<ControllerLock> {
    fs::create_dir_all(root(c)?)?;
    let f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(root(c)?.join("controller.lock"))?;
    f.try_lock_exclusive()?;
    Ok(ControllerLock(f))
}

pub(super) fn record_review(c: &Config) -> Result<()> {
    let reviewed = review(c)?;
    if reviewed["reason"] == ACK_REASON
        || reviewed["reason"] == STOPPED_CARRY_REASON
        || reviewed["reason"] == HALTED_CARRY_REASON
    {
        return Ok(());
    }
    atomic(&c.directory.join("review.json"), &reviewed)
}

/// Bounded, read-only book admission for release activation. Unknown state defers.
pub(crate) struct BookAdmission {
    pub open_positions: u64,
    pub halted: bool,
    /// An open session book that an operator stopped; a restart keeps it stopped.
    pub open_stopped: bool,
    pub flat_safe: bool,
    pub stopped_safe: bool,
    /// A closed, settled model-led book already carrying positions. The
    /// updater may adopt it only while the exchange is closed (#153).
    pub carried_safe: bool,
}
pub(super) fn update_book(path: &Path) -> Result<BookAdmission> {
    let c: Config = serde_json::from_slice(&fs::read(path)?)?;
    let mut mandate = c.clone();
    if mandate.strategy == "deterministic_momentum_v1" {
        mandate.strategy = "model_abstain".into();
    }
    mandate.validate()?;
    let c = current(&c)?;
    let (db, body) = bound_state(&c)?;
    let (open, stopped, halted, closed) = book_flags(&body)?;
    let portfolio = serde_json::from_str::<Portfolio>(&body).ok();
    // The flat rule stays the in-session contract. A carried book can only be
    // adopted read-only when every journal is settled; the successor session
    // still starts through the reviewed rollover/carry path.
    let carried_safe = closed
        && !halted
        && !redecision::pending_attempt(&db)?
        && portfolio.as_ref().is_some_and(|p| {
            p.reconcile().is_ok()
                && p.closed
                && p.model_led
                && !p.stopped
                && !p.halted
                && !p.positions.is_empty()
        });
    Ok(BookAdmission {
        open_positions: open,
        halted,
        open_stopped: stopped && !closed,
        flat_safe: open == 0 && closed && !halted,
        // Carried holdings are allowed because the book is preserved and the
        // controller stays stopped until an explicit resume. A closed flat book
        // needs no restart either, even when a planner halt remains: this is the
        // reviewed path that deploys first and clears the halt with the new
        // release, avoiding a cross-version authority rebind (#158).
        stopped_safe: portfolio
            .as_ref()
            .is_some_and(|p| (p.stopped && !p.halted) || (p.closed && p.positions.is_empty())),
        carried_safe,
    })
}

/// Exchange-closed probe for the carried-adoption rule. The NSE calendar is
/// authoritative; an unreadable calendar defers (returns false).
pub(super) fn market_closed(path: &Path, now: i64) -> Result<bool> {
    let c: Config = serde_json::from_slice(&fs::read(path)?)?;
    Ok(!c
        .calendar_at(local(now).date_naive())?
        .is_open(nivra_core::time::Timestamp::from_epoch_millis(now)?)?)
}

/// Flat admission plus the calendar-closed carried rule. Kept for the
/// behavioral admission tests; activation uses [`update_book`] directly.
#[cfg(test)]
pub(super) fn update_safe(path: &Path, now: i64) -> Result<bool> {
    let book = update_book(path)?;
    Ok(book.flat_safe || (book.carried_safe && market_closed(path, now)?))
}

/// Operator-stopped deployment admission: a stopped, non-halted book with no
/// pending orders. Carried holdings are allowed because the book is preserved
/// byte-for-byte and the controller stays stopped until an explicit resume.
pub(super) fn stopped_book_safe(path: &Path) -> Result<bool> {
    Ok(update_book(path)?.stopped_safe)
}

#[cfg(test)]
mod tests;
