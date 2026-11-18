use super::*;
use crate::runtime::phases::PlannerLogPhase;

/// Make one durable owner recheck for a terminal registered-analysis task.
/// The event identity is bound to the book/task revision rather than a clock,
/// so a driver restart can retry insertion without manufacturing another wake.
pub(crate) fn enqueue_owner_research_event(
    db: &Connection,
    c: &Config,
    task_id: &str,
    revision: u32,
    result_sha256: Option<&str>,
    status: &str,
    observed_ms: i64,
) -> Result<()> {
    enqueue_owner_research_event_kind(
        db,
        c,
        task_id,
        revision,
        result_sha256,
        status,
        observed_ms,
        "registered",
    )
}

/// Terminal event with an explicit task capability. The owner must use the
/// candidate tools for a candidate receipt, never the registered alias.
#[allow(clippy::too_many_arguments)]
pub(crate) fn enqueue_owner_research_event_kind(
    db: &Connection,
    c: &Config,
    task_id: &str,
    revision: u32,
    result_sha256: Option<&str>,
    status: &str,
    observed_ms: i64,
    task_kind: &str,
) -> Result<()> {
    let binding = book_binding(c)?;
    let valid_id = task_id.len() == 64
        && task_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    let valid_result_hash = result_sha256.is_none_or(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    let result_matches_status = (status == "succeeded") == result_sha256.is_some();
    if revision == 0
        || !valid_id
        || !valid_result_hash
        || !result_matches_status
        || observed_ms < 0
        || !matches!(status, "succeeded" | "terminated" | "cancelled" | "refused")
        || !matches!(task_kind, "registered" | "candidate")
    {
        return Err("planner_owner_research_event_invalid".into());
    }
    if !exists(db)? {
        return Err("planner_owner_research_authority_missing".into());
    }
    let id = digest(format!("owner-research-terminal:{binding}:{task_id}:{revision}").as_bytes());
    let body = json!({
        "id":id.clone(),
        "kind":"recheck",
        "reason":"research_task_terminal",
        "observed_ms":observed_ms,
        "session_binding":binding,
        "research_task":{
            "task_id":task_id,
            "revision":revision,
            "kind":task_kind,
            "status":status,
            "result_sha256":result_sha256,
        }
    })
    .to_string();
    db.execute(
        "INSERT OR IGNORE INTO planner_event_log VALUES(?,?,?,?)",
        rusqlite::params![id, observed_ms, body, PlannerLogPhase::Pending.as_str()],
    )?;
    let existing: String = db.query_row(
        "SELECT body FROM planner_event_log WHERE id=?",
        [&id],
        |row| row.get(0),
    )?;
    if existing != body {
        return Err("planner_event_identity_changed".into());
    }
    Ok(())
}

/// A host exit review becomes a proximity event. It does not trade.
pub(crate) fn enqueue_exit_review(
    db: &Connection,
    binding: &str,
    key: &str,
    reason: &str,
    at: i64,
) -> Result<()> {
    if !exists(db)? {
        return Ok(());
    }
    let id = digest(format!("exit-review:{binding}:{key}:{reason}:{at}").as_bytes());
    let present: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM planner_event_log WHERE id=?)",
        [&id],
        |row| row.get(0),
    )?;
    if present {
        return Ok(());
    }
    let body = json!({"id":id,"kind":"proximity","reason":reason,"instrument_key":key,"observed_ms":at,"session_binding":binding});
    db.execute(
        "INSERT INTO planner_event_log VALUES(?,?,?,?)",
        rusqlite::params![id, at, body.to_string(), PlannerLogPhase::Pending.as_str()],
    )?;
    Ok(())
}
/// The logged reason for a digest: startup, idle_interval, a scanner trigger, or the event kind.
pub(crate) fn trigger_reason(trigger: &Value) -> String {
    match trigger["kind"].as_str() {
        Some("initial") => "startup".into(),
        Some("recheck") => trigger["cause"]
            .as_str()
            .or(trigger["reason"].as_str())
            .unwrap_or("recheck")
            .into(),
        Some(kind) => kind.into(),
        None => "unknown".into(),
    }
}
pub(crate) fn recheck_signal(kind: &str) -> bool {
    matches!(
        kind,
        "price_move" | "volume_move" | "gap" | "proximity" | "recheck"
    )
}
pub(crate) struct LoggedEvent {
    pub(crate) id: String,
    pub(crate) at: i64,
    pub(crate) kind: String,
    pub(crate) instrument_key: String,
    /// What actually fired: the event kind, or a recheck's own reason (#39).
    pub(crate) cause: String,
    pub(crate) research_task: Option<Value>,
}
pub(crate) fn logged_pending(db: &Connection) -> Result<Vec<LoggedEvent>> {
    let mut statement = db
        .prepare("SELECT id,at,body FROM planner_event_log WHERE state='pending' ORDER BY at,id")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut events = Vec::new();
    for row in rows {
        let (id, at, body) = row?;
        if body.len() > 16_384 {
            return Err("planner_event_projection_bounds".into());
        }
        let value: Value =
            serde_json::from_str(&body).map_err(|_| "planner_event_state_invalid")?;
        let kind = value["kind"].as_str().unwrap_or_default().to_owned();
        let cause = if kind == "recheck" {
            value["cause"]
                .as_str()
                .or(value["reason"].as_str())
                .unwrap_or("recheck")
                .to_owned()
        } else {
            kind.clone()
        };
        events.push(LoggedEvent {
            id,
            at,
            kind,
            instrument_key: value["instrument_key"]
                .as_str()
                .or(value["trigger"]["keys"][0].as_str())
                .unwrap_or_default()
                .to_owned(),
            cause,
            research_task: value.get("research_task").cloned(),
        });
    }
    Ok(events)
}
pub(crate) fn pending_count(db: &Connection) -> Result<i64> {
    Ok(db.query_row(
        "SELECT count(*) FROM planner_event_log WHERE state='pending'",
        [],
        |row| row.get(0),
    )?)
}
pub(crate) fn mark_collapsed(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.execute(
        "UPDATE planner_event_log SET state=? WHERE id=? AND state=?",
        rusqlite::params![
            PlannerLogPhase::Collapsed.as_str(),
            id,
            PlannerLogPhase::Pending.as_str()
        ],
    )? == 1)
}
/// Most recent collapsed trigger keys carried into a recheck (#39).
pub(crate) const COLLAPSED_KEYS_MAX: usize = 8;
pub(crate) fn insert_recheck(
    db: &Connection,
    binding: &str,
    now: i64,
    collapsed: u64,
    triggers: &[&LoggedEvent],
) -> Result<()> {
    let id = digest(format!("recheck:{binding}:{now}:{collapsed}").as_bytes());
    let mut kinds = std::collections::BTreeMap::<&str, u64>::new();
    for event in triggers {
        *kinds.entry(event.cause.as_str()).or_default() += 1;
    }
    let mut recent = triggers.to_vec();
    recent.sort_by(|a, b| (b.at, &b.id).cmp(&(a.at, &a.id)));
    let mut keys = Vec::new();
    let mut research_tasks: Vec<Value> = Vec::new();
    let mut research_tasks_truncated = false;
    for event in recent {
        let entry = json!([event.cause, event.instrument_key]);
        if !event.instrument_key.is_empty() && !keys.contains(&entry) {
            keys.push(entry);
        }
        if keys.len() >= COLLAPSED_KEYS_MAX {
            break;
        }
    }
    for event in triggers {
        if let Some(task) = &event.research_task
            && !research_tasks.iter().any(|existing| {
                existing["task_id"] == task["task_id"] && existing["revision"] == task["revision"]
            })
        {
            let mut task = task.clone();
            task["source_event_id"] = json!(event.id);
            task["source_observed_ms"] = json!(event.at);
            if research_tasks.len() < COLLAPSED_KEYS_MAX {
                research_tasks.push(task);
            } else {
                research_tasks_truncated = true;
            }
        }
    }
    // The dominant collapsed kind is the real cause; the backlog is only plumbing.
    let cause = kinds
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        .map(|(kind, _)| {
            kind.strip_prefix("collapsed_")
                .map(|k| format!("collapsed_{k}"))
                .unwrap_or_else(|| format!("collapsed_{kind}"))
        })
        .unwrap_or_else(|| "planner_event_backlog_collapsed".into());
    let body = json!({
        "id": id,
        "kind": "recheck",
        "observed_ms": now,
        "session_binding": binding,
        "reason": "planner_event_backlog_collapsed",
        "collapsed": collapsed,
        "cause": cause,
        "collapsed_kinds": kinds,
        "collapsed_triggers": keys,
        "collapsed_research_tasks": research_tasks,
        "collapsed_research_tasks_truncated": research_tasks_truncated,
    })
    .to_string();
    if let Some(existing) = db
        .query_row(
            "SELECT body FROM planner_event_log WHERE id=?",
            [&id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    {
        if existing != body {
            return Err("planner_event_identity_changed".into());
        }
        return Ok(());
    }
    db.execute(
        "INSERT INTO planner_event_log VALUES(?,?,?,?)",
        rusqlite::params![id, now, body, PlannerLogPhase::Pending.as_str()],
    )?;
    Ok(())
}
/// Copy research receipts from events this reservation will collapse.
/// The startup event replaces an earlier pending row, but the receipt stays on
/// the claim. Original event bodies are not rewritten.
pub(crate) fn fold_pending_research_tasks(
    db: &Connection,
    selected_id: &str,
    now: i64,
    trigger: &mut Value,
) -> Result<()> {
    let mut statement = db.prepare(
        "SELECT id,at,body FROM planner_event_log WHERE state='pending' AND id!=? AND at<=? ORDER BY at,id",
    )?;
    let rows = statement.query_map(rusqlite::params![selected_id, now], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut tasks = trigger
        .get("collapsed_research_tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut truncated = trigger
        .get("collapsed_research_tasks_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let direct = trigger
        .get("research_task")
        .map(|task| (task["task_id"].clone(), task["revision"].clone()));
    for row in rows {
        let (id, at, body) = row?;
        if body.len() > 16_384 {
            return Err("planner_event_projection_bounds".into());
        }
        let value: Value =
            serde_json::from_str(&body).map_err(|_| "planner_event_state_invalid")?;
        let mut incoming = Vec::new();
        if let Some(task) = value.get("research_task") {
            let mut task = task.clone();
            if task
                .get("source_event_id")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                task["source_event_id"] = json!(id);
                task["source_observed_ms"] = json!(at);
            }
            incoming.push(task);
        }
        if let Some(nested) = value
            .get("collapsed_research_tasks")
            .and_then(Value::as_array)
        {
            incoming.extend(nested.iter().cloned());
        }
        for task in incoming {
            let key = (task["task_id"].clone(), task["revision"].clone());
            if direct.as_ref() == Some(&key) {
                continue;
            }
            if tasks.iter().any(|existing| {
                existing["task_id"] == task["task_id"] && existing["revision"] == task["revision"]
            }) {
                continue;
            }
            if tasks.len() < COLLAPSED_KEYS_MAX {
                tasks.push(task);
            } else {
                truncated = true;
            }
        }
    }
    if !tasks.is_empty() || truncated {
        trigger["collapsed_research_tasks"] = Value::from(tasks);
        trigger["collapsed_research_tasks_truncated"] = json!(truncated);
    }
    Ok(())
}
/// Latest recheck per kind and instrument stays pending. A stuck or over-limit
/// backlog is marked `collapsed` and replaced by one fresh recheck. Fills, news
/// and the initial event are left untouched, and no row is deleted.
pub(crate) fn collapse_recheck_backlog(
    db: &Connection,
    binding: &str,
    max_pending: i64,
    now: i64,
    recover_stuck: bool,
) -> Result<i64> {
    let pending = logged_pending(db)?;
    let mut collapsed = 0u64;
    let mut collapsed_ids = std::collections::BTreeSet::<String>::new();
    if recover_stuck {
        for event in &pending {
            if recheck_signal(&event.kind) && mark_collapsed(db, &event.id)? {
                collapsed += 1;
                collapsed_ids.insert(event.id.clone());
            }
        }
    } else {
        let mut newest = std::collections::BTreeMap::<(String, String), (i64, String)>::new();
        for event in &pending {
            if !recheck_signal(&event.kind) {
                continue;
            }
            let key = (event.kind.clone(), event.instrument_key.clone());
            let replace = match newest.get(&key) {
                Some((at, id)) => (event.at, event.id.as_str()) > (*at, id.as_str()),
                None => true,
            };
            if replace {
                newest.insert(key, (event.at, event.id.clone()));
            }
        }
        for event in &pending {
            if !recheck_signal(&event.kind) {
                continue;
            }
            let key = (event.kind.clone(), event.instrument_key.clone());
            let keeper = newest.get(&key).is_some_and(|(_, id)| id == &event.id);
            if !keeper && mark_collapsed(db, &event.id)? {
                collapsed += 1;
                collapsed_ids.insert(event.id.clone());
            }
        }
        let signals = logged_pending(db)?
            .iter()
            .filter(|event| recheck_signal(&event.kind))
            .count();
        if pending_count(db)? > max_pending && signals > 0 {
            for event in logged_pending(db)? {
                if recheck_signal(&event.kind) && mark_collapsed(db, &event.id)? {
                    collapsed += 1;
                    collapsed_ids.insert(event.id.clone());
                }
            }
        }
    }
    let signals = logged_pending(db)?
        .iter()
        .filter(|event| recheck_signal(&event.kind))
        .count();
    if pending_count(db)? < max_pending && signals == 0 && (collapsed > 0 || recover_stuck) {
        let triggers = pending
            .iter()
            .filter(|event| {
                collapsed_ids.contains(&event.id)
                    && event.cause != "planner_event_backlog_collapsed"
            })
            .collect::<Vec<_>>();
        insert_recheck(db, binding, now, collapsed, &triggers)?;
    }
    pending_count(db)
}
/// Observations are copied only after protection/financial commit. No network under this lock.
pub(crate) fn observe(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    batch: &[MarketObservation],
    news: &[NewsObservation],
    now: i64,
) -> Result<()> {
    let Some(settings) = settings(cfg) else {
        return Ok(());
    };
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let Some(mut state) = load(&tx)? else {
        return Err("planner_event_state_missing".into());
    };
    check(&state, c)?;
    // Retry a recoverable planner failure and a full recheck queue. Operator,
    // configuration, fill-history, data, and news blocks stay sticky.
    let original = state.clone();
    let recover_stuck = state.blocked.as_deref() == Some("planner_event_queue_full");
    if state
        .blocked
        .as_deref()
        .is_some_and(|reason| !recoverable_block(reason))
    {
        return Ok(());
    }
    state.blocked = None;
    let result = (|| -> Result<()> {
        if state.config_hash != config_hash(cfg)? {
            return Err("planner_event_configuration_changed".into());
        }
        if p.fills.len() < state.fill_count
            || digest(&serde_json::to_vec(&p.fills[..state.fill_count])?) != state.fill_hash
        {
            return Err("planner_event_fill_history_changed".into());
        }
        let mut events = Vec::new();
        for observation in batch {
            events.extend(
                state
                    .detector
                    .observe_market(&settings.policy, observation, now)?,
            );
            state.market_observations = state.market_observations.saturating_add(1);
            state.missing_volume = state
                .missing_volume
                .saturating_add(u64::from(observation.cumulative_volume.is_none()));
            state.missing_prior_close = state
                .missing_prior_close
                .saturating_add(u64::from(observation.prior_close.is_none()));
        }
        for (index, fill) in p.fills.iter().enumerate().skip(state.fill_count) {
            events.extend(state.detector.observe_fill(
                &settings.policy,
                &format!("{}:{index}", state.binding),
                fill,
                now,
            )?);
        }
        state.fill_count = p.fills.len();
        state.fill_hash = digest(&serde_json::to_vec(&p.fills)?);
        for item in news {
            events.extend(state.detector.observe_news(&settings.policy, item, now)?);
        }
        for event in &events {
            append(&tx, event)?;
        }
        let binding = state.binding.clone();
        collapse_recheck_backlog(
            &tx,
            &binding,
            settings.max_pending as i64,
            now,
            recover_stuck,
        )?;
        tx.execute(
            "UPDATE planner_event_log SET state=? WHERE state=? AND at<?",
            rusqlite::params![
                PlannerLogPhase::Expired.as_str(),
                PlannerLogPhase::Pending.as_str(),
                now.saturating_sub(settings.policy.max_event_age_ms)
            ],
        )?;
        if pending_count(&tx)? > settings.max_pending as i64 {
            return Err("planner_event_queue_full".into());
        }
        Ok(())
    })();
    if let Err(error) = result {
        // Keep observed evidence and the original claim. A detector error cannot undo protection.
        state = original;
        state.blocked = Some(
            match error.to_string().as_str() {
                "planner_event_queue_full" => "planner_event_queue_full",
                "planner_event_fill_history_changed" => "planner_event_fill_history_changed",
                "planner_event_configuration_changed" => "planner_event_configuration_changed",
                _ => "planner_event_observation_invalid",
            }
            .into(),
        );
        state.active = None;
    }
    save(&tx, &state)?;
    tx.commit()?;
    Ok(())
}
/// A scanner refresh wakes Luna only for deterministic prospect triggers: a sampled
/// move at or beyond `trigger_move_bps` on a key not triggered within the cooldown.
/// Returns the trigger reason when one was logged.
pub(crate) fn scan_refreshed(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    receipt: i64,
    candidates: &[(String, Option<f64>)],
    market_baseline: Option<f64>,
    market_sample_count: usize,
) -> Result<Option<String>> {
    if cfg.uses_pi_paper() {
        return Ok(None);
    }
    let Some(discovery) = cfg.discovery.as_ref() else {
        return Ok(None);
    };
    let champion = discovery
        .strategy
        .as_ref()
        .filter(|s| s.enabled)
        .and_then(|s| s.specs.iter().find(|spec| spec.id == s.champion));
    if champion.is_none()
        && discovery.trigger_market_relative == Some(true)
        && market_baseline.is_none()
    {
        return Ok(None);
    }
    let threshold = if champion.is_some() {
        1.0
    } else {
        discovery
            .trigger_move_bps
            .unwrap_or(super::super::discovery::DEFAULT_TRIGGER_MOVE_BPS)
    };
    if discovery.strategy.as_ref().is_some_and(|s| s.enabled) && champion.is_none() {
        return Err("strategy_champion_missing".into());
    }
    let strategy_identity = champion
        .map(nivra_store::strategy_results::proposal_hash)
        .transpose()?;
    let cooldown = champion.map(|s| s.cooldown_ms).unwrap_or_else(|| {
        discovery
            .trigger_cooldown_ms
            .unwrap_or(super::super::discovery::DEFAULT_TRIGGER_COOLDOWN_MS)
    });
    let gap = discovery
        .min_trigger_gap_ms
        .unwrap_or(super::super::discovery::DEFAULT_MIN_TRIGGER_GAP_MS);
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut logged = None;
    if let Some(state) = load(&tx)? {
        check(&state, c)?;
        // Keys that fired stay disarmed until seen back below the threshold.
        let armed_path = c.directory.join("model/scanner-trigger.json");
        let mut fired: std::collections::BTreeSet<String> = fs::read(&armed_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .filter(|v| {
                v["session_binding"] == state.binding.as_str()
                    && v["strategy_identity"] == json!(strategy_identity)
            })
            .and_then(|v| serde_json::from_value(v["fired"].clone()).ok())
            .unwrap_or_default();
        let before = fired.clone();
        let rearm_market = if champion.is_none() && discovery.trigger_market_relative == Some(true)
        {
            market_baseline.unwrap_or(0.0)
        } else {
            0.0
        };
        for (k, bps) in candidates {
            if bps.is_some_and(|b| (b - rearm_market).abs() < threshold) {
                fired.remove(k);
            }
        }
        let last: Option<i64> = tx.query_row(
            "SELECT max(at) FROM planner_event_log WHERE at<=? AND body LIKE '%scanner_prospect_move%'",
            [receipt],
            |r| r.get(0),
        )?;
        let in_gap = last.is_some_and(|l| receipt - l < gap);
        if state.blocked.is_none() && !in_gap {
            let mut recent = std::collections::BTreeSet::new();
            let mut stmt = tx.prepare(
                "SELECT body FROM planner_event_log WHERE at>? AND at<=? AND body LIKE '%scanner_prospect_move%'",
            )?;
            let rows = stmt.query_map([receipt.saturating_sub(cooldown), receipt], |r| {
                r.get::<_, String>(0)
            })?;
            for body in rows {
                if let Ok(v) = serde_json::from_str::<Value>(&body?) {
                    if let Some(spec) = champion
                        && (v["trigger"]["strategy_spec_id"] != spec.id
                            || v["trigger"]["strategy_spec_version"] != spec.version
                            || v["trigger"]["strategy_spec_hash"] != json!(strategy_identity))
                    {
                        continue;
                    }
                    for k in v["trigger"]["keys"].as_array().into_iter().flatten() {
                        if let Some(k) = k.as_str() {
                            recent.insert(k.to_owned());
                        }
                    }
                }
            }
            drop(stmt);
            let market = if champion.is_none() && discovery.trigger_market_relative == Some(true) {
                market_baseline.unwrap_or(0.0)
            } else {
                0.0
            };
            let mut hits = candidates
                .iter()
                .filter_map(|(k, bps)| Some((k, (*bps)? - market)))
                .filter(|(k, bps)| {
                    bps.abs() >= threshold && !recent.contains(*k) && !fired.contains(*k)
                })
                .map(|(k, bps)| (k.clone(), bps))
                .collect::<Vec<_>>();
            hits.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()).then_with(|| a.0.cmp(&b.0)));
            if !hits.is_empty() {
                let keys = hits.iter().map(|h| h.0.clone()).collect::<Vec<_>>();
                let moves = hits.iter().map(|h| h.1).collect::<Vec<_>>();
                let reason = format!(
                    "scanner_prospect_move:{} keys >= {threshold}bps (top {} {:.0}bps)",
                    keys.len(),
                    keys[0],
                    moves[0]
                );
                let reason = format!(
                    "{reason} market_baseline_bps={market_baseline:?} market_samples={market_sample_count}"
                );
                let id = digest(format!("scanner-refresh:{}:{receipt}", state.binding).as_bytes());
                let body = json!({"id":id,"kind":"recheck","observed_ms":receipt,"session_binding":state.binding,"reason":"scanner_prospect_move","trigger":{"strategy_spec_id":champion.map(|s|s.id.as_str()),"strategy_spec_version":champion.map(|s|s.version),"strategy_spec_hash":strategy_identity,"kind":"scanner_prospect_move","threshold_bps":threshold,"cooldown_ms":cooldown,"market_baseline_bps":market_baseline,"market_sample_count":market_sample_count,"baseline_source":"all_fresh_cache","keys":keys,"moves_bps":moves}});
                tx.execute(
                    "INSERT OR IGNORE INTO planner_event_log VALUES(?,?,?,?)",
                    rusqlite::params![
                        id,
                        receipt,
                        body.to_string(),
                        PlannerLogPhase::Pending.as_str()
                    ],
                )?;
                if let Some(settings) = settings(cfg) {
                    collapse_recheck_backlog(
                        &tx,
                        &state.binding,
                        settings.max_pending as i64,
                        receipt,
                        false,
                    )?;
                }
                fired.extend(keys.iter().cloned());
                logged = Some(reason);
            }
        }
        if fired != before {
            if let Some(dir) = armed_path.parent() {
                fs::create_dir_all(dir)?;
            }
            rollover::atomic(
                &armed_path,
                &json!({"session_binding":state.binding,"strategy_identity":strategy_identity,"fired":fired}),
            )?;
        }
    }
    tx.commit()?;
    Ok(logged)
}
