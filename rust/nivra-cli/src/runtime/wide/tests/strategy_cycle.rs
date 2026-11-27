use super::super::strategy_run::{Frozen, StrategyRun, recent_actions};
use super::*;
use crate::runtime::strategy_library::{
    Author, Library, acceptance, build_mismatch,
    review::{EntryReview, review_policy_sha256},
};
use crate::runtime::{
    controller_events::{Event as ControllerEvent, Inbox},
    strategy::WorkerConfig,
};
use nivra_core::execution::ExitIntent;
use serde_json::{Value, json};
use std::time::Instant;

const MANIFEST: &str = include_str!("../../../../../../fixtures/strategy/manifest.json");
const EVENTS: &str = include_str!("../../../../../../fixtures/strategy/events.jsonl");
const SOURCE: &[u8] = include_bytes!("../../../../../../fixtures/strategy/strategy.py");
const PARAMETERS: &str = include_str!("../../../../../../fixtures/strategy/parameters.json");

fn worker_config() -> Option<WorkerConfig> {
    let c = WorkerConfig::default();
    c.python_path.is_file().then_some(c)
}

/// Register `source` with a report naming this build, edited by `edit`.
fn register(library: &Library, source: &[u8], params: Value, edit: fn(&mut Value)) -> String {
    let mut report = json!({
        "engine_sha256": nivra_core::replay::engine_fingerprint(),
        "adapter_sha256": crate::runtime::strategy::replay::adapter_sha256(),
        "parameters": {"version": 1, "top_n": 20, "params": params},
    });
    edit(&mut report);
    let report = nivra_cli::canonical_json::bytes(&report).unwrap();
    let source_sha256 = digest(source);
    let accepted = acceptance::Accepted {
        report: &report,
        report_sha256: digest(&report),
        source,
        source_sha256: source_sha256.clone(),
        params,
        summary: json!({"trades": 12, "net_pnl": "41.5"}),
        tape_dates: vec!["2026-11-25".into()],
        thresholds: acceptance::Thresholds::default(),
    };
    let review = EntryReview {
        verdict: "safe".into(),
        model: "fixture".into(),
        source_sha256,
        runner_sha256: crate::runtime::strategy::runner_sha256(),
        policy_sha256: review_policy_sha256(),
        reasons: Vec::new(),
        reviewed_at_ms: 1,
    };
    let author = Author {
        lane: "research".into(),
        model: Some("fixture".into()),
        model_revision: None,
        turn_id: None,
        session: None,
    };
    library
        .register(&accepted, &review, author, 1)
        .unwrap()
        .entry
        .strategy_id
}

/// A fixture-tape book: the controller's quote commit runs held exits on
/// each receipt, and the strategy then sees the depth fresh at that time.
struct Tape {
    _dir: tempfile::TempDir,
    c: Config,
    db: Connection,
    p: Portfolio,
    universe: Vec<Instrument>,
    scan: Scanner,
    events: Vec<Value>,
}
impl Tape {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut c = crate::runtime::tests::config(dir.path());
        c.session = "2026-11-25".parse().unwrap();
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        let mut p = Portfolio::with_capital(a("5000"));
        c.paper_costs = Some(serde_json::from_value(manifest["paper_costs"].clone()).unwrap());
        p.costs = c.paper_costs.clone();
        p.model_led = true;
        let db = open_book(&c).unwrap();
        Self {
            _dir: dir,
            c,
            db,
            p,
            universe: serde_json::from_value(manifest["universe"].clone()).unwrap(),
            scan: Scanner::default(),
            events: EVENTS
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect(),
        }
    }
    fn library(&self) -> Library {
        Library::at(self.c.directory.join("strategy-library"))
    }
    /// Commit receipt `step` and return its time.
    fn protect(&mut self, step: usize) -> i64 {
        let at = self.events[step]["at"].as_i64().unwrap();
        self.protect_at(step, at);
        at
    }

    fn protect_at(&mut self, step: usize, at: i64) {
        let event = &self.events[step];
        let shift = at - event["at"].as_i64().unwrap();
        for o in event["observations"].as_array().unwrap() {
            let key = o["key"].as_str().unwrap().to_owned();
            let mut quote: Quote = serde_json::from_value(o["quote"].clone()).unwrap();
            quote.source += shift;
            quote.receipt += shift;
            self.scan.samples.entry(key).or_default().executable = Some(quote);
        }
        let fresh = self.fresh(at);
        commit_quotes(
            &mut self.db,
            &self.c,
            &mut self.p,
            &fresh,
            at,
            "fixture",
            || at,
        )
        .unwrap();
    }
    fn fresh(&self, at: i64) -> BTreeMap<String, Quote> {
        self.scan
            .samples
            .iter()
            .filter_map(|(key, sample)| {
                let q = sample.executable.as_ref()?.clone();
                q.fresh(at, 2000).then(|| (key.clone(), q))
            })
            .collect()
    }
    fn frozen(&self, at: i64) -> Frozen {
        Frozen {
            at_ms: at,
            prospects: Vec::new(),
            quotes: self.fresh(at),
            portfolio: self.p.clone(),
            universe: self
                .universe
                .iter()
                .map(|i| (i.key.clone(), i.clone()))
                .collect(),
        }
    }
    /// One scan: wait for startup/hot-swap if settling, start a call, wait and apply.
    fn scan(&mut self, run: &mut StrategyRun, at: i64) {
        if run.settle_startup(&self.db, &self.c, at).unwrap().is_some() {
            run.start(self.frozen(at), &|| at);
            run.wait();
        }
        run.poll(&mut self.db, &self.c, &mut self.p, &mut self.scan, &|| at)
            .unwrap();
    }
    fn failures(&self) -> Vec<String> {
        self.db
            .prepare("SELECT body FROM ticks WHERE event='strategy_failure' ORDER BY rowid")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|b| serde_json::from_str::<Value>(&b.unwrap()).unwrap()["reason"].to_string())
            .collect()
    }
}

#[test]
fn active_strategy_fills_refuses_and_exits_through_the_gate() {
    let Some(worker) = worker_config() else {
        return;
    };
    let mut tape = Tape::new();
    let params: Value = serde_json::from_str::<Value>(PARAMETERS).unwrap()["params"].clone();
    let id = register(&tape.library(), SOURCE, params, |_| {});
    let mut run = StrategyRun::new(tape.library(), worker, Default::default(), None);
    for step in 0..tape.events.len() {
        let at = tape.protect(step);
        tape.scan(&mut run, at);
    }
    // The same fills as the after-cost replay: two strategy entries, the
    // revised target and the stop executed by protection.
    let fills: Vec<_> = tape
        .p
        .fills
        .iter()
        .map(|f| {
            (
                f.buy,
                f.units,
                f.price.amount().to_string(),
                f.fee.to_string(),
            )
        })
        .collect();
    let row = |buy, units, price: &str, fee: &str| (buy, units, price.into(), fee.into());
    assert_eq!(
        fills,
        [
            row(true, 10, "100", "1"),
            row(false, 10, "100.45", "1.01"),
            row(true, 9, "100.5", "0.91"),
            row(false, 9, "99.95", "0.9"),
        ]
    );
    assert!(
        tape.p
            .fills
            .iter()
            .all(|f| f.decision_id.as_deref() == Some(id.as_str()))
    );
    assert!(tape.p.positions.is_empty() && !tape.p.halted);
    let outcomes: Vec<_> = recent_actions(&tape.db)
        .unwrap()
        .iter()
        .map(|a| a["outcome"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        outcomes,
        [
            "action_sell_unheld",
            "filled",
            "exit_revised",
            "price_above_max",
            "filled"
        ]
    );
    // The gate refusal reaches the decision brief with its numbers.
    let start = tape.events[0]["at"].as_i64().unwrap();
    let refusals = recent_refusals(&tape.db, 5).unwrap();
    assert_eq!(refusals.len(), 1);
    assert_eq!(
        refusals[0]["source"],
        json!({"kind":"strategy","id":format!("{id}@{start}"),"index":1})
    );
    assert_eq!(refusals[0]["reason"], "price_above_max");
    assert_eq!(
        refusals[0]["facts"],
        json!({"ask":"50","price_with_costs":"50","max_price":"49.95"})
    );
    // The book row holds the same state the controller holds.
    assert_eq!(load_wide(&mut tape.db, &tape.c).unwrap(), tape.p);
    let status = run.status(&tape.db, &tape.p);
    assert_eq!(status["strategy"]["strategy_id"], id.as_str());
    assert_eq!(status["fills"].as_array().unwrap().len(), 4);
    assert_eq!(status["calls"], 6);
    let latency = &status["latency"];
    assert_eq!(latency["calls"], 6);
    assert_eq!(latency["submit_to_reply"]["samples"], 6);
    assert_eq!(latency["quote_source_to_gate"]["samples"], 3);
    assert_eq!(latency["book_lock_to_gate"]["p99_ms"], 0);
    assert!(tape.failures().is_empty());
}

#[test]
fn worker_reply_applies_fixture_on_one_notification() {
    let worker = worker_config().expect("fixture requires python3");
    let mut tape = Tape::new();
    let start_ms = millis() + 200;
    let mut params = serde_json::from_str::<Value>(PARAMETERS).unwrap()["params"].clone();
    params["start_ms"] = json!(start_ms);
    let library = tape.library();
    register(&library, SOURCE, params, |_| {});
    let inbox = Inbox::new();
    let mut run = StrategyRun::new(library, worker, Default::default(), Some(inbox.notify()));
    run.ready(&tape.db, &tape.c, millis()).unwrap();
    assert!(
        inbox
            .wait(Duration::from_secs(3))
            .contains(ControllerEvent::Strategy)
    );
    assert!(run.ready(&tape.db, &tape.c, millis()).unwrap().is_some());

    thread::sleep(Duration::from_millis(
        start_ms.saturating_sub(millis()).max(0) as u64,
    ));
    let at = millis();
    tape.c.session = local(at).date_naive();
    tape.p.settings.session_start_minute = 0;
    tape.p.settings.session_end_minute = 1440;
    tape.protect_at(0, at);
    run.start(tape.frozen(at), &millis);
    assert!(
        inbox
            .wait(Duration::from_secs(1))
            .contains(ControllerEvent::Strategy)
    );
    run.poll(&mut tape.db, &tape.c, &mut tape.p, &mut tape.scan, &millis)
        .unwrap();

    let actions = recent_actions(&tape.db).unwrap();
    assert_eq!(actions[0]["outcome"], "price_above_max");
    assert_eq!(actions[1]["outcome"], "filled");
    assert_eq!(tape.p.fills.len(), 1);
    let status = run.status(&tape.db, &tape.p);
    assert_eq!(status["failures"], 0);
    let latency = &status["latency"];
    assert_eq!(latency["submit_to_reply"]["samples"], 1);
    assert_eq!(latency["reply_to_gate"]["samples"], 2);
    assert!(latency["reply_to_gate"]["p99_ms"].as_i64().unwrap() < 1000);
    eprintln!("fixture_strategy_latency {latency}");
}

#[test]
fn failed_strategy_calls_trade_nothing_and_protection_continues() {
    let Some(worker) = worker_config() else {
        return;
    };
    let mut tape = Tape::new();
    let at = tape.protect(0);
    // A held position with a stop at 100.00, as an earlier fill left it.
    let a = tape.universe[0].clone();
    let exit = ExitIntent {
        stop: Some(Price::parse("100.00").unwrap()),
        ..ExitIntent::default()
    };
    let q = tape.fresh(at)[&a.key].clone();
    let max = Price::parse("100.00").unwrap();
    tape.p
        .execute_plan(at, &a, &q, true, 10, max, exit, "seed")
        .unwrap()
        .unwrap();
    register(
        &tape.library(),
        b"def on_observation(obs):\n    return {'actions': []}\nraise SystemExit(3)\n",
        json!({}),
        |_| {},
    );
    let mut crash = StrategyRun::new(tape.library(), worker.clone(), Default::default(), None);
    tape.scan(&mut crash, at);
    for step in 1..3 {
        let at = tape.protect(step);
        tape.scan(&mut crash, at);
    }
    // One journal line and one diagnostics line for a repeating crash.
    assert_eq!(tape.failures(), ["\"strategy_exit\""]);
    assert_eq!(crash.status(&tape.db, &tape.p)["failures"], 3);
    let diagnostics = fs::read_to_string(tape.c.directory.join(diagnostics::FILE)).unwrap();
    assert_eq!(diagnostics.matches("\"stage\":\"strategy\"").count(), 1);
    // A call past the deadline: the next scan is skipped, not queued, and
    // the stop still executes while the call runs.
    let slow = WorkerConfig {
        call_deadline_ms: 600,
        ..worker
    };
    let library = Library::at(tape.c.directory.join("slow-library"));
    register(
        &library,
        b"def on_observation(obs):\n    while True:\n        pass\n",
        json!({}),
        |_| {},
    );
    let inbox = Inbox::new();
    let mut timeout = StrategyRun::new(library, slow, Default::default(), Some(inbox.notify()));
    let at = tape.protect(3);
    assert!(
        timeout
            .settle_startup(&tape.db, &tape.c, at)
            .unwrap()
            .is_some()
    );
    assert!(
        inbox
            .wait(Duration::ZERO)
            .contains(ControllerEvent::Strategy)
    );
    timeout.start(tape.frozen(at), &|| at);
    assert_eq!(timeout.ready(&tape.db, &tape.c, at).unwrap(), None);
    let due = Instant::now() + Duration::from_millis(40);
    let wait = due.saturating_duration_since(Instant::now());
    assert!(!inbox.wait(wait).contains(ControllerEvent::Strategy));
    assert!(Instant::now() >= due);
    let at = tape.protect(4);
    assert!(
        tape.p.positions.is_empty(),
        "stop executes at the protection deadline"
    );
    assert!(timeout.has_call());
    assert!(
        inbox
            .wait(Duration::from_secs(3))
            .contains(ControllerEvent::Strategy)
    );
    timeout
        .poll(&mut tape.db, &tape.c, &mut tape.p, &mut tape.scan, &|| at)
        .unwrap();
    assert_eq!(timeout.status(&tape.db, &tape.p)["skipped"], 1);
    assert_eq!(
        tape.failures(),
        ["\"strategy_exit\"", "\"strategy_wall_timeout\""]
    );
    // An entry accepted on another engine never runs here.
    let library = Library::at(tape.c.directory.join("old-library"));
    register(&library, SOURCE, json!({}), |r| {
        r["engine_sha256"] = json!("0".repeat(64))
    });
    let mut old = StrategyRun::new(library, WorkerConfig::default(), Default::default(), None);
    assert_eq!(old.ready(&tape.db, &tape.c, at).unwrap(), None);
    assert_eq!(
        old.status(&tape.db, &tape.p)["reason"],
        "strategy_engine_changed"
    );
    let mut report = json!({"engine_sha256": nivra_core::replay::engine_fingerprint()});
    assert_eq!(build_mismatch(&report), Some("strategy_adapter_changed"));
    report["adapter_sha256"] = json!(crate::runtime::strategy::replay::adapter_sha256());
    assert_eq!(build_mismatch(&report), None);
    // Only the seed buy and the protective stop traded; the book runs on.
    assert_eq!(tape.p.fills.len(), 2);
    assert!(!tape.p.fills[1].buy && !tape.p.halted && !tape.p.stopped);
    assert!(recent_actions(&tape.db).unwrap().is_empty());
}

#[test]
fn a_fallback_runs_labelled_until_a_strategy_is_accepted_and_honours_stand_down() {
    let Some(worker) = worker_config() else {
        return;
    };
    let mut tape = Tape::new();
    let policy = crate::runtime::strategy_library::choice::ChoicePolicy {
        fallback: vec!["vwap_trend".into()],
        policy_sha256: String::new(),
        ..Default::default()
    };
    let mut run = StrategyRun::new(tape.library(), worker, policy.clone(), None);
    let first = tape.protect(0);
    tape.scan(&mut run, first);
    let status = run.status(&tape.db, &tape.p);
    assert_eq!(status["state"], "ready", "{status}");
    assert_eq!(status["reason"], "no_active_strategy");
    assert_eq!(status["strategy"]["basis"], "fallback");
    assert_eq!(status["strategy"]["strategy_id"], "fallback:vwap_trend");
    assert_eq!(status["calls"], 1);
    let gen1 = status["activation_generation"].as_u64().unwrap();
    assert!(gen1 >= 1);
    assert!(tape.failures().is_empty(), "{:?}", tape.failures());
    // An accepted version takes over via non-blocking hot swap at the next library read.
    let params: Value = serde_json::from_str::<Value>(PARAMETERS).unwrap()["params"].clone();
    let id = register(&tape.library(), SOURCE, params, |_| {});
    for step in 1..tape.events.len() {
        let at = tape.protect(step) + 30_000;
        tape.scan(&mut run, at.max(first + 30_000));
    }
    let status = run.status(&tape.db, &tape.p);
    assert_eq!(status["strategy"]["basis"], "accepted", "{status}");
    assert_eq!(status["strategy"]["strategy_id"], id.as_str());
    assert_eq!(status["reason"], Value::Null);
    let gen2 = status["activation_generation"].as_u64().unwrap();
    assert!(gen2 > gen1, "hot swap must bump activation_generation");
    // Stand-down revokes the live worker and bumps activation_generation immediately.
    tape.library()
        .select_by_lane(None, &policy, "stand down", None, first + 65_000)
        .unwrap();
    assert_eq!(
        run.settle_startup(&tape.db, &tape.c, first + 65_000)
            .unwrap(),
        None
    );
    let status = run.status(&tape.db, &tape.p);
    assert!(
        run.host().live_key().is_none(),
        "stand-down revokes live worker"
    );
    assert_eq!(status["reason"], "strategy_lane_stood_down");
    assert!(status["activation_generation"].as_u64().unwrap() > gen2);
}
