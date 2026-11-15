use super::*;
use crate::runtime::phases::PlannerLogPhase;

pub(crate) fn reserve(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    now: i64,
    eligible: bool,
) -> Result<Option<Claim>> {
    let Some(settings) = settings(cfg) else {
        return Ok(None);
    };
    if cfg.model_led == Some(false)
        || (cfg.orchestrator_owns_decisions()
            && !super::super::decision_owner::in_window(c, cfg, now))
        || !eligible
        || p.stopped
        || p.halted
        || p.closed
        || local(now).date_naive() != c.session
        || !(u32::from(settings.decision_start_minute.unwrap_or(480))..930).contains(&minute(now))
    {
        return Ok(None);
    }
    planner::require_usage(c)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut state = load(&tx)?.ok_or("planner_event_state_missing")?;
    check(&state, c)?;
    if state.config_hash != config_hash(cfg)? {
        return Err("planner_event_configuration_changed".into());
    }
    if state.blocked.is_some() || state.pending.is_some() {
        return Ok(None);
    }
    if state.last_started_ms.is_some_and(|last| now < last) {
        return Err("planner_event_clock_reversed".into());
    }
    let supervisor_wake =
        super::super::supervisor::take_wake(&c.directory, &c.session.to_string(), now, 60_000);
    if supervisor_wake {
        enqueue_supervisor_wake(&tx, &state.binding, now)?;
    }
    if !supervisor_wake
        && state
            .last_started_ms
            .is_some_and(|last| now - last < settings.cooldown_ms)
    {
        return Ok(None);
    }
    tx.execute(
        "UPDATE planner_event_log SET state=? WHERE state=? AND at<?",
        rusqlite::params![
            PlannerLogPhase::Expired.as_str(),
            PlannerLogPhase::Pending.as_str(),
            now.saturating_sub(settings.policy.max_event_age_ms)
        ],
    )?;
    let signals = logged_pending(&tx)?
        .into_iter()
        .filter(|e| e.at <= now && recheck_signal(&e.kind))
        .collect::<Vec<_>>();
    if signals.len() > 1 {
        for event in &signals {
            mark_collapsed(&tx, &event.id)?;
        }
        let triggers = signals
            .iter()
            .filter(|e| e.cause != "planner_event_backlog_collapsed")
            .collect::<Vec<_>>();
        insert_recheck(&tx, &state.binding, now, signals.len() as u64, &triggers)?;
    }
    let mut event: Option<(String,String)> = tx.query_row("SELECT id,body FROM planner_event_log WHERE state='pending' AND at<=? ORDER BY at,id LIMIT 1", [now], |r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if event.is_none()
        && cfg.orchestrator_owns_decisions()
        && !state.initial_available
        && let Some(settings) = cfg.discovery.as_ref()
    {
        let cadence = settings
            .digest_cadence_ms
            .unwrap_or(super::super::discovery::DEFAULT_DIGEST_CADENCE_MS);
        if state
            .last_started_ms
            .is_none_or(|last| now - last >= cadence)
        {
            let id = digest(format!("cadence:{}:{now}", state.binding).as_bytes());
            let body = json!({"id":id,"kind":"recheck","observed_ms":now,"session_binding":state.binding,"reason":"digest_cadence","cause":"digest_cadence","interval_ms":cadence,"last_started_ms":state.last_started_ms});
            tx.execute(
                "INSERT INTO planner_event_log VALUES(?,?,?,?)",
                rusqlite::params![id, now, body.to_string(), PlannerLogPhase::Pending.as_str()],
            )?;
            event = Some((id, body.to_string()));
        }
    }
    if event.is_none()
        && !state.initial_available
        && let Some(interval) = cfg
            .discovery
            .as_ref()
            .and_then(|d| d.idle_digest_interval_ms)
        && state
            .last_started_ms
            .is_none_or(|last| now - last >= interval)
    {
        let id = digest(format!("idle:{}:{now}", state.binding).as_bytes());
        let body = json!({"id":id,"kind":"recheck","observed_ms":now,"session_binding":state.binding,"reason":"idle_interval","interval_ms":interval,"last_started_ms":state.last_started_ms});
        tx.execute(
            "INSERT INTO planner_event_log VALUES(?,?,?,?)",
            rusqlite::params![id, now, body.to_string(), PlannerLogPhase::Pending.as_str()],
        )?;
        event = Some((id, body.to_string()));
    }
    if state.initial_available {
        let initial = digest(format!("initial:{}", state.binding).as_bytes());
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM planner_event_log WHERE id=?)",
            [&initial],
            |r| r.get(0),
        )?;
        let id = if exists {
            let count: i64 =
                tx.query_row("SELECT count(*) FROM planner_event_log", [], |r| r.get(0))?;
            digest(format!("operator-recovery:{}:{count}:{now}", state.binding).as_bytes())
        } else {
            initial
        };
        let body =
            json!({"id":id,"kind":"initial","observed_ms":now,"session_binding":state.binding});
        tx.execute(
            "INSERT INTO planner_event_log VALUES(?,?,?,?)",
            rusqlite::params![id, now, body.to_string(), PlannerLogPhase::Pending.as_str()],
        )?;
        event = Some((id, body.to_string()));
        state.initial_available = false;
    }
    let Some((event_id, body)) = event else {
        tx.commit()?;
        return Ok(None);
    };
    let id = digest(format!("attempt:{}:{event_id}", state.binding).as_bytes());
    let mut trigger: Value = serde_json::from_str(&body)?;
    trigger["observed_ms"]
        .as_i64()
        .ok_or("planner_event_time_invalid")?;
    let before_research = trigger.clone();
    fold_pending_research_tasks(&tx, &event_id, now, &mut trigger)?;
    // The startup row is host-written in this transaction. Keep its body equal
    // to the claim so a later reader sees the receipts that were collapsed.
    if trigger != before_research && trigger["kind"] == "initial" {
        tx.execute(
            "UPDATE planner_event_log SET body=? WHERE id=? AND state='pending'",
            rusqlite::params![trigger.to_string(), &event_id],
        )?;
    }
    let claim = Claim {
        id: id.clone(),
        event_id: event_id.clone(),
        trigger,
        binding: state.binding.clone(),
        portfolio_hash: portfolio_hash(p)?,
        started_ms: now,
        deadline_ms: now
            .checked_add(
                settings
                    .decision_timeout_ms
                    .unwrap_or(settings.policy.max_event_age_ms),
            )
            .ok_or("planner_event_clock_range")?,
        predecessor: if cfg.orchestrator_owns_decisions() {
            state.active.clone()
        } else {
            state.active.take()
        },
        previous_attempts: tx
            .query_row("SELECT count(*) FROM planner_attempt_log", [], |r| {
                r.get::<_, i64>(0)
            })?
            .try_into()?,
    };
    tx.execute(
        "INSERT INTO planner_attempt_log VALUES(?,?,?,?,NULL)",
        rusqlite::params![
            id,
            event_id,
            serde_json::to_string(&claim)?,
            PlannerLogPhase::Reserved.as_str()
        ],
    )?;
    tx.execute(
        "UPDATE planner_event_log SET state=? WHERE id=?",
        rusqlite::params![PlannerLogPhase::Reserved.as_str(), &claim.event_id],
    )?;
    tx.execute(
        "UPDATE planner_event_log SET state=? WHERE state=? AND at<=?",
        rusqlite::params![
            PlannerLogPhase::Collapsed.as_str(),
            PlannerLogPhase::Pending.as_str(),
            now
        ],
    )?;
    state.pending = Some(claim.clone());
    state.last_started_ms = Some(now);
    save(&tx, &state)?;
    tx.commit()?;
    Ok(Some(claim))
}
/// Complete only the exact reserved claim. Late/failed results never reactivate its predecessor.
#[allow(clippy::too_many_arguments)] // The reserved identity, result, current book and injected clock are independent checks.
pub(crate) fn settle(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    claim: &Claim,
    result: std::result::Result<Value, String>,
    now: i64,
    eligible: bool,
) -> Result<Option<Value>> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut state = load(&tx)?.ok_or("planner_event_state_missing")?;
    check(&state, c)?;
    if state.pending.as_ref() != Some(claim) || claim.binding != state.binding {
        return Err("planner_attempt_not_pending".into());
    }
    let reserved: String = tx.query_row(
        "SELECT state FROM planner_attempt_log WHERE id=?",
        [&claim.id],
        |r| r.get(0),
    )?;
    if PlannerLogPhase::parse(&reserved) != Ok(PlannerLogPhase::Reserved) {
        return Err("planner_attempt_not_pending".into());
    }
    let owner_policy = cfg.orchestrator_owns_decisions();
    let mut failure = None;
    let mut superseded = false;
    let sticky = state
        .blocked
        .as_deref()
        .filter(|reason| !recoverable_block(reason))
        .map(str::to_owned);
    let accepted = if let Some(blocked) = sticky {
        failure = Some(blocked);
        None
    } else if !eligible
        || p.stopped
        || p.halted
        || p.closed
        || local(now).date_naive() != c.session
        || !(u32::from(
            settings(cfg)
                .and_then(|s| s.decision_start_minute)
                .unwrap_or(480),
        )..930)
            .contains(&minute(now))
    {
        failure = Some("planner_completion_outside_session".to_owned());
        None
    } else if state.config_hash != config_hash(cfg)? {
        failure = Some("planner_event_configuration_changed".to_owned());
        None
    } else if now < claim.started_ms || now > claim.deadline_ms {
        failure = Some("planner_completion_expired_or_changed".to_owned());
        None
    } else if let Err(reason) = &result {
        failure = Some(settled_failure(reason));
        None
    } else if result.as_ref().is_ok_and(|plan| {
        plan["attempt_id"] != claim.id
            || plan["decision_id"] != format!("{}-{}", c.session, claim.id)
            || planner::validate_saved(c, plan).is_err()
            || (owner_policy && !owner_digest_completion(plan))
    }) {
        failure = Some("planner_result_binding_invalid".to_owned());
        None
    } else if portfolio_hash(p)? != claim.portfolio_hash {
        failure = Some("planner_snapshot_superseded".to_owned());
        superseded = true;
        None
    } else {
        match result {
            Ok(plan) => {
                if plan["attempt_id"] != claim.id
                    || plan["decision_id"] != format!("{}-{}", c.session, claim.id)
                    || planner::validate_saved(c, &plan).is_err()
                    || (owner_policy && !owner_digest_completion(&plan))
                {
                    failure = Some("planner_result_binding_invalid".to_owned());
                    None
                } else {
                    Some(plan)
                }
            }
            Err(_) => {
                failure = Some("planner_failed_no_replay".to_owned());
                None
            }
        }
    };
    let deferred = accepted
        .as_ref()
        .is_some_and(|plan| owner_policy || plan["luna_handoff_requested"] == false);
    let mut cadence_facts = None;
    if owner_policy && let Some(plan) = accepted.as_ref() {
        // The digest and its durable decision wake share this claim transaction.
        // A rollback leaves the claim pending and cannot activate a plan.
        let enqueued = super::super::decision_owner::enqueue_digest(
            &tx,
            c,
            cfg,
            claim,
            plan,
            now,
            state.last_cadence_facts_sha256.as_deref(),
        )?;
        if enqueued
            && super::super::decision_owner::is_cadence_trigger(&claim.trigger)
            && let Some(facts) = plan["owner_digest"]["facts_sha256"].as_str()
        {
            cadence_facts = Some(facts.to_owned());
        }
    }
    let status = PlannerLogPhase::settle(deferred, accepted.is_some(), superseded);
    let record = accepted
        .clone()
        .unwrap_or_else(|| json!({"status":failure}));
    if deferred {
        if !owner_policy {
            state.active = None;
        }
        state.blocked = None;
    } else if let Some(plan) = &accepted {
        let id = plan["decision_id"]
            .as_str()
            .ok_or("planner_saved_plan_invalid")?;
        // An owner-policy digest is deferred above and never reaches this
        // branch, so it cannot replace the committed owner plan head.
        tx.execute(
            "INSERT INTO planner_plan VALUES(?,?)",
            rusqlite::params![id, serde_json::to_string(plan)?],
        )?;
        state.active = Some(id.to_owned());
        state.blocked = None;
    } else if owner_policy && failure.as_deref().is_some_and(recoverable_block) {
        // A failed digest is one bad attempt, not a decision block. The owner
        // keeps the earlier valid digest and the attempt keeps its reason (#221).
        state.digest_failed = failure;
    } else if !superseded {
        state.blocked = failure;
    }
    tx.execute(
        "UPDATE planner_attempt_log SET state=?,result=? WHERE id=? AND state=?",
        rusqlite::params![
            status.as_str(),
            record.to_string(),
            claim.id,
            PlannerLogPhase::Reserved.as_str()
        ],
    )?;
    tx.execute(
        "UPDATE planner_event_log SET state=? WHERE id=?",
        rusqlite::params![status.as_str(), claim.event_id],
    )?;
    if let Some(facts) = cadence_facts {
        state.last_cadence_facts_sha256 = Some(facts);
    }
    state.pending = None;
    save(&tx, &state)?;
    tx.commit()?;
    Ok(if deferred { None } else { accepted })
}
pub(crate) fn status(db: &Connection, c: &Config) -> Result<Option<Value>> {
    let Some(state) = load(db)? else {
        return Ok(None);
    };
    check(&state, c)?;
    let pending: i64 = db.query_row(
        "SELECT count(*) FROM planner_event_log WHERE state='pending'",
        [],
        |r| r.get(0),
    )?;
    let attempts = db.prepare("SELECT id,event_id,state FROM planner_attempt_log ORDER BY rowid DESC LIMIT 20")?.query_map([],|r| Ok(json!({"attempt_id":r.get::<_,String>(0)?,"event_id":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let latest_review:Option<String>=db.query_row("SELECT result FROM planner_attempt_log WHERE state IN ('planned','deferred') ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0)).optional()?;
    let mut view = json!({"status":state.blocked.as_deref().unwrap_or(if state.pending.is_some(){"planning_no_new_entries"}else if state.active.is_some(){"validated_plan"}else{"waiting_for_eligible_event"}),"active_decision":state.active,"pending_events":pending,"pending_attempt":state.pending.as_ref().map(|p|&p.id),"attempts":attempts,"coverage":{"market_observations":state.market_observations,"missing_volume_observations":state.missing_volume,"missing_prior_close_observations":state.missing_prior_close}});
    if let Some(body) = latest_review {
        let reviewed: Value = serde_json::from_str(&body)?;
        view["last_review"] = json!({"model_id":reviewed["model_id"],"decision_id":reviewed["decision_id"],"handoff_requested":reviewed["luna_handoff_requested"],"stance":reviewed["parsed_plan"]["stance"],"notes":reviewed["parsed_plan"]["notes"]});
    }
    if let Some(reason) = state.digest_failed.as_deref() {
        view["last_digest_failure"] = json!(reason);
    }
    if let Some(plan) = active_plan(db, c)?.flatten() {
        let plan = planner::status(&plan);
        for key in ["decision_id", "model_id", "provider", "actions", "budgets"] {
            view[key] = plan[key].clone();
        }
    }
    let rows = db
        .prepare("SELECT id,at,state,body FROM planner_event_log ORDER BY rowid DESC LIMIT 20")?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut events = Vec::new();
    for (id, at, state, body) in rows {
        if body.len() > 16384 {
            return Err("planner_event_projection_bounds".into());
        }
        let event: Value = serde_json::from_str(&body)?;
        events.push(json!({"id":id,"observed_ms":at,"status":state,"kind":event["kind"],"instrument_key":event["instrument_key"]}));
    }
    view["events"] = json!(events);
    Ok(Some(view))
}

pub(crate) struct Worker {
    pub claim: Claim,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    pub receiver: std::sync::mpsc::Receiver<std::result::Result<Value, String>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Release);
    }
}
pub(crate) fn launch(
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    instruments: &[sadr_core::execution::Instrument],
    claim: Claim,
    context: Option<super::super::decision_evidence::Snapshot>,
) -> Worker {
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = Worker {
        claim: claim.clone(),
        cancel: cancel.clone(),
        receiver,
    };
    let c = c.clone();
    let cfg = cfg.clone();
    let p = p.clone();
    let instruments = instruments.to_vec();
    thread::spawn(move || {
        let result = planner::produce_clocked(
            &c,
            &cfg,
            &p,
            &instruments,
            None,
            false,
            Some(&claim),
            Some(&cancel),
            &millis,
            None,
            context.as_ref(),
        )
        .map_err(|e| planner::logged_reason(e.as_ref()).to_owned());
        let _ = sender.send(result);
    });
    worker
}
pub(crate) fn block(db: &mut Connection, c: &Config, reason: &str) -> Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if let Some(mut state) = load(&tx)? {
        check(&state, c)?;
        let escalate = state.blocked.as_deref() == Some(READ_TRANSIENT_BLOCK)
            && !recoverable_block(reason)
            && reason != READ_TRANSIENT_BLOCK;
        if state.blocked.is_none() || escalate {
            state.blocked = Some(reason.to_owned());
        }
        state.active = None;
        save(&tx, &state)?;
    }
    tx.commit()?;
    Ok(())
}
/// Clear only a transport/quota read block, and only when no attempt is
/// pending or unsettled. The caller must also have a validated fresh quote.
/// Failed attempts, their evidence and their charges are never replayed.
pub(crate) fn clear_transient_read_block(db: &mut Connection, c: &Config) -> Result<bool> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let Some(mut state) = load(&tx)? else {
        return Ok(false);
    };
    check(&state, c)?;
    if state.blocked.as_deref() != Some(READ_TRANSIENT_BLOCK) || state.pending.is_some() {
        return Ok(false);
    }
    let unsettled: i64 = tx.query_row(
        "SELECT count(*) FROM planner_attempt_log WHERE state NOT IN ('planned','failed','superseded','deferred')",
        [],
        |r| r.get(0),
    )?;
    if unsettled != 0 {
        return Ok(false);
    }
    let prior = state.blocked.take();
    tx.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'planner_read_block_cleared')",
        rusqlite::params![
            millis(),
            json!({"prior_block":prior,"validated_fresh_quote":true,"attempts_preserved":true,"no_replay":true})
                .to_string()
        ],
    )?;
    save(&tx, &state)?;
    tx.commit()?;
    Ok(true)
}
#[cfg(test)]
pub(crate) fn blocked(db: &Connection, c: &Config) -> Result<bool> {
    let Some(state) = load(db)? else {
        return Ok(false);
    };
    check(&state, c)?;
    Ok(state.blocked.is_some())
}
/// Operator and setup blocks cancel an in-flight run. A recoverable timing or
/// model failure does not.
pub(crate) fn sticky_blocked(db: &Connection, c: &Config) -> Result<bool> {
    let Some(state) = load(db)? else {
        return Ok(false);
    };
    check(&state, c)?;
    Ok(state
        .blocked
        .as_deref()
        .is_some_and(|reason| !recoverable_block(reason)))
}
/// Explicit externally archived news only. This adapter never fetches an article.
/// Supplied hashes/citations are declarations, not authenticated source provenance.
pub(crate) fn news(
    cfg: &planner::PlannerConfig,
    c: &Config,
    instruments: &[sadr_core::execution::Instrument],
) -> Result<Vec<NewsObservation>> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Feed {
        session_binding: String,
        session: NaiveDate,
        items: Vec<NewsObservation>,
    }
    let Some(path) = settings(cfg).and_then(|s| s.news_file.as_deref()) else {
        return Ok(vec![]);
    };
    let raw = crate::read_bounded(path, 262144).map_err(|_| "planner_news_unreadable")?;
    let feed: Feed = serde_json::from_value(
        sadr_core::native::decode(&raw).map_err(|_| "planner_news_invalid")?,
    )
    .map_err(|_| "planner_news_invalid")?;
    if feed.session_binding != book_binding(c)?
        || feed.session != c.session
        || feed.items.len() > 200
        || feed
            .items
            .iter()
            .any(|n| !instruments.iter().any(|i| i.key == n.instrument_key))
    {
        return Err("planner_news_binding_invalid".into());
    }
    Ok(feed.items)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn observe_quotes(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    instruments: &[sadr_core::execution::Instrument],
    quotes: &std::collections::BTreeMap<String, Quote>,
    rows: &serde_json::Map<String, Value>,
    now: i64,
) -> Result<()> {
    if settings(cfg).is_none() {
        return Ok(());
    }
    // Latest daily close strictly before this session. Upstox v2 full quotes have
    // no dated previous-close field; undated ohlc.close tracks the last price.
    let reference: Option<String> = db
        .query_row(
            "SELECT body FROM planner_plan ORDER BY rowid DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let reference: Option<Value> = reference.map(|v| serde_json::from_str(&v)).transpose()?;
    if let Some(reference) = &reference {
        planner::validate_saved(c, reference)?;
    }
    let session_closes = planner::session_references(c)?;
    let mut batch = Vec::new();
    for instrument in instruments {
        let Some(quote) = quotes
            .get(&instrument.key)
            .filter(|q| q.fresh(now, sadr_core::market::MAX_FINANCIAL_AGE_MS))
        else {
            continue;
        };
        if local(quote.source).date_naive() != c.session
            || local(quote.receipt).date_naive() != c.session
        {
            continue;
        }
        let Some(row) = rows
            .values()
            .find(|r| r["instrument_token"] == instrument.key)
        else {
            continue;
        };
        // Scanner screens also apply to quote-derived Luna event candidates.
        // Held-position events retain their protection/exit bypass.
        let policy = cfg
            .discovery
            .as_ref()
            .and_then(|d| d.screens.clone())
            .unwrap_or_else(sadr_core::execution::ScreenPolicy::recommended);
        if cfg.discovery.is_some()
            && !p.positions.contains_key(&instrument.key)
            && !sadr_core::execution::Scanner::market_allows_with_policy(
                now, instrument, quote, row, &policy,
            )
        {
            continue;
        }
        let prior_close = reference
            .as_ref()
            .and_then(|v| planner::close_from_references(&v["market_references"], &instrument.key))
            .or_else(|| {
                reference
                    .as_ref()
                    .and_then(|v| v["input"]["shortlist"].as_array())
                    .and_then(|a| {
                        a.iter().find(|v| {
                            v["instrument"]["key"] == instrument.key
                                && v["as_of"] == c.previous_session.to_string()
                        })
                    })
                    .and_then(|v| v["prior_close"].as_str().map(str::to_owned))
            })
            .or_else(|| planner::close_from_references(&session_closes, &instrument.key))
            .map(|text| sadr_core::finance::Price::parse(&text))
            .transpose()?;
        // Upstox documents ohlc.open as the session open and volume as today's
        // traded volume: https://upstox.com/developer/api-documentation/get-full-market-quote/
        let session_open = row["ohlc"]["open"]
            .as_number()
            .and_then(|v| sadr_core::finance::Price::parse(&v.to_string()).ok());
        batch.push(MarketObservation {
            instrument: instrument.clone(),
            quote: quote.clone(),
            cumulative_volume: row["volume"].as_u64(),
            prior_close,
            session_open,
            exit_intent: p
                .positions
                .get(&instrument.key)
                .and_then(|p| p.exit_intent.clone()),
        });
    }
    observe(db, c, cfg, p, &batch, &[], now)
}
