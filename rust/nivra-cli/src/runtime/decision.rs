//! Durable digest-to-orchestrator work. Models never write this ledger or fill.
//! The event journal owns original digest evidence; this ledger pins its exact
//! bytes and atomically joins a persistent turn to the existing paper plan head.
use super::*;
use nivra_core::execution::{Instrument, Portfolio};
use nivra_core::finance::{Amount, Price};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) struct Context<'a> {
    pub db: &'a mut Connection,
    pub portfolio: &'a Portfolio,
    pub instruments: &'a [Instrument],
}

fn exists(db: &Connection) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='owner_digest_work')", [], |r| r.get(0))?)
}

pub(super) fn retire_for_pi_cutover(db: &Connection, c: &Config) -> Result<()> {
    if exists(db)? {
        db.execute(
            "UPDATE owner_digest_work SET state='superseded',result=? WHERE binding=? AND state IN ('pending','running')",
            rusqlite::params![
                json!({"version":1,"status":"superseded","reason":"pi_authority_cutover","no_replay":true}).to_string(),
                book_binding(c)?
            ],
        )?;
    }
    Ok(())
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

pub(crate) fn in_window(c: &Config, now: i64) -> bool {
    local(now).date_naive() == c.session
        && (c.execution.session_start_minute..c.execution.session_end_minute).contains(&minute(now))
}

pub(crate) fn supersede_plan_actions(
    db: &Connection,
    c: &Config,
    id: &str,
    reason: &str,
) -> Result<()> {
    let raw: String = db.query_row("SELECT body FROM planner_plan WHERE id=?", [id], |r| {
        r.get(0)
    })?;
    let plan: Value = serde_json::from_str(&raw)?;
    planner::validate_saved(c, &plan)?;
    if plan["decision_id"] != id {
        return Err("owner_plan_identity_invalid".into());
    }
    let verdicts: Vec<planner::Verdict> = serde_json::from_value(plan["verdicts"].clone())?;
    for verdict in verdicts.into_iter().filter(|v| v.action.is_some()) {
        db.execute(
            "INSERT OR IGNORE INTO planner_action VALUES(?,?,?)",
            rusqlite::params![id, verdict.index as i64, reason],
        )?;
    }
    Ok(())
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
pub(crate) use tests::decision_fixture_config;
