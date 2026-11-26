use super::*;

pub(super) fn evidence(bundle: &Value, id: &str) -> Value {
    match id {
        "bundle" => {
            let mut projection = bundle.clone();
            if let Some(list) = bundle["allowlist"].as_array() {
                projection.as_object_mut().unwrap().remove("allowlist");
                projection["allowlist_reference"] = json!({"count":list.len(),"sha256":digest(&serde_json::to_vec(list).unwrap_or_default()),"location":"original_request.allowlist","omitted_from_this_tool_result":true});
            }
            projection
                .as_object_mut()
                .unwrap()
                .remove("historical_features");
            if bundle["shortlist"].is_array() {
                let shortlist = shortlist_evidence(bundle);
                projection["shortlist"] = shortlist["rows"].clone();
                projection["shortlist_reference"] = shortlist["reference"].clone();
            }
            if let Some(discovery) = projection
                .get_mut("discovery")
                .and_then(Value::as_object_mut)
            {
                discovery.remove("prospects");
                discovery.insert(
                    "prospects_location".into(),
                    json!("get_scanner_prospects; request get_instrument_features by key"),
                );
            }
            projection["feature_access"] = json!({"tool":"get_instrument_features","available":bundle["historical_features"].as_array().map(|a|a.len())});
            projection
        }
        "portfolio" => {
            json!({"cash":bundle["cash"],"positions":bundle["positions"],"session":bundle["session"]})
        }
        "shortlist" => shortlist_evidence(bundle),
        "fees" => bundle["fees"].clone(),
        "session_evidence" => bundle["session_evidence"].clone(),
        _ => Value::Null,
    }
}
fn shortlist_evidence(bundle: &Value) -> Value {
    let Some(rows) = bundle["shortlist"].as_array() else {
        return json!({"status":"unknown","reason":"shortlist_unavailable","rows":[],"execution_authorized":false});
    };
    let compact: Vec<_> = rows
        .iter()
        .take(MAX_INSTRUMENT_KEYS)
        .map(|row| {
            let mut compact = serde_json::Map::new();
            for name in [
                "instrument",
                "instrument_key",
                "history_as_of",
                "history_current",
                "last_close",
                "return_1d",
                "return_5d",
                "return_20d",
                "volatility_20d",
                "volume_z_score",
                "gap",
                "liquidity",
            ] {
                if let Some(value) = row.get(name) {
                    compact.insert(name.into(), value.clone());
                }
            }
            Value::Object(compact)
        })
        .collect();
    json!({"status":"available","rows":compact,"reference":{
        "count":rows.len(),"presented":compact.len(),"rows_truncated":compact.len()<rows.len(),
        "sha256":digest(&serde_json::to_vec(rows).unwrap_or_default()),"omitted_details":true,
        "detail_tool":"get_instrument_features","location":"original_request.shortlist"},"execution_authorized":false})
}
pub(super) fn instrument_features(bundle: &Value, keys: &[String]) -> Result<Value> {
    let allowed = bundle["allowlist"]
        .as_array()
        .ok_or("harness_current_universe_unavailable")?;
    // Unknown keys are reported per key; the allowlisted remainder is still answered.
    let (keys, rejected): (Vec<&String>, Vec<&String>) = keys
        .iter()
        .partition(|k| allowed.iter().any(|i| i["key"] == **k));
    if keys.is_empty() {
        return Err("harness_features_not_allowlisted".into());
    }
    let rows = keys.iter().map(|key| {
        bundle["historical_features"].as_array().into_iter().flatten().find(|r|r["instrument"]["key"]==**key).cloned()
            .unwrap_or_else(||json!({"instrument_key":key,"status":"history_unavailable","financial_quote":false}))
    }).collect::<Vec<_>>();
    let scanner = keys
        .iter()
        .filter_map(|key| {
            bundle["discovery"]["prospects"]
                .as_array()
                .and_then(|rows| rows.iter().find(|r| r["instrument_key"] == **key))
                .cloned()
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"session":bundle["session"],"features":rows,"scanner_observations":scanner,"rejected_keys":rejected.iter().map(|k|json!({"instrument_key":k,"reason":"not_in_validated_allowlist"})).collect::<Vec<_>>(),"execution_authorized":false}),
    )
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse::<f64>().ok()))
}
pub(super) fn scanner_prospects(bundle: &Value, limit: usize, sort: &str) -> Result<Value> {
    let Some(rows) = bundle["discovery"]["prospects"].as_array() else {
        return Ok(
            json!({"status":"unknown","reason":"scanner_prospects_unavailable","prospects":[],"execution_authorized":false}),
        );
    };
    let median = number(&bundle["discovery"]["market_median_bps"]);
    let trigger_reason = bundle["trigger"]["reason"]
        .as_str()
        .or_else(|| bundle["trigger"]["cause"].as_str())
        .unwrap_or("none");
    let mut rows = rows
        .iter()
        .map(|row| {
            let change = number(&row["sample_change_bps"]);
            json!({
                "instrument_key":row["instrument_key"],
                "symbol":row["symbol"],
                "sample_change_bps":row["sample_change_bps"],
                "market_relative_bps":change.zip(median).map(|(c,m)|c-m),
                "bid":row["bid"],
                "ask":row["ask"],
                "bid_units":row["bid_units"],
                "ask_units":row["ask_units"],
                "quote_age_ms":row["quote_age_ms"],
                "screening_age_ms":row["screening_age_ms"],
                "observed_ms":row["observed_ms"],
                "screening_reason":row["screening_reason"],
                "trigger_reason":trigger_reason,
                "financial_quote":false,
            })
        })
        .collect::<Vec<_>>();
    match sort {
        "scanner" => {}
        "change_bps" => rows.sort_by(|a, b| {
            number(&b["sample_change_bps"])
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&number(&a["sample_change_bps"]).unwrap_or(f64::NEG_INFINITY))
                .then_with(|| {
                    a["instrument_key"]
                        .as_str()
                        .cmp(&b["instrument_key"].as_str())
                })
        }),
        "market_relative_bps" => rows.sort_by(|a, b| {
            number(&b["market_relative_bps"])
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&number(&a["market_relative_bps"]).unwrap_or(f64::NEG_INFINITY))
                .then_with(|| {
                    a["instrument_key"]
                        .as_str()
                        .cmp(&b["instrument_key"].as_str())
                })
        }),
        "symbol" => rows.sort_by(|a, b| a["symbol"].as_str().cmp(&b["symbol"].as_str())),
        _ => return Err("harness_scanner_sort_invalid".into()),
    }
    let total = rows.len();
    rows.truncate(limit);
    Ok(json!({
        "status":if total==0 {"no_fresh_prospects"} else {"available"},
        "reason":if total==0 {json!("scanner_has_no_fresh_eligible_prospects")} else {Value::Null},
        "scanner_rank_order":true,
        "total":total,
        "presented":rows.len(),
        "market_median_bps":bundle["discovery"]["market_median_bps"],
        "prospects":rows,
        "execution_authorized":false
    }))
}
pub(super) fn rank_universe(bundle: &Value, args: &RankArgs) -> Result<Value> {
    let allowed = bundle["allowlist"]
        .as_array()
        .ok_or("harness_current_universe_unavailable")?;
    let Some(features) = bundle["historical_features"].as_array() else {
        return Ok(
            json!({"status":"unknown","reason":"historical_features_unavailable","rows":[],"execution_authorized":false}),
        );
    };
    let sort_by = args.sort_by.as_deref().unwrap_or("return_20d");
    let order = args.order.as_deref().unwrap_or("desc");
    let limit = args.limit.unwrap_or(ToolsSettings::default().ranking_limit);
    let mut rows = Vec::new();
    let mut unavailable = Vec::new();
    for instrument in allowed {
        let Some(key) = instrument["key"].as_str() else {
            continue;
        };
        let Some(row) = features
            .iter()
            .find(|r| r["instrument"]["key"].as_str() == Some(key))
        else {
            unavailable.push(json!({"instrument_key":key,"symbol":instrument["symbol"],"status":"history_unavailable"}));
            continue;
        };
        if args
            .min_liquidity
            .is_some_and(|min| number(&row["liquidity"]).is_none_or(|l| l < min))
        {
            unavailable.push(json!({"instrument_key":key,"symbol":instrument["symbol"],"status":"below_min_liquidity_or_unknown"}));
            continue;
        }
        rows.push(json!({
            "instrument_key":key,
            "symbol":instrument["symbol"],
            "history_as_of":row["history_as_of"],
            "history_current":row["history_current"],
            "last_close":row["last_close"],
            "return_1d":row["return_1d"],
            "return_5d":row["return_5d"],
            "return_20d":row["return_20d"],
            "volatility_20d":row["volatility_20d"],
            "volume_z_score":row["volume_z_score"],
            "gap":row["gap"],
            "liquidity":row["liquidity"],
        }));
    }
    rows.sort_by(|a, b| {
        let left = number(&a[sort_by]);
        let right = number(&b[sort_by]);
        let ordering = match (left, right) {
            (Some(l), Some(r)) => l.total_cmp(&r),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        let ordering = if order == "desc" {
            ordering.reverse()
        } else {
            ordering
        };
        ordering.then_with(|| {
            a["instrument_key"]
                .as_str()
                .cmp(&b["instrument_key"].as_str())
        })
    });
    let total = rows.len();
    rows.truncate(limit);
    let unavailable_count = unavailable.len();
    let unavailable_reasons = unavailable.iter().fold(BTreeMap::new(), |mut counts, row| {
        if let Some(reason) = row["status"].as_str() {
            *counts.entry(reason.to_owned()).or_insert(0usize) += 1;
        }
        counts
    });
    unavailable.truncate(limit);
    let mut result = json!({
        "status":if total==0 {"no_rankable_history"} else {"available"},
        "session":bundle["session"],
        "sort_by":sort_by,
        "order":order,
        "min_liquidity":args.min_liquidity,
        "total":total,
        "presented":rows.len(),
        "unavailable":unavailable,
        "unavailable_count":unavailable_count,
        "unavailable_reasons":unavailable_reasons,
        "rows":rows,
        "execution_authorized":false
    });
    // The row limit applies to diagnostics too. Keep exact totals and explicit
    // omission flags; a complete-universe missing-history list must not abort
    // the digest or silently masquerade as complete evidence.
    loop {
        result["presented"] = json!(result["rows"].as_array().map_or(0, Vec::len));
        result["rows_truncated"] = json!(result["presented"].as_u64() != Some(total as u64));
        result["unavailable_truncated"] =
            json!(result["unavailable"].as_array().map_or(0, Vec::len) < unavailable_count);
        if serde_json::to_vec(&result)?.len() <= TOOL_OUTPUT_MAX_BYTES {
            return Ok(result);
        }
        if result["unavailable"]
            .as_array_mut()
            .and_then(Vec::pop)
            .is_none()
            && result["rows"].as_array_mut().and_then(Vec::pop).is_none()
        {
            return Err("harness_tool_output_cap".into());
        }
    }
}
fn parse_amount(value: &Value) -> Option<nivra_core::finance::Amount> {
    nivra_core::finance::Amount::parse(value.as_str()?).ok()
}
fn parse_price(value: &Value) -> Option<nivra_core::finance::Price> {
    serde_json::from_value(value.clone()).ok()
}
fn allocate_fee(
    fee: nivra_core::finance::Amount,
    taken: u64,
    total: u64,
) -> Option<nivra_core::finance::Amount> {
    fee.checked_mul(nivra_core::finance::Amount::from_unsigned(taken))
        .ok()?
        .checked_div(nivra_core::finance::Amount::from_unsigned(total))
        .ok()
}
/// Remaining buy lots behind a position. `Err` means the bounded fill window
/// does not contain the opening purchase (carried or evicted), never zero.
fn open_lots(bundle: &Value, key: &str) -> std::result::Result<Vec<Value>, ()> {
    use nivra_core::execution::Fill;
    use std::collections::VecDeque;
    let rows = bundle["fills"].as_array().ok_or(())?;
    let mut lots: VecDeque<(Fill, u64, nivra_core::finance::Amount)> = VecDeque::new();
    for row in rows {
        let fill: Fill = serde_json::from_value(row.clone()).map_err(|_| ())?;
        if fill.instrument.key != key {
            continue;
        }
        if fill.buy {
            lots.push_back((fill.clone(), fill.units, fill.fee));
            continue;
        }
        let mut remaining = fill.units;
        while remaining > 0 {
            let (_lot, units, fee) = lots.front_mut().ok_or(())?;
            let taken = remaining.min(*units);
            let allocated = if taken == *units {
                *fee
            } else {
                allocate_fee(*fee, taken, *units).ok_or(())?
            };
            *fee = fee.checked_sub(allocated).map_err(|_| ())?;
            *units -= taken;
            remaining -= taken;
            if *units == 0 {
                lots.pop_front();
            }
        }
    }
    Ok(lots
        .into_iter()
        .map(|(fill, units, fee)| {
            json!({
                "at_ms":fill.at,
                "units":units,
                "price":fill.price,
                "fee":fee,
                "slippage_bps":fill.slippage_bps,
                "decision_id":fill.decision_id,
            })
        })
        .collect())
}
fn plan_action(bundle: &Value, key: &str, decision_id: Option<&str>) -> Option<Value> {
    let mut plans = Vec::new();
    if let Some(rows) = bundle["decision_history"]["grok"].as_array() {
        plans.extend(rows.iter());
    }
    if bundle["previous_grok_decision"].is_object() {
        plans.push(&bundle["previous_grok_decision"]);
    }
    for plan in plans {
        if decision_id.is_some_and(|id| plan["decision_id"] != id) {
            continue;
        }
        let parsed = plan
            .get("parsed_plan")
            .or_else(|| plan.get("plan"))
            .unwrap_or(plan);
        if let Some(action) = parsed["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|action| action["instrument_key"] == key && action["side"] == "buy")
        {
            return Some(action.clone());
        }
    }
    None
}
fn prior_thesis(bundle: &Value) -> Value {
    let luna = &bundle["previous_luna"];
    luna["digest"]["handoff"]["thesis"]
        .as_object()
        .map(|_| luna["digest"]["handoff"]["thesis"].clone())
        .or_else(|| {
            luna["digest"]["thesis"]
                .as_object()
                .map(|_| luna["digest"]["thesis"].clone())
        })
        .unwrap_or(Value::Null)
}
fn last_review(bundle: &Value, key: &str) -> Value {
    bundle["previous_luna"]["digest"]["positions_review"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|review| review["instrument_key"] == key)
        .cloned()
        .unwrap_or(Value::Null)
}
pub(super) fn portfolio_view(bundle: &Value, now: i64, fill_limit: usize) -> Value {
    let positions = bundle["positions"]
        .as_object()
        .map(|positions| {
            positions
                .iter()
                .map(|(key, position)| {
                    let units = position["units"].as_u64().unwrap_or(0);
                    let lots = open_lots(bundle, key);
                    let (lots, lots_unavailable) = match lots {
                        Ok(lots) if !lots.is_empty() => (lots, false),
                        _ => (Vec::new(), true),
                    };
                    let mut basis = nivra_core::finance::Amount::ZERO;
                    let mut kept = 0_u64;
                    for lot in &lots {
                        let lot_units = lot["units"].as_u64().unwrap_or(0);
                        if let (Some(price), Some(fee)) =
                            (parse_price(&lot["price"]), parse_amount(&lot["fee"]))
                            && let Ok(notional) = price
                                .amount()
                                .checked_mul(nivra_core::finance::Amount::from_unsigned(lot_units))
                            && let Ok(lot_basis) = notional.checked_add(fee)
                        {
                            basis = basis
                                .checked_add(lot_basis)
                                .unwrap_or(nivra_core::finance::Amount::ZERO);
                            kept = kept.saturating_add(lot_units);
                        }
                    }
                    let first_at = lots
                        .first()
                        .and_then(|lot| lot["at_ms"].as_i64())
                        .or_else(|| position["marked_at"].as_i64());
                    let (avg_entry_after_fees, unrealized_pnl_net, unrealized_bps) =
                        if kept == units && kept > 0 {
                            let avg = basis
                                .checked_div(nivra_core::finance::Amount::from_unsigned(kept))
                                .map(|value| value.to_string())
                                .unwrap_or_else(|_| {
                                    position["entry"].as_str().unwrap_or("0").into()
                                });
                            let estimate = (|| {
                                let instrument: nivra_core::execution::Instrument =
                                    serde_json::from_value(position["instrument"].clone()).ok()?;
                                let fees: nivra_core::finance::FeeArithmetic =
                                    serde_json::from_value(bundle["fees"].clone()).ok()?;
                                let mark = parse_price(&position["mark"])?;
                                let price = nivra_core::execution::Portfolio::execution_with_costs(
                                    mark, instrument.tick, false, &fees,
                                )
                                .ok()?;
                                let gross = price
                                    .amount()
                                    .checked_mul(nivra_core::finance::Amount::from_unsigned(units))
                                    .ok()?;
                                let charge = fees
                                    .charge(
                                        nivra_core::finance::Side::Sell,
                                        nivra_core::finance::Money::try_from(gross).ok()?,
                                    )
                                    .ok()?
                                    .amount();
                                let net = gross.checked_sub(charge).ok()?;
                                let pnl = net.checked_sub(basis).ok()?;
                                let bps = pnl
                                    .checked_mul(nivra_core::finance::Amount::from_integer(10000))
                                    .ok()
                                    .and_then(|value| value.checked_div(basis).ok())
                                    .map(|value| value.to_string());
                                Some((pnl.to_string(), bps))
                            })();
                            (
                                avg,
                                estimate
                                    .as_ref()
                                    .map(|v| json!(v.0))
                                    .unwrap_or(Value::Null),
                                estimate
                                    .as_ref()
                                    .map(|v| json!(v.1))
                                    .unwrap_or(Value::Null),
                            )
                        } else {
                            (
                                position["entry"].as_str().unwrap_or("0").to_owned(),
                                Value::Null,
                                Value::Null,
                            )
                        };
                    let action = plan_action(bundle, key, position["decision_id"].as_str());
                    json!({
                        "instrument_key":key,
                        "instrument":position["instrument"],
                        "units":position["units"],
                        "available_to_sell":position["units"],
                        "entry":position["entry"],
                        "avg_entry_after_fees":avg_entry_after_fees,
                        "mark":position["mark"],
                        "marked_at":position["marked_at"],
                        "exit_intent":position["exit_intent"],
                        "decision_id":position["decision_id"],
                        "lots":lots,
                        "lots_unavailable_carried":lots_unavailable,
                        "unrealized_pnl_net":unrealized_pnl_net,
                        "unrealized_bps":unrealized_bps,
                        "held_minutes":first_at.map(|at| (now.saturating_sub(at) / 60_000).max(0)),
                        "held_sessions":bundle["session"].as_str().and_then(|s| s.parse::<NaiveDate>().ok()).and_then(|session| {
                            first_at.map(|at| (session - local(at).date_naive()).num_days().max(0))
                        }),
                        "high_bid_since_entry":Value::Null,
                        "low_bid_since_entry":Value::Null,
                        "entry_rationale":action.as_ref().and_then(|a| a["rationale"].as_str()),
                        "entry_thesis":prior_thesis(bundle),
                        "last_review":last_review(bundle, key),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let session = bundle["session"]
        .as_str()
        .and_then(|s| s.parse::<NaiveDate>().ok());
    let today = bundle["fills"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|fill| {
            fill["at"]
                .as_i64()
                .is_some_and(|at| session == Some(local(at).date_naive()))
        })
        .cloned()
        .collect::<Vec<_>>();
    let fills_today = today
        .iter()
        .rev()
        .take(fill_limit)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    json!({
        "status":if positions.is_empty() {"no_open_positions"} else {"available"},
        "cash":bundle["cash"],
        "session":bundle["session"],
        "positions":positions,
        "book":bundle["book_state"],
        "session_realized_pnl_net":bundle["account_performance"]["session_realized_pnl_net"],
        "fees_paid_session":bundle["account_performance"]["fees_paid_session"],
        "fills_today":fills_today,
        "fills_total":bundle["fills_total"],
        "high_low_status":"unavailable_retained_marks_not_stored",
        "execution_authorized":false
    })
}
pub(super) fn recent_decisions(bundle: &Value, limit: usize) -> Value {
    let history = &bundle["decision_history"];
    let luna = if history["luna"].is_null() {
        bundle["previous_luna"].clone()
    } else {
        history["luna"].clone()
    };
    let mut grok = if let Some(rows) = history["grok"].as_array() {
        rows.clone()
    } else if bundle["previous_grok_decision"].is_null() {
        Vec::new()
    } else {
        vec![bundle["previous_grok_decision"].clone()]
    };
    grok.truncate(limit);
    let advisory = if history["advisory_memory"].is_null() {
        bundle["advisory_memory"].clone()
    } else {
        history["advisory_memory"].clone()
    };
    let available = !luna.is_null() || !grok.is_empty() || !advisory.is_null();
    json!({
        "status":if available {"available_untrusted_prior_observations"} else {"no_prior_decisions"},
        "reason":if available {Value::Null} else {json!("no_prior_decision_history")},
        "luna":luna,
        "grok":grok,
        "advisory_memory":advisory,
        "execution_authorized":false
    })
}
pub(super) fn session_status(c: &Config, bundle: &Value, now: i64) -> Value {
    let day = super::super::local(now).date_naive();
    let minute = super::super::minute(now);
    let book = &bundle["book_state"];
    let closed = book["closed"].as_bool().unwrap_or(false);
    let phase = if day != c.session {
        "different_session"
    } else if closed {
        "closed"
    } else if minute < c.execution.session_start_minute {
        "pre_open"
    } else if minute >= c.execution.session_end_minute {
        "post_close"
    } else if c.is_trading_day(day).unwrap_or(false) {
        "open"
    } else {
        "closed_market"
    };
    json!({
        "status":"available",
        "now_ms":now,
        "session":c.session,
        "market_phase":phase,
        "minutes_to_close":if phase=="open" {Some(c.execution.session_end_minute.saturating_sub(minute))} else {None},
        "book":book,
        "trigger":bundle["trigger"],
        "execution_authorized":false
    })
}
/// Host error codes are static snake_case identifiers; anything else is redacted.
pub(super) fn tool_failure_reason(e: &(dyn std::error::Error + 'static)) -> String {
    let text = e.to_string();
    if !text.is_empty()
        && text.len() <= 96
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        text
    } else {
        "tool_error_redacted".into()
    }
}
pub(super) fn snapshot(
    c: &Config,
    bundle: &Value,
    keys: &[String],
    archive: &Path,
    id: &str,
) -> Result<Value> {
    if bundle["session"] != local(millis()).date_naive().to_string() {
        return Err("harness_current_universe_unavailable".into());
    }
    let allowed: Vec<nivra_core::execution::Instrument> =
        serde_json::from_value(bundle["allowlist"].clone())?;
    if keys.iter().any(|k| !allowed.iter().any(|i| i.key == *k)) {
        return Err("harness_snapshot_not_allowlisted".into());
    }
    let client = data::Client::new(&c.market_data_transport)?;
    let refs: Vec<_> = keys.iter().map(String::as_str).collect();
    let (requested, received, raw) = client.quotes(c, &refs)?;
    let output = snapshot_response(&allowed, keys, requested, received, &raw, millis(), id)?;
    let purpose = "research_observation";
    let envelope = json!({"source":"upstox-readonly-v1","purpose":purpose,
        "requested_ms":requested,"received_ms":received,"body":raw})
    .to_string();
    let batch = super::super::market_archive::normalize::capture_quotes(&envelope)?;
    let mut database = super::super::market_archive::open(c)?;
    nivra_store::market_archive::write_quotes(&mut database, &batch)?;
    rollover::atomic(
        &archive.join(format!("{id}-quote-receipt.json")),
        &json!({"requested_ms":requested,"received_ms":received,"reference":
            serde_json::from_str::<Value>(&nivra_store::market_archive::quote_reference(purpose, &envelope))?}),
    )?;
    Ok(output)
}
pub(super) fn snapshot_response(
    allowed: &[nivra_core::execution::Instrument],
    keys: &[String],
    requested: i64,
    received: i64,
    raw: &str,
    now: i64,
    id: &str,
) -> Result<Value> {
    if requested > received || received > now {
        return Err("harness_quote_clock_invalid".into());
    }
    let v: Value = serde_json::from_str(raw)?;
    if v["status"] != "success" {
        return Err("harness_quote_failed".into());
    }
    let rows = v["data"].as_object().ok_or("harness_quote_failed")?;
    let mut quotes = vec![];
    for key in keys {
        let i = allowed
            .iter()
            .find(|i| i.key == *key)
            .ok_or("harness_snapshot_not_allowlisted")?;
        let matching: Vec<_> = rows
            .values()
            .filter(|r| r["instrument_token"] == *key)
            .collect();
        if matching.len() != 1 {
            return Err("harness_quote_identity".into());
        }
        let q = data::parse_row(matching[0], received, &i.key, &i.symbol, i.tick)?;
        if q.source > received || !q.fresh(now, nivra_core::market::MAX_FINANCIAL_AGE_MS) {
            return Err("harness_quote_stale".into());
        }
        quotes.push(json!({"instrument":i,"quote":q,"volume":matching[0]["volume"],"ohlc":matching[0]["ohlc"]}));
    }
    Ok(
        json!({"evidence_id":id,"requested_ms":requested,"received_ms":received,"quotes":quotes,"execution_authorized":false}),
    )
}
