use super::super::strategy_run::{Frozen, StrategyRun, recent_actions};
use super::*;
use crate::runtime::strategy_library::build_mismatch;
use crate::runtime::strategy_library::{Author, Library, acceptance};
use nivra_core::execution::ExitIntent;
use serde_json::{Value, json};

const MANIFEST: &str = include_str!("../../../../../../fixtures/strategy/manifest.json");
const EVENTS: &str = include_str!("../../../../../../fixtures/strategy/events.jsonl");
const SOURCE: &[u8] = include_bytes!("../../../../../../fixtures/strategy/strategy.py");
const PARAMETERS: &str = include_str!("../../../../../../fixtures/strategy/parameters.json");

/// The real no-network sandbox, or a skip where the host has none.
fn sandbox() -> Option<nivra_sandbox::SandboxConfig> {
    let c = nivra_sandbox::SandboxConfig::default();
    let ready = c.bwrap_path.is_file()
        && c.python_path.is_file()
        && nivra_sandbox::run_document(&c, b"print('{}')", &json!({})).is_ok();
    assert!(
        ready || !nivra_sandbox::sandbox_required(),
        "strategy sandbox required but unavailable"
    );
    ready.then_some(c)
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
    let accepted = acceptance::Accepted {
        report: &report,
        report_sha256: digest(&report),
        source,
        source_sha256: digest(source),
        params,
        summary: json!({"trades": 12, "net_pnl": "41.5"}),
        tape_dates: vec!["2026-11-25".into()],
        thresholds: acceptance::Thresholds::default(),
    };
    let author = Author {
        lane: "research".into(),
        model: Some("fixture".into()),
        model_revision: None,
        turn_id: None,
        session: None,
    };
    library
        .register(&accepted, author, 1)
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
    depth: BTreeMap<String, Quote>,
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
            depth: BTreeMap::new(),
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
        let event = &self.events[step];
        let at = event["at"].as_i64().unwrap();
        for o in event["observations"].as_array().unwrap() {
            let key = o["key"].as_str().unwrap().to_owned();
            self.depth
                .insert(key, serde_json::from_value(o["quote"].clone()).unwrap());
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
        at
    }
    fn fresh(&self, at: i64) -> BTreeMap<String, Quote> {
        self.depth
            .iter()
            .filter(|(_, q)| q.fresh(at, 2000))
            .map(|(k, q)| (k.clone(), q.clone()))
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
    /// One scan: start a call, let it finish and apply it at `at`.
    fn scan(&mut self, run: &mut StrategyRun, at: i64) {
        if run.ready(&self.db, &self.c, at).unwrap().is_some() {
            run.start(self.frozen(at));
            run.wait();
        }
        run.poll(&mut self.db, &self.c, &mut self.p, &|| at)
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
    let Some(sandbox) = sandbox() else { return };
    let mut tape = Tape::new();
    let params: Value = serde_json::from_str::<Value>(PARAMETERS).unwrap()["params"].clone();
    let id = register(&tape.library(), SOURCE, params, |_| {});
    let mut run = StrategyRun::new(tape.library(), sandbox, Default::default());
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
    assert!(tape.failures().is_empty());
}

#[test]
fn failed_strategy_calls_trade_nothing_and_protection_continues() {
    let Some(sandbox) = sandbox() else { return };
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
    register(&tape.library(), b"raise SystemExit(3)\n", json!({}), |_| {});
    let mut crash = StrategyRun::new(tape.library(), sandbox.clone(), Default::default());
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
    // A call past the wall bound: the next scan is skipped, not queued, and
    // the stop still executes while the call runs.
    let slow = nivra_sandbox::SandboxConfig {
        wall_ms: 300,
        ..sandbox
    };
    let library = Library::at(tape.c.directory.join("slow-library"));
    register(&library, b"while True:\n    pass\n", json!({}), |_| {});
    let mut timeout = StrategyRun::new(library, slow, Default::default());
    let at = tape.protect(3);
    assert!(timeout.ready(&tape.db, &tape.c, at).unwrap().is_some());
    timeout.start(tape.frozen(at));
    assert_eq!(timeout.ready(&tape.db, &tape.c, at).unwrap(), None);
    let at = tape.protect(4);
    assert!(tape.p.positions.is_empty(), "protection waits for no call");
    timeout.wait();
    timeout
        .poll(&mut tape.db, &tape.c, &mut tape.p, &|| at)
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
    let mut old = StrategyRun::new(
        library,
        nivra_sandbox::SandboxConfig::default(),
        Default::default(),
    );
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
fn a_fallback_runs_labelled_until_a_strategy_is_accepted() {
    let Some(sandbox) = sandbox() else { return };
    let mut tape = Tape::new();
    let mut run = StrategyRun::new(
        tape.library(),
        sandbox,
        crate::runtime::strategy_library::choice::ChoicePolicy {
            fallback: vec!["vwap_trend".into()],
            policy_sha256: String::new(),
        },
    );
    let first = tape.protect(0);
    tape.scan(&mut run, first);
    let status = run.status(&tape.db, &tape.p);
    assert_eq!(status["state"], "ready", "{status}");
    assert_eq!(status["reason"], "no_active_strategy");
    assert_eq!(status["strategy"]["basis"], "fallback");
    assert_eq!(status["strategy"]["strategy_id"], "fallback:vwap_trend");
    assert_eq!(status["calls"], 1);
    assert!(tape.failures().is_empty(), "{:?}", tape.failures());
    // An accepted version takes over at the next library read.
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
}
