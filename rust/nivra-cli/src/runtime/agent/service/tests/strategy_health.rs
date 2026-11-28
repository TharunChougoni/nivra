use super::*;

#[test]
fn strategy_health_dispatches_without_a_ranking_and_reaches_the_rendered_brief() {
    use crate::runtime::wide::strategy_run::{HealthBasis, StrategyHealth};
    use nivra_core::finance::Amount;

    let dir = tempfile::tempdir().unwrap();
    let (c, mut cfg) = crate::runtime::decision::decision_fixture_config(dir.path());
    cfg.agent.as_mut().unwrap().mode = AgentMode::Shadow;
    cfg.strategy_execution = Some(planner::StrategyExecution {
        enabled: true,
        ..Default::default()
    });
    let mut service = LaneService::start(&cfg).unwrap();
    let (jobs, inbox) = sync_channel(1);
    service.jobs = Some(jobs);
    let portfolio = crate::runtime::tests::test_portfolio();
    let scanner = Scanner::default();
    let store = FeatureStore::new(Default::default()).unwrap();
    let mut db = crate::runtime::open_book(&c).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339(&format!("{}T10:00:00+05:30", c.session))
        .unwrap()
        .timestamp_millis();
    let health = StrategyHealth {
        active_strategy_id: Some("fallback:vwap_trend".into()),
        active_basis: HealthBasis::Fallback,
        blocked_reason: Some("strategy_engine_changed"),
        eligible_strategy_id: Some("eligible".into()),
        fresh_prospects: true,
        idle_minutes: 15,
        applied_actions: 2,
        session_net_pnl: Some(Amount::from_integer(-150)),
        session_net_pnl_bps: Some(-100.0),
        cash_drawdown_bps: Some(160.0),
        completed_calls: 5,
        failed_calls: 3,
        recent_actions: 5,
        refusals_by_code: BTreeMap::from([("price_above_max".into(), 3)]),
        ..StrategyHealth::default()
    };
    let mut cycle = Cycle {
        c: &c,
        cfg: &cfg,
        now,
        minute: 600,
        sealed: false,
        portfolio: &portfolio,
        instruments: &[],
        ranking: &[],
        ranked_at: 0,
        decision_ready: true,
        prepared: true,
        scanner: &scanner,
        store: &store,
        index: None,
        regime: None,
        strategy_health: Some(&health),
        spot: None,
        option_archive: None,
    };
    service.tick(&cycle, &mut db).unwrap();
    let work = inbox.try_recv().unwrap().work;
    assert_eq!(work.lane, Lane::Decision);
    let render =
        |work: &Work| serde_json::from_str::<Value>(&work.brief.render().unwrap()).unwrap();
    let brief = render(&work);
    let reasons = |work: &Work| {
        work.triggers
            .iter()
            .filter(|t| t.kind == TriggerKind::StrategyHealth)
            .map(|t| t.reason.to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        reasons(&work),
        [
            "idle",
            "losing",
            "failing",
            "refusals",
            "fallback_while_eligible"
        ]
    );
    assert_eq!(brief["strategy_health"]["active_basis"], "fallback");
    assert_eq!(brief["strategy_health"]["eligible_strategy_id"], "eligible");
    assert_eq!(
        brief["strategy_health"]["blocked_reason"],
        "strategy_engine_changed"
    );
    assert_eq!(brief["strategy_health"]["idle_minutes"], 15);
    assert_eq!(brief["strategy_health"]["applied_actions"], 2);
    assert_eq!(brief["strategy_health"]["session_net_pnl"], "-150");
    assert_eq!(brief["strategy_health"]["cash_drawdown_bps"], 160.0);
    assert_eq!(
        brief["strategy_health"]["refusals_by_code"]["price_above_max"],
        3
    );

    let accepted = StrategyHealth {
        active_basis: HealthBasis::Accepted,
        session_net_pnl_bps: Some(5.0),
        replay_expected_bps: Some(80.0),
        ..StrategyHealth::default()
    };
    cycle.strategy_health = Some(&accepted);
    cycle.now += 1;
    service.busy = None;
    service.tick(&cycle, &mut db).unwrap();
    let work = inbox.try_recv().unwrap().work;
    assert_eq!(reasons(&work), ["below_expectation"]);
    assert_eq!(
        render(&work)["strategy_health"]["replay_expected_bps"],
        80.0
    );
    let journal = std::fs::read_to_string(lanes::root(&c).join("triggers.ndjson")).unwrap();
    assert!(journal.contains(r#""kind":"strategy_health","reason":"below_expectation""#));
    service.stop();
}
