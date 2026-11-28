use super::super::strategy_run::{Frozen, HealthBasis, StrategyRun, recent_actions};
use super::*;
use crate::runtime::strategy_library::{
    Author, Control, ControlRecord, Library, acceptance, build_mismatch,
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
        // The fixture's hand-checked fills assume its own sizing policy.
        let sizing = &serde_json::from_str::<Value>(PARAMETERS).unwrap()["sizing"];
        c.sleeves.cash = serde_json::from_value(
            serde_json::json!({"capital": "5000"})
                .as_object()
                .unwrap()
                .clone()
                .into_iter()
                .chain(sizing.as_object().unwrap().clone())
                .collect(),
        )
        .unwrap();
        p.sizing = Some(c.sleeves.policies());
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
            let sample = self.scan.samples.entry(key).or_default();
            sample.current = Some(quote.bid);
            sample.seen = quote.receipt;
            sample.source = Some(quote.source);
            sample.executable = Some(quote);
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
        use crate::runtime::strategy::selection::{Selection, executable};
        let mut selection = Selection::default();
        let index: BTreeMap<_, _> = self.universe.iter().map(|i| (i.key.as_str(), i)).collect();
        selection.refresh(
            &self.scan,
            at,
            None,
            &self.p.positions.keys().cloned().collect(),
            index.keys().copied(),
            20,
        );
        Frozen::capture(
            &self.scan,
            at,
            20,
            &self.p,
            &selection,
            |key| index.get(key).copied(),
            |key| executable(&self.scan, key, at).is_some(),
        )
    }
    /// One scan: wait for startup/hot-swap if settling, start a call, wait and apply.
    fn scan(&mut self, run: &mut StrategyRun, at: i64) {
        if run.settle_startup(&self.db, &self.c, at).unwrap().is_some() {
            run.start(self.frozen(at), &self.p, &|| at);
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
    let id = register(&tape.library(), SOURCE, params, |report| {
        report["capital"] = json!("5000");
    });
    let mut run = StrategyRun::new(tape.library(), worker, Default::default(), None);
    let start = tape.events[0]["at"].as_i64().unwrap();
    let session = start..start + 375 * 60_000;
    for step in 0..tape.events.len() {
        let at = tape.protect(step);
        tape.scan(&mut run, at);
        run.health(&tape.p, at, session.clone());
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
    let health = run.health(&tape.p, start + 187 * 60_000 + 30_000, session);
    assert_eq!(health.session_net_pnl, Some(a("-4.27")));
    assert_eq!(health.replay_expected_bps, Some(41.5));
    assert!(health.cash_drawdown_bps.unwrap() > 0.0);
    assert_eq!(health.applied_actions, 3);
    assert_eq!(health.recent_actions, 5);
    assert_eq!(health.completed_calls, 6);
    assert_eq!(health.failed_calls, 0);
    assert_eq!(health.refusals_by_code["action_sell_unheld"], 1);
    assert_eq!(health.refusals_by_code["price_above_max"], 1);
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
    let inbox = Inbox::new();
    let policy = crate::runtime::strategy_library::choice::ChoicePolicy {
        fallback: vec!["vwap_trend".into()],
        ..Default::default()
    };
    let mut run = StrategyRun::new(tape.library(), worker, policy, Some(inbox.notify()));
    run.ready(&tape.db, &tape.c, millis()).unwrap();
    assert!(
        inbox
            .wait(Duration::from_secs(3))
            .contains(ControllerEvent::Strategy)
    );
    assert!(run.ready(&tape.db, &tape.c, millis()).unwrap().is_some());

    let start_ms = millis();
    let mut params = serde_json::from_str::<Value>(PARAMETERS).unwrap()["params"].clone();
    params["start_ms"] = json!(start_ms);
    register(&tape.library(), SOURCE, params, |_| {});
    let switch_at = millis();
    run.refresh_choice(&tape.db, &tape.c, switch_at, true)
        .unwrap();
    assert!(
        inbox
            .wait(Duration::from_secs(3))
            .contains(ControllerEvent::Strategy)
    );

    let at = millis();
    tape.c.session = local(at).date_naive();
    tape.p.settings.session_start_minute = 0;
    tape.p.settings.session_end_minute = 1440;
    tape.protect_at(0, at);
    run.request_observation();
    assert!(
        run.take_observation(&tape.db, &tape.c, at)
            .unwrap()
            .is_some()
    );
    run.start(tape.frozen(at), &tape.p, &millis);
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
    assert_eq!(latency["switch_to_first_call"]["samples"], 1);
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
    // The readiness event comes from the worker's I/O thread, so it may
    // trail the readiness check slightly; the bound is generous.
    assert!(
        inbox
            .wait(Duration::from_secs(5))
            .contains(ControllerEvent::Strategy)
    );
    timeout.start(tape.frozen(at), &tape.p, &|| at);
    timeout.request_observation();
    timeout.request_observation();
    assert_eq!(
        timeout.take_observation(&tape.db, &tape.c, at).unwrap(),
        None
    );
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
    let health = timeout.health(&tape.p, at, at..at + 60_000);
    assert_eq!((health.completed_calls, health.failed_calls), (1, 1));
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
    // A library notification brings the accepted worker in before the cache expires.
    let params: Value = serde_json::from_str::<Value>(PARAMETERS).unwrap()["params"].clone();
    let id = register(&tape.library(), SOURCE, params, |_| {});
    let switched = tape.protect(1);
    assert!(switched - first < 30_000);
    run.refresh_choice(&tape.db, &tape.c, switched, true)
        .unwrap();
    let first_call = tape.protect(2);
    tape.scan(&mut run, first_call);
    let timing = run.status(&tape.db, &tape.p);
    assert_eq!(timing["latency"]["switch_to_first_call"]["p50_ms"], 0);
    assert_eq!(
        timing["latency"]["switch_to_first_call"]["p99_ms"],
        first_call - switched
    );
    let status = run.status(&tape.db, &tape.p);
    assert_eq!(status["strategy"]["basis"], "accepted", "{status}");
    assert_eq!(status["strategy"]["strategy_id"], id.as_str());
    assert_eq!(status["reason"], Value::Null);
    let gen2 = status["activation_generation"].as_u64().unwrap();
    assert!(gen2 > gen1, "hot swap must bump activation_generation");
    // Stand-down revokes the live worker and bumps activation_generation immediately.
    let revoked = tape.protect(3);
    tape.library()
        .select_by_lane(None, &policy, "stand down", None, revoked)
        .unwrap();
    run.refresh_choice(&tape.db, &tape.c, revoked, true)
        .unwrap();
    assert_eq!(
        run.settle_startup(&tape.db, &tape.c, revoked).unwrap(),
        None
    );
    let status = run.status(&tape.db, &tape.p);
    assert!(
        run.host().live_key().is_none(),
        "stand-down revokes live worker"
    );
    assert_eq!(status["reason"], "strategy_lane_stood_down");
    assert!(status["activation_generation"].as_u64().unwrap() > gen2);
    let session = first..first + 60_000;
    let health = run.health(&tape.p, revoked, session.clone());
    assert_eq!(health.active_basis, HealthBasis::StoodDown);
    assert_eq!(health.eligible_strategy_id.as_deref(), Some(id.as_str()));

    let stale = register(&tape.library(), SOURCE, json!({"version": 2}), |report| {
        report["engine_sha256"] = json!("0".repeat(64));
    });
    tape.library()
        .append(ControlRecord {
            at_ms: revoked + 1,
            control: Control::Pin { strategy_id: stale },
        })
        .unwrap();
    run.refresh_choice(&tape.db, &tape.c, revoked + 1, true)
        .unwrap();
    let health = run.health(&tape.p, revoked + 1, session);
    assert_eq!(health.active_basis, HealthBasis::Blocked);
    assert_eq!(health.blocked_reason, Some("strategy_engine_changed"));
    assert_eq!(health.eligible_strategy_id.as_deref(), Some(id.as_str()));
}

#[test]
fn idle_health_needs_fresh_executable_prospects_and_resets_after_a_gap() {
    let Some(worker) = worker_config() else {
        return;
    };
    let mut tape = Tape::new();
    let source = b"def on_observation(obs):\n    return {'actions': []}\n";
    register(&tape.library(), source, json!({}), |_| {});
    let mut run = StrategyRun::new(tape.library(), worker, Default::default(), None);
    let start = tape.protect(0);
    let session = start..start + 375 * 60_000;
    for minute in 0..=15 {
        let at = start + minute * 60_000;
        tape.protect_at(0, at);
        tape.scan(&mut run, at);
    }
    let at = start + 15 * 60_000;
    let health = run.health(&tape.p, at, session.clone());
    assert!(health.fresh_prospects);
    assert_eq!(health.idle_minutes, 15);
    assert_eq!(health.applied_actions, 0);
    assert!(
        !run.health(&tape.p, at + 2_001, session.clone())
            .fresh_prospects
    );

    let after_gap = at + 60_001;
    tape.protect_at(0, after_gap);
    tape.scan(&mut run, after_gap);
    let health = run.health(&tape.p, after_gap, session);
    assert!(health.fresh_prospects);
    assert_eq!(health.idle_minutes, 0);
}

#[test]
fn busy_updates_request_one_call_with_the_latest_receipt() {
    let mut tape = Tape::new();
    let library = tape.library();
    let params: Value = serde_json::from_str::<Value>(PARAMETERS).unwrap()["params"].clone();
    register(&library, SOURCE, params, |_| {});
    let worker = worker_config().expect("fixture requires python3");
    let mut run = StrategyRun::new(library, worker, Default::default(), None);
    let first = tape.protect(0);
    assert!(
        run.settle_startup(&tape.db, &tape.c, first)
            .unwrap()
            .is_some()
    );
    run.request_observation();
    assert!(
        run.take_observation(&tape.db, &tape.c, first)
            .unwrap()
            .is_some()
    );
    run.start(tape.frozen(first), &tape.p, &|| first);
    for step in [1, 3] {
        let now = tape.protect(step);
        run.request_observation();
        assert_eq!(run.take_observation(&tape.db, &tape.c, now).unwrap(), None);
    }

    let latest = tape.events[3]["at"].as_i64().unwrap();
    run.wait();
    run.poll(&mut tape.db, &tape.c, &mut tape.p, &mut tape.scan, &|| {
        latest
    })
    .unwrap();
    assert!(
        tape.p.fills.is_empty(),
        "the first observation was superseded"
    );

    assert!(
        run.take_observation(&tape.db, &tape.c, latest)
            .unwrap()
            .is_some()
    );
    run.start(tape.frozen(latest), &tape.p, &|| latest);
    run.wait();
    run.poll(&mut tape.db, &tape.c, &mut tape.p, &mut tape.scan, &|| {
        latest
    })
    .unwrap();
    assert_eq!(
        run.take_observation(&tape.db, &tape.c, latest).unwrap(),
        None
    );
    assert_eq!(tape.p.fills.len(), 1);
    assert_eq!(tape.p.fills[0].price, Price::parse("100.50").unwrap());
    let status = run.status(&tape.db, &tape.p);
    assert_eq!(status["calls"], 2);
    assert_eq!(status["skipped"], 1);
}

#[test]
fn a_published_stand_down_fences_a_reply_before_notification_dispatch() {
    let mut tape = Tape::new();
    let library = tape.library();
    let policy = crate::runtime::strategy_library::choice::ChoicePolicy::default();
    let params: Value = serde_json::from_str::<Value>(PARAMETERS).unwrap()["params"].clone();
    register(&library, SOURCE, params, |_| {});
    let worker = worker_config().expect("fixture requires python3");
    let mut run = StrategyRun::new(tape.library(), worker, policy.clone(), None);
    let now = tape.protect(0);
    assert!(
        run.settle_startup(&tape.db, &tape.c, now)
            .unwrap()
            .is_some()
    );
    run.start(tape.frozen(now), &tape.p, &|| now);
    run.wait();
    library
        .select_by_lane(None, &policy, "stand down", None, now)
        .unwrap();
    run.poll(&mut tape.db, &tape.c, &mut tape.p, &mut tape.scan, &|| now)
        .unwrap();
    assert!(tape.p.fills.is_empty());
    assert!(recent_actions(&tape.db).unwrap().is_empty());
    assert_eq!(tape.failures(), ["\"strategy_activation_stale\""]);
}

#[test]
fn switched_strategy_cannot_change_inherited_holdings_until_protection_flattens_them() {
    use crate::runtime::strategy;
    let mut tape = Tape::new();
    let at = tape.protect(0);
    let key = "NSE_EQ|TESTA";
    let mut quotes = tape.fresh(at);
    let origin = "fallback:old";
    let buy = json!({"instrument_key":key,"side":"buy","max_price":"100",
        "rationale":"fixture","exit_intent":{"target":"100.50","stop":"99.50"}});
    let results = strategy::apply(
        &mut tape.p,
        &|| at,
        &mut quotes,
        &tape.universe,
        vec![buy.clone()],
        origin,
    )
    .unwrap();
    assert_eq!(results[0].outcome, "filled");
    let mut restarted: Portfolio =
        serde_json::from_value(serde_json::to_value(&tape.p).unwrap()).unwrap();
    restarted.sizing = tape.p.sizing.clone();
    let before = restarted.clone();
    let actions = vec![
        buy,
        json!({"instrument_key":key,"side":"sell","units":1,"max_price":"99.95","rationale":"fixture"}),
        json!({"instrument_key":key,"side":"revise_exit","exit_intent":{"target":"110"},"rationale":"fixture"}),
    ];
    for action in actions {
        let results = strategy::apply(
            &mut restarted,
            &|| at,
            &mut quotes,
            &tape.universe,
            vec![action],
            "fallback:new",
        )
        .unwrap();
        assert_eq!(results[0].outcome, "strategy_position_inherited");
    }
    assert_eq!(restarted, before);
    let quote = quotes.get_mut(key).unwrap();
    quote.bid = Price::parse("100.50").unwrap();
    quote.ask = quote.bid;
    restarted.protect(at, &quotes).unwrap();
    assert!(restarted.positions.is_empty());
    assert_eq!(
        restarted.fills.last().unwrap().decision_id.as_deref(),
        Some(origin)
    );
}

struct ShadowFixture {
    tape: Tape,
    run: StrategyRun,
    candidate: String,
    library: Library,
    clock: std::sync::Arc<std::sync::atomic::AtomicI64>,
    setup: super::super::strategy_run::shadow::Setup,
}

impl ShadowFixture {
    fn new(worker: WorkerConfig, source: &[u8]) -> Self {
        use crate::runtime::strategy_library::{Control, ControlRecord};
        let tape = Tape::new();
        let root = crate::runtime::strategy_library::root(&tape.c).unwrap();
        let library = Library::at(root.clone());
        let params = serde_json::from_str::<Value>(PARAMETERS).unwrap()["params"].clone();
        let live = register(&library, SOURCE, params.clone(), |_| {});
        let candidate = register(&library, source, params, |_| {});
        library
            .append(ControlRecord {
                at_ms: 1,
                control: Control::Pin { strategy_id: live },
            })
            .unwrap();
        let first = tape.events[0]["at"].as_i64().unwrap();
        let setup = super::super::strategy_run::shadow::Setup {
            config: tape.c.clone(),
            choice: Default::default(),
            worker: worker.clone(),
            settings: tape.p.settings.clone(),
            sessions: 1,
            open_ms: first - 5 * 60_000,
            end_ms: first + 300_000,
            notify: None,
        };
        let mut run = StrategyRun::new(Library::at(root), worker, Default::default(), None);
        let clock = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(setup.open_ms - 1));
        let time = clock.clone();
        run.host_shadow_fixture(
            super::super::strategy_run::shadow::ShadowSlot::start_with_clock(
                setup.clone(),
                move || time.load(std::sync::atomic::Ordering::Relaxed),
            )
            .unwrap(),
        );
        wait_for(|| run.shadow_slot().is_some());
        run.settle_startup(&tape.db, &tape.c, setup.open_ms - 1)
            .unwrap();
        wait_for(|| run.shadow_slot().is_some_and(|slot| slot.ready()));
        assert_eq!(run.host().pool_len(), 2);
        Self {
            tape,
            run,
            candidate,
            library,
            clock,
            setup,
        }
    }

    fn set_clock(&self, at: i64) {
        self.clock.store(at, std::sync::atomic::Ordering::Relaxed);
    }

    fn input(&mut self, step: usize) -> i64 {
        let at = self.tape.events[step]["at"].as_i64().unwrap();
        self.input_at(step, at);
        at
    }

    /// Replay fixture event `step` as if its receipts arrived at `at`.
    fn input_at(&mut self, step: usize, at: i64) {
        use super::super::strategy_run::Capture;
        self.set_clock(at);
        self.tape.protect_at(step, at);
        self.run
            .shadow_slot()
            .unwrap()
            .receipts(self.tape.fresh(at));
        self.run.start_shared(
            self.tape.frozen(at),
            &self.tape.p,
            Capture {
                top_n: 20,
                live: true,
                shadow: true,
            },
            &|| at,
        );
    }

    fn settle(&mut self, at: i64) {
        self.run.wait();
        self.run
            .poll(
                &mut self.tape.db,
                &self.tape.c,
                &mut self.tape.p,
                &mut self.tape.scan,
                &|| at,
            )
            .unwrap();
    }

    fn wait_shadow(&self) {
        wait_for(|| self.run.shadow_slot().is_some_and(|slot| !slot.busy()));
    }

    fn status(&self) -> Value {
        super::super::strategy_run::shadow::status(&self.tape.c, Some(&self.candidate)).unwrap()
    }
}

fn wait_for(mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < until, "strategy fixture timed out");
        std::thread::sleep(Duration::from_millis(3));
    }
}

#[test]
fn shadow_and_live_reconcile_identical_stream_and_seal_one_session() {
    let Some(worker) = worker_config() else {
        return;
    };
    let mut source = SOURCE.to_vec();
    source.extend_from_slice(b"\n# Independent version of the fixture.\n");
    let mut f = ShadowFixture::new(worker, &source);
    for step in 0..f.tape.events.len() {
        let at = f.input(step);
        f.settle(at);
        f.wait_shadow();
    }
    assert!(f.status()["summary"]["shadow"].is_null());
    f.set_clock(f.setup.end_ms);
    f.run.finish_shadow(&f.tape.db, &f.tape.p);
    wait_for(|| f.status()["state"] == "completed");
    let report = f.status();
    let evidence = &report["summary"];
    assert_eq!(evidence["coverage_bps"], 10_000);
    assert_eq!(evidence["paired_scans"], f.tape.events.len());
    assert_eq!(evidence["comparable"], true);
    assert_eq!(
        evidence["shadow"],
        evidence["live"]
            .as_object()
            .map(|live| {
                let mut live = live.clone();
                live.remove("strategy_id");
                live.remove("stable");
                live.remove("scans");
                Value::Object(live)
            })
            .unwrap()
    );
    let store = crate::runtime::autoresearch::open(&f.tape.c).unwrap();
    let window = store.shadow_window(Some(&f.candidate)).unwrap().unwrap();
    let ledger: Value = serde_json::from_slice(
        &store
            .shadow_ledger(window.id, f.tape.c.session)
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let mut shadow: Portfolio = serde_json::from_value(ledger["book"].clone()).unwrap();
    shadow.reconcile().unwrap();
    assert_eq!(shadow.fills.len(), 4);
    for fill in &mut shadow.fills {
        // The books attribute fills to their own source version.
        fill.decision_id.clone_from(&f.tape.p.fills[0].decision_id);
    }
    assert_eq!(shadow.fills, f.tape.p.fills);
    assert_eq!(shadow.cash, f.tape.p.cash);
    assert_eq!(f.library.entries().unwrap().len(), 2);
    assert_eq!(window.plan.sessions, vec![f.tape.c.session]);
    use crate::runtime::strategy_library::{Control, ControlRecord};
    f.library
        .append(ControlRecord {
            at_ms: f.setup.end_ms,
            control: Control::Unpin,
        })
        .unwrap();
    let chosen = f
        .library
        .select_by_lane(
            Some(&f.candidate),
            &f.setup.choice,
            "Matched forward fixture",
            None,
            f.setup.end_ms,
        )
        .unwrap();
    assert_eq!(chosen.active.as_deref(), Some(f.candidate.as_str()));
}

#[test]
fn slow_shadow_skips_a_scan_and_disable_leaves_live_protection_running() {
    use crate::runtime::strategy_library::{Control, ControlRecord};
    let Some(worker) = worker_config() else {
        return;
    };
    let slow = b"def on_observation(obs):\n    for i in range(100000000):\n        pass\n    return {'actions': []}\n";
    let mut f = ShadowFixture::new(worker, slow);
    let first = f.input(0);
    f.settle(first);
    assert!(f.run.shadow_slot().unwrap().busy());
    let second = f.input(1);
    f.settle(second);
    f.library
        .append(ControlRecord {
            at_ms: second,
            control: Control::Disable {
                strategy_id: f.candidate.clone(),
            },
        })
        .unwrap();
    wait_for(|| f.status()["state"] == "inconclusive");
    assert_eq!(f.status()["reason"], "strategy_shadow_disabled");
    assert_eq!(f.status()["summary"]["skipped_scans"], 1);
    let third = f.tape.events[2]["at"].as_i64().unwrap();
    f.tape.protect_at(2, third);
    assert_eq!(f.tape.p.fills.len(), 2);
    assert!(f.tape.p.positions.is_empty());
}

#[test]
fn abandoned_shadow_is_inconclusive_on_restart() {
    let Some(worker) = worker_config() else {
        return;
    };
    let mut source = SOURCE.to_vec();
    source.extend_from_slice(b"\n# Restart fixture.\n");
    let f = ShadowFixture::new(worker, &source);
    let clock = f.clock.clone();
    // A running row is the durable state after an unclean controller exit.
    // Recover it without allowing the second slot to start another worker.
    let recovered = super::super::strategy_run::shadow::ShadowSlot::start_with_clock(
        f.setup.clone(),
        move || clock.load(std::sync::atomic::Ordering::Relaxed),
    )
    .unwrap();
    wait_for(|| f.status()["reason"] == "strategy_shadow_restarted");
    assert!(!recovered.worker_reserved());
    assert!(f.tape.p.fills.is_empty());
}

#[test]
fn shadow_rechecks_original_receipt_age_after_worker_reply() {
    let Some(worker) = worker_config() else {
        return;
    };
    let worker = WorkerConfig {
        call_deadline_ms: 2_000,
        ..worker
    };
    let source = b"def on_observation(obs):\n    for i in range(8000000):\n        pass\n    return {'actions': [{'instrument_key':'NSE_EQ|TESTA','side':'buy','max_price':'100','rationale':'fixture','exit_intent':{'stop':'99.50'}}]}\n";
    let mut f = ShadowFixture::new(worker, source);
    let first = f.input(0);
    wait_for(|| {
        f.run
            .shadow_slot()
            .is_some_and(|slot| slot.busy() && !slot.ready())
    });
    f.set_clock(first + nivra_core::market::MAX_FINANCIAL_AGE_MS + 1);
    f.wait_shadow();
    f.settle(first);
    f.set_clock(f.setup.end_ms);
    f.run.finish_shadow(&f.tape.db, &f.tape.p);
    wait_for(|| f.status()["state"] == "completed");
    let report = f.status();
    assert_eq!(report["summary"]["evaluated_scans"], 1);
    assert_eq!(report["summary"]["shadow"]["fills"], 0);
    assert_eq!(
        report["summary"]["shadow"]["outcomes"],
        json!({"stale_quote":1})
    );
    assert_eq!(f.tape.p.fills.len(), 1);
}

/// Peak summed RSS and CPU of the process tree, sampled from `/proc`. The
/// Python workers are grandchildren of the test binary.
struct TreeSampler {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    peak_rss_kib: std::sync::Arc<std::sync::atomic::AtomicU64>,
    peak_worker_kib: std::sync::Arc<std::sync::atomic::AtomicU64>,
    cpu_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TreeSampler {
    fn start() -> Self {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        };
        let stop = Arc::new(AtomicBool::new(false));
        let peak_rss_kib = Arc::new(AtomicU64::new(0));
        let peak_worker_kib = Arc::new(AtomicU64::new(0));
        let cpu_ms = Arc::new(AtomicU64::new(0));
        let (flag, rss, workers, cpu) = (
            stop.clone(),
            peak_rss_kib.clone(),
            peak_worker_kib.clone(),
            cpu_ms.clone(),
        );
        let handle = std::thread::spawn(move || {
            let own = std::process::id();
            let mut last: BTreeMap<u32, u64> = BTreeMap::new();
            let mut ticks = 0u64;
            while !flag.load(Relaxed) {
                let pids = proc_tree(own);
                let (mut total, mut children) = (0u64, 0u64);
                for pid in &pids {
                    let kib = proc_rss_kib(*pid).unwrap_or(0);
                    total += kib;
                    if *pid != own {
                        children += kib;
                    }
                    if let Some(now) = proc_cpu_ticks(*pid) {
                        ticks += now.saturating_sub(*last.get(pid).unwrap_or(&now));
                        last.insert(*pid, now);
                    }
                }
                rss.fetch_max(total, Relaxed);
                workers.fetch_max(children, Relaxed);
                std::thread::sleep(Duration::from_millis(20));
            }
            cpu.store(ticks * 10, Relaxed);
        });
        Self {
            stop,
            peak_rss_kib,
            peak_worker_kib,
            cpu_ms,
            handle: Some(handle),
        }
    }

    fn stop(mut self) -> (u64, u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        self.stop.store(true, Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        (
            self.peak_rss_kib.load(Relaxed),
            self.peak_worker_kib.load(Relaxed),
            self.cpu_ms.load(Relaxed),
        )
    }
}

fn proc_tree(root: u32) -> Vec<u32> {
    let mut pids = vec![root];
    let mut index = 0;
    while index < pids.len() {
        let pid = pids[index];
        if let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
            pids.extend(
                text.split_whitespace()
                    .filter_map(|c| c.parse::<u32>().ok()),
            );
        }
        index += 1;
    }
    pids
}

fn proc_rss_kib(pid: u32) -> Option<u64> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find(|line| line.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn proc_cpu_ticks(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = text.rsplit_once(") ")?.1.split_whitespace().collect();
    let user: u64 = fields.get(11)?.parse().ok()?;
    let system: u64 = fields.get(12)?.parse().ok()?;
    Some(user + system)
}

/// Reports peak memory and CPU for one live and one shadow worker replaying the
/// fixture tape. Ignored: it measures rather than gates, and the numbers only
/// compare on the same host.
#[test]
#[ignore = "measurement: reports peak memory and CPU of the live and shadow workers"]
fn shadow_fixture_resource_use() {
    let Some(worker) = worker_config() else {
        return;
    };
    let mut source = SOURCE.to_vec();
    source.extend_from_slice(b"\n# Resource fixture.\n");
    let mut f = ShadowFixture::new(worker, &source);
    let sampler = TreeSampler::start();
    let mut scans = 0u64;
    for round in 0..24usize {
        let step = round % f.tape.events.len();
        let at = f.tape.events[step]["at"].as_i64().unwrap() + round as i64 * 1_000;
        f.input_at(step, at);
        f.settle(at);
        f.wait_shadow();
        scans += 1;
    }
    let (peak_rss_kib, peak_worker_kib, cpu_ms) = sampler.stop();
    f.set_clock(f.setup.end_ms);
    f.run.finish_shadow(&f.tape.db, &f.tape.p);
    wait_for(|| f.status()["state"] == "completed");
    eprintln!(
        "forward_shadow_fixture scans={scans} peak_tree_rss_kib={peak_rss_kib} \
         peak_worker_rss_kib={peak_worker_kib} cpu_ms={cpu_ms}"
    );
    assert_eq!(f.status()["summary"]["evaluated_scans"], json!(scans));
    // Regression bounds only: a debug test binary dominates the tree, so this
    // catches a worker that materialises the tape, not a small regression.
    assert!(peak_rss_kib < 1_024 * 1_024, "tree peak {peak_rss_kib} KiB");
    assert!(
        peak_worker_kib < 120 * 1_024,
        "two workers peaked at {peak_worker_kib} KiB"
    );
}
