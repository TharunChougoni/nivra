use super::*;
use serde_json::json;

const SOURCE: &[u8] = include_bytes!("../../../../../fixtures/strategy/strategy.py");
const MANIFEST: &[u8] = include_bytes!("../../../../../fixtures/strategy/manifest.json");
const EVENTS: &[u8] = include_bytes!("../../../../../fixtures/strategy/events.jsonl");
const PARAMETERS: &[u8] = include_bytes!("../../../../../fixtures/strategy/parameters.json");

fn worker_config() -> Option<WorkerConfig> {
    let c = WorkerConfig::default();
    c.python_path.is_file().then_some(c)
}

#[test]
fn fixture_strategy_replays_with_hand_checked_fills_and_fees() {
    let Some(c) = worker_config() else { return };
    let parameters: replay::Parameters = serde_json::from_slice(PARAMETERS).unwrap();
    let report = replay::run(&c, SOURCE, &parameters, MANIFEST, EVENTS, &|| None)
        .unwrap()
        .unwrap();
    // Costs: 10 bps each side, fees rounded up to 0.01, no slippage.
    // T0 buy 10 A @ 100.00: fee 1.00. B at max 49.95 under a 50.00 ask is refused.
    // T1 revise A's target 100.50 -> 100.40. T2 bid 100.45 hits it: sell 10 @ 100.45,
    //    fee 1.0045 -> 1.01, net 1004.50 - 1.01 - 1001.00 = 2.49.
    // T3 buy Rs 1000 of A at 100.50: 9 units (904.50 + fee 0.91 = 905.41).
    // T4 bid 99.95 hits the 100.00 stop: sell 9 @ 99.95, fee 0.90, net -6.76.
    // T5 selling A again is refused by the planner check: nothing is held.
    let fills: Vec<_> = report["fills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["buy"].clone(),
                f["units"].clone(),
                f["price"].clone(),
                f["fee"].clone(),
            )
        })
        .collect();
    assert_eq!(
        fills,
        [
            (json!(true), json!(10), json!("100"), json!("1")),
            (json!(false), json!(10), json!("100.45"), json!("1.01")),
            (json!(true), json!(9), json!("100.5"), json!("0.91")),
            (json!(false), json!(9), json!("99.95"), json!("0.9")),
        ]
    );
    assert_eq!(
        report["trades"],
        json!([
            {"key":"NSE_EQ|TESTA","exit_at_ms":1795578602000_i64,"units":10,"entry_price":"100",
             "exit_price":"100.45","exit_reason":"target","net_pnl":"2.49"},
            {"key":"NSE_EQ|TESTA","exit_at_ms":1795578604000_i64,"units":9,"entry_price":"100.5",
             "exit_price":"99.95","exit_reason":"stop","net_pnl":"-6.76"}
        ])
    );
    assert_eq!(
        report["summary"],
        json!({"trades":2,"wins":1,"win_rate":"0.5","net_pnl":"-4.27","median_trade":"-2.135",
               "worst_drawdown":"6.76","fees":"3.82","turnover":"3808.55","ending_cash":"4995.73",
               "marked_equity":"4995.73","open_positions":0,"unmarked_positions":[]})
    );
    assert_eq!(
        report["refusals"],
        json!([
            {"at_ms":1795578600000_i64,"index":1,"key":"NSE_EQ|TESTB","side":"buy","reason":"price_above_max"},
            {"at_ms":1795578605000_i64,"index":0,"key":"NSE_EQ|TESTA","side":null,"reason":"action_sell_unheld"}
        ])
    );
    assert_eq!(report["action_outcomes"]["exit_revised"], 1);
    assert_eq!(report["tape"]["dates"], json!(["2026-11-25"]));
    assert_eq!(report["tape"]["strategy_calls"], 6);
    assert_eq!(report["strategy_failures"], json!({}));
    // Fills carry the strategy hash where model fills carry a decision id.
    assert_eq!(report["fills"][0]["decision_id"], report["source_sha256"]);
    // Stable and hashable: the same inputs give the same canonical bytes.
    let again = replay::run(&c, SOURCE, &parameters, MANIFEST, EVENTS, &|| None)
        .unwrap()
        .unwrap();
    assert_eq!(
        nivra_cli::canonical_json::bytes(&report).unwrap(),
        nivra_cli::canonical_json::bytes(&again).unwrap()
    );
}

#[test]
fn invalid_strategy_output_fails_with_a_reason_and_no_actions() {
    let Some(c) = worker_config() else { return };
    let instrument: Instrument = serde_json::from_value(
        json!({"key":"NSE_EQ|TESTA","symbol":"TESTA","series":"EQ","tick":"0.05"}),
    )
    .unwrap();
    let universe = BTreeMap::from([(instrument.key.clone(), instrument)]);
    let quotes = BTreeMap::from([(
        "NSE_EQ|TESTA".to_owned(),
        serde_json::from_value::<Quote>(json!({"bid":"99.95","ask":"100","bid_units":5,
            "ask_units":5,"source":1_000,"receipt":1_000}))
        .unwrap(),
    )]);
    let costs: FeeArithmetic = serde_json::from_value(json!({"per_order":"0","buy_bps":"10",
        "sell_bps":"10","slippage_bps":"0","rounding_increment":"0.01"}))
    .unwrap();
    let portfolio = Portfolio::new();
    let params = json!({});
    let observation = Observation::new(1_000, &params, &[], &quotes, &portfolio, &universe, &costs);
    let buy = |key: &str| {
        format!(
            "def on_observation(obs):\n    return {{\"actions\":[{{\"instrument_key\":\"{key}\",\"side\":\"buy\",\"units\":1,\"rupees\":None,\"max_price\":\"100\",\"rationale\":\"\",\"exit_intent\":{{}}}}]}}\n"
        )
    };
    assert_eq!(
        call(&c, buy("NSE_EQ|TESTA").as_bytes(), &observation)
            .unwrap()
            .len(),
        1
    );
    for (source, reason) in [
        (buy("NSE_EQ|UNKNOWN"), "strategy_unknown_instrument"),
        (
            "def on_observation(obs):\n    return {'actions':[{'instrument_key':'NSE_EQ|TESTA','units':float('nan')}]}\n".into(),
            "strategy_output_non_finite",
        ),
        (
            "def on_observation(obs):\n    return {'actions': ['x' * 70000]}\n".into(),
            "strategy_output_limit",
        ),
        (
            "import socket\ndef on_observation(obs):\n    return {'actions':[]}\n".into(),
            "strategy_review_unsafe",
        ),
        (
            "def on_observation(obs):\n    raise SystemExit(3)\n".into(),
            "strategy_exit",
        ),
        (
            "def on_observation(obs):\n    return {'actions':[], 'notes':1}\n".into(),
            "strategy_output_schema",
        ),
    ] {
        assert_eq!(call(&c, source.as_bytes(), &observation), Err(reason), "{source}");
    }
    let slow_cfg = WorkerConfig {
        call_deadline_ms: 100,
        ..c.clone()
    };
    assert_eq!(
        call(
            &slow_cfg,
            b"def on_observation(obs):\n    while True:\n        pass\n",
            &observation
        ),
        Err("strategy_wall_timeout")
    );
    // OS hardening blocks file reads, file writes, sockets, and forks even if
    // a Worker is started directly without the AST review gate.
    let mut hardened = Worker::start(
        WorkerKey {
            source_sha256: "test".into(),
            params_sha256: "test".into(),
        },
        br#"import os, socket
def on_observation(obs):
    results = []
    for fn in (lambda: open('/etc/passwd').read(), lambda: socket.socket(), lambda: os.fork()):
        try:
            fn()
            results.append('ALLOWED')
        except OSError:
            results.append('blocked')
    return {'actions': results}
"#
        .to_vec(),
        json!({}),
        c,
        1_000,
    )
    .unwrap();
    let raw = hardened
        .call_sync(&serde_json::to_vec(&observation).unwrap(), 1_000)
        .unwrap();
    let parsed: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(parsed["actions"], json!(["blocked", "blocked", "blocked"]));
}

#[test]
fn tape_dates_count_only_sessions_the_strategy_ran_on() {
    let Some(c) = worker_config() else { return };
    let mut parameters: replay::Parameters = serde_json::from_slice(PARAMETERS).unwrap();
    // The window opens after the last receipt, so the strategy never runs.
    parameters.from_ms = Some(1_795_578_700_000);
    parameters.to_ms = Some(1_795_578_800_000);
    let report = replay::run(&c, SOURCE, &parameters, MANIFEST, EVENTS, &|| None)
        .unwrap()
        .unwrap();
    assert_eq!(report["tape"]["strategy_calls"], 0);
    assert_eq!(report["tape"]["dates"], json!([]));
}
