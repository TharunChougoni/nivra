//! Explicit offline paper simulation. No model subscription or market client is constructed.
use super::*;
use sadr_core::{
    execution::{Instrument, Portfolio, Scanner},
    finance::Amount,
    historical::{Event, EventKind},
};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{BufRead, BufReader, Write},
    os::unix::fs::DirBuilderExt,
    rc::Rc,
};
mod export;
pub use export::*;
mod receipts;
pub(crate) use receipts::*;
mod tape;
pub(crate) use tape::*;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    paper_costs: Option<sadr_core::finance::FeeArithmetic>,
    schema: u32,
    session: NaiveDate,
    previous_session: NaiveDate,
    capital: Amount,
    universe: Vec<Instrument>,
    events_sha256: String,
    synthetic: bool,
    max_observation_gap_ms: i64,
    history_directory: Option<PathBuf>,
    provenance: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    role: String,
    latency_ms: i64,
    output: Value,
}
struct Recorded {
    tape: Connection,
    clock: Rc<Cell<i64>>,
    replies: VecDeque<Reply>,
    calls: BTreeMap<String, u64>,
}
impl planner::Backend for Recorded {
    fn call(&mut self, role: &planner::ModelRole, prompt: &str) -> Result<String> {
        let frame: harness::Frame = serde_json::from_str(prompt)?;
        let reply = self
            .replies
            .pop_front()
            .ok_or("paper_replay_transcript_exhausted")?;
        if reply.role != frame.role || reply.latency_ms < 0 {
            return Err("paper_replay_response_role_or_clock".into());
        }
        *self.calls.entry(frame.role).or_default() += 1;
        self.clock.set(
            self.clock
                .get()
                .checked_add(reply.latency_ms.min(role.timeout_ms as i64))
                .ok_or("paper_replay_clock_range")?,
        );
        if reply.latency_ms > role.timeout_ms as i64 {
            return Err("planner_call_timeout".into());
        }
        Ok(if let Some(raw) = reply.output.as_str() {
            raw.to_owned()
        } else {
            reply.output.to_string()
        })
    }
    fn provenance(&self) -> Value {
        json!({"kind":"offline_recorded_model_responses","network":false,"calls":self.calls})
    }
    fn snapshot(&mut self, bundle: &Value, keys: &[String], at: i64) -> Option<Result<Value>> {
        Some((|| {
            let allowed: Vec<Instrument> = serde_json::from_value(bundle["allowlist"].clone())?;
            if keys
                .iter()
                .any(|key| !allowed.iter().any(|i| i.key == *key))
            {
                return Err("paper_replay_quote_not_allowlisted".into());
            }
            let mut found = BTreeMap::new();
            let mut statement = self
                .tape
                .prepare("SELECT body FROM replay_events WHERE at<=? AND at>=? ORDER BY id DESC")?;
            let rows = statement.query_map(
                rusqlite::params![
                    at,
                    at.saturating_sub(sadr_core::market::MAX_FINANCIAL_AGE_MS)
                ],
                |r| r.get::<_, String>(0),
            )?;
            for row in rows {
                let event: Event = serde_json::from_str(&row?)?;
                for observation in event.observations {
                    if keys.contains(&observation.key)
                        && observation
                            .quote
                            .fresh(at, sadr_core::market::MAX_FINANCIAL_AGE_MS)
                    {
                        found.entry(observation.key.clone()).or_insert(json!({"instrument":allowed.iter().find(|i|i.key==observation.key),"quote":observation.quote,"volume":observation.volume}));
                    }
                }
            }
            if keys.iter().any(|key| !found.contains_key(key)) {
                return Err("paper_replay_fresh_quote_unavailable".into());
            }
            Ok(
                json!({"quotes":keys.iter().map(|k|&found[k]).collect::<Vec<_>>(),"execution_authorized":false,"offline_recorded_receipts":true}),
            )
        })())
    }
}
fn create_output(output: &Path) -> Result<()> {
    if !output.is_absolute() || output.exists() {
        return Err("paper_replay_new_output_required".into());
    }
    sadr_store::native_grant::file_path_ok(output)
        .map_err(|_| "paper_replay_output_path_invalid")?;
    fs::DirBuilder::new().mode(0o700).create(output)?;
    Ok(())
}
fn config(manifest: &Manifest, output: &Path) -> Result<Config> {
    Ok(serde_json::from_value(
        json!({"account":"offline-paper-replay","session":manifest.session,"previous_session":manifest.previous_session,"calendar_source":"offline-replay-declaration","directory":output,"token_file":output.join("NO_CREDENTIAL"),"dashboard_token_file":output.join("NO_CREDENTIAL"),"read_journal":output.join("NO_READ_JOURNAL"),"read_scope":"offline-recorded-quotes","read_journal_device":0,"read_journal_inode":0,"strategy":"model_planner_v1","capital":manifest.capital,"paper_costs":manifest.paper_costs}),
    )?)
}
fn times(c: &Config) -> Result<(i64, i64)> {
    let (open, close) = c
        .calendar_at(c.session)?
        .hours(sadr_core::time::ExchangeDate::parse(
            &c.session.to_string(),
        )?)?
        .ok_or("paper_replay_not_trading_session")?;
    let parse = |time| -> Result<i64> {
        Ok(
            DateTime::parse_from_rfc3339(&format!("{}T{time}+05:30", c.session))?
                .timestamp_millis(),
        )
    };
    Ok((parse(open)?, parse(close)?))
}
fn load_tape(db: &mut Connection, events: &Path, m: &Manifest) -> Result<(u64, i64, i64, i64)> {
    db.execute_batch("CREATE TABLE replay_events(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,body TEXT NOT NULL); CREATE INDEX replay_events_at ON replay_events(at);")?;
    let mut reader = BufReader::new(fs::File::open(events)?);
    let mut hash = Sha256::new();
    let mut line = Vec::new();
    let mut count = 0u64;
    let mut previous = None;
    let mut first = 0;
    let mut max_gap = 0;
    let tx = db.transaction()?;
    loop {
        line.clear();
        let n = reader
            .by_ref()
            .take(2_000_001)
            .read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        if n > 2_000_000 {
            return Err("paper_replay_event_too_large".into());
        }
        hash.update(&line);
        let event: Event = serde_json::from_slice(&line)?;
        if previous.is_some_and(|at| event.at < at)
            || event.available_at > event.at
            || DateTime::<Utc>::from_timestamp_millis(event.at)
                .ok_or("paper_replay_clock_range")?
                .with_timezone(&FixedOffset::east_opt(19800).expect("IST"))
                .date_naive()
                != m.session
            || event.observations.len() > m.universe.len()
        {
            return Err("paper_replay_chronology_or_future_evidence".into());
        }
        let mut seen = BTreeSet::new();
        for o in &event.observations {
            let i = m
                .universe
                .iter()
                .find(|i| i.key == o.key)
                .ok_or("paper_replay_quote_identity")?;
            if !seen.insert(&o.key)
                || o.quote.source > o.quote.receipt
                || o.quote.receipt > event.available_at
                || o.quote.bid > o.quote.ask
                || sadr_core::finance::CashOrderGrid::new(1, i.tick)?
                    .validate(1, o.quote.bid)
                    .is_err()
                || sadr_core::finance::CashOrderGrid::new(1, i.tick)?
                    .validate(1, o.quote.ask)
                    .is_err()
            {
                return Err("paper_replay_quote_identity_or_leakage".into());
            }
        }
        if let Some(prior) = previous {
            max_gap = max_gap.max(event.at - prior);
        } else {
            first = event.at;
        }
        previous = Some(event.at);
        count += 1;
        tx.execute(
            "INSERT INTO replay_events(at,body) VALUES(?,?)",
            rusqlite::params![event.at, std::str::from_utf8(&line)?],
        )?;
    }
    let sha = hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if sha != m.events_sha256 || count == 0 {
        return Err("paper_replay_events_binding_or_empty".into());
    }
    tx.commit()?;
    Ok((count, first, previous.expect("nonempty"), max_gap))
}
fn next_event(db: &Connection, after: i64) -> Result<Option<(i64, Event)>> {
    use rusqlite::OptionalExtension;
    let row: Option<(i64, String)> = db
        .query_row(
            "SELECT id,body FROM replay_events WHERE id>? ORDER BY id LIMIT 1",
            [after],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(id, body)| Ok((id, serde_json::from_str(&body)?)))
        .transpose()
}
#[allow(clippy::too_many_arguments)]
fn apply_event(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &mut Portfolio,
    scan: &mut Scanner,
    universe: &[Instrument],
    event: &Event,
) -> Result<()> {
    // A research-observation quote is virtual authority only. Offline paper
    // replay has no such authority and must refuse it rather than let a
    // research quote reach the paper book.
    if matches!(event.kind, EventKind::VirtualObservation) {
        return Err("paper_replay_virtual_observation_unauthorized".into());
    }
    let mut quotes = BTreeMap::new();
    for o in &event.observations {
        let i = universe
            .iter()
            .find(|i| i.key == o.key)
            .ok_or("paper_replay_quote_identity")?;
        if matches!(event.kind, EventKind::Screen) {
            scan.observe_fields(event.at, i, &o.quote, &o.fields())?;
        }
        quotes.insert(o.key.clone(), o.quote.clone());
    }
    if matches!(event.kind, EventKind::Screen) {
        wide::checkpoint_scanner(db, scan)?;
    }
    wide::commit_quotes(
        db,
        c,
        p,
        &quotes,
        &[],
        event.available_at,
        "offline-recorded-quote-event",
        || event.at,
    )?;
    let observations = event
        .observations
        .iter()
        .map(|o| sadr_core::redecision::MarketObservation {
            instrument: universe
                .iter()
                .find(|i| i.key == o.key)
                .expect("checked")
                .clone(),
            quote: o.quote.clone(),
            cumulative_volume: Some(o.volume),
            prior_close: None,
            session_open: None,
            exit_intent: None,
        })
        .collect::<Vec<_>>();
    redecision::observe(db, c, cfg, p, &observations, &[], event.at)?;
    if matches!(event.kind, EventKind::Screen)
        && let Some(settings) = cfg.discovery.as_ref()
    {
        let projection = super::discovery::project(scan, universe, settings, event.available_at);
        let candidates = super::discovery::candidates(&projection);
        redecision::scan_refreshed(
            db,
            c,
            cfg,
            event.available_at,
            &candidates,
            projection["market_median_bps"].as_f64(),
            projection["market_sample_count"].as_u64().unwrap_or(0) as usize,
        )?;
    }
    Ok(())
}
fn run(
    manifest: &Path,
    events: &Path,
    planner: &Path,
    responses: &Path,
    output: &Path,
) -> Result<Value> {
    let m: Manifest = serde_json::from_slice(
        &crate::read_bounded(manifest, 8_000_000)
            .map_err(|_| "paper_replay_manifest_unreadable")?,
    )?;
    if m.schema != 1
        || m.max_observation_gap_ms <= 0
        || m.previous_session >= m.session
        || m.capital <= Amount::ZERO
        || m.universe.is_empty()
        || m.universe.len() > 10000
        || m.universe
            .iter()
            .map(|i| &i.key)
            .collect::<BTreeSet<_>>()
            .len()
            != m.universe.len()
    {
        return Err("paper_replay_manifest_invalid".into());
    }
    m.paper_costs
        .as_ref()
        .ok_or("paper_costs_required")?
        .validate()?;
    let mut cfg = planner::PlannerConfig::load_for_session(planner, m.session)?;
    if cfg.discovery.is_none() {
        return Err("paper_replay_discovery_required".into());
    }
    // Recorded replies only: remove credential references, native network search and workers.
    cfg.model_config = None;
    cfg.digest.model_config = None;
    cfg.digest.tools.clear();
    cfg.research_tools = None;
    cfg.usage_namespace = Some("usage".into());
    let reply_bytes = crate::read_bounded(responses, 64_000_000)
        .map_err(|_| "paper_replay_responses_unreadable")?;
    let replies: VecDeque<Reply> = serde_json::from_slice(&reply_bytes)?;
    create_output(output)?;
    let mut c = config(&m, output)?;
    c.history_directory = m.history_directory.clone();
    c.planner_config = Some(output.join("planner.json"));
    rollover::atomic(
        c.planner_config.as_ref().expect("set"),
        &serde_json::to_value(&cfg)?,
    )?;
    let (open, close) = times(&c)?;
    let mut tape = Connection::open(output.join("replay-tape.sqlite"))?;
    let (count, first, last, max_gap) = load_tape(&mut tape, events, &m)?;
    let clock = Rc::new(Cell::new(first));
    let mut backend = Recorded {
        tape: Connection::open_with_flags(
            output.join("replay-tape.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?,
        clock: clock.clone(),
        replies,
        calls: BTreeMap::new(),
    };
    let mut db = open_book(&c)?;
    wide::ensure_actions(&db)?;
    let mut p = Portfolio::with_capital(m.capital);
    p.costs = m.paper_costs.clone();
    p.model_led = true;
    let mut scan = Scanner::default();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), first)?;
    let mut cursor = 0;
    let mut cycles = 0;
    let mut transcript_exhausted = false;
    while let Some((id, event)) = next_event(&tape, cursor)? {
        cursor = id;
        clock.set(event.at);
        apply_event(&mut db, &c, &cfg, &mut p, &mut scan, &m.universe, &event)?;
        if event.at >= close {
            continue;
        }
        if backend.replies.is_empty() {
            transcript_exhausted = true;
            continue;
        }
        let eligible = discovery::project(
            &scan,
            &m.universe,
            cfg.discovery.as_ref().expect("checked"),
            clock.get(),
        )["fresh_count"]
            .as_u64()
            .is_some_and(|n| n > 0);
        let Some(claim) = redecision::reserve(&mut db, &c, &cfg, &p, clock.get(), eligible)? else {
            continue;
        };
        cycles += 1;
        let result = planner::produce_clocked(
            &c,
            &cfg,
            &p,
            &m.universe,
            false,
            Some(&claim),
            None,
            &|| clock.get(),
            Some(&mut backend),
            None,
        )
        .map_err(|e| planner::reason(e.as_ref()).to_owned());
        let finished = clock.get();
        // Protection continues on recorded observations while model latency elapses.
        while let Some((id, next)) = next_event(&tape, cursor)? {
            if next.at > finished {
                break;
            }
            cursor = id;
            apply_event(&mut db, &c, &cfg, &mut p, &mut scan, &m.universe, &next)?;
        }
        redecision::settle(&mut db, &c, &cfg, &p, &claim, result, finished, true)?;
    }
    if wide::close_portfolio(&mut p) {
        wide::persist(
            &mut db,
            &c,
            &p,
            clock.get().max(close),
            None,
            None,
            "offline_session_closed",
        )?;
    }
    p.reconcile()?;
    let fees = p
        .fills
        .iter()
        .try_fold(Amount::ZERO, |total, f| total.checked_add(f.fee))?;
    let states = db
        .prepare("SELECT state,count(*) FROM planner_attempt_log GROUP BY state")?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
    let result = json!({"schema":1,"paper_only":true,"live_enabled":false,"offline":true,"model_source":"recorded_or_scripted_responses_no_network","synthetic":m.synthetic,"session":m.session,"source_events_sha256":m.events_sha256,"responses_sha256":digest(&reply_bytes),"coverage":{"market_open_ms":open,"market_close_ms":close,"first_observation_ms":first,"last_observation_ms":last,"largest_gap_ms":max_gap,"full_session_window":first<=open.saturating_add(m.max_observation_gap_ms) && last>=close.saturating_sub(m.max_observation_gap_ms),"max_observation_gap_ms":m.max_observation_gap_ms,"within_gap_tolerance":max_gap<=m.max_observation_gap_ms,},"events":count,"analysis_cycles":cycles,"attempt_states":states,"provider_requests":backend.calls,"usage":planner::usage(&c)?,"transcript_exhausted":transcript_exhausted,"unused_responses":backend.replies.len(),"fees":fees,"realised_net_pnl_by_fill":p.realised_net_pnl(),"portfolio":p,"operationally_accepted":false,"limitations":["Recorded quotes are sparse; unavailable quotes are not fabricated or retimed.","Historical virtual receipts test the shared two-second gate, not real network freshness.","Scripted model responses test orchestration, not model intelligence or historical profitability.","No original session, usage quota, credentials, source journal or service is modified."]});
    rollover::atomic(&output.join("report.json"), &result)?;
    Ok(
        json!({"paper_only":true,"offline":true,"synthetic":m.synthetic,"events":count,"analysis_cycles":cycles,"fills":result["portfolio"]["fills"].as_array().map(|f|f.len()),"coverage":result["coverage"],"attempt_states":states,"report":"report.json"}),
    )
}
pub fn entry(
    manifest: &Path,
    events: &Path,
    planner: &Path,
    responses: &Path,
    output: &Path,
) -> ExitCode {
    match run(manifest, events, planner, responses, output) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("paper_replay_refused: {}", failure_code(error.as_ref()));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
pub(super) fn exercise() {
    // Replay artifacts obey the same no-repository path boundary as real runs.
    // Prefer the system temp filesystem and fall back outside a checkout when
    // TMPDIR itself is inside one. The fixture is fsync-heavy, so tmpfs keeps
    // this full-session scenario affordable without weakening durability.
    let dir = tempfile::tempdir().unwrap();
    let dir = if sadr_store::native_grant::file_path_ok(dir.path()).is_ok() {
        dir
    } else {
        tempfile::tempdir_in("/var/tmp").unwrap()
    };
    let input = dir.path().join("input");
    demo(&input).unwrap();
    let original_events = fs::read(input.join("events.jsonl")).unwrap();
    let output = dir.path().join("run");
    run(
        &input.join("manifest.json"),
        &input.join("events.jsonl"),
        &input.join("planner.json"),
        &input.join("responses.json"),
        &output,
    )
    .unwrap();
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["events"], 376);
    assert_eq!(report["coverage"]["full_session_window"], true);
    assert_eq!(
        report["provider_requests"]["digest"], 375,
        "owner policy records a digest for each screened minute in this fixture"
    );
    assert!(report["provider_requests"].get("decide").is_none());
    assert!(report["usage"].get("decide").is_none());
    assert_eq!(report["attempt_states"]["deferred"], 375);
    assert_eq!(report["attempt_states"]["planned"].as_u64().unwrap_or(0), 0);
    assert_eq!(report["portfolio"]["fills"].as_array().unwrap().len(), 0);
    let replay_book: Portfolio = serde_json::from_value(report["portfolio"].clone()).unwrap();
    assert_eq!(
        report["realised_net_pnl_by_fill"],
        json!(replay_book.realised_net_pnl())
    );
    assert!(
        replay_book
            .realised_net_pnl()
            .into_iter()
            .flatten()
            .next()
            .is_none()
    );
    assert_eq!(replay_book.cash, replay_book.allocation().unwrap());

    assert!(
        report["portfolio"]["positions"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert_eq!(report["portfolio"]["halted"], false);
    assert_eq!(report["portfolio"]["closed"], true);
    assert_eq!(report["transcript_exhausted"], false);
    assert_eq!(
        Amount::parse(report["fees"].as_str().unwrap()).unwrap(),
        Amount::ZERO
    );
    assert!(!output.join("NO_CREDENTIAL").exists());
    assert!(!output.join("NO_READ_JOURNAL").exists());
    assert_eq!(
        original_events,
        fs::read(input.join("events.jsonl")).unwrap()
    );
    assert!(
        run(
            &input.join("manifest.json"),
            &input.join("events.jsonl"),
            &input.join("planner.json"),
            &input.join("responses.json"),
            &output
        )
        .is_err()
    );
    let inputs = fs::read_dir(output.join("model"))
        .unwrap()
        .filter_map(|f| {
            let path = f.ok()?.path().join("input.json");
            fs::read(path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        })
        .collect::<Vec<_>>();
    assert!(inputs.iter().all(|v| {
        v["input"].get("previous_luna").is_none()
            && v["input"].get("previous_grok_decision").is_none()
    }));
    assert!(
        inputs
            .iter()
            .all(|v| v["input"]["discovery"]["fresh_count"] == 1)
    );
    // A provider timeout consumes its allowance, remains no-trade, and never halts.
    let timeout = dir.path().join("timeout.json");
    fs::write(
        &timeout,
        serde_json::to_vec(&vec![Reply {
            role: "digest".into(),
            latency_ms: 120001,
            output: json!({}),
        }])
        .unwrap(),
    )
    .unwrap();
    let timeout_output = dir.path().join("timeout-run");
    run(
        &input.join("manifest.json"),
        &input.join("events.jsonl"),
        &input.join("planner.json"),
        &timeout,
        &timeout_output,
    )
    .unwrap();
    let report: Value =
        serde_json::from_slice(&fs::read(timeout_output.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["attempt_states"]["failed"], 1);
    assert_eq!(report["portfolio"]["halted"], false);
    assert_eq!(report["usage"]["digest"], 0);
    assert_eq!(report["portfolio"]["fills"].as_array().unwrap().len(), 0);
    assert_eq!(report["transcript_exhausted"], true);
    // Protection/entry observations cannot manufacture scanner refresh evidence.
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(input.join("manifest.json")).unwrap()).unwrap();
    let purpose_output = dir.path().join("purpose-run");
    create_output(&purpose_output).unwrap();
    let c = config(&manifest, &purpose_output).unwrap();
    let cfg = planner::PlannerConfig::load(&input.join("planner.json")).unwrap();
    let mut db = open_book(&c).unwrap();
    wide::ensure_actions(&db).unwrap();
    let mut p = Portfolio::with_capital(manifest.capital);
    if let Some(costs) = &manifest.paper_costs {
        costs.validate().unwrap();
    }
    p.costs = manifest.paper_costs.clone();
    p.model_led = true;
    let mut scan = Scanner::default();
    let first = original_events.split(|b| *b == b'\n').next().unwrap();
    let mut event: Event = serde_json::from_slice(first).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), event.at).unwrap();
    for kind in [EventKind::Protection, EventKind::Entry] {
        event.kind = kind;
        apply_event(
            &mut db,
            &c,
            &cfg,
            &mut p,
            &mut scan,
            &manifest.universe,
            &event,
        )
        .unwrap();
        assert!(scan.samples.is_empty());
    }
    event.kind = EventKind::Screen;
    apply_event(
        &mut db,
        &c,
        &cfg,
        &mut p,
        &mut scan,
        &manifest.universe,
        &event,
    )
    .unwrap();
    assert_eq!(scan.samples.len(), 1);
    // Exact source receipts and as-of clocks cannot be moved forward to obtain fills.
    let mut m: Manifest =
        serde_json::from_slice(&fs::read(input.join("manifest.json")).unwrap()).unwrap();
    let first = original_events.split(|b| *b == b'\n').next().unwrap();
    let mut future: Value = serde_json::from_slice(first).unwrap();
    future["observations"][0]["quote"]["receipt"] = json!(future["at"].as_i64().unwrap() + 1);
    let future = format!("{future}\n");
    m.events_sha256 = digest(future.as_bytes());
    let future_file = dir.path().join("future.jsonl");
    fs::write(&future_file, future).unwrap();
    let mut db = Connection::open_in_memory().unwrap();
    assert_eq!(
        load_tape(&mut db, &future_file, &m)
            .unwrap_err()
            .to_string(),
        "paper_replay_quote_identity_or_leakage"
    );
    // Large configured allowances are valid; they are not compiled role caps.
    let mut cfg: Value =
        serde_json::from_slice(&fs::read(input.join("planner.json")).unwrap()).unwrap();
    cfg["digest"]["daily_call_budget"] = json!(5000);
    rollover::atomic(&dir.path().join("liberal-planner.json"), &cfg).unwrap();
    planner::PlannerConfig::load(&dir.path().join("liberal-planner.json")).unwrap();
}
