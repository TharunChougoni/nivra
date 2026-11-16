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
    cfg.decide.model_config = None;
    cfg.digest.tools.clear();
    cfg.decide.tools.clear();
    cfg.research_tools = None;
    cfg.post_close = None;
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
        if let Some(plan) =
            redecision::settle(&mut db, &c, &cfg, &p, &claim, result, finished, true)?
        {
            rollover::atomic(&output.join("model/session-plan.json"), &plan)?;
        }
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
/// Explicit archived quote purpose. Research-observation receipts are virtual
/// authority only: they are never mapped to protection or screen and never
/// become executable paper evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QuotePurposeV1 {
    Protection,
    Screen,
    ResearchObservation,
}
impl QuotePurposeV1 {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Protection => "protection",
            Self::Screen => "screen",
            Self::ResearchObservation => "research_observation",
        }
    }
    fn archive_str(self) -> &'static str {
        match self {
            Self::Protection => "execution_or_protection",
            Self::Screen => "scanner",
            Self::ResearchObservation => "research_observation",
        }
    }
    fn from_archive(value: Option<&str>) -> Option<Self> {
        match value {
            Some("scanner" | "scanner_metadata") => Some(Self::Screen),
            Some("execution_or_protection") => Some(Self::Protection),
            Some("research_observation") => Some(Self::ResearchObservation),
            _ => None,
        }
    }
}
/// Original quote receipts shared by feature export and lifecycle marks. Only
/// archived receipt times and source observation times are returned; nothing is
/// interpolated or filled forward.
pub(crate) struct QuoteReceipt {
    pub at_ms: i64,
    pub receipt_ms: i64,
    pub kind: QuotePurposeV1,
    pub observations: Vec<Value>,
}
pub(crate) struct QuoteEvidence {
    pub instruments: Vec<Instrument>,
    pub receipts: Vec<QuoteReceipt>,
}
/// Quote receipts whose original source observation is at or before the host
/// eligibility clock. A future row can never enter a candidate dataset or a
/// maturation window.
pub(crate) fn quote_receipts_bounded(
    source: &Path,
    session: NaiveDate,
    max_rows: usize,
    max_source_ms: i64,
) -> std::result::Result<QuoteEvidence, String> {
    if !source.is_absolute() || !(1..=10_000).contains(&max_rows) || max_source_ms < 0 {
        return Err("scorer_snapshots_unavailable".into());
    }
    let universe: Value = serde_json::from_slice(
        &crate::read_bounded(&source.join("wide-universe.json"), 8_000_000)
            .map_err(|_| "scorer_snapshots_unavailable".to_string())?,
    )
    .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    let declared: NaiveDate = serde_json::from_value(universe["session"].clone())
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    let instruments: Vec<Instrument> = serde_json::from_value(universe["instruments"].clone())
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    if declared != session || instruments.is_empty() || instruments.len() > 10_000 {
        return Err("scorer_snapshots_unavailable".into());
    }
    let db = Connection::open_with_flags(
        source.join("paper.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    db.pragma_update(None, "query_only", true)
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    db.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 2_000_000)
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    let mut statement = db
        .prepare(
            "SELECT at,receipt,body FROM ticks WHERE event IN ('wide_quotes','scanner_metadata') AND at<=?2 ORDER BY id DESC LIMIT ?1",
        )
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    let rows = statement
        .query_map(rusqlite::params![max_rows as i64, max_source_ms], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    let mut reversed = Vec::new();
    for row in rows {
        reversed.push(row.map_err(|_| "scorer_snapshots_unavailable".to_string())?);
    }
    reversed.reverse();
    let mut receipts = Vec::with_capacity(reversed.len());
    let mut previous = None;
    for (at, receipt, raw) in reversed {
        if raw.len() > 2_000_000 {
            return Err("scorer_snapshots_unavailable".into());
        }
        let receipt = receipt.ok_or("scorer_snapshots_receipt_missing")?;
        let archive: Value =
            serde_json::from_str(&raw).map_err(|_| "scorer_snapshots_invalid".to_string())?;
        if archive["kind"] == "compacted_quote_receipt"
            || archive["received_ms"] != receipt
            || receipt > at
            || at > max_source_ms
            || receipt > max_source_ms
        {
            return Err("scorer_snapshots_receipt_binding".into());
        }
        let body: Value = serde_json::from_str(
            archive["body"]
                .as_str()
                .ok_or("scorer_snapshots_not_quote_receipt")?,
        )
        .map_err(|_| "scorer_snapshots_invalid".to_string())?;
        if body["status"] != "success" {
            return Err("scorer_snapshots_quote_status".into());
        }
        let kind = QuotePurposeV1::from_archive(archive["purpose"].as_str())
            .ok_or("scorer_snapshots_purpose_unknown")?;
        // The ORIGINAL archived receipt SHA for every observation. It is never
        // the whole-tape digest and never another key's SHA.
        let archive_sha256 = sadr_core::replay::digest(raw.as_bytes());
        let mut observations = Vec::new();
        let rows = body["data"]
            .as_object()
            .ok_or("scorer_snapshots_quote_rows")?;
        for row in rows.values() {
            let Some(instrument) = instruments
                .iter()
                .find(|instrument| row["instrument_token"] == instrument.key)
            else {
                continue;
            };
            let quote = match data::parse_row(
                row,
                receipt,
                &instrument.key,
                &instrument.symbol,
                instrument.tick,
            ) {
                Ok(quote) => quote,
                Err(_) => continue,
            };
            let (lower, upper, volume) = match (
                row["lower_circuit_limit"].as_number(),
                row["upper_circuit_limit"].as_number(),
                row["volume"].as_u64(),
            ) {
                (Some(lower), Some(upper), Some(volume)) => {
                    (lower.to_string(), upper.to_string(), volume)
                }
                _ => continue,
            };
            observations.push(json!({
                "key":instrument.key,"quote":quote,
                "lower_circuit_limit":lower,"upper_circuit_limit":upper,"volume":volume,
                "archive_sha256":archive_sha256,
                "purpose":kind.archive_str()
            }));
        }
        if previous.is_some_and(|prior| at <= prior) {
            return Err("scorer_snapshots_chronology".into());
        }
        previous = Some(at);
        receipts.push(QuoteReceipt {
            at_ms: at,
            receipt_ms: receipt,
            kind,
            observations,
        });
    }
    if receipts.is_empty() {
        return Err("scorer_snapshots_unavailable".into());
    }
    Ok(QuoteEvidence {
        instruments,
        receipts,
    })
}

/// Frozen executable quote tape for the optional after-cost cash replay.
///
/// It is built from the same `quote_receipts_bounded` reader as the scorer
/// dataset, so there is no second quote normalizer. Only receipts whose
/// original source observation is at or before the submission clock enter the
/// tape; a later exit quote is never fetched. Screen receipts stay in the tape
/// as ranking-only evidence and the adapter refuses them for fills.
pub(crate) struct ExecutionTape {
    pub manifest: Vec<u8>,
    pub events: Vec<u8>,
    pub manifest_sha256: String,
    pub events_sha256: String,
    pub protection_receipts: usize,
    pub protection_observations: usize,
    pub screen_receipts: usize,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execution_tape_bounded(
    source: &Path,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    capital: Amount,
    max_quote_events: usize,
    max_observations: usize,
    max_tape_bytes: u64,
    now: i64,
) -> std::result::Result<ExecutionTape, String> {
    if capital <= Amount::ZERO
        || !(1..=10_000).contains(&max_quote_events)
        || !(1..=100_000).contains(&max_observations)
        || !(1..=16 * 1024 * 1024).contains(&max_tape_bytes)
        || now < 0
    {
        return Err("candidate_execution_tape_invalid".into());
    }
    let evidence = quote_receipts_bounded(source, session, max_quote_events, now)?;
    let mut events = Vec::new();
    let mut observations = 0usize;
    let mut protection_receipts = 0usize;
    let mut protection_observations = 0usize;
    let mut screen_receipts = 0usize;
    let mut max_gap_ms = 0i64;
    let mut previous_receipt_ms = None;
    for receipt in &evidence.receipts {
        // Research-observation receipts are virtual authority only. The
        // historical protection tape skips them entirely instead of relabelling
        // them as screening evidence.
        if receipt.kind == QuotePurposeV1::ResearchObservation {
            continue;
        }
        if let Some(previous) = previous_receipt_ms {
            max_gap_ms = max_gap_ms.max(receipt.at_ms.saturating_sub(previous));
        }
        previous_receipt_ms = Some(receipt.at_ms);
        observations = observations
            .checked_add(receipt.observations.len())
            .ok_or("candidate_execution_tape_too_large")?;
        if observations > max_observations {
            return Err("candidate_execution_tape_too_large".into());
        }
        if receipt.kind == QuotePurposeV1::Protection {
            protection_receipts = protection_receipts.saturating_add(1);
            protection_observations =
                protection_observations.saturating_add(receipt.observations.len());
        } else {
            screen_receipts = screen_receipts.saturating_add(1);
        }
        events.extend_from_slice(
            serde_json::to_string(&json!({
                "at":receipt.at_ms,
                "available_at":receipt.receipt_ms,
                "kind":receipt.kind.as_str(),
                "observations":receipt.observations
            }))
            .map_err(|_| "candidate_execution_tape_invalid".to_string())?
            .as_bytes(),
        );
        events.push(b'\n');
        if events.len() as u64 > max_tape_bytes {
            return Err("candidate_execution_tape_too_large".into());
        }
    }
    if protection_receipts == 0 || protection_observations == 0 {
        return Err("candidate_execution_quotes_unavailable".into());
    }
    let events_sha256 = sadr_core::replay::digest(&events);
    // The archive validator needs a strictly earlier date for this field, but
    // the action engine never uses it. Prefer the caller's verified calendar
    // walkback; otherwise keep a truthful calendar-day placeholder and mark the
    // semantics instead of inventing a holiday-aware session.
    let previous_session_verified = previous_session.is_some();
    let previous_session = previous_session.unwrap_or(
        session
            .pred_opt()
            .ok_or("candidate_execution_tape_invalid")?,
    );
    let manifest = json!({
        "schema":1,
        "session":session,
        "previous_session":previous_session,
        "capital":capital,
        "universe":evidence.instruments,
        "events_sha256":events_sha256,
        "synthetic":false,
        "max_observation_gap_ms":max_gap_ms.max(1),
        "history_directory":null,
        "paper_costs":null,
        "provenance":{
            "source":"original_paper_quote_journal_read_only",
            "research_capital_assumption":true,
            "research_costs_assumption":true,
            "account_funds_claim":false,
            "legacy_quote_purpose_unknown":0,
            "protection_receipts":protection_receipts,
            "screen_receipts":screen_receipts,
            "previous_session_semantics":if previous_session_verified{"verified_calendar_trading_day"}else{"calendar_day_not_verified_trading_day"},
            "max_observation_gap_ms_semantics":"observed_max_receipt_gap_not_an_engine_tolerance"
        }
    });
    let manifest = serde_json::to_vec(&manifest)
        .map_err(|_| "candidate_execution_tape_invalid".to_string())?;
    if manifest.len() > 512 * 1024 {
        return Err("candidate_execution_tape_too_large".into());
    }
    sadr_cli::strategy_archive::validate_archive(&manifest, &events)?;
    Ok(ExecutionTape {
        manifest_sha256: sadr_core::replay::digest(&manifest),
        events_sha256,
        manifest,
        events,
        protection_receipts,
        protection_observations,
        screen_receipts,
    })
}

/// Build the session's own paper export in memory and extract the same feature
/// snapshots as `strategy-lab export-features`. The source book is opened
/// read-only; no manifest, journal or runtime state is created or changed.
pub(crate) fn scorer_snapshots(
    source: &Path,
    session: NaiveDate,
    max_snapshots: usize,
    max_age_ms: i64,
    lookback_samples: usize,
) -> std::result::Result<Vec<sadr_core::strategy::FeatureSnapshot>, String> {
    scorer_snapshots_bounded(
        source,
        session,
        None,
        max_snapshots,
        max_age_ms,
        lookback_samples,
        i64::MAX,
    )
}

/// Candidate freeze path: only quote receipts whose original source time is at
/// or before the submitted host clock enter the dataset. Later ticks cannot
/// mutate the frozen bytes because the caller stores them before admission.
pub(crate) fn scorer_snapshots_bounded(
    source: &Path,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    max_snapshots: usize,
    max_age_ms: i64,
    lookback_samples: usize,
    now: i64,
) -> std::result::Result<Vec<sadr_core::strategy::FeatureSnapshot>, String> {
    if !source.is_absolute()
        || !(1..=10_000).contains(&max_snapshots)
        || !(1..=300_000).contains(&max_age_ms)
        || !(1..=20).contains(&lookback_samples)
        || now < 0
    {
        return Err("scorer_snapshots_unavailable".into());
    }
    let warmup = max_snapshots.saturating_add(lookback_samples).min(10_000);
    let evidence = quote_receipts_bounded(source, session, warmup, now)?;
    let mut events = String::new();
    let mut observations = 0usize;
    let mut max_gap_ms = 0i64;
    let mut previous_receipt_ms = None;
    for receipt in &evidence.receipts {
        // Research-observation receipts are virtual authority only. The
        // historical protection tape skips them entirely instead of relabelling
        // them as screening evidence.
        if receipt.kind == QuotePurposeV1::ResearchObservation {
            continue;
        }
        if let Some(previous) = previous_receipt_ms {
            max_gap_ms = max_gap_ms.max(receipt.at_ms.saturating_sub(previous));
        }
        previous_receipt_ms = Some(receipt.at_ms);
        observations = observations.saturating_add(receipt.observations.len());
        events.push_str(&format!(
            "{}\n",
            json!({"at":receipt.at_ms,"available_at":receipt.receipt_ms,"kind":receipt.kind,"observations":receipt.observations})
        ));
    }
    let previous_session_verified = previous_session.is_some();
    let previous_session =
        previous_session.unwrap_or(session.pred_opt().ok_or("scorer_snapshots_unavailable")?);
    let manifest = json!({
        "schema":1,"session":session,
        "previous_session":previous_session,
        "capital":"5000","universe":evidence.instruments,
        "events_sha256":sadr_core::replay::digest(events.as_bytes()),
        "synthetic":false,"max_observation_gap_ms":max_gap_ms.max(1),"history_directory":null,
        "provenance":{
            "legacy_quote_purpose_unknown":0,
            "source":"session_paper_quotes_read_only",
            "previous_session_semantics":if previous_session_verified{"verified_calendar_trading_day"}else{"calendar_day_not_verified_trading_day"},
            "max_observation_gap_ms_semantics":"observed_max_receipt_gap_not_an_engine_tolerance"
        }
    });
    let config = sadr_cli::strategy_archive::ArchiveConfig {
        max_archive_bytes: (events.len() as u64).clamp(1, 128 * 1024 * 1024),
        max_observations: observations.clamp(1, 1_000_000),
        from_ms: None,
        to_ms: None,
    };
    let mut snapshots = sadr_cli::strategy_archive::export_features_configured(
        &serde_json::to_vec(&manifest).map_err(|_| "scorer_snapshots_invalid".to_string())?,
        events.as_bytes(),
        max_age_ms,
        lookback_samples,
        &config,
    )?;
    snapshots.retain(|snapshot| snapshot.observed_ms <= now);
    if snapshots.len() > max_snapshots {
        snapshots.drain(..snapshots.len() - max_snapshots);
    }
    if snapshots.is_empty() {
        return Err("scorer_snapshots_unavailable".into());
    }
    Ok(snapshots)
}

/// Original-receipt marks for lifecycle maturation. Only quotes whose archived
/// receipt time is inside the requested window are returned; the source clock is
/// the tick observation time. No future or interpolated quote is invented.
pub(crate) fn candidate_quotes(
    source: &Path,
    session: NaiveDate,
    keys: &BTreeSet<String>,
    from_ms: i64,
    to_ms: i64,
    max_rows: usize,
    now: i64,
) -> std::result::Result<Value, String> {
    if keys.is_empty()
        || from_ms < 0
        || to_ms < from_ms
        || now < 0
        || !(1..=10_000).contains(&max_rows)
    {
        return Err("candidate_quotes_invalid".into());
    }
    let evidence = quote_receipts_bounded(source, session, 10_000, now)?;
    let end_of_evidence_ms = evidence
        .receipts
        .last()
        .map(|receipt| receipt.at_ms)
        .unwrap_or(0);
    let mut quotes = Vec::new();
    let mut truncated = false;
    for receipt in &evidence.receipts {
        // Old lifecycle marks are protection/screen evidence only. A research
        // receipt is not a mark and is never substituted for one.
        if receipt.kind == QuotePurposeV1::ResearchObservation {
            continue;
        }
        if receipt.receipt_ms < from_ms || receipt.receipt_ms > to_ms {
            continue;
        }
        for observation in &receipt.observations {
            let Some(key) = observation["key"].as_str() else {
                continue;
            };
            if !keys.contains(key) {
                continue;
            }
            if quotes.len() == max_rows {
                truncated = true;
                break;
            }
            quotes.push(json!({
                "key":key,
                "source_ms":receipt.at_ms,
                "receipt_ms":receipt.receipt_ms,
                "interpolated":false
            }));
        }
        if truncated {
            break;
        }
    }
    let coverage = if end_of_evidence_ms < from_ms {
        "source_ended_before_window"
    } else if quotes.is_empty() {
        "missing_quotes_in_window"
    } else {
        "quotes_present"
    };
    Ok(json!({
        "quotes":quotes,
        "end_of_evidence_ms":end_of_evidence_ms,
        "coverage":coverage,
        "truncated":truncated,
        "interpolated":false
    }))
}

/// One original quote receipt row from the archived journal. `receipt_ms` is
/// the archived financial receipt clock and `event_at_ms` is the journal insert
/// clock; neither is replaced by a later read time. A retention-compacted row
/// keeps its original archive SHA and explicit unknown key/purpose instead of
/// being silently dropped.
#[allow(clippy::too_many_arguments)]
fn candidate_quotes_window_with(
    source: &Path,
    session: NaiveDate,
    keys: &BTreeSet<String>,
    from_ms: i64,
    to_ms: i64,
    max_rows: usize,
    now: i64,
    purposes: &[QuotePurposeV1],
    newest_first: bool,
    immutable: bool,
) -> std::result::Result<Value, String> {
    if keys.is_empty()
        || from_ms < 0
        || to_ms < from_ms
        || now < 0
        || !(1..=10_000).contains(&max_rows)
        || purposes.is_empty()
    {
        return Err("candidate_quotes_invalid".into());
    }
    let raw = crate::read_bounded(&source.join("wide-universe.json"), 8_000_000)
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let universe: Value =
        serde_json::from_slice(&raw).map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let declared: NaiveDate = serde_json::from_value(universe["session"].clone())
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let instruments: Vec<Instrument> = serde_json::from_value(universe["instruments"].clone())
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    if declared != session || instruments.is_empty() || instruments.len() > 10_000 {
        return Err("candidate_quotes_unavailable".into());
    }
    let universe_sha256 = digest(&raw);
    let db = if immutable {
        // A CLOSED predecessor book is read without creating `-wal`/`-shm`
        // sidecars. A non-empty WAL is refused by the immutable opener.
        super::open_book_immutable(&source.join("paper.sqlite"))
            .map_err(|_| "candidate_quotes_unavailable".to_string())?
    } else {
        Connection::open_with_flags(
            source.join("paper.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| "candidate_quotes_unavailable".to_string())?
    };
    db.pragma_update(None, "query_only", true)
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    db.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 2_000_000)
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    // Purpose and financial receipt window are filtered before the bounded row
    // limit. The old reader ordered by insert id and applied the limit first,
    // so a late read could miss the first eligible receipt in the window.
    // Research-observation receipts have their own tick event (#240), so
    // trading retention never compacts them; receipts written before that
    // split stay readable in `wide_quotes`. Trading purposes never read the
    // research event.
    let purpose_filter = purposes
        .iter()
        .map(|purpose| {
            let events = match purpose {
                QuotePurposeV1::ResearchObservation => {
                    format!("'{}','wide_quotes'", super::wide::RESEARCH_QUOTES_EVENT)
                }
                QuotePurposeV1::Protection | QuotePurposeV1::Screen => "'wide_quotes'".into(),
            };
            format!(
                "(event IN ({events}) AND instr(body,'\"purpose\":\"{}\"')>0)",
                purpose.archive_str()
            )
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let order = if newest_first {
        "receipt DESC,id DESC"
    } else {
        "receipt ASC,id ASC"
    };
    let sql = format!(
        "SELECT at,receipt,body FROM ticks WHERE receipt IS NOT NULL AND receipt>0 AND receipt>=?1 AND receipt<=?2 AND ({purpose_filter}) ORDER BY {order} LIMIT ?3"
    );
    let mut statement = db
        .prepare(&sql)
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let rows = statement
        .query_map(
            rusqlite::params![from_ms, to_ms, (max_rows + 1) as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let mut receipt_rows = Vec::new();
    for row in rows {
        receipt_rows.push(row.map_err(|_| "candidate_quotes_unavailable".to_string())?);
    }
    let truncated = receipt_rows.len() > max_rows;
    receipt_rows.truncate(max_rows);
    // A compacted row has no readable body, but its original receipt time and
    // archive SHA are retained so a first-eligible exit can never be assumed.
    // Entry freeze needs the NEWEST compacted receipt (it may hide the latest
    // entry); exit needs the OLDEST (it may hide the first exit). Ordering both
    // ascending would hide a newer compacted entry beyond the row limit.
    let compacted_order = if newest_first {
        "receipt DESC,id DESC"
    } else {
        "receipt ASC,id ASC"
    };
    let compacted_sql = format!(
        "SELECT at,receipt,body FROM ticks WHERE event='wide_quotes' AND receipt IS NOT NULL AND receipt>0 AND receipt>=?1 AND receipt<=?2 AND instr(body,'\"kind\":\"compacted_quote_receipt\"')>0 ORDER BY {compacted_order} LIMIT ?3"
    );
    let mut compacted_statement = db
        .prepare(&compacted_sql)
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let compacted_rows = compacted_statement
        .query_map(rusqlite::params![from_ms, to_ms, max_rows as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let mut compacted = Vec::new();
    for row in compacted_rows {
        let (at, receipt, raw) = row.map_err(|_| "candidate_quotes_unavailable".to_string())?;
        let archive: Value =
            serde_json::from_str(&raw).map_err(|_| "candidate_quotes_invalid".to_string())?;
        compacted.push(json!({
            "key":null,
            "source_ms":at,
            "receipt_ms":receipt,
            "event_at_ms":at,
            "available_at_ms":receipt,
            "purpose":null,
            "evidence_status":"compacted",
            // Only a recorded original archive SHA is provenance. A digest of
            // the compacted body is NOT the original receipt and is never
            // substituted; an unrecorded SHA stays explicitly null.
            "archive_sha256":archive["sha256"].as_str().map(str::to_owned)
        }));
    }
    let mut quotes = Vec::new();
    for (at, receipt, raw) in receipt_rows {
        // The replayable view is bounded by the caller's authoritative clock.
        // A later archived event is NOT consumed and does not poison an
        // otherwise valid earlier receipt: it is simply not yet knowable.
        if at > now {
            continue;
        }
        if receipt > at {
            return Err("candidate_quotes_receipt_binding".into());
        }
        if raw.len() > 2_000_000 {
            return Err("candidate_quotes_unavailable".into());
        }
        let archive: Value =
            serde_json::from_str(&raw).map_err(|_| "candidate_quotes_invalid".to_string())?;
        let purpose = QuotePurposeV1::from_archive(archive["purpose"].as_str())
            .ok_or("candidate_quotes_purpose_unknown")?;
        if !purposes.contains(&purpose) {
            continue;
        }
        if archive["received_ms"] != receipt {
            return Err("candidate_quotes_receipt_binding".into());
        }
        let body: Value = serde_json::from_str(
            archive["body"]
                .as_str()
                .ok_or("candidate_quotes_not_receipt")?,
        )
        .map_err(|_| "candidate_quotes_invalid".to_string())?;
        if body["status"] != "success" {
            return Err("candidate_quotes_status".into());
        }
        let archive_sha256 = digest(raw.as_bytes());
        let rows = body["data"].as_object().ok_or("candidate_quotes_rows")?;
        for row in rows.values() {
            let Some(instrument) = instruments
                .iter()
                .find(|instrument| row["instrument_token"] == instrument.key)
            else {
                continue;
            };
            if !keys.contains(&instrument.key) {
                continue;
            }
            let Ok(quote) = data::parse_row(
                row,
                receipt,
                &instrument.key,
                &instrument.symbol,
                instrument.tick,
            ) else {
                continue;
            };
            let (Some(lower), Some(upper), Some(volume)) = (
                row["lower_circuit_limit"].as_number(),
                row["upper_circuit_limit"].as_number(),
                row["volume"].as_u64(),
            ) else {
                continue;
            };
            quotes.push(json!({
                "key":instrument.key,
                // The original quote source clock comes from the archived body;
                // `at` is the actual completion/event clock, which may be later
                // than the data receipt. They are never conflated.
                "source_ms":quote.source,
                "receipt_ms":receipt,
                "event_at_ms":at,
                "available_at_ms":receipt,
                "bid":quote.bid,
                "ask":quote.ask,
                "bid_units":quote.bid_units,
                "ask_units":quote.ask_units,
                "lower_circuit_limit":lower.to_string(),
                "upper_circuit_limit":upper.to_string(),
                "volume":volume,
                "purpose":purpose.archive_str(),
                "evidence_status":"ok",
                "archive_sha256":archive_sha256,
                "symbol":instrument.symbol,
                "series":instrument.series,
                "tick":instrument.tick.amount().to_string(),
                "universe_sha256":universe_sha256
            }));
        }
    }
    let coverage = if quotes.is_empty() {
        if !compacted.is_empty() {
            "compacted_only"
        } else {
            "missing_quotes_in_window"
        }
    } else {
        "quotes_present"
    };
    Ok(json!({
        "quotes":quotes,
        "compacted":compacted,
        "coverage":coverage,
        "truncated":truncated,
        "interpolated":false,
        "universe_sha256":universe_sha256,
        "session":session.to_string(),
        "window":{"from_ms":from_ms,"to_ms":to_ms},
        "purposes":purposes.iter().map(|purpose| purpose.as_str()).collect::<Vec<_>>()
    }))
}

/// One frozen virtual-observation tape. It is built only from original
/// research-observation or protection receipts; screening receipts are never
/// accepted and no quote is fetched after the freeze.
pub(crate) struct VirtualTape {
    pub manifest: Vec<u8>,
    pub events: Vec<u8>,
    pub manifest_sha256: String,
    pub events_sha256: String,
    pub observations: usize,
    pub receipts: usize,
    pub purpose: QuotePurposeV1,
    /// SHA-256 of the exact original archived receipt body, never a digest of
    /// the re-encoded tape events.
    pub archive_sha256: String,
    pub universe_sha256: String,
    pub receipt_ms: i64,
    pub event_at_ms: i64,
    pub budget_truncated: bool,
    pub coverage_gap: Option<Value>,
}

/// Explicit tape selection mode. Entry freezes the latest eligible original
/// quote per key at or before T0; exit takes the FIRST eligible original
/// receipt in `[due, deadline]`. They are not interchangeable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TapeMode {
    EntryFreeze,
    ExitWindow,
}

/// A typed tape refusal that retains the original receipt provenance instead of
/// collapsing a compacted or budget-truncated source into a missing quote.
#[derive(Debug)]
pub(crate) struct TapeGap {
    pub reason: &'static str,
    pub gap: Value,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn virtual_observation_tape_bounded(
    source: &Path,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    keys: &BTreeSet<String>,
    from_ms: i64,
    to_ms: i64,
    capital: Amount,
    max_rows: usize,
    max_observations: usize,
    max_tape_bytes: u64,
    now: i64,
    mode: TapeMode,
) -> std::result::Result<VirtualTape, TapeGap> {
    virtual_observation_tape_bounded_with(
        source,
        session,
        previous_session,
        keys,
        from_ms,
        to_ms,
        capital,
        max_rows,
        max_observations,
        max_tape_bytes,
        now,
        mode,
        false,
    )
}

/// Predecessor continuation read: the ORIGINAL closed book is opened with
/// `immutable=1`, so no predecessor `-wal`/`-shm` file is created or changed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn virtual_observation_tape_bounded_immutable(
    source: &Path,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    keys: &BTreeSet<String>,
    from_ms: i64,
    to_ms: i64,
    capital: Amount,
    max_rows: usize,
    max_observations: usize,
    max_tape_bytes: u64,
    now: i64,
    mode: TapeMode,
) -> std::result::Result<VirtualTape, TapeGap> {
    virtual_observation_tape_bounded_with(
        source,
        session,
        previous_session,
        keys,
        from_ms,
        to_ms,
        capital,
        max_rows,
        max_observations,
        max_tape_bytes,
        now,
        mode,
        true,
    )
}

#[allow(clippy::too_many_arguments)]
fn virtual_observation_tape_bounded_with(
    source: &Path,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    keys: &BTreeSet<String>,
    from_ms: i64,
    to_ms: i64,
    capital: Amount,
    max_rows: usize,
    max_observations: usize,
    max_tape_bytes: u64,
    now: i64,
    mode: TapeMode,
    immutable: bool,
) -> std::result::Result<VirtualTape, TapeGap> {
    if capital <= Amount::ZERO
        || !(1..=10_000).contains(&max_rows)
        || !(1..=100_000).contains(&max_observations)
        || !(1..=16 * 1024 * 1024).contains(&max_tape_bytes)
    {
        return Err(TapeGap {
            reason: "observation_tape_invalid",
            gap: json!({"kind":"invalid_tape_request"}),
        });
    }
    let purposes: &[QuotePurposeV1] = match mode {
        TapeMode::EntryFreeze => &[QuotePurposeV1::ResearchObservation],
        TapeMode::ExitWindow => &[
            QuotePurposeV1::ResearchObservation,
            QuotePurposeV1::Protection,
        ],
    };
    let full = candidate_quotes_window_with(
        source,
        session,
        keys,
        from_ms,
        to_ms,
        max_rows,
        now,
        purposes,
        mode == TapeMode::EntryFreeze,
        immutable,
    )
    .map_err(|reason| TapeGap {
        reason: "observation_quote_unavailable",
        gap: json!({"kind":"quote_query_unavailable","detail":reason}),
    })?;
    let quotes = full["quotes"].as_array().cloned().unwrap_or_default();
    let compacted = full["compacted"].as_array().cloned().unwrap_or_default();
    let truncated = full["truncated"].as_bool().unwrap_or(false);
    let mut chosen = BTreeMap::<String, Value>::new();
    for quote in &quotes {
        let Some(key) = quote["key"].as_str() else {
            continue;
        };
        if !keys.contains(key) || quote["evidence_status"] != "ok" {
            continue;
        }
        if chosen.contains_key(key) {
            continue;
        }
        chosen.insert(key.to_owned(), quote.clone());
    }
    let missing = keys
        .iter()
        .filter(|key| !chosen.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    if chosen.is_empty() {
        if let Some(first) = compacted.first() {
            return Err(TapeGap {
                reason: "observation_quote_compacted",
                gap: json!({
                    "kind":"compacted_receipt",
                    "session":session.to_string(),
                    "at_ms":first["event_at_ms"],
                    "receipt_ms":first["receipt_ms"],
                    "archive_sha256":first["archive_sha256"],
                    "key":null,
                    "purpose":null,
                    "first_receipt_unknown":true
                }),
            });
        }
        if truncated {
            return Err(TapeGap {
                reason: "observation_coverage_budget_truncated",
                gap: json!({
                    "kind":"coverage_budget_truncated",
                    "session":session.to_string(),
                    "missing_keys":missing,
                    "max_rows":max_rows
                }),
            });
        }
        return Err(TapeGap {
            reason: "observation_quote_unavailable",
            gap: json!({
                "kind":"missing_quote",
                "session":session.to_string(),
                "from_ms":from_ms,
                "to_ms":to_ms
            }),
        });
    }
    // The FIRST eligible exit is only known when no earlier compacted receipt
    // could have occupied the same window. The same rule protects the latest
    // entry freeze from a compacted newer receipt.
    let chosen_receipt = chosen
        .values()
        .filter_map(|quote| quote["receipt_ms"].as_i64())
        .min()
        .unwrap_or(0);
    let first_receipt_unknown = compacted.iter().any(|row| {
        row["receipt_ms"]
            .as_i64()
            .is_some_and(|receipt| match mode {
                // Entry freezes the latest quote; a newer compacted receipt hides it.
                TapeMode::EntryFreeze => receipt >= chosen_receipt,
                // Exit takes the first receipt; an older compacted receipt hides it.
                TapeMode::ExitWindow => receipt <= chosen_receipt,
            })
    });
    if first_receipt_unknown {
        return Err(TapeGap {
            reason: "observation_quote_first_receipt_unknown",
            gap: json!({
                "kind":"first_receipt_unknown",
                "session":session.to_string(),
                "chosen_receipt_ms":chosen_receipt,
                "compacted":compacted,
                "first_receipt_unknown":true
            }),
        });
    }
    let mut observations = Vec::new();
    let mut purpose = QuotePurposeV1::ResearchObservation;
    let mut archive_sha256 = String::new();
    let mut receipt_ms = 0i64;
    let mut event_at_ms = 0i64;
    for (key, quote) in &chosen {
        let quote_purpose = QuotePurposeV1::from_archive(quote["purpose"].as_str())
            .unwrap_or(QuotePurposeV1::ResearchObservation);
        purpose = quote_purpose;
        archive_sha256 = quote["archive_sha256"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        receipt_ms = receipt_ms.max(quote["receipt_ms"].as_i64().unwrap_or(0));
        event_at_ms = event_at_ms.max(quote["event_at_ms"].as_i64().unwrap_or(0));
        observations.push(json!({
            "key":key,
            "quote":{
                "bid":quote["bid"],
                "ask":quote["ask"],
                "bid_units":quote["bid_units"],
                "ask_units":quote["ask_units"],
                "source":quote["source_ms"],
                "receipt":quote["receipt_ms"]
            },
            "lower_circuit_limit":quote["lower_circuit_limit"],
            "upper_circuit_limit":quote["upper_circuit_limit"],
            "volume":quote["volume"],
            // Per-quote ORIGINAL receipt provenance. A multi-key tape must not
            // share one global archive SHA.
            "archive_sha256":quote["archive_sha256"],
            "event_at_ms":quote["event_at_ms"],
            "available_at_ms":quote["available_at_ms"],
            "purpose":quote["purpose"]
        }));
    }
    if observations.is_empty() || observations.len() > max_observations {
        return Err(TapeGap {
            reason: "observation_tape_too_large",
            gap: json!({"kind":"observation_budget","observations":observations.len()}),
        });
    }
    let coverage_gap = if missing.is_empty() {
        None
    } else {
        Some(json!({
            "kind":"coverage_budget_truncated",
            "missing_keys":missing,
            "max_rows":max_rows,
            "truncated":truncated
        }))
    };
    let kind = match purpose {
        QuotePurposeV1::ResearchObservation => "virtual_observation",
        QuotePurposeV1::Protection => "protection",
        QuotePurposeV1::Screen => {
            return Err(TapeGap {
                reason: "observation_tape_invalid",
                gap: json!({"kind":"screen_purpose"}),
            });
        }
    };
    let events = format!(
        "{}\n",
        serde_json::to_string(&json!({
            "at":event_at_ms,
            "available_at":receipt_ms,
            "kind":kind,
            "observations":observations
        }))
        .map_err(|_| TapeGap {
            reason: "observation_tape_invalid",
            gap: json!({"kind":"event_encoding"}),
        })?
    )
    .into_bytes();
    if events.len() as u64 > max_tape_bytes {
        return Err(TapeGap {
            reason: "observation_tape_too_large",
            gap: json!({"kind":"tape_bytes","bytes":events.len()}),
        });
    }
    let universe: Value = serde_json::from_slice(
        &crate::read_bounded(&source.join("wide-universe.json"), 8_000_000).map_err(|_| {
            TapeGap {
                reason: "observation_tape_invalid",
                gap: json!({"kind":"universe_unavailable"}),
            }
        })?,
    )
    .map_err(|_| TapeGap {
        reason: "observation_tape_invalid",
        gap: json!({"kind":"universe_invalid"}),
    })?;
    let instruments: Vec<Instrument> = serde_json::from_value(universe["instruments"].clone())
        .map_err(|_| TapeGap {
            reason: "observation_tape_invalid",
            gap: json!({"kind":"universe_invalid"}),
        })?;
    let universe_sha256 = full["universe_sha256"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let events_sha256 = sadr_core::replay::digest(&events);
    let previous_session = previous_session.unwrap_or(session.pred_opt().ok_or(TapeGap {
        reason: "observation_tape_invalid",
        gap: json!({"kind":"previous_session"}),
    })?);
    let per_key = chosen
        .iter()
        .map(|(key, quote)| {
            (
                key.clone(),
                json!({
                    "archive_sha256":quote["archive_sha256"],
                    "event_at_ms":quote["event_at_ms"],
                    "available_at_ms":quote["available_at_ms"],
                    "source_ms":quote["source_ms"],
                    "receipt_ms":quote["receipt_ms"],
                    "purpose":quote["purpose"],
                    "key":key
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let manifest = serde_json::to_vec(&json!({
        "schema":1,
        "session":session,
        "previous_session":previous_session,
        "capital":capital,
        "universe":instruments,
        "events_sha256":events_sha256,
        "synthetic":false,
        "max_observation_gap_ms":1,
        "history_directory":null,
        "paper_costs":null,
        "provenance":{
            "source":"original_paper_quote_journal_read_only",
            "quote_authority":"research_observation_virtual_only",
            "not_protection":true,
            "research_capital_assumption":true,
            "account_funds_claim":false,
            "purpose":purpose.as_str(),
            "mode":match mode { TapeMode::EntryFreeze => "entry_freeze", TapeMode::ExitWindow => "exit_window" },
            // Whole-tape identity is separate from any one key's original
            // receipt SHA. `archive_sha256` is only set for a single-key tape.
            "archive_sha256":if chosen.len() == 1 { json!(archive_sha256) } else { Value::Null },
            "whole_tape_sha256":digest(&events),
            "per_key":per_key,
            "universe_sha256":universe_sha256,
            "receipt_ms":receipt_ms,
            "budget_truncated":truncated,
            "coverage_gap":coverage_gap
        }
    }))
    .map_err(|_| TapeGap {
        reason: "observation_tape_invalid",
        gap: json!({"kind":"manifest_encoding"}),
    })?;
    if manifest.len() > 512 * 1024 {
        return Err(TapeGap {
            reason: "observation_tape_too_large",
            gap: json!({"kind":"manifest_bytes","bytes":manifest.len()}),
        });
    }
    sadr_cli::strategy_archive::validate_archive(&manifest, &events).map_err(|_| TapeGap {
        reason: "observation_tape_invalid",
        gap: json!({"kind":"archive_invalid"}),
    })?;
    Ok(VirtualTape {
        manifest_sha256: sadr_core::replay::digest(&manifest),
        events_sha256,
        manifest,
        events,
        observations: observations.len(),
        receipts: 1,
        purpose,
        // A multi-key tape has no single original receipt SHA. Never expose
        // the last key's SHA as if it were the tape's receipt.
        archive_sha256: if chosen.len() == 1 {
            archive_sha256
        } else {
            String::new()
        },
        universe_sha256,
        receipt_ms,
        event_at_ms,
        budget_truncated: truncated || coverage_gap.is_some(),
        coverage_gap,
    })
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
fn demo(output: &Path) -> Result<Value> {
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
            "../../../../fixtures/paper-costs.json"
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
        "decision_owner":{"version":1,"owner":"orchestrator"},
        "decide":{"enabled":false,"model_id":"grok-4.7","transport":"direct_oauth","timeout_ms":150000,"run_timeout_ms":450000,"output_cap":8192,"daily_call_budget":500,"budget_unit":"event","max_turns":64,"max_read_tools":256,"minimum_turn_ms":30000},
        "orchestrator":{
            "enabled":true,
            "model":{"model_id":"grok-4.7","transport":"direct_oauth","timeout_ms":120000,"output_cap":8192,"daily_call_budget":200},
            "triggers":{
                "supervisor":{"enabled":true,"prompt":supervisor_prompt},
                "operations":{"enabled":false,"prompt":supervisor_prompt},
                "strategy":{"enabled":false,"prompt":supervisor_prompt}
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
    assert_eq!(
        report["provider_requests"]["decide"].as_u64().unwrap_or(0),
        0
    );
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
    cfg["decide"]["daily_call_budget"] = json!(500);
    rollover::atomic(&dir.path().join("liberal-planner.json"), &cfg).unwrap();
    planner::PlannerConfig::load(&dir.path().join("liberal-planner.json")).unwrap();
}
