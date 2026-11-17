use super::*;

fn export(
    source: &Path,
    output: &Path,
    capital: Amount,
    history: Option<&Path>,
    max_gap_ms: i64,
) -> Result<Value> {
    if capital <= Amount::ZERO
        || max_gap_ms <= 0
        || !source.is_absolute()
        || output.starts_with(source)
    {
        return Err("paper_replay_export_arguments".into());
    }
    let universe: Value = serde_json::from_slice(
        &crate::read_bounded(&source.join("wide-universe.json"), 8_000_000)
            .map_err(|_| "paper_replay_source_universe_unreadable")?,
    )?;
    let session: NaiveDate = serde_json::from_value(universe["session"].clone())?;
    let instruments: Vec<Instrument> = serde_json::from_value(universe["instruments"].clone())?;
    let mut m = Manifest {
        paper_costs: None,
        schema: 1,
        session,
        previous_session: session.pred_opt().ok_or("paper_replay_date_range")?,
        capital,
        universe: instruments,
        events_sha256: String::new(),
        synthetic: false,
        max_observation_gap_ms: max_gap_ms,
        history_directory: history.map(Path::to_path_buf),
        provenance: json!({"source":"original_paper_quote_journal_read_only","historical_membership_certified":false}),
    };
    let c = config(&m, output)?;
    while !c.is_trading_day(m.previous_session)? {
        m.previous_session = m
            .previous_session
            .pred_opt()
            .ok_or("paper_replay_date_range")?;
    }
    let source_db = Connection::open_with_flags(
        source.join("paper.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    // Preserve the source book's explicit policy; missing legacy policy remains unknown.
    let body: String =
        source_db.query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))?;
    let source_book: Portfolio = serde_json::from_str(&body)?;
    source_book.reconcile()?;
    if let Some(costs) = &source_book.costs {
        costs.validate()?;
    }
    m.paper_costs = source_book.costs;
    create_output(output)?;
    let mut events = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output.join("events.jsonl"))?;
    let mut hasher = Sha256::new();
    let mut source_hash = Sha256::new();
    let mut count = 0;
    let mut rejected = 0;
    let mut unknown_purpose = 0;
    let mut stmt = source_db
        .prepare("SELECT at,receipt,body FROM ticks WHERE event='wide_quotes' ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (at, receipt, raw) = row?;
        source_hash.update(at.to_be_bytes());
        source_hash.update(raw.as_bytes());
        let receipt = receipt.ok_or("paper_replay_source_receipt_missing")?;
        let archive: Value = serde_json::from_str(&raw)?;
        if archive["kind"] == "compacted_quote_receipt" {
            return Err("paper_replay_compacted_quote_body".into());
        }
        if archive["received_ms"] != receipt || receipt > at {
            return Err("paper_replay_source_receipt_binding".into());
        }
        let body: Value = serde_json::from_str(
            archive["body"]
                .as_str()
                .ok_or("paper_replay_source_not_quote_receipt")?,
        )?;
        if body["status"] != "success" {
            return Err("paper_replay_source_quote_status".into());
        }
        let rows = body["data"]
            .as_object()
            .ok_or("paper_replay_source_quote_rows")?;
        let mut observations = vec![];
        for row in rows.values() {
            let Some(i) = m.universe.iter().find(|i| row["instrument_token"] == i.key) else {
                rejected += 1;
                continue;
            };
            let quote = match data::parse_row(row, receipt, &i.key, &i.symbol, i.tick) {
                Ok(q) => q,
                Err(_) => {
                    rejected += 1;
                    continue;
                }
            };
            let fields = match (
                row["lower_circuit_limit"].as_number(),
                row["upper_circuit_limit"].as_number(),
                row["volume"].as_u64(),
            ) {
                (Some(lower), Some(upper), Some(volume)) => {
                    (lower.to_string(), upper.to_string(), volume)
                }
                _ => {
                    rejected += 1;
                    continue;
                }
            };
            observations.push(json!({"key":i.key,"quote":quote,"lower_circuit_limit":fields.0,"upper_circuit_limit":fields.1,"volume":fields.2}));
        }
        let kind = match archive["purpose"].as_str() {
            Some("scanner") => "screen",
            Some("execution_or_protection") => "protection",
            Some("research_observation") => "virtual_observation",
            _ => {
                unknown_purpose += 1;
                "screen"
            }
        };
        let row = format!(
            "{}\n",
            json!({"at":at,"available_at":receipt,"kind":kind,"observations":observations})
        );
        hasher.update(row.as_bytes());
        events.write_all(row.as_bytes())?;
        count += 1;
    }
    events.sync_all()?;
    m.events_sha256 = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    m.provenance["original_quote_rows_sha256"] = json!(
        source_hash
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    m.provenance["rejected_or_missing_market_rows"] = json!(rejected);
    m.provenance["legacy_quote_purpose_unknown"] = json!(unknown_purpose);
    if count == 0 {
        return Err("paper_replay_source_quotes_empty".into());
    }
    rollover::atomic(&output.join("manifest.json"), &serde_json::to_value(&m)?)?;
    Ok(
        json!({"paper_only":true,"traded":false,"exported_quote_events":count,"legacy_quote_purpose_unknown":unknown_purpose,"rejected_market_rows":rejected,"paper_costs_known":m.paper_costs.is_some(),"source_unchanged":true}),
    )
}

pub fn export_entry(
    source: &Path,
    output: &Path,
    capital: &str,
    history: Option<&Path>,
    max_gap_ms: i64,
) -> ExitCode {
    match Amount::parse(capital)
        .map_err(|_| "paper_replay_capital_invalid".into())
        .and_then(|capital| export(source, output, capital, history, max_gap_ms))
    {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "paper_replay_export_refused: {}",
                failure_code(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}
pub(super) fn demo(output: &Path) -> Result<Value> {
    create_output(output)?;
    let session = NaiveDate::from_ymd_opt(2026, 10, 16).expect("fixture");
    let open = DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")?.timestamp_millis();
    let mut events = String::new();
    let mut replies = vec![];
    let key = "SYNTHETIC|PAPER_REPLAY";
    for n in 0..=375 {
        let at = open + n * 60_000;
        let bid = 10000 + n * 5;
        events.push_str(&format!("{}\n",json!({"at":at,"available_at":at,"kind":"screen","observations":[{"key":key,"quote":{"bid":format!("{}.{:02}",bid/100,bid%100),"ask":format!("{}.{:02}",(bid+5)/100,(bid+5)%100),"source":at,"receipt":at,"bid_units":10000,"ask_units":10000},"lower_circuit_limit":"50","upper_circuit_limit":"200","volume":1000000+n*1000}]})));
        if n == 375 {
            continue;
        }
        replies.push(Reply {
            role: "digest".into(),
            latency_ms: 0,
            output: json!({
                "complete":true,
                "as_of_ms":at,
                "coverage":{"universe":1,"scanned":1,"quoted":1,"featured":0,"experiments":0},
                "gaps":["No historical features in the synthetic full-day fixture"],
                "changed_prospects":[],
                "active_rankings":[],
                "candidate_rankings":[],
                "bar_context":[],
                "spread_depth":[],
                "holdings":[],
                "cash":{"available":"0"},
                "fees":{"per_order":"0","buy_bps":"0","sell_bps":"0"},
                "recent_actions":[],
                "experiments":[],
                "evidence_ids":["bundle"]
            }),
        });
    }
    let m = Manifest {
        paper_costs: Some(serde_json::from_str(include_str!(
            "../../../../../fixtures/paper-costs.json"
        ))?),
        schema: 1,
        session,
        previous_session: NaiveDate::from_ymd_opt(2026, 10, 15).expect("fixture"),
        capital: Amount::from_integer(5000),
        universe: vec![Instrument {
            key: key.into(),
            symbol: "PAPER_REPLAY".into(),
            series: "EQ".into(),
            tick: sadr_core::finance::Price::parse("0.05")?,
        }],
        events_sha256: digest(events.as_bytes()),
        synthetic: true,
        max_observation_gap_ms: 60000,
        history_directory: None,
        provenance: json!({"source":"explicit_synthetic_fixture","real_market_evidence":false}),
    };
    fs::write(output.join("events.jsonl"), events)?;
    rollover::atomic(&output.join("manifest.json"), &serde_json::to_value(m)?)?;
    rollover::atomic(
        &output.join("responses.json"),
        &serde_json::to_value(replies)?,
    )?;
    // Keep this file outside `prompts/` so PlannerConfig::load does not treat
    // a partial sibling prompt directory as an incomplete installation.
    let supervisor_prompt = output.join("owner-supervisor.md");
    fs::write(&supervisor_prompt, "paper replay owner fixture")?;
    let cfg = json!({
        "enabled":true,
        "model_led":true,
        "require_digest":true,
        "full_universe_features":true,
        "discovery":{"max_scan_age_ms":180000,"max_prospects":10000},
        "digest":{"enabled":true,"model_id":"gpt-6-luna","transport":"openai_codex","timeout_ms":120000,"output_cap":8192,"daily_call_budget":5000,"budget_unit":"event","max_turns":64,"max_read_tools":256},
        "orchestrator":{
            "enabled":true,
            "model":{"model_id":"grok-4.7","transport":"direct_oauth","timeout_ms":120000,"output_cap":8192,"daily_call_budget":200},
            "triggers":{
                "supervisor":{"enabled":true,"prompt":supervisor_prompt}
            }
        },
        "events":{"enabled":true,"decision_timeout_ms":600000,"decision_start_minute":555,"cooldown_ms":0,"max_pending":40,"news_file":null,"policy":{"price_move_bps":100,"max_event_age_ms":300000,"max_news_age_ms":3600000,"max_instruments":10000,"max_identities":50000}}
    });
    rollover::atomic(&output.join("planner.json"), &cfg)?;
    Ok(
        json!({"paper_only":true,"synthetic":true,"market_window":"09:15–15:30 IST","events":376,"luna_responses":375,"grok_responses":0,"run_command":"paper-replay --manifest manifest.json --events events.jsonl --planner planner.json --responses responses.json --output NEW_DIRECTORY"}),
    )
}
pub fn demo_entry(output: &Path) -> ExitCode {
    match demo(output) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "paper_replay_demo_refused: {}",
                failure_code(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}
