use super::*;

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
