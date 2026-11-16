use super::*;
use std::collections::VecDeque;

fn call(name: &str, args: &str) -> Value {
    json!({"type":"function_call","call_id":"call_fixture","name":name,"arguments":args})
}

#[test]
fn duplicate_snapshot_keys_are_folded_and_do_not_abort_the_run() {
    let tools = definitions("digest", &[], false).unwrap();
    // 2026-10-29 paper digest: Luna sent INE614G01033 / INE398A01010 / INE727B01026 twice.
    let Tool::Features(keys) = checked_call(
        &call(
            "get_instrument_features",
            r#"{"instrument_keys":["INE614G01033","INE398A01010","INE614G01033"]}"#,
        ),
        &tools,
    )
    .unwrap()
    .1
    else {
        panic!("expected features");
    };
    assert_eq!(
        keys,
        vec!["INE614G01033".to_string(), "INE398A01010".to_string()]
    );
    let mut twenty = (0..19).map(|n| format!("K{n}")).collect::<Vec<_>>();
    twenty.push("K0".into());
    let Tool::Snapshot(keys) = checked_call(
        &call(
            "get_market_snapshot",
            &json!({"instrument_keys": twenty}).to_string(),
        ),
        &tools,
    )
    .unwrap()
    .1
    else {
        panic!("expected snapshot");
    };
    assert_eq!(keys.len(), 19);
    assert_eq!(keys[0], "K0");
    assert_eq!(
        checked_call(
            &call("get_market_snapshot", r#"{"instrument_keys":[]}"#),
            &tools
        )
        .err()
        .map(|error| error.to_string())
        .as_deref(),
        Some("harness_snapshot_args_invalid")
    );
    let too_many: Vec<String> = (0..21).map(|n| format!("K{n}")).collect();
    assert_eq!(
        checked_call(
            &call(
                "get_market_snapshot",
                &json!({"instrument_keys": too_many}).to_string()
            ),
            &tools
        )
        .err()
        .map(|error| error.to_string())
        .as_deref(),
        Some("harness_snapshot_args_invalid")
    );

    struct Script(VecDeque<Value>);
    impl Backend for Script {
        fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
            Ok(self.0.pop_front().ok_or("fixture_exhausted")?.to_string())
        }
    }
    let digest = json!({"complete":true,"as_of_ms":0,"coverage":{"universe":1,"scanned":1,"quoted":1,"featured":1,"experiments":0},"gaps":[],"changed_prospects":[],"active_rankings":[],"candidate_rankings":[],"bar_context":[],"spread_depth":[],"holdings":[],"cash":{"available":"0"},"fees":{"per_order":"0","buy_bps":"0","sell_bps":"0"},"recent_actions":[],"experiments":[],"evidence_ids":["bundle"]});
    let batch = json!({"output":[
        {"type":"function_call","call_id":"feat","name":"get_instrument_features","arguments":"{\"instrument_keys\":[\"INE614G01033\",\"INE614G01033\"]}"},
        {"type":"function_call","call_id":"ev","name":"read_evidence","arguments":"{\"evidence_id\":\"bundle\"}"}
    ]});
    let terminal = json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":digest.to_string()}]}]});
    let directory = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(directory.path());
    let role = ModelRole {
        enabled: true,
        model_id: crate::model::LUNA.into(),
        transport: sadr_cli::native_transport::Transport::OpenaiCodex,
        daily_call_budget: 8,
        ..Default::default()
    };
    let mut fake = Script(VecDeque::from([batch, terminal]));
    let result = run(
        &c,
        &PlannerConfig::default(),
        "digest",
        &role,
        "Duplicate snapshot keys fixture",
        &json!({"shortlist":[]}),
        &mut fake,
        &planner::preview_guard(),
    )
    .unwrap();
    validate_owner_digest(&result, &Value::Null).unwrap();
}
