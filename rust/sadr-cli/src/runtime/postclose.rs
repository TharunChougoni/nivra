//! Advisory post-close lane. Financial reports, rollover and protection remain independent.
use super::*;
use rusqlite::OptionalExtension;
use sadr_core::{execution::Portfolio, postclose as contract};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
const PROMPT_VERSION: &str = "postclose-harness-v3";
const PROMPT: &str = "postclose-harness-v3: Apply the review system instructions. Sealed evidence follows; cite only its supplied evidence IDs.";
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub enabled: bool,
    pub operations: planner::ModelRole,
    pub strategy: planner::ModelRole,
    pub memory_limit: usize,
    #[serde(skip_serializing_if = "Settings::default_cutoff_minute")]
    pub cutoff_minute: u32,
    #[serde(skip_serializing_if = "Settings::default_max_fill_evidence")]
    pub max_fill_evidence: usize,
    #[serde(skip_serializing_if = "Settings::default_max_planner_evidence_rows")]
    pub max_planner_evidence_rows: usize,
    #[serde(skip_serializing_if = "Settings::default_max_tick_evidence_rows")]
    pub max_tick_evidence_rows: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            operations: planner::ModelRole::default(),
            strategy: planner::ModelRole::default(),
            memory_limit: 16,
            cutoff_minute: 1080,
            max_fill_evidence: 128,
            max_planner_evidence_rows: 24,
            max_tick_evidence_rows: 48,
        }
    }
}
impl Settings {
    // Preserve configuration hashes for existing sealed jobs at the old defaults.
    fn default_cutoff_minute(value: &u32) -> bool {
        *value == Self::default().cutoff_minute
    }
    fn default_max_fill_evidence(value: &usize) -> bool {
        *value == Self::default().max_fill_evidence
    }
    fn default_max_planner_evidence_rows(value: &usize) -> bool {
        *value == Self::default().max_planner_evidence_rows
    }
    fn default_max_tick_evidence_rows(value: &usize) -> bool {
        *value == Self::default().max_tick_evidence_rows
    }
    pub fn validate(&self) -> Result<()> {
        if self.memory_limit > 32
            || !(931..=1439).contains(&self.cutoff_minute)
            || !(1..=128).contains(&self.max_fill_evidence)
            || !(1..=24).contains(&self.max_planner_evidence_rows)
            || !(1..=48).contains(&self.max_tick_evidence_rows)
        {
            return Err("postclose_settings_invalid".into());
        }
        for role in [&self.operations, &self.strategy] {
            if role.max_analysis_tools > 64
                || role.max_turns == Some(0)
                || role.max_read_tools == Some(0)
                || role.minimum_turn_ms == Some(0)
                || role.model_id == crate::model::FAST_VARIANT
                || role
                    .tools
                    .iter()
                    .any(|t| !["web_search", "x_search"].contains(&t.as_str()))
                || role
                    .run_timeout_ms
                    .is_some_and(|n| n < role.timeout_ms || n > 1_800_000)
                || !(1..=120000).contains(&role.timeout_ms)
                || !(64..=16384).contains(&role.output_cap)
                || role
                    .budget_unit
                    .as_deref()
                    .is_some_and(|s| !["request", "event"].contains(&s))
                || (role.enabled
                    && (role.model_id.is_empty()
                        || role.model_id.len() > 128
                        || role.daily_call_budget == 0))
            {
                return Err("postclose_role_invalid".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    sealed: contract::SealedEvidence,
    configuration_hash: String,
    jobs: BTreeMap<String, String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    id: String,
    role: contract::ReviewRole,
    model: planner::ModelRole,
    input_hash: String,
    configuration_hash: String,
    started_ms: i64,
    deadline_ms: i64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Completed {
    output: contract::ReviewOutput,
    completed_ms: i64,
}
pub(crate) struct Worker {
    cancel: Arc<AtomicBool>,
    receiver: std::sync::mpsc::Receiver<std::result::Result<String, String>>,
    job: Job,
    settled: bool,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
/// Retain cancelled callbacks across book rollover without blocking the controller.
pub(crate) fn drain(worker: &mut Option<Worker>) {
    if let Some(active) = worker.as_mut() {
        active.cancel.store(true, Ordering::Release);
        if !matches!(
            active.receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ) {
            drop(worker.take());
        }
    }
}
fn role_name(role: &contract::ReviewRole) -> &'static str {
    match role {
        contract::ReviewRole::Operations => "operations",
        contract::ReviewRole::Strategy => "strategy",
    }
}
fn account(c: &Config) -> String {
    digest(format!("upstox\0{}", c.account).as_bytes())
}
fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    if raw.len() > 1_000_000 {
        return Err("postclose_journal_bound".into());
    }
    Ok(serde_json::from_value(sadr_core::native::decode(
        raw.as_bytes(),
    )?)?)
}
fn marked(db: &Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM ticks WHERE event='postclose_authority')",
        [],
        |r| r.get(0),
    )?)
}
fn load(db: &Connection, c: &Config) -> Result<Option<Control>> {
    if !marked(db)? {
        return Ok(None);
    }
    let body: String = db.query_row("SELECT body FROM postclose_control WHERE id=1", [], |r| {
        r.get(0)
    })?;
    let control: Control = decode(&body)?;
    control.sealed.validate()?;
    if control.sealed.input.account != account(c)
        || control.sealed.input.session != c.session
        || control.sealed.input.session_binding != book_binding(c)?
    {
        return Err("postclose_binding_invalid".into());
    }
    let jobs: i64 = db.query_row("SELECT COUNT(*) FROM postclose_job", [], |r| r.get(0))?;
    if jobs != 2 || control.jobs.len() != 2 {
        return Err("postclose_jobs_changed".into());
    }
    for name in ["operations", "strategy"] {
        let (id, body): (String, String) = db.query_row(
            "SELECT id,body FROM postclose_job WHERE role=?",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let job: Job = decode(&body)?;
        if control.jobs.get(name) != Some(&id)
            || job.id != id
            || role_name(&job.role) != name
            || job_id(c, &job)? != id
            || job.input_hash != control.sealed.sha256
            || job.configuration_hash != control.configuration_hash
        {
            return Err("postclose_jobs_changed".into());
        }
    }
    // An admitted review never switches to a replaced financial report.
    let report = crate::read_bounded(&c.directory.join("report.json"), 16_000_000)
        .map_err(|_| "postclose_report_unreadable")?;
    if digest(&report) != control.sealed.input.report_sha256 {
        return Err("postclose_report_changed".into());
    }
    Ok(Some(control))
}
pub(crate) fn seal_evidence(
    db: &Connection,
    c: &Config,
    p: &Portfolio,
    settings: &Settings,
) -> Result<contract::SealedEvidence> {
    if !p.closed {
        return Err("postclose_requires_closed_report".into());
    }
    p.reconcile()?;
    let report = crate::read_bounded(&c.directory.join("report.json"), 16_000_000)
        .map_err(|_| "postclose_report_unreadable")?;
    let v: Value = serde_json::from_slice(&report)?;
    let report_p: Portfolio = serde_json::from_value(v["portfolio"].clone())?;
    let (binding, body): (String, String) =
        db.query_row("SELECT binding,body FROM state WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    if binding != book_binding(c)?
        || v["account"] != c.account
        || v["session"] != c.session.to_string()
        || v["broker"] != "upstox"
        || v["paper_only"] != true
        || v["live_enabled"] != false
        || !report_p.closed
        || report_p != *p
        || serde_json::from_str::<Portfolio>(&body)? != *p
    {
        return Err("postclose_report_state_mismatch".into());
    }
    let report_hash = digest(&report);
    let mut items=vec![contract::EvidenceItem {id:"report".into(),kind:contract::EvidenceKind::Report,artifact_sha256:report_hash.clone(),summary:json!({"cash":p.cash,"account_allocation":p.allocation()?,"positions":p.positions.len(),"cumulative_fill_count":p.fills.len(),"closed":true,"stopped":p.stopped,"halted":p.halted,"paper_only":true,"live_enabled":false,"daily_pnl":"not_inferred_from_cumulative_book"}).to_string()}];
    // Fills carry across days: only original current-session timestamps count here.
    let daily = p
        .fills
        .iter()
        .enumerate()
        .filter(|(_, f)| local(f.at).date_naive() == c.session)
        .collect::<Vec<_>>();
    let fees = daily
        .iter()
        .try_fold(sadr_core::finance::Amount::ZERO, |sum, (_, f)| {
            sum.checked_add(f.fee)
        })?;
    items.push(contract::EvidenceItem {id:"daily_activity".into(),kind:contract::EvidenceKind::Health,artifact_sha256:digest(body.as_bytes()),summary:json!({"session_fill_count":daily.len(),"session_fill_fees":fees,"individual_fill_coverage":daily.len().min(settings.max_fill_evidence),"attribution":"original_fill_session_timestamp"}).to_string()});
    for (index, f) in daily.into_iter().take(settings.max_fill_evidence) {
        items.push(contract::EvidenceItem {id:format!("fill:{index}"),kind:contract::EvidenceKind::Fill,artifact_sha256:digest(&serde_json::to_vec(f)?),summary:json!({"at":f.at,"units":f.units,"buy":f.buy,"price":f.price,"fee":f.fee,"instrument_hash":digest(f.instrument.key.as_bytes()),"decision_hash":f.decision_id.as_ref().map(|id|digest(id.as_bytes()))}).to_string()});
    }
    // Hash original rows, expose only typed identity/timing, never raw model text/logs.
    for (table, kind, prefix) in [
        ("planner_event_log", contract::EvidenceKind::Event, "event"),
        (
            "planner_attempt_log",
            contract::EvidenceKind::Attempt,
            "attempt",
        ),
    ] {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
            [table],
            |r| r.get(0),
        )?;
        if exists {
            let mut stmt = db.prepare(&format!(
                "SELECT id,body,state FROM {table} ORDER BY rowid DESC LIMIT ?"
            ))?;
            for row in stmt.query_map([settings.max_planner_evidence_rows as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (id, body, state) = row?;
                let record: Value = decode(&body)?;
                let state = if [
                    "pending",
                    "reserved",
                    "expired",
                    "interrupted",
                    "planned",
                    "superseded",
                    "failed",
                ]
                .contains(&state.as_str())
                {
                    state.as_str()
                } else {
                    "unrecognized"
                };
                let trigger = record
                    .get("kind")
                    .or_else(|| record.get("trigger").and_then(|v| v.get("kind")))
                    .and_then(Value::as_str)
                    .filter(|s| {
                        [
                            "price_move",
                            "volume_move",
                            "gap",
                            "proximity",
                            "news",
                            "fill",
                            "initial",
                        ]
                        .contains(s)
                    });
                items.push(contract::EvidenceItem {
                    id: format!("{prefix}:{}", digest(id.as_bytes())),
                    kind: kind.clone(),
                    artifact_sha256: digest(body.as_bytes()),
                    summary: json!({"record_hash":digest(id.as_bytes()),"state":state,"trigger":trigger,"observed_ms":record["observed_ms"].as_i64(),"started_ms":record["started_ms"].as_i64(),"deadline_ms":record["deadline_ms"].as_i64(),"retained":true,"coverage":format!("latest_{}_records_per_kind", settings.max_planner_evidence_rows)})
                        .to_string(),
                });
            }
        }
    }
    let mut stmt = db.prepare("SELECT id,at,receipt,event FROM ticks ORDER BY id DESC LIMIT ?")?;
    for row in stmt.query_map([settings.max_tick_evidence_rows as i64], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, String>(3)?,
        ))
    })? {
        let (id, at, receipt, event) = row?;
        let code = if [
            "wide_quotes",
            "post_close",
            "planner_authority_invalid",
            "operator_stopped",
            "stopped",
            "wide_reconciled",
            "wide_read_failure",
            "wide_read_failed",
            "read_unavailable",
            "invalid_quote_json",
            "invalid_quote_status",
            "invalid_quote_data",
            "duplicate_quote",
            "read_failed",
            "wide_data_failure",
            "planner_failed",
            "daily_session_created",
        ]
        .contains(&event.as_str())
        {
            event.as_str()
        } else {
            "other_retained_event"
        };
        items.push(contract::EvidenceItem {id:format!("tick:{id}"),kind:contract::EvidenceKind::Health,artifact_sha256:digest(&serde_json::to_vec(&json!([id,at,receipt,event]))?),summary:json!({"at":at,"receipt":receipt,"receipt_age_ms":receipt.and_then(|r|at.checked_sub(r)),"category":code,"coverage":format!("latest_{}_journal_events", settings.max_tick_evidence_rows)}).to_string()});
    }
    let ticks: i64 = db.query_row("SELECT COUNT(*) FROM ticks", [], |r| r.get(0))?;
    items.push(contract::EvidenceItem {id:"health".into(),kind:contract::EvidenceKind::Health,artifact_sha256:digest(body.as_bytes()),summary:json!({"journal_events":ticks,"stopped":p.stopped,"halted":p.halted,"note":"Raw error logs and provider responses excluded"}).to_string()});
    contract::seal(contract::EvidenceInput {
        account: account(c),
        session: c.session,
        session_binding: binding,
        report_sha256: report_hash,
        state_sha256: digest(body.as_bytes()),
        evidence: items,
    })
}
fn initialize(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    now: i64,
) -> Result<Control> {
    if let Some(control) = load(db, c)? {
        return Ok(control);
    }
    let settings = cfg
        .post_close
        .as_ref()
        .ok_or("postclose_settings_missing")?;
    let mut control = Control {
        sealed: seal_evidence(db, c, p, settings)?,
        configuration_hash: digest(&serde_json::to_vec(cfg)?),
        jobs: BTreeMap::new(),
    };
    let mut jobs = Vec::new();
    for (role, model) in [
        (
            contract::ReviewRole::Operations,
            cfg.role(super::orchestrator_model::Trigger::Operations),
        ),
        (
            contract::ReviewRole::Strategy,
            cfg.role(super::orchestrator_model::Trigger::Strategy),
        ),
    ] {
        let mut job = Job {
            id: String::new(),
            role,
            model,
            input_hash: control.sealed.sha256.clone(),
            configuration_hash: control.configuration_hash.clone(),
            started_ms: 0,
            deadline_ms: 0,
        };
        job.id = job_id(c, &job)?;
        control
            .jobs
            .insert(role_name(&job.role).into(), job.id.clone());
        jobs.push(job);
    }
    // One original usage namespace even on a zero-trade day. This creates no allowance.
    planner::prepare_review_usage(c)?;
    let tx = db.transaction()?;
    tx.execute_batch("CREATE TABLE postclose_control(id INTEGER PRIMARY KEY CHECK(id=1),body TEXT NOT NULL); CREATE TABLE postclose_job(id TEXT PRIMARY KEY,role TEXT NOT NULL UNIQUE,body TEXT NOT NULL,status TEXT NOT NULL,result TEXT);")?;
    tx.execute(
        "INSERT INTO postclose_control VALUES(1,?)",
        [serde_json::to_string(&control)?],
    )?;
    tx.execute(
        "INSERT INTO ticks(at,event) VALUES(?,'postclose_authority')",
        [now],
    )?;
    for job in jobs {
        tx.execute(
            "INSERT INTO postclose_job(id,role,body,status) VALUES(?,?,?,?)",
            rusqlite::params![
                job.id,
                role_name(&job.role),
                serde_json::to_string(&job)?,
                if job.model.enabled {
                    "queued"
                } else {
                    "disabled"
                }
            ],
        )?;
    }
    tx.commit()?;
    Ok(control)
}
fn job_id(c: &Config, job: &Job) -> Result<String> {
    Ok(digest(&serde_json::to_vec(
        &json!({"session":c.session,"role":job.role,"input":job.input_hash,"configuration":job.configuration_hash,"prompt":PROMPT_VERSION,"model":job.model}),
    )?))
}
fn settle(
    db: &Connection,
    c: &Config,
    control: &Control,
    job: &Job,
    raw: std::result::Result<String, String>,
    now: i64,
) -> Result<()> {
    let saved: Option<(String, String)> = db
        .query_row(
            "SELECT body,status FROM postclose_job WHERE id=?",
            [&job.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((body, status)) = saved else {
        return Err("postclose_job_missing".into());
    };
    if body != serde_json::to_string(job)?
        || status != "reserved"
        || job.input_hash != control.sealed.sha256
        || control.configuration_hash != job.configuration_hash
    {
        return Err("postclose_job_changed".into());
    }
    let outcome = raw.and_then(|raw| {
        if now < job.started_ms || now > job.deadline_ms || local(now).date_naive() != c.session {
            return Err("deadline".into());
        }
        let output = contract::validate_output(&raw, &control.sealed)
            .map_err(|_| "invalid_output".to_string())?;
        super::harness::publish_validated(
            c,
            match job.role {
                contract::ReviewRole::Operations => {
                    super::orchestrator_model::Trigger::Operations.as_str()
                }
                contract::ReviewRole::Strategy => {
                    super::orchestrator_model::Trigger::Strategy.as_str()
                }
            },
            &job.model,
            &raw,
        );
        Ok(output)
    });
    let (status, result) = match outcome {
        Ok(output) => (
            "completed",
            serde_json::to_string(&Completed {
                output,
                completed_ms: now,
            })?,
        ),
        Err(_) => (
            "unavailable",
            json!({"error_category":"review_failed_or_expired"}).to_string(),
        ),
    };
    db.execute(
        "UPDATE postclose_job SET status=?,result=? WHERE id=? AND status='reserved'",
        rusqlite::params![status, result, job.id],
    )?;
    Ok(())
}
/// Called after the deterministic report; errors affect only this lane's status.
pub(crate) fn tick(
    db: &mut Connection,
    c: &Config,
    cfg: Option<&planner::PlannerConfig>,
    p: &Portfolio,
    now: i64,
    worker: &mut Option<Worker>,
) -> Result<Value> {
    if let Some(active) = worker.as_mut() {
        let received = match active.receiver.try_recv() {
            Ok(raw) => Some(raw),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("worker_lost".into())),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
        };
        let control = load(db, c)?.ok_or("postclose_control_missing")?;
        let changed = cfg.is_none_or(|cfg| {
            serde_json::to_vec(cfg)
                .map(|b| digest(&b) != control.configuration_hash)
                .unwrap_or(true)
        });
        if !active.settled
            && (now > active.job.deadline_ms || local(now).date_naive() != c.session || changed)
        {
            active.cancel.store(true, Ordering::Release);
            settle(
                db,
                c,
                &control,
                &active.job,
                Err("cancelled_or_expired".into()),
                now,
            )?;
            active.settled = true;
        }
        if let Some(raw) = received {
            if !active.settled {
                settle(db, c, &control, &active.job, raw, now)?;
            }
            drop(worker.take());
        } else {
            // Cancellation cannot stop a synchronous provider call. Drain it before another role.
            return status(db, c);
        }
    }
    let Some(cfg) =
        cfg.filter(|cfg| cfg.enabled && cfg.post_close.as_ref().is_some_and(|r| r.enabled))
    else {
        return Ok(json!({"status":"disabled"}));
    };
    if !p.closed || !c.directory.join("report.json").is_file() {
        return Ok(json!({"status":"awaiting_sealed_close_report"}));
    }
    let cutoff_minute = cfg
        .post_close
        .as_ref()
        .ok_or("postclose_settings_missing")?
        .cutoff_minute;
    let control = initialize(db, c, cfg, p, now)?;
    // No worker survived a process restart: preserve any uncertain invocation, never replay it.
    db.execute(
        "UPDATE postclose_job SET status='interrupted' WHERE status='reserved'",
        [],
    )?;
    if control.configuration_hash != digest(&serde_json::to_vec(cfg)?) {
        return Err("postclose_configuration_changed".into());
    }
    if local(now).date_naive() != c.session || minute(now) < 930 || minute(now) >= cutoff_minute {
        if local(now).date_naive() > c.session
            || (local(now).date_naive() == c.session && minute(now) >= cutoff_minute)
        {
            db.execute(
                "UPDATE postclose_job SET status='expired_not_started' WHERE status='queued'",
                [],
            )?;
        }
        return status(db, c);
    }
    for name in ["operations", "strategy"] {
        let (body, status): (String, String) = db.query_row(
            "SELECT body,status FROM postclose_job WHERE role=?",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if status != "queued" {
            continue;
        }
        let mut job: Job = decode(&body)?;
        job.started_ms = now;
        let cutoff = now + i64::from(cutoff_minute - minute(now)) * 60_000
            - i64::from(local(now).second()) * 1000
            - i64::from(local(now).timestamp_subsec_millis());
        job.deadline_ms = now
            .checked_add(job.model.run_budget_ms() as i64)
            .ok_or("postclose_deadline")?
            .min(cutoff);
        db.execute(
            "UPDATE postclose_job SET body=?,status='reserved' WHERE id=? AND status='queued'",
            rusqlite::params![serde_json::to_string(&job)?, job.id],
        )?;
        let prompt = format!(
            "{PROMPT}\nRole: {name}\n{}",
            serde_json::to_string(&control.sealed)?
        );
        let (sender, receiver) = std::sync::mpsc::channel();
        let evidence = serde_json::to_value(&control.sealed)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let (c, cfg, job_copy, worker_cancel) =
            (c.clone(), cfg.clone(), job.clone(), Arc::clone(&cancel));
        thread::spawn(move || {
            let result = (|| -> Result<String> {
                let path = cfg
                    .model_config
                    .as_deref()
                    .or(job_copy.model.model_config.as_deref())
                    .ok_or("postclose_model_config_required")?;
                let mut backend = planner::Subscription::new(path);
                planner::review_call(
                    &c,
                    &cfg,
                    role_name(&job_copy.role),
                    &job_copy.model,
                    &prompt,
                    &evidence,
                    &worker_cancel,
                    job_copy.started_ms,
                    job_copy.deadline_ms,
                    &mut backend,
                )
            })()
            .map_err(|_| "review_unavailable".to_string());
            let _ = sender.send(result);
        });
        *worker = Some(Worker {
            cancel,
            receiver,
            job,
            settled: false,
        });
        break;
    }
    status(db, c)
}
fn status(db: &Connection, c: &Config) -> Result<Value> {
    let Some(control) = load(db, c)? else {
        return Ok(json!({"status":"not_admitted"}));
    };
    let mut stmt = db.prepare("SELECT id,role,status,result FROM postclose_job ORDER BY rowid")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.len() > 2 {
        return Err("postclose_job_bound".into());
    }
    let jobs = rows
        .into_iter()
        .map(|(id, role, status, result)| -> Result<Value> {
            let result = if status == "completed" {
                let completed: Completed = decode(&result.ok_or("postclose_result_missing")?)?;
                contract::validate_output(
                    &serde_json::to_string(&completed.output)?,
                    &control.sealed,
                )?;
                Some(completed)
            } else {
                None
            };
            Ok(json!({"id":id,"role":role,"status":status,"result":result}))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"status":"advisory_only","input_hash":control.sealed.sha256,"jobs":jobs,"research_execution":"not_enabled"}),
    )
}
/// Load only the verified immediate predecessor. Never discover arbitrary old text.
#[cfg(test)]
pub(super) fn behavioral_assertions() {
    use sadr_core::{
        execution::{ExitIntent, Instrument, Quote, a},
        finance::Price,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sessions");
    let mut c = super::tests::config(&root.join("2026-10-16"));
    c.sessions_root = Some(root.clone());
    fs::create_dir_all(&c.directory).unwrap();
    let mut db = open_book(&c).unwrap();
    let at = DateTime::parse_from_rfc3339("2026-10-16T15:40:00+05:30")
        .unwrap()
        .timestamp_millis();
    let mut p = crate::runtime::tests::test_portfolio();
    p.model_led = true;
    let i = Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    for time in [at - 86400000 - 18000000, at - 18000000] {
        let q = Quote {
            bid: Price::parse("100").unwrap(),
            ask: Price::parse("100").unwrap(),
            bid_units: 100,
            ask_units: 100,
            source: time,
            receipt: time,
        };
        p.protect(time, &BTreeMap::from([(i.key.clone(), q.clone())]))
            .unwrap();
        assert!(
            p.execute_plan(
                time,
                &i,
                &q,
                true,
                1,
                Price::parse("101").unwrap(),
                ExitIntent::default(),
                "synthetic"
            )
            .unwrap()
        );
    }
    p.closed = true;
    let body = serde_json::to_string(&p).unwrap();
    db.execute(
        "INSERT INTO state VALUES(1,?,?)",
        rusqlite::params![book_binding(&c).unwrap(), body],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ticks(at,receipt,event) VALUES(?,?,?)",
        rusqlite::params![at - 60000, at - 60001, "wide_quotes"],
    )
    .unwrap();
    let report = json!({"paper_only":true,"live_enabled":false,"broker":"upstox","account":c.account,"session":c.session,"portfolio":p});
    fs::write(
        c.directory.join("report.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    let before = p.clone();
    let sealed = seal_evidence(&db, &c, &p, &Settings::default()).unwrap();
    let daily: Value = serde_json::from_str(
        &sealed
            .input
            .evidence
            .iter()
            .find(|e| e.id == "daily_activity")
            .unwrap()
            .summary,
    )
    .unwrap();
    assert_eq!(daily["session_fill_count"], 1);
    assert_eq!(
        daily["session_fill_fees"],
        serde_json::to_value(p.fills[1].fee).unwrap()
    );
    assert_eq!(
        sealed
            .input
            .evidence
            .iter()
            .filter(|e| e.kind == contract::EvidenceKind::Fill)
            .count(),
        1
    );
    assert!(sealed.input.evidence.iter().any(|e| e.id == "tick:1"));
    for (field, value) in [
        ("broker", json!("other")),
        ("paper_only", json!(false)),
        ("live_enabled", json!(true)),
    ] {
        let mut wrong = report.clone();
        wrong[field] = value;
        fs::write(c.directory.join("report.json"), wrong.to_string()).unwrap();
        assert!(seal_evidence(&db, &c, &p, &Settings::default()).is_err());
    }
    fs::write(
        c.directory.join("report.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    let model = planner::ModelRole {
        enabled: true,
        model_id: "fixture-model".into(),
        daily_call_budget: 2,
        ..Default::default()
    };
    let cfg = planner::PlannerConfig {
        enabled: true,
        post_close: Some(Settings {
            enabled: true,
            operations: model.clone(),
            strategy: model,
            ..Default::default()
        }),
        ..Default::default()
    };
    let control = initialize(&mut db, &c, &cfg, &p, at).unwrap();
    assert_eq!(
        initialize(&mut db, &c, &cfg, &p, at).unwrap().sealed.sha256,
        control.sealed.sha256
    );
    let reserve = |db: &Connection, name: &str| {
        let raw: String = db
            .query_row("SELECT body FROM postclose_job WHERE role=?", [name], |r| {
                r.get(0)
            })
            .unwrap();
        let mut job: Job = decode(&raw).unwrap();
        job.started_ms = at;
        job.deadline_ms = at + 20000;
        db.execute(
            "UPDATE postclose_job SET body=?,status='reserved' WHERE id=?",
            rusqlite::params![serde_json::to_string(&job).unwrap(), job.id],
        )
        .unwrap();
        job
    };
    let operations = reserve(&db, "operations");
    let raw=json!({"observations":[{"text":"The report retained carried positions","evidence_ids":["report"]}],"hypotheses":[],"uncertainties":["Synthetic fixtures do not establish strategy quality"],"memory":[{"kind":"observation","text":"The report retained carried positions","evidence_ids":["report"],"uncertainty":"Synthetic evidence only"}]}).to_string();
    settle(&db, &c, &control, &operations, Ok(raw.clone()), at + 1).unwrap();
    assert!(settle(&db, &c, &control, &operations, Ok(raw), at + 2).is_err());
    // Timeout records failure but preserves the live callback slot until drained.
    let strategy = reserve(&db, "strategy");
    let (sender, receiver) = std::sync::mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut worker = Some(Worker {
        cancel: Arc::clone(&cancel),
        receiver,
        job: strategy.clone(),
        settled: false,
    });
    tick(&mut db, &c, Some(&cfg), &p, at + 20001, &mut worker).unwrap();
    assert!(worker.is_some());
    assert!(cancel.load(Ordering::Acquire));
    assert_eq!(
        db.query_row(
            "SELECT status FROM postclose_job WHERE role='strategy'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "unavailable"
    );
    sender.send(Ok("late output".into())).unwrap();
    tick(&mut db, &c, Some(&cfg), &p, at + 20002, &mut worker).unwrap();
    assert!(worker.is_none());
    assert_eq!(p, before);
    assert_eq!(p.allocation().unwrap(), a("5000"));
    db.execute("DELETE FROM postclose_job WHERE role='strategy'", [])
        .unwrap();
    assert!(initialize(&mut db, &c, &cfg, &p, at + 3).is_err());
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM postclose_job", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        fs::read(c.directory.join("report.json")).unwrap(),
        serde_json::to_vec(&report).unwrap()
    );
    assert_eq!(p, before);
}

#[cfg(test)]
fn review_fixture() -> (tempfile::TempDir, Config, Connection, Portfolio, i64) {
    use sadr_core::{
        execution::{ExitIntent, Instrument, Quote},
        finance::Price,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sessions");
    let mut c = super::tests::config(&root.join("2026-10-16"));
    c.sessions_root = Some(root.clone());
    fs::create_dir_all(&c.directory).unwrap();
    let db = open_book(&c).unwrap();
    let at = DateTime::parse_from_rfc3339("2026-10-16T15:40:00+05:30")
        .unwrap()
        .timestamp_millis();
    let mut p = crate::runtime::tests::test_portfolio();
    p.model_led = true;
    let i = Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    for time in [at - 86400000 - 18000000, at - 18000000] {
        let q = Quote {
            bid: Price::parse("100").unwrap(),
            ask: Price::parse("100").unwrap(),
            bid_units: 100,
            ask_units: 100,
            source: time,
            receipt: time,
        };
        p.protect(time, &BTreeMap::from([(i.key.clone(), q.clone())]))
            .unwrap();
        assert!(
            p.execute_plan(
                time,
                &i,
                &q,
                true,
                1,
                Price::parse("101").unwrap(),
                ExitIntent::default(),
                "synthetic"
            )
            .unwrap()
        );
    }
    p.closed = true;
    let body = serde_json::to_string(&p).unwrap();
    db.execute(
        "INSERT INTO state VALUES(1,?,?)",
        rusqlite::params![book_binding(&c).unwrap(), body],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ticks(at,receipt,event) VALUES(?,?,?)",
        rusqlite::params![at - 60000, at - 60001, "wide_quotes"],
    )
    .unwrap();
    let report = json!({"paper_only":true,"live_enabled":false,"broker":"upstox","account":c.account,"session":c.session,"portfolio":p});
    fs::write(
        c.directory.join("report.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    (dir, c, db, p, at)
}

#[test]
fn orchestrator_reviews_use_the_single_model_for_both_roles() {
    use super::orchestrator_model::{ModelDefaults, OrchestratorModelSettings, Trigger};
    let (_dir, c, mut db, p, at) = review_fixture();
    let settings = OrchestratorModelSettings {
        enabled: true,
        model: ModelDefaults {
            model_id: "fixture-model".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(settings.triggers.contains_key(&Trigger::Operations));
    assert!(settings.triggers.contains_key(&Trigger::Strategy));
    let cfg = planner::PlannerConfig {
        enabled: true,
        post_close: Some(Settings {
            enabled: true,
            ..Default::default()
        }),
        orchestrator: Some(settings),
        ..Default::default()
    };
    let control = initialize(&mut db, &c, &cfg, &p, at).unwrap();
    assert_eq!(control.jobs.len(), 2);
    let mut roles = Vec::new();
    for id in control.jobs.values() {
        let raw: String = db
            .query_row("SELECT body FROM postclose_job WHERE id=?", [id], |r| {
                r.get(0)
            })
            .unwrap();
        let job: Job = decode(&raw).unwrap();
        assert_eq!(job.model.model_id, "fixture-model");
        roles.push(job.role);
    }
    assert!(roles.contains(&contract::ReviewRole::Operations));
    assert!(roles.contains(&contract::ReviewRole::Strategy));
}

#[test]
fn settings_defaults_and_bounds() {
    let defaults: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(
        (
            defaults.cutoff_minute,
            defaults.max_fill_evidence,
            defaults.max_planner_evidence_rows,
            defaults.max_tick_evidence_rows
        ),
        (1080, 128, 24, 48)
    );
    defaults.validate().unwrap();
    let serialized = serde_json::to_value(&defaults).unwrap();
    for key in [
        "cutoff_minute",
        "max_fill_evidence",
        "max_planner_evidence_rows",
        "max_tick_evidence_rows",
    ] {
        assert!(serialized.get(key).is_none());
    }
    for (key, values) in [
        ("cutoff_minute", [930, 1440]),
        ("max_fill_evidence", [0, 129]),
        ("max_planner_evidence_rows", [0, 25]),
        ("max_tick_evidence_rows", [0, 49]),
    ] {
        for value in values {
            let settings: Settings = serde_json::from_value(json!({key: value})).unwrap();
            assert!(settings.validate().is_err(), "{key}={value}");
        }
    }
}
