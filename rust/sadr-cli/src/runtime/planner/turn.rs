//! Digest turn production, bounded reuse, trigger envelopes and durable results.
use super::*;

pub(crate) fn produce(
    c: &Config,
    cfg: &PlannerConfig,
    p: &Portfolio,
    instruments: &[Instrument],
    preview: bool,
) -> Result<Value> {
    produce_for(c, cfg, p, instruments, preview, None, None)
}
#[allow(clippy::too_many_arguments)] // Exact durable claim and cancellation accompany the immutable input snapshot.
pub(crate) fn produce_for(
    c: &Config,
    cfg: &PlannerConfig,
    p: &Portfolio,
    instruments: &[Instrument],
    preview: bool,
    claim: Option<&redecision::Claim>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Value> {
    produce_clocked(
        c,
        cfg,
        p,
        instruments,
        preview,
        claim,
        cancel,
        &millis,
        None,
        None,
    )
}
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn produce_with_backend(
    c: &Config,
    cfg: &PlannerConfig,
    p: &Portfolio,
    instruments: &[Instrument],
    claim: &redecision::Claim,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    backend: &mut dyn Backend,
    clock: &dyn Fn() -> i64,
) -> Result<Value> {
    produce_clocked(
        c,
        cfg,
        p,
        instruments,
        false,
        Some(claim),
        cancel,
        clock,
        Some(backend),
        None,
    )
}
/// Read the original session's charged successful digest, including pre-upgrade turns.
/// Never refund attempts or replace a missing/damaged usage namespace.
#[cfg(test)]
pub(super) fn existing_digest(
    c: &Config,
    role: &ModelRole,
    guard: &CallGuard<'_>,
) -> Result<Option<String>> {
    let journal = usage_directory(c, guard.journal_required.get())?;
    let _lock = usage_lock(c, false)?;
    let records = checked_reservations(c, &journal)?;
    for name in records.keys().filter(|n| n.starts_with("digest-")) {
        let attempt: Value = serde_json::from_slice(
            &crate::read_bounded(&journal.join(name), 4096)
                .map_err(|_| "planner_usage_manifest_invalid")?,
        )?;
        if guard
            .context_hash
            .is_some_and(|hash| attempt["context_sha256"] != hash)
        {
            continue;
        }
        if attempt["model"] != role.model_id || attempt["transport"] != json!(role.transport) {
            continue;
        }
        let result_path = journal.join(name.replace(".attempt.json", ".result.json"));
        if !result_path.exists() {
            continue;
        }
        let result: Value = serde_json::from_slice(
            &crate::read_bounded(&result_path, 1_000_000)
                .map_err(|_| "planner_saved_digest_invalid")?,
        )?;
        if result["failed"] != false {
            continue;
        }
        let Some(raw) = result["output"].as_str() else {
            continue;
        };
        let response: Value = serde_json::from_str(raw)?;
        let text = if let Some(output) = response["output"].as_array() {
            let Ok(text) = super::super::harness::final_text(output) else {
                continue;
            };
            text
        } else {
            raw.to_owned()
        };
        if super::super::harness::validate_digest(&text, &Value::Null).is_ok() {
            return Ok(Some(text));
        }
    }
    Ok(None)
}
/// The only market-facing content a `tools_only` model receives: when, why and
/// which keys woke it, plus the host clock, session and exact objective.
fn trigger_envelope(
    c: &Config,
    cfg: &PlannerConfig,
    input: &Value,
    now: i64,
    target: &str,
) -> Value {
    let trigger = &input["trigger"];
    let kind = trigger["kind"]
        .as_str()
        .or_else(|| trigger["event_kind"].as_str())
        .unwrap_or("initial");
    let reason = trigger["reason"]
        .as_str()
        .or_else(|| trigger["cause"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| redecision::trigger_reason(trigger));
    let event_at_ms = trigger["observed_ms"]
        .as_i64()
        .or_else(|| trigger["event_at_ms"].as_i64())
        .or_else(|| input["as_of_ms"].as_i64())
        .unwrap_or(now);
    let keys = trigger["keys"]
        .as_array()
        .cloned()
        .or_else(|| {
            trigger["instrument_key"]
                .as_str()
                .map(|key| vec![json!(key)])
        })
        .unwrap_or_default();
    let mut envelope = json!({
        "trigger":{"kind":kind,"reason":reason,"event_at_ms":event_at_ms},
        "now_ms":now,
        "session":c.session.to_string(),
        "objective":super::super::harness::objective_for(cfg),
    });
    if !keys.is_empty() {
        envelope["trigger"]["keys"] = json!(keys);
    }
    let max_active = cfg
        .profile_value("directives")
        .and_then(|value| value["max_active"].as_u64())
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(3)
        .min(3);
    envelope["directives"] =
        super::super::directives::envelope(&c.directory, now, target, max_active);
    envelope
}
#[allow(clippy::too_many_arguments)] // Exact durable claim and cancellation accompany the immutable input snapshot.
pub(in crate::runtime) fn produce_clocked(
    c: &Config,
    cfg: &PlannerConfig,
    p: &Portfolio,
    instruments: &[Instrument],
    preview: bool,
    claim: Option<&redecision::Claim>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    clock: &dyn Fn() -> i64,
    injected: Option<&mut dyn Backend>,
    market_context: Option<&super::super::decision_evidence::Snapshot>,
) -> Result<Value> {
    let mut guard = CallGuard {
        cancel,
        deadline: claim.map(|c| c.deadline_ms),
        not_before: claim.map(|c| c.started_ms),
        configuration: if claim.is_some() {
            Some((c, cfg))
        } else {
            None
        },
        journal_required: std::cell::Cell::new(claim.is_some()),
        // Event attempts are not provider calls. Digest reuse and failures before
        // inference make their counts diverge; the pinned manifest owns charges.
        minimum_prior_calls: 0,
        invocation_id: claim.map(|c| c.id.as_str()),
        context_hash: None,
        clock,
        duration: None,
        started_wall_ms: None,
    };
    guard.check()?;
    #[cfg(not(test))]
    cfg.runtime_decision_owner_gate()?;
    let orchestrator_owns_decisions = cfg.orchestrator_owns_decisions();
    let digest_role = cfg.role(Trigger::Digest);
    let decide_role = cfg.role(Trigger::Decide);
    if orchestrator_owns_decisions && !digest_role.enabled {
        return Err("decision_owner_digest_required".into());
    }
    let mode = cfg.context.as_ref().map(|c| c.mode).unwrap_or_default();
    let (mut input, references) = bundle_with_references(c, p, instruments, cfg.shortlist_n)?;
    if let Some(settings) = &cfg.discovery {
        input["discovery"] = super::super::discovery::snapshot(c, instruments, settings, clock())?;
        if !orchestrator_owns_decisions
            && let Ok(bytes) =
                crate::read_bounded(&c.directory.join("model/luna-latest.json"), 131072)
            && let Ok(prior) = serde_json::from_slice::<Value>(&bytes)
            && prior["session_binding"] == book_binding(c)?
            && prior["at_ms"].as_i64().is_some_and(|at| at <= clock())
        {
            input["previous_luna"] = prior;
        }
        if !orchestrator_owns_decisions {
            let db = Connection::open_with_flags(
                c.directory.join("paper.sqlite"),
                OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            use rusqlite::OptionalExtension;
            if let Some(body) = db
                .query_row(
                    "SELECT body FROM planner_plan ORDER BY rowid DESC LIMIT 1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
            {
                let plan: Value = serde_json::from_str(&body)?;
                input["previous_grok_decision"] =
                    json!({"decision_id":plan["decision_id"],"plan":plan["parsed_plan"]});
            }
        }
    }
    input["paper_mandate"] = json!({"paper_only":true,"capital":c.allocation()?,"objective":super::super::harness::objective_for(cfg),"risk_caps":"none","overnight_holds_allowed":true,"operator_context":cfg.operator_context,"initialization":"validated_universe_then_history_then_digest_then_orchestrator_then_fresh_quote_execution"});
    input["paper_mandate"]["products"] = if c.nifty_options.enabled {
        json!(["cash_equity", "nifty_option_long"])
    } else {
        json!(["cash_equity"])
    };
    // Additive bounded option shortlist plus spot; never the cash feature store.
    if let Some(nifty_options) = super::super::options::snapshot(c, p, clock())? {
        input["nifty_options"] = nifty_options;
    }
    input["book_state"] = json!({"halted":p.halted,"stopped":p.stopped,"closed":p.closed,"health":p.health,"model_led":p.model_led});
    // Call-time portfolio context: bounded original fills and exact session
    // performance. The financial ledger remains authoritative; this is display
    // and review evidence only.
    let fill_limit = cfg.effective_tools().fill_limit;
    let start = p.fills.len().saturating_sub(fill_limit);
    input["fills"] = json!(&p.fills[start..]);
    input["fills_total"] = json!(p.fills.len());
    let realized = p.realised_net_pnl();
    let mut session_realized = Some(Amount::ZERO);
    let mut session_fees = Amount::ZERO;
    for (fill, pnl) in p.fills.iter().zip(realized.iter()) {
        if local(fill.at).date_naive() == c.session {
            session_fees = session_fees.checked_add(fill.fee)?;
            session_realized = match (session_realized, pnl) {
                (Some(total), Some(value)) => Some(total.checked_add(*value)?),
                _ => None,
            };
        }
    }
    input["account_performance"] = json!({
        "session_realized_pnl_net":session_realized.map(|value| value.to_string()),
        "fees_paid_session":session_fees.to_string(),
    });
    if cfg.discovery.is_some() && !orchestrator_owns_decisions {
        let mut grok = Vec::new();
        if let Ok(db) = Connection::open_with_flags(
            c.directory.join("paper.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) && let Ok(mut stmt) =
            db.prepare("SELECT body FROM planner_plan ORDER BY rowid DESC LIMIT ?1")
            && let Ok(rows) = stmt.query_map([cfg.effective_tools().history_limit as i64], |r| {
                r.get::<_, String>(0)
            })
        {
            for body in rows.flatten() {
                if let Ok(plan) = serde_json::from_str::<Value>(&body) {
                    grok.push(json!({"decision_id":plan["decision_id"],"session":plan["decision_id"].as_str().and_then(|id|id.split('-').next()),"input_hash":plan["input_hash"],"parsed_plan":plan["parsed_plan"],"luna_handoff_requested":plan["luna_handoff_requested"]}));
                }
            }
        }
        input["decision_history"] = json!({
            "luna":input["previous_luna"],
            "grok":grok,
            "advisory_memory":input["advisory_memory"],
        });
    }
    let digest_context = digest(&serde_json::to_vec(
        &json!({"features":input["historical_features"],"mandate":input["paper_mandate"],"cash":input["cash"],"positions":input["positions"]}),
    )?);
    if cfg.full_universe_features == Some(true) {
        guard.context_hash = Some(&digest_context);
        input["presented_feature_count"] = json!(
            input["historical_features"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0)
        );
        input["feature_coverage_partial"] = json!(
            input["historical_features"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0)
                < instruments.len()
        );
        input["shortlist_is_navigation_only"] = json!(true);
    }
    let reference_hash = digest(&serde_json::to_vec(&references)?);
    persist_session_references(c, &references)?;
    if cfg.post_close.as_ref().is_some_and(|s| s.enabled) {
        input["advisory_memory"] = postclose::memory(
            c,
            claim.map(|c| c.started_ms).unwrap_or_else(millis),
            cfg.post_close.as_ref().unwrap().memory_limit,
        )?;
    }
    if cfg.discovery.is_some() {
        input["decision_history"]["advisory_memory"] = input["advisory_memory"].clone();
    }
    if orchestrator_owns_decisions && let Some(object) = input.as_object_mut() {
        for key in [
            "previous_luna",
            "previous_grok_decision",
            "decision_history",
            "advisory_memory",
        ] {
            object.remove(key);
        }
    }
    if let Some(claim) = claim {
        if claim.binding != book_binding(c)? {
            return Err("planner_attempt_binding_invalid".into());
        }
        input["trigger"] = claim.trigger.clone();
        input["attempt_id"] = json!(claim.id);
        input["as_of_ms"] = json!(claim.started_ms);
        input["predecessor"] = json!(claim.predecessor);
    }
    let hash = digest(&serde_json::to_vec(&input)?);
    if !cfg.enabled || cfg.model_led == Some(false) {
        return Err("model_plan_unavailable_no_trades".into());
    }
    if !orchestrator_owns_decisions && !decide_role.enabled {
        return Err("model_plan_unavailable_no_trades".into());
    }
    // Nothing to buy and nothing to manage is a normal no-op, not a failed
    // attempt or a block (#221).
    if !has_decision_work(&input, p, instruments) {
        return Err(NO_DECISION_WORK.into());
    }
    let decision_id = format!(
        "{}-{}{}",
        c.session,
        claim.map(|c| c.id.as_str()).unwrap_or(&hash[..16]),
        if preview {
            format!("-preview-{}", millis())
        } else {
            String::new()
        }
    );
    let audit = c.directory.join("model").join(&decision_id);
    fs::create_dir_all(c.directory.join("model"))?;
    fs::create_dir(&audit)?;
    rollover::atomic(
        &audit.join("input.json"),
        &json!({"input":input,"input_hash":hash,"prompt_version":PROMPT_VERSION,"provider":cfg.provider,"digest":digest_role,"decide":decide_role}),
    )?;
    let start = std::time::Instant::now();
    #[cfg(test)]
    let handoff_requested = std::cell::Cell::new(true);
    #[cfg(not(test))]
    let handoff_requested = std::cell::Cell::new(false);
    let digest_attribution = std::cell::RefCell::new(Value::Null);
    let strategy_attribution = super::super::discovery::attribution(&input["discovery"]);
    let ask = |backend: &mut dyn Backend| -> Result<String> {
        let mut projection = input.clone();
        // These are call-time tool facts, not part of the v2 rollback message.
        projection.as_object_mut().unwrap().remove("book_state");
        projection
            .as_object_mut()
            .unwrap()
            .remove("decision_history");
        if cfg.full_universe_features == Some(true) {
            projection["feature_columns"] = json!([
                "instrument_key",
                "symbol",
                "history_as_of",
                "current",
                "last_close",
                "return_1d",
                "return_5d",
                "return_20d",
                "volatility_20d",
                "volume_z_score",
                "gap",
                "liquidity"
            ]);
            projection["universe_features"] = json!(
                input["historical_features"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| json!([
                        r["instrument"]["key"],
                        r["instrument"]["symbol"],
                        r["history_as_of"],
                        r["history_current"],
                        r["last_close"],
                        r["return_1d"],
                        r["return_5d"],
                        r["return_20d"],
                        r["volatility_20d"],
                        r["volume_z_score"],
                        r["gap"],
                        r["liquidity"]
                    ]))
                    .collect::<Vec<_>>()
            );
        }
        if let Some(rows) = input["discovery"]["prospects"].as_array() {
            projection["discovery"]["prospect_columns"] = json!([
                "instrument_key",
                "symbol",
                "observed_ms",
                "sampled_price_ms",
                "latest_screening_price",
                "price_kind",
                "sample_count",
                "sample_change_bps",
                "screening_age_ms",
                "bid",
                "ask",
                "bid_units",
                "ask_units",
                "quote_age_ms"
            ]);
            projection["discovery"]["prospects"] = json!(
                rows.iter()
                    .map(|r| json!([
                        r["instrument_key"],
                        r["symbol"],
                        r["observed_ms"],
                        r["sampled_price_ms"],
                        r["latest_screening_price"],
                        r["price_kind"],
                        r["sample_count"],
                        r["sample_change_bps"],
                        r["screening_age_ms"],
                        r["bid"],
                        r["ask"],
                        r["bid_units"],
                        r["ask_units"],
                        r["quote_age_ms"]
                    ]))
                    .collect::<Vec<_>>()
            );
        }
        projection
            .as_object_mut()
            .unwrap()
            .remove("historical_features");
        if let Some(settings) = &cfg.context {
            compact_projection(&mut projection, settings);
        }
        let mut digest_tools = Vec::new();
        let data = if digest_role.enabled {
            if orchestrator_owns_decisions && !super::super::harness::owner_feed_usable(&input) {
                // An empty or unusable feed must not spend a digest call.
                super::super::harness::minimal_owner_digest()
            } else if let Some(raw) = if !orchestrator_owns_decisions && cfg.discovery.is_none() {
                {
                    #[cfg(test)]
                    {
                        existing_digest(c, &digest_role, &guard)?
                    }
                    #[cfg(not(test))]
                    {
                        None::<String>
                    }
                }
            } else {
                None
            } {
                raw
            } else {
                let prompt = match mode {
                    ContextMode::ToolsOnly => {
                        trigger_envelope(c, cfg, &input, clock(), "luna").to_string()
                    }
                    ContextMode::Projection => format!(
                        "Summarise this untrusted data, retain identities, fees, dates and missing data. No trade instructions. {projection}"
                    ),
                };
                super::super::harness::run_collect(
                    c,
                    cfg,
                    "digest",
                    &digest_role,
                    &prompt,
                    &input,
                    backend,
                    &guard,
                    &mut digest_tools,
                    mode == ContextMode::Projection,
                )?
            }
        } else {
            json!({"shortlist":input["shortlist"],"cash":input["cash"],"positions":input["positions"],"fees":input["fees"],"missing_history":input["history_missing"]}).to_string()
        };
        let digest_id = digest(data.as_bytes());
        if orchestrator_owns_decisions {
            let parsed_digest: Value =
                serde_json::from_str(&data).map_err(|_| "planner_owner_digest_invalid")?;
            handoff_requested.set(false);
            let eligible = super::super::harness::owner_digest_eligible(&parsed_digest, &input);
            let facts_sha256 = super::super::harness::owner_facts_sha256(&json!({
                "digest": parsed_digest,
                "positions": input["positions"],
                "discovery": input["discovery"],
                "cash": input["cash"],
                "fees": input["fees"],
            }))?;
            let owner_digest = json!({
                "version": 1,
                "digest": parsed_digest,
                "tool_results": digest_tools,
                "eligible": eligible,
                "facts_sha256": facts_sha256,
            });
            *digest_attribution.borrow_mut() = json!({
                "digest_id": digest_id,
                "strategy": strategy_attribution,
                "owner_digest": owner_digest,
            });
            // This is a host-created evidence-completion wrapper. The
            // persistent orchestrator, not the legacy decide role, consumes
            // the digest and owns any later paper decision.
            return Ok(json!({
                "actions": [],
                "stance": "wait",
                "notes": "orchestrator_digest_ready",
            })
            .to_string());
        }
        Err("decision_owner_migration_required".into())
    };
    let (raw_result, provider_evidence) = if let Some(backend) = injected {
        let result = ask(backend);
        let evidence = backend.provenance();
        (result, evidence)
    } else {
        let path = cfg
            .model_config
            .as_deref()
            .or(digest_role.model_config.as_deref())
            .ok_or("planner_model_config_required")?;
        let mut backend = Subscription::new(path)
            .with_alerts(c.alert_webhook_file.as_deref())
            .with_context(market_context);
        let result = ask(&mut backend);
        let evidence = backend.provenance();
        (result, evidence)
    };
    let raw = match raw_result {
        Ok(raw) => raw,
        Err(e) => {
            rollover::atomic(
                &audit.join("failure.json"),
                &failure_record_with_provider(
                    reason(e.as_ref()),
                    e.as_ref(),
                    start.elapsed().as_millis(),
                    &provider_evidence,
                ),
            )?;
            return Err(e);
        }
    };
    if let Err(e) = guard.check_finished() {
        rollover::atomic(
            &audit.join("failure.json"),
            &failure_record_with_provider(
                reason(e.as_ref()),
                e.as_ref(),
                start.elapsed().as_millis(),
                &provider_evidence,
            ),
        )?;
        return Err(e);
    }
    rollover::atomic(
        &audit.join("output.json"),
        &json!({"raw_output":raw,"latency_ms":start.elapsed().as_millis()}),
    )?;
    let verdicts = match validate(&raw, p, instruments) {
        Ok(v) => v,
        Err(e) => {
            rollover::atomic(
                &audit.join("failure.json"),
                &failure_record_with_provider(
                    "schema_failed_no_entries",
                    e.as_ref(),
                    start.elapsed().as_millis(),
                    &provider_evidence,
                ),
            )?;
            return Err(e);
        }
    };
    let parsed = plan_document(&raw)?;
    // Missing acknowledgements are a warning, never a plan rejection.
    for id in super::super::directives::unacknowledged_for(&c.directory, &parsed, clock(), "decide")
    {
        eprintln!("directive_unacknowledged id={id}");
    }
    // A host no-trade plan can be recorded before any model reservation exists.
    let usage_dir = usage_path(c)?;
    let budget_used = if !usage_dir.is_dir() {
        0
    } else {
        fs::read_dir(&usage_dir)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".attempt.json")
            })
            .count()
    };
    // Bind the financial and policy revision the intent was produced against.
    // Execution revalidates these before any action; older plans without them
    // keep the previous behavior.
    let digest_attribution = digest_attribution.into_inner();
    // A policy/profile hash is only recorded when the same policy can be
    // reloaded at execution time; in-memory fixture policies keep the legacy
    // unbound representation.
    let configurable = !preview && c.planner_config.is_some();
    let policy_sha256 = if configurable {
        Some(digest(&policy_bytes(cfg)?))
    } else {
        None
    };
    let portfolio_revision = redecision::portfolio_hash(p)?;
    let profile_sha256 = if configurable {
        cfg.profile_sha256().map(str::to_owned)
    } else {
        None
    };
    let budgets = if orchestrator_owns_decisions {
        json!({
            "digest": digest_role.daily_call_budget,
            "supervisor": cfg.role(Trigger::Supervisor).daily_call_budget,
        })
    } else {
        json!({
            "digest": digest_role.daily_call_budget,
            "decide": decide_role.daily_call_budget,
        })
    };
    let owner_digest =
        orchestrator_owns_decisions.then(|| digest_attribution["owner_digest"].clone());
    let mut result = json!({"strategy_attribution":strategy_attribution,"selection_provenance":super::super::decision_owner::selected_version(&json!({"strategy_attribution":strategy_attribution})),"digest_attribution":digest_attribution,"digest_id":digest_attribution["digest_id"].clone(),"portfolio_revision":portfolio_revision,"policy_sha256":policy_sha256,"profile_sha256":profile_sha256,"decision_id":decision_id,"attempt_id":claim.map(|c|&c.id),"valid_until_ms":claim.map(|c|c.deadline_ms),"session_binding":book_binding(c)?,"input_hash":hash,"input":input,"market_references":references,"market_references_hash":reference_hash,"prompt_version":PROMPT_VERSION,"provider":cfg.provider,"model_id":if handoff_requested.get() {&decide_role.model_id} else {&digest_role.model_id},"luna_handoff_requested":if orchestrator_owns_decisions {Some(false)}else if cfg.discovery.is_some() {Some(handoff_requested.get())}else{None},"raw_output":raw,"parsed_plan":parsed,"verdicts":verdicts,"latency_ms":start.elapsed().as_millis(),"fake":false,"dry_run":preview,"budgets":budgets,"budget_used":budget_used});
    if let Some(owner_digest) = owner_digest {
        result["owner_digest"] = owner_digest;
    }
    #[cfg(test)]
    if handoff_requested.get() {
        super::super::harness::publish_validated(c, Trigger::Decide.as_str(), &decide_role, &raw);
    }
    rollover::atomic(&audit.join("plan.json"), &result)?;
    Ok(result)
}

/// A decision has work only when discovery produced a prospect or the book holds
/// a position this planner manages.
fn has_decision_work(input: &Value, p: &Portfolio, instruments: &[Instrument]) -> bool {
    if p.positions
        .values()
        .any(|position| instruments.contains(&position.instrument))
    {
        return true;
    }
    if input.get("discovery").is_some() {
        input["discovery"]["prospects"]
            .as_array()
            .is_some_and(|prospects| !prospects.is_empty())
    } else {
        input["shortlist"]
            .as_array()
            .is_some_and(|shortlist| !shortlist.is_empty())
    }
}
