use super::*;

fn registration(root: &Path, runtime: &Path) -> Registration {
    let code = root.join("analysis.py");
    let input = root.join("input.json");
    std::fs::write(&code, b"print('fixture')\n").unwrap();
    std::fs::write(&input, b"{\"fixture\":true}\n").unwrap();
    Registration {
        analysis_id: "shared_core_backtest".into(),
        dataset_id: "synthetic_no_quotes_fixture".into(),
        code_file: code,
        runtime_file: runtime.to_path_buf(),
        input_file: input,
        code_sha256: digest(b"print('fixture')\n"),
        runtime_sha256: digest(b"nivra-analysis-fixture"),
        input_sha256: digest(b"{\"fixture\":true}\n"),
        memory_bytes: 64 * 1024 * 1024,
        wall_millis: 5_000,
        limits: ResourceLimits {
            cpu_millis: 1_000,
            tasks: 2,
            workspace_bytes: 1024 * 1024,
            file_bytes: 65536,
        },
    }
}

fn settings(root: &Path, runtime: &Path) -> ResearchSettings {
    ResearchSettings {
        queue: root.join("research.sqlite"),
        stage_root: root.join("stage"),
        task_ttl_ms: default_task_ttl_ms(),
        termination_grace_ms: default_termination_grace_ms(),
        admission_limits: None,
        library_max_candidates: None,
        library_max_bytes: None,
        candidate_compute: None,
        quote_capture: None,
        registrations: vec![registration(root, runtime)],
    }
}

fn owner_fixture(root: &Path, source: &ResearchSettings) -> (Config, PlannerConfig) {
    let c = super::super::super::tests::config(root);
    let mut research = source.clone();
    research.queue = root.join("research.sqlite");
    research.stage_root = root.join("research-stage");
    let cfg = super::super::super::planner::PlannerConfig {
        research_tools: Some(research),
        ..Default::default()
    };
    (c, cfg)
}

fn candidate_at_on(day: &str, hour: u32, minute: u32) -> i64 {
    chrono::DateTime::parse_from_rfc3339(&format!("{day}T{hour:02}:{minute:02}:00+05:30"))
        .unwrap()
        .timestamp_millis()
}

fn candidate_at(hour: u32, minute: u32) -> i64 {
    candidate_at_on("2026-10-16", hour, minute)
}

/// A real synthetic verified universe: the three raw archives are written and
/// their exact hashes/mapping are recorded in the complete frozen manifest, so
/// the production `wide::universe::cached` reader accepts it. This is a
/// synthetic fixture, never a genuine market-instrument claim.
fn write_verified_universe(c: &Config, session: &str, file_date: &str) {
    use std::io::Write;
    let zip = |text: &str| {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap()
    };
    let security = zip(
        "TckrSymb,SctySrs,ISIN,NewBrdLotQty,ElgbltyNrmlMkt\nAAA,EQ,INE000000001,1,1\nBBB,EQ,INF000000002,1,1\n",
    );
    let broker = zip(
        &json!([
            {"segment":"NSE_EQ","exchange":"NSE","trading_symbol":"AAA","instrument_type":"EQ","isin":"INE000000001","instrument_key":"NSE_EQ|INE000000001","lot_size":1,"tick_size":5,"security_type":"NORMAL"},
            {"segment":"NSE_EQ","exchange":"NSE","trading_symbol":"BBB","instrument_type":"EQ","isin":"INF000000002","instrument_key":"NSE_EQ|INF000000002","lot_size":1,"tick_size":5,"security_type":"NORMAL"}
        ])
        .to_string(),
    );
    let suspended = zip("[]");
    let day: chrono::NaiveDate = file_date.parse().unwrap();
    fs::write(
        c.directory
            .join(format!("NSE_CM_security_{}.csv.gz", day.format("%d%m%Y"))),
        &security,
    )
    .unwrap();
    fs::write(c.directory.join("upstox-NSE.json.gz"), &broker).unwrap();
    fs::write(c.directory.join("upstox-suspended.json.gz"), &suspended).unwrap();
    let manifest = json!({
        "instruments":[
            {"key":"NSE_EQ|INE000000001","symbol":"AAA","series":"EQ","tick":"0.05"},
            {"key":"NSE_EQ|INF000000002","symbol":"BBB","series":"EQ","tick":"0.05"}
        ],
        "exclusions":{},
        "file_date":file_date,
        "session":session,
        "security_sha256":digest(&security),
        "broker_sha256":digest(&broker),
        "suspended_sha256":digest(&suspended),
        "received_ms":0
    });
    fs::write(
        c.directory.join("wide-universe.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

fn candidate_settings(root: &Path, base: &ResearchSettings) -> ResearchSettings {
    let mut settings = base.clone();
    settings.queue = root.join("candidate-research.sqlite");
    settings.stage_root = root.join("candidate-stage");
    settings.admission_limits = Some(jobs::AdmissionLimits {
        max_trials_per_input: 40,
        max_session_trials: 100,
    });
    settings.candidate_compute = Some(CandidateComputeSettings {
        enabled: true,
        sandbox: nivra_sandbox::SandboxConfig {
            wall_ms: 2_000,
            ..Default::default()
        },
        max_source_bytes: 262_144,
        max_params_bytes: 65_536,
        wall_millis: 120_000,
        unit_tasks: 16,
        unit_memory_bytes: 512 * 1024 * 1024,
        unit_cpu_millis: 0,
        unit_wall_millis: 0,
        datasets: vec![CandidateDataset {
            dataset_id: "current_session_features_v1".into(),
            kind: CandidateDatasetKind::CurrentSessionFeatures,
        }],
        strategy: None,
    });
    settings
}

fn candidate_config(
    root: &Path,
    base: &PlannerConfig,
    settings: ResearchSettings,
) -> (Config, PlannerConfig) {
    let c = super::super::super::tests::config(root);
    let mut cfg = base.clone();
    cfg.research_tools = Some(settings);
    (c, cfg)
}

fn candidate_submit_args(task_key: &str, source: &str) -> BacktestArgs {
    BacktestArgs {
        task_key: task_key.into(),
        hypothesis: "AAA should trade after costs on the frozen session tape".into(),
        source: source.into(),
        params: json!({}),
        dataset_id: "current_session_features_v1".into(),
        training_from_ms: candidate_at(9, 20),
        training_to_ms: candidate_at(9, 20),
        selection_from_ms: candidate_at(9, 21),
        selection_to_ms: candidate_at(9, 22),
        archive_daily_window: None,
        as_of_receipt_id: None,
        archive_bars: None,
    }
}

fn candidate_sandbox_ready(settings: &ResearchSettings) -> bool {
    let Some(candidate) = settings.candidate_compute.as_ref() else {
        return false;
    };
    let sandbox = &candidate.sandbox;
    sandbox.python_path.is_file()
        && sandbox.bwrap_path.is_file()
        && std::process::Command::new(&sandbox.bwrap_path)
            .args(["--unshare-all", "--ro-bind", "/", "/", "/usr/bin/true"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

fn candidate_execution_book(c: &Config) {
    write_verified_universe(c, "2026-10-16", "2026-10-15");
    let db = super::super::super::open_book(c).unwrap();
    let body = |at: i64, bid: f64, ask: f64| {
        json!({
            "status":"success",
            "data":{
                "NSE_EQ:INE000000001":{"instrument_token":"NSE_EQ|INE000000001","symbol":"AAA","timestamp":at,
                    "depth":{"buy":[{"price":bid,"quantity":100}],"sell":[{"price":ask,"quantity":100}]},
                    "lower_circuit_limit":80,"upper_circuit_limit":120,"volume":987654321012u64},
                "NSE_EQ:INF000000002":{"instrument_token":"NSE_EQ|INF000000002","symbol":"BBB","timestamp":at,
                    "depth":{"buy":[{"price":200,"quantity":100}],"sell":[{"price":200.05,"quantity":100}]},
                    "lower_circuit_limit":160,"upper_circuit_limit":240,"volume":1_000_000}
            }
        })
    };
    for (at, purpose, bid, ask) in [
        (candidate_at(9, 20), "scanner_metadata", 99.0, 100.05),
        (
            candidate_at(9, 20) + 59_500,
            "execution_or_protection",
            99.0,
            100.05,
        ),
        (candidate_at(9, 21), "scanner_metadata", 99.0, 100.05),
        (
            candidate_at(9, 21) + 59_500,
            "execution_or_protection",
            110.0,
            110.05,
        ),
        (candidate_at(9, 22), "scanner_metadata", 110.0, 110.05),
        (
            candidate_at(9, 22) + 500,
            "execution_or_protection",
            110.0,
            110.05,
        ),
    ] {
        let archive = json!({
            "source":"upstox-readonly-v1",
            "purpose":purpose,
            "requested_ms":at,
            "received_ms":at,
            "body":body(at, bid, ask).to_string()
        });
        db.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?2,?3,'wide_quotes')",
            rusqlite::params![at, at, archive.to_string()],
        )
        .unwrap();
    }
    drop(db);
}

/// `backtest` on a quote dataset replays a strategy_actions_v1 strategy
/// over the session tape now and keeps the report by its content hash.
fn candidate_strategy_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-strategy-contract");
    fs::create_dir_all(&dir).unwrap();
    let mut settings = candidate_settings(&dir, base_settings);
    let (c, cfg) = candidate_config(&dir, base_policy, settings.clone());
    candidate_execution_book(&c);
    let source = r#"def on_observation(o):
    a=[{"instrument_key":k,"side":"buy","max_price":"120","rationale":"","conviction":"0.02",
        "exit_intent":{"stop":"90","time":o["at_ms"]+500}} for k,q in o["quotes"].items()
       if k.endswith("INE000000001") and k not in o["positions"]]
    return {"actions":a}"#;
    let submit = |cfg: &PlannerConfig| {
        let args = candidate_submit_args("candidate-strategy", source);
        backtest_submit(
            &c,
            cfg,
            "grok-4.7",
            "turn-candidate",
            args,
            candidate_at(9, 23),
            &|| false,
        )
        .unwrap()
    };
    assert_eq!(submit(&cfg)["reason"], "candidate_strategy_disabled");
    settings.candidate_compute.as_mut().unwrap().strategy = Some(CandidateStrategySettings {
        max_archive_window_days: default_archive_window_days(),
        recent_archive_sessions: 3,
        enabled: true,
        abi: "strategy_actions_v1".into(),
        max_quote_events: 100,
        max_observations: 1_000,
        max_tape_bytes: 1_048_576,
        max_wall_ms: 20_000,
    });
    let (_, cfg) = candidate_config(&dir, base_policy, settings);
    // A waiting higher-priority turn stops the replay before its first call.
    let args = candidate_submit_args("candidate-strategy", source);
    let stopped = backtest_submit(
        &c,
        &cfg,
        "grok-4.7",
        "turn-candidate",
        args,
        candidate_at(9, 23),
        &|| true,
    )
    .unwrap();
    assert_eq!(stopped["reason"], "strategy_replay_preempted", "{stopped}");
    assert_eq!(stopped["strategy_calls"], 0);
    assert!(!c.directory.join("model/strategy-replays").exists());
    let done = submit(&cfg);
    assert_eq!(done["status"], "completed", "{done}");
    // Three reads update a prospect or a holding. A timed exit removes
    // the held quote from the next call until the ranker selects it again.
    assert_eq!(done["tape"]["strategy_calls"], 3, "{done}");
    assert_eq!(done["action_outcomes"]["filled"], 1, "{done}");
    assert_eq!(done["summary"]["trades"], 1, "{done}");
    assert_eq!(done["trades"][0]["exit_reason"], "time");
    let sha = done["report_sha256"].as_str().unwrap();
    let kept = fs::read(
        c.directory
            .join(format!("model/strategy-replays/{sha}.json")),
    )
    .unwrap();
    assert_eq!(digest(&kept), sha);
    // The losing tape misses the default acceptance thresholds.
    assert_eq!(done["acceptance"]["verdict"], "rejected", "{done}");
    assert_eq!(
        done["acceptance"]["reasons"],
        json!(["too_few_trades", "net_pnl_too_low", "too_few_tape_dates"])
    );
    assert!(
        !c.sessions_root
            .as_ref()
            .unwrap()
            .join("strategy-library")
            .exists()
    );
    assert_eq!(submit(&cfg)["report_sha256"], sha);
}

// #231: the contracts below used to run as one chain inside
// `registered_file_failures_name_the_exact_artifact`, so the first failed
// assert hid every later scenario. Each group is its own test, every case gets
// a fresh fixture, and a failure names each scenario that broke.
type Contract = fn(&Path, &ResearchSettings, &PlannerConfig);

fn run_contracts(cases: &[(&str, Contract)], sandboxed: bool) {
    let mut failed = Vec::new();
    for (name, case) in cases {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("nivra-analysis");
        fs::write(&runtime, b"nivra-analysis-fixture").unwrap();
        let current = settings(root.path(), &runtime);
        if sandboxed && !candidate_sandbox_ready(&candidate_settings(root.path(), &current)) {
            assert!(
                std::env::var_os("NIVRA_REQUIRE_SCORER_SANDBOX").is_none(),
                "candidate sandbox required but python3 or bubblewrap is unavailable"
            );
            eprintln!("skipping {name}: python3 or bwrap unavailable on host");
            continue;
        }
        let owner_dir = tempfile::tempdir().unwrap();
        let (_owner_config, owner_policy) = owner_fixture(owner_dir.path(), &current);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            case(root.path(), &current, &owner_policy)
        }));
        if outcome.is_err() {
            failed.push(*name);
        }
    }
    assert!(failed.is_empty(), "failed research contracts: {failed:?}");
}

#[test]
fn strategy_replay_contracts() {
    run_contracts(&[("strategy", candidate_strategy_contract)], true);
}

#[test]
#[ignore = "requires TimescaleDB at NIVRA_ARCHIVE_TEST_URL"]
fn archive_strategy_replay_pins_quotes_across_sessions() -> Result<()> {
    use crate::runtime::{
        market_archive,
        paper_replay::{ArchiveWindow, DailyWindow, TapeLimits, archive_window_tape},
    };
    use nivra_store::market_archive::{init, write_quotes};
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir()?;
    let dir = tmp.path();
    let archive = crate::runtime::market_archive::test_settings(tmp.path())?;
    let mut writer = market_archive::connect(&archive)?;
    init(&mut writer)?;

    const MANIFEST: &str = include_str!("../../../../../../fixtures/strategy/manifest.json");
    const EVENTS: &str = include_str!("../../../../../../fixtures/strategy/events.jsonl");
    const SOURCE: &str = include_str!("../../../../../../fixtures/strategy/strategy.py");
    const PARAMETERS: &str = include_str!("../../../../../../fixtures/strategy/parameters.json");
    // The reviewed calendar excludes November 24 and the intervening weekend.
    let days = [
        "2026-11-23",
        "2026-11-25",
        "2026-11-26",
        "2026-11-27",
        "2026-11-30",
    ];
    let manifest: Value = serde_json::from_str(MANIFEST)?;
    let events = EVENTS
        .lines()
        .map(serde_json::from_str)
        .collect::<std::result::Result<Vec<Value>, _>>()?;
    let mut research = settings(dir, &dir.join("unused-runtime"));
    let (_, base) = owner_fixture(dir, &research);
    research = candidate_settings(dir, &research);
    let candidate = research.candidate_compute.as_mut().unwrap();
    candidate.datasets.push(CandidateDataset {
        dataset_id: "archive-test".into(),
        kind: CandidateDatasetKind::ArchiveWindow {
            from_session: days[0].parse()?,
            to_session: days[3].parse()?,
        },
    });
    let limits = TapeLimits {
        max_quote_events: 10_000,
        max_observations: 100_000,
        max_tape_bytes: 16 * 1024 * 1024,
    };
    let max_wall_ms = 20_000;
    candidate.strategy = Some(CandidateStrategySettings {
        max_archive_window_days: default_archive_window_days(),
        recent_archive_sessions: 3,
        enabled: true,
        abi: "strategy_actions_v1".into(),
        max_wall_ms,
        max_quote_events: limits.max_quote_events,
        max_observations: limits.max_observations,
        max_tape_bytes: limits.max_tape_bytes,
    });
    assert!(
        crate::runtime::strategy::WorkerConfig::default()
            .python_path
            .is_file(),
        "fixture needs python3"
    );
    let (mut c, cfg) = candidate_config(dir, &base, research);
    c.session = days[3].parse()?;
    c.paper_costs = Some(serde_json::from_value(manifest["paper_costs"].clone())?);
    c.market_archive = Some(archive);
    for day in days {
        let directory = if day.parse::<chrono::NaiveDate>()? == c.session {
            c.directory.clone()
        } else {
            c.sessions_root.as_ref().unwrap().join(day)
        };
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join("wide-universe.json"),
            serde_json::to_vec(&json!({"session":day,"instruments":manifest["universe"]}))?,
        )?;
    }
    let write = |writer: &mut _, event: &Value, at: i64| -> Result<()> {
        let data: serde_json::Map<String, Value> = event["observations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| {
                let key = o["key"].as_str().unwrap();
                let price = |side: &str| {
                    o["quote"][side]
                        .as_str()
                        .unwrap()
                        .parse::<serde_json::Number>()
                        .unwrap()
                };
                let row = json!({
                    "instrument_token":key,"timestamp":at-100,"last_price":price("bid"),
                    "volume":o["volume"],"lower_circuit_limit":10,"upper_circuit_limit":1000,
                    "depth":{
                        "buy":[{"price":price("bid"),"quantity":o["quote"]["bid_units"]}],
                        "sell":[{"price":price("ask"),"quantity":o["quote"]["ask_units"]}]
                    }
                });
                (key.to_owned(), row)
            })
            .collect();
        let raw = json!({
            "source":"upstox-readonly-v1","purpose":"execution_or_protection",
            "requested_ms":at-200,"received_ms":at,
            "body":json!({"status":"success","data":data}).to_string()
        })
        .to_string();
        let batch = market_archive::normalize::capture_quotes(&raw)?;
        write_quotes(writer, &batch)?;
        Ok(())
    };
    for day in &days[..3] {
        let start = candidate_at_on(day, 9, 20);
        for (step, event) in events.iter().enumerate() {
            write(&mut writer, event, start + 1_000 * step as i64)?;
        }
    }
    let start = candidate_at_on(days[0], 9, 20);
    let end = candidate_at_on(days[3], 9, 21);
    let submit = |receipt_id| {
        let mut args = candidate_submit_args("archive-strategy", SOURCE);
        args.params = serde_json::from_str::<Value>(PARAMETERS).unwrap()["params"].clone();
        args.params["start_ms"] = json!(start);
        args.dataset_id = "archive-test".into();
        args.training_from_ms = start - 1;
        args.training_to_ms = start - 1;
        args.selection_from_ms = start;
        args.selection_to_ms = end;
        args.as_of_receipt_id = receipt_id;
        backtest_submit(
            &c,
            &cfg,
            "fixture-model",
            "turn-archive",
            args,
            end,
            &|| false,
        )
    };
    let mut rolling = candidate_submit_args("rolling-strategy", SOURCE);
    rolling.params = serde_json::from_str::<Value>(PARAMETERS)?["params"].clone();
    rolling.params["start_ms"] = json!(start);
    rolling.dataset_id = "recent_archive".into();
    rolling.training_from_ms = start - 1;
    rolling.training_to_ms = start - 1;
    rolling.selection_from_ms = start;
    rolling.selection_to_ms = candidate_at_on(days[2], 15, 30) - 1;
    let run_rolling = || {
        backtest_submit(
            &c,
            &cfg,
            "fixture-model",
            "turn-archive",
            rolling.clone(),
            end,
            &|| false,
        )
    };
    let recent = run_rolling()?;
    assert_eq!(recent["status"], "completed", "{recent}");
    assert_eq!(
        recent["tape"]["archive_window"]["sessions"],
        json!(&days[..3])
    );
    assert_eq!(
        recent["tape"]["archive_window"]["missing_sessions"],
        json!([])
    );
    assert_eq!(recent["acceptance"]["verdict"], "rejected");
    let first = submit(None)?;
    assert_eq!(first["status"], "completed", "{first}");
    assert_eq!(first["tape"]["dates"], json!(&days[..3]), "{first}");
    assert_eq!(
        first["tape"]["archive_window"]["sessions"],
        json!(&days[..4])
    );
    assert_eq!(
        first["tape"]["archive_window"]["missing_sessions"],
        json!([days[3]])
    );
    assert_eq!(first["tape"]["strategy_calls"], 18);
    assert_eq!(first["summary"]["trades"], 2, "{first}");
    assert!(
        !first["acceptance"]["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("too_few_tape_dates"))
    );
    let watermark = first["tape"]["archive_window"]["as_of_receipt_id"]
        .as_i64()
        .unwrap();

    // The append's receipt clock falls inside the original window.
    write(&mut writer, &events[0], end - 1_000)?;
    let repeated = submit(Some(watermark))?;
    assert_eq!(repeated["tape"], first["tape"]);
    assert_eq!(repeated["report_sha256"], first["report_sha256"]);
    let fresh = submit(None)?;
    assert_ne!(
        fresh["tape"]["events_sha256"],
        first["tape"]["events_sha256"]
    );
    assert_eq!(
        fresh["tape"]["archive_window"]["missing_sessions"],
        json!([])
    );

    if std::env::var_os("NIVRA_ARCHIVE_WINDOW_PERF").is_some() {
        for day in days {
            let open = candidate_at_on(day, 9, 30);
            for batch in 0..4_400 {
                write(&mut writer, &events[0], open + batch * 5_000)?;
            }
        }
        let window = ArchiveWindow {
            max_days: default_archive_window_days(),
            from_session: days[0].parse()?,
            to_session: days[4].parse()?,
            from_ms: candidate_at_on(days[0], 9, 30),
            to_ms: candidate_at_on(days[4], 10, 30),
            daily: Some(DailyWindow {
                from_minute: 570,
                to_minute: 630,
            }),
            receipt_id: None,
        };
        let started = Instant::now();
        let budget = Duration::from_millis(max_wall_ms);
        let (tape, coverage) = archive_window_tape(&c, &window, c.allocation()?, limits, &|| {
            (started.elapsed() >= budget).then_some("strategy_replay_wall_budget")
        })?;
        let elapsed = started.elapsed();
        let tape = tape.expect("five sessions of quote tape");
        assert_eq!(coverage.sessions.len(), 5);
        assert!(coverage.missing_sessions.is_empty());
        assert!(elapsed < budget);
        eprintln!(
            "archive_window_tape_ms={:.1} max_wall_ms={max_wall_ms} stored_batches=22000 tape_bytes={}",
            elapsed.as_secs_f64() * 1_000.0,
            tape.events.len()
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires NIVRA_ARCHIVE_TEST_URL, bubblewrap, a systemd user bus and a built nivra-analysis"]
fn archive_bar_job_replays_closed_splits() -> Result<()> {
    use nivra_core::finance::Amount;
    use nivra_store::market_archive::{BarUnit, BarWindow, init, ist_midnight};
    use std::time::Instant;

    // A fresh database per run: concurrent archive schema work must never
    // change the version this replay is built against.
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path();
    let archive = crate::runtime::market_archive::test_settings(dir)?;
    let perf = std::env::var_os("NIVRA_ARCHIVE_BAR_PERF");
    let (unit, counts, from, to, split) = match perf.as_deref().and_then(|v| v.to_str()) {
        Some("day") => (BarUnit::Day, 2760, "2020-01-01", "2024-12-31", "2024-01-01"),
        Some("minute") => (
            BarUnit::Minute,
            50,
            "2024-01-01",
            "2024-12-31",
            "2024-10-01",
        ),
        _ => (BarUnit::Day, 12, "2024-01-01", "2024-02-29", "2024-02-01"),
    };
    let window = BarWindow {
        unit,
        from: from.parse()?,
        to: to.parse()?,
        instruments: None,
    };
    let split = ist_midnight(split.parse()?);
    let now = millis();
    let prefix = format!("NSE_EQ|BAR-FIXTURE-{now}-");
    let keys: Vec<_> = (0..counts).map(|i| format!("{prefix}{i:04}")).collect();
    let mut db = crate::runtime::market_archive::connect(&archive)?;
    init(&mut db)?;
    {
        let mut tx = db.transaction()?;
        tx.batch_execute("SET LOCAL statement_timeout = '120s'")?;
        let receipt: i64 = tx
            .query_one(
                "INSERT INTO receipts(kind, requested_ms, received_ms, sha256, body)
             VALUES (2, $1, $1, $2, $2) RETURNING id",
                &[&now, &Sha256::digest(prefix.as_bytes()).as_slice()],
            )?
            .get(0);
        let ids: Vec<i32> = tx
            .query(
                "INSERT INTO instruments(key) SELECT unnest($1::text[]) RETURNING id",
                &[&keys],
            )?
            .into_iter()
            .map(|row| row.get(0))
            .collect();
        let span = "SELECT $1::date + n AS day, (10000 + (n % 20) * 25)::bigint AS o
             FROM generate_series(0, $2::date - $1::date) n
             WHERE extract(isodow FROM $1::date + n) < 6";
        let insert = match unit {
            BarUnit::Day => format!(
                "INSERT INTO daily_bars(instrument_id, day, o, h, l, c, volume, oi, receipt_id)
                 SELECT i, day, o, o+50, o-10, o+25, 500000, 0, $4
                 FROM ({span}) dates CROSS JOIN unnest($3::int[]) i"
            ),
            BarUnit::Minute => format!(
                "INSERT INTO minute_bars(instrument_id, day, bar_ms, o, h, l, c, volume, oi, receipt_id)
                 SELECT i, day, (extract(epoch FROM day::timestamp AT TIME ZONE 'Asia/Kolkata')::bigint
                     + 33300 + m * 60) * 1000, o+m, o+m+50, o+m-10, o+m+25, 10000, 0, $4
                 FROM ({span}) dates CROSS JOIN generate_series(0, 374) m CROSS JOIN unnest($3::int[]) i"
            ),
        };
        let inserted = tx.execute(&insert, &[&window.from, &window.to, &ids, &receipt])?;
        tx.commit()?;
        assert_eq!(inserted, expected_bar_fixture_rows(&window, counts));
    }
    // The research loop's tape check reads the archive's daily-bar watermark.
    if unit == BarUnit::Day {
        let last = nivra_store::market_archive::last_daily_bar_day(&mut db)?;
        assert!(last.is_some_and(|last| last >= window.to), "{last:?}");
    }
    let mut window = window;
    window.instruments = Some(keys);
    let base = settings(dir, &dir.join("unused-runtime"));
    let research = candidate_settings(dir, &base);
    let (mut c, mut cfg) = owner_fixture(dir, &research);
    c.market_archive = Some(archive);
    let research = cfg.research_tools.as_mut().unwrap();
    let candidate = research.candidate_compute.as_mut().unwrap();
    candidate.strategy = Some(CandidateStrategySettings {
        enabled: true,
        abi: crate::runtime::strategy::ABI.into(),
        max_quote_events: 10_000,
        max_observations: 100_000,
        max_tape_bytes: 16 * 1024 * 1024,
        max_wall_ms: 20_000,
        max_archive_window_days: default_archive_window_days(),
        recent_archive_sessions: 3,
    });
    candidate.wall_millis = 360_000;
    if perf.is_none() {
        candidate.datasets.push(serde_json::from_value(json!({
            "dataset_id":"named-bars", "kind":{"archive_bars":&window},
        }))?);
    }
    // One small round trip per split after the strategy has inspected a full batch.
    let source = r#"from statistics import mean
def on_bars(at, split, bars, params, state):
    n = state.get(split, 0)
    state[split] = n + 1
    average_close = mean(row[4] for row in bars)
    if n == 0 and average_close > params["minimum"]:
        return [[0, 1]]
    if n == 1:
        return [[0, 0]]
    return []
"#;
    let mut args = candidate_submit_args("archive-bars-integration", source);
    args.params = json!({"minimum":0});
    args.dataset_id = ARCHIVE_BARS_DATASET_ID.into();
    args.archive_bars = Some(Box::new(window.clone()));
    args.training_from_ms = ist_midnight(window.from);
    args.training_to_ms = split - 1;
    args.selection_from_ms = split;
    args.selection_to_ms = ist_midnight(window.to.succ_opt().unwrap()) - 1;
    let run = |args: BacktestArgs| -> Result<Value> {
        let start = Instant::now();
        let queued = backtest_submit(
            &c,
            &cfg,
            "fixture-model",
            "bar-test-turn",
            args,
            millis(),
            &|| false,
        )?;
        assert_eq!(queued["status"], "pending", "{queued}");
        let task = queued["task_id"].as_str().unwrap();
        let revision = queued["revision"].as_u64().unwrap() as u32;
        assert_eq!(
            backtest_status(&c, &cfg, task, revision)?["status"],
            "pending"
        );
        drive_task(
            cfg.research_tools.clone().unwrap(),
            c.directory.join("model"),
            task.into(),
            revision,
            &|| false,
        )?;
        let result = backtest_result(&c, &cfg, task, revision)?;
        assert_eq!(result["result_status"], "verified", "{result}");
        assert_eq!(result["report"]["evidence"], "bar_level");
        assert_eq!(result["acceptance"]["verdict"], "research_only");
        let report = &result["report"];
        // The loop scores this exact report: daily net P&L and drawdown ride along.
        let fold: nivra_core::autoresearch::qualification::Fold =
            serde_json::from_value(report["test"].clone())?;
        assert_eq!(fold.closed_trades, 1);
        assert!(fold.max_drawdown_bps >= 0);
        assert!(
            report["test"]["daily_net_pnl"]
                .as_array()
                .is_some_and(|days| !days.is_empty()),
            "{report}"
        );
        let total =
            report["training"]["bars"].as_u64().unwrap() + report["test"]["bars"].as_u64().unwrap();
        assert_eq!(total, expected_bar_fixture_rows(&window, counts));
        for part in ["training", "test"] {
            assert_eq!(report[part]["closed_trades"], 1, "{report}");
            let fills = report[part]["fill_sample"].as_array().unwrap();
            assert_eq!(fills.len(), 2);
            for fill in fills {
                assert!(
                    fill["fill_bar_ms"].as_i64().unwrap() > fill["signal_bar_ms"].as_i64().unwrap()
                );
                assert!(Amount::parse(fill["fee"].as_str().unwrap())? > Amount::ZERO);
            }
        }
        eprintln!(
            "archive_bar_job unit={unit:?} instruments={counts} rows={total} elapsed_s={:.3}",
            start.elapsed().as_secs_f64()
        );
        Ok(result)
    };
    let first = run(args.clone())?;
    if perf.is_none() {
        args.dataset_id = "named-bars".into();
        args.archive_bars = None;
        let second = run(args)?;
        assert_eq!(first["report_sha256"], second["report_sha256"]);
        assert_eq!(first["report"], second["report"]);
    }
    Ok(())
}

fn expected_bar_fixture_rows(
    window: &nivra_store::market_archive::BarWindow,
    instruments: u64,
) -> u64 {
    use chrono::Datelike;
    let sessions = window
        .from
        .iter_days()
        .take_while(|date| *date <= window.to)
        .filter(|date| date.weekday().number_from_monday() <= 5)
        .count() as u64;
    let bars_per_session = match window.unit {
        nivra_store::market_archive::BarUnit::Day => 1,
        nivra_store::market_archive::BarUnit::Minute => 375,
    };
    sessions * instruments * bars_per_session
}
