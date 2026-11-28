use super::*;

fn kinds(triggers: &[Trigger]) -> Vec<TriggerKind> {
    triggers.iter().map(|trigger| trigger.kind).collect()
}

#[test]
fn triggers_count_distinct_scans_and_prioritize_decision_over_research() {
    let mut engine = TriggerEngine::new(TriggerSettings::default());
    let ranking = [Ranked {
        instrument_key: "candidate".into(),
        score: 25.0,
    }];
    let held = [Held {
        instrument_key: "holding".into(),
        entry: 100.0,
        mark: 95.0,
        stop: Some(95.0),
        target: None,
    }];
    let mut observation = Observation {
        now: 1_000,
        minute: 555,
        open_minute: 555,
        phase: Phase::Open,
        held: &[],
        fills: 0,
        ranking: &ranking,
        ranked_at: 1_000,
        decision_spent: 0.0,
        research_budget_left: true,
        requests_queued: 0,
        rankers: &[],
        after_close_due: false,
        review_running: false,
        strategy_active: false,
        strategy_supervision: false,
        strategy_accepted: false,
        strategy_health: None,
        strategy_fills: 0,
        regime: None,
    };
    assert_eq!(
        kinds(&engine.observe(&observation)),
        [TriggerKind::Schedule]
    );
    assert!(engine.observe(&observation).is_empty());

    observation.now += 1_000;
    observation.ranked_at = observation.now;
    observation.requests_queued = 1;
    observation.fills = 1;
    observation.held = &held;
    let fired = engine.observe(&observation);
    assert_eq!(
        kinds(&fired),
        [
            TriggerKind::PositionRisk,
            TriggerKind::PositionRisk,
            TriggerKind::Fill,
            TriggerKind::EntryCandidate,
            TriggerKind::Request,
        ]
    );
    assert_eq!(fired[3].rank, Some(1));
    assert_eq!(fired[3].score, Some(25.0));
    let (lane, batch) = engine.next().unwrap();
    assert_eq!(lane, Lane::Decision);
    assert_eq!(batch.last().unwrap().kind, TriggerKind::Schedule);
    assert_eq!(engine.next().unwrap().0, Lane::Research);
    observation.requests_queued = 0;

    observation.phase = Phase::PreClose;
    observation.now += engine.settings().research.quiet_ms;
    assert_eq!(
        kinds(&engine.observe(&observation)),
        [TriggerKind::Schedule, TriggerKind::Quiet]
    );
    assert!(engine.observe(&observation).is_empty());
    assert_eq!(engine.entry_bar(1.0), (50.0, 1));
}

#[test]
fn returning_candidate_waits_out_cooldown_without_repeating() {
    let settings = TriggerSettings {
        decision: DecisionTriggers {
            cooldown_ms: 5_000,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut engine = TriggerEngine::new(settings);
    let ranking = [Ranked {
        instrument_key: "candidate".into(),
        score: 25.0,
    }];
    let mut observation = Observation {
        now: 1_000,
        minute: 590,
        open_minute: 555,
        phase: Phase::Open,
        held: &[],
        fills: 0,
        ranking: &ranking,
        ranked_at: 1_000,
        decision_spent: 0.0,
        research_budget_left: false,
        requests_queued: 0,
        rankers: &[],
        after_close_due: false,
        review_running: false,
        strategy_active: false,
        strategy_supervision: false,
        strategy_accepted: false,
        strategy_health: None,
        strategy_fills: 0,
        regime: None,
    };
    for (step, rows, expected) in [
        (1_000, ranking.as_slice(), false),
        (2_000, ranking.as_slice(), true),
        (3_000, &[], false),
        (4_000, ranking.as_slice(), false),
        (5_000, ranking.as_slice(), false),
        (7_000, ranking.as_slice(), true),
        (12_000, ranking.as_slice(), false),
    ] {
        observation.now = step;
        observation.ranked_at = step;
        observation.ranking = rows;
        let fired = engine.observe(&observation);
        assert_eq!(
            kinds(&fired).contains(&TriggerKind::EntryCandidate),
            expected,
            "scan at {step}"
        );
    }
}

#[test]
fn drift_seal_and_after_close_research_fire_once_after_the_review() {
    let mut engine = TriggerEngine::new(TriggerSettings::default());
    let rankers = [RankerStats {
        name: "test-ranker".into(),
        live_hit_rate: 0.3,
        backtest_hit_rate: 0.6,
        samples: 20,
    }];
    let mut observation = Observation {
        now: 20_000,
        minute: 930,
        open_minute: 555,
        phase: Phase::Sealed,
        held: &[],
        fills: 0,
        ranking: &[],
        ranked_at: 0,
        decision_spent: 0.0,
        research_budget_left: true,
        requests_queued: 0,
        rankers: &rankers,
        after_close_due: true,
        review_running: false,
        strategy_active: false,
        strategy_supervision: false,
        strategy_accepted: false,
        strategy_health: None,
        strategy_fills: 0,
        regime: None,
    };
    // The after-close research turn waits while the review is queued or runs.
    let fired = engine.observe(&observation);
    assert_eq!(kinds(&fired), [TriggerKind::Drift, TriggerKind::Seal]);
    assert_eq!(
        fired[0].instrument_key.as_deref(),
        Some("ranker:test-ranker")
    );
    assert!(engine.observe(&observation).is_empty());
    assert_eq!(engine.next().unwrap().0, Lane::Research);
    assert_eq!(engine.next().unwrap().0, Lane::Review);
    observation.review_running = true;
    assert!(engine.observe(&observation).is_empty());

    // The review leaves an agenda: one research turn takes both, on the
    // schedule and not on quiet, and never fires again.
    engine.push(Trigger::new(
        TriggerKind::Agenda,
        "review_agenda",
        observation.now,
    ));
    observation.review_running = false;
    let fired = engine.observe(&observation);
    assert_eq!(kinds(&fired), [TriggerKind::AfterClose]);
    assert_eq!(fired[0].reason, "post_close");
    let (lane, batch) = engine.next().unwrap();
    assert_eq!(lane, Lane::Research);
    assert_eq!(
        kinds(&batch),
        [TriggerKind::AfterClose, TriggerKind::Agenda]
    );
    observation.now += 4 * engine.settings().research.quiet_ms;
    assert!(engine.observe(&observation).is_empty());
    assert!(engine.next().is_none());

    // A failed review leaves no agenda; research still runs after the close.
    let mut engine = TriggerEngine::new(TriggerSettings::default());
    observation.rankers = &[];
    assert_eq!(kinds(&engine.observe(&observation)), [TriggerKind::Seal]);
    assert_eq!(engine.next().unwrap().0, Lane::Review);
    assert_eq!(
        kinds(&engine.observe(&observation)),
        [TriggerKind::AfterClose]
    );
}

#[test]
fn active_strategy_suppresses_cash_entries_and_triggers_only_for_supervision() {
    let mut engine = TriggerEngine::new(TriggerSettings::default());
    let ranking = [
        Ranked {
            instrument_key: "NSE_EQ|A".into(),
            score: 25.0,
        },
        Ranked {
            instrument_key: "NSE_EQ|B".into(),
            score: 22.0,
        },
        Ranked {
            instrument_key: "NSE_EQ|C".into(),
            score: 20.0,
        },
    ];
    let mut o = Observation {
        now: 1_000,
        minute: 555,
        open_minute: 555,
        phase: Phase::Open,
        held: &[],
        fills: 0,
        ranking: &ranking,
        ranked_at: 1_000,
        decision_spent: 0.0,
        research_budget_left: false,
        requests_queued: 0,
        rankers: &[],
        after_close_due: false,
        review_running: false,
        strategy_active: true,
        strategy_supervision: true,
        strategy_accepted: true,
        strategy_health: None,
        strategy_fills: 0,
        regime: Some("unknown"),
    };
    // Open fires once; top-K cash candidates and strategy fills do not trigger decision/gate.
    assert_eq!(
        engine
            .observe(&o)
            .iter()
            .map(|t| (t.kind, t.reason.as_str()))
            .collect::<Vec<_>>(),
        [(TriggerKind::Schedule, "open")]
    );
    for step in 1..=3 {
        o.now += 5_000;
        o.ranked_at = o.now;
        o.fills = step;
        o.strategy_fills = step;
        assert!(engine.observe(&o).is_empty(), "step {step} stays quiet");
    }
    // Crossing the hour with new top-K names does not trigger while strategy is active.
    let new_ranking = [
        Ranked {
            instrument_key: "NSE_EQ|D".into(),
            score: 30.0,
        },
        Ranked {
            instrument_key: "NSE_EQ|E".into(),
            score: 28.0,
        },
    ];
    o.minute = 600;
    o.now += 5_000;
    o.ranked_at = o.now;
    o.ranking = &new_ranking;
    assert!(engine.observe(&o).is_empty());

    // First known regime after an unknown open fires `regime_changed`.
    o.now += 5_000;
    o.regime = Some("up_trend/normal_vol/strong/no_gap");
    assert_eq!(
        engine
            .observe(&o)
            .iter()
            .map(|t| (t.kind, t.reason.as_str()))
            .collect::<Vec<_>>(),
        [(TriggerKind::Schedule, "regime_changed")]
    );
    // Transient unknown does not re-fire when the same regime returns.
    o.now += 400_000;
    o.regime = Some("unknown");
    assert!(engine.observe(&o).is_empty());
    o.now += 5_000;
    o.regime = Some("up_trend/normal_vol/strong/no_gap");
    assert!(engine.observe(&o).is_empty());

    // A real regime shift after cooldown fires `regime_changed`.
    o.now += 400_000;
    o.regime = Some("down_trend/high_vol/weak/gap");
    assert_eq!(
        engine
            .observe(&o)
            .iter()
            .map(|t| (t.kind, t.reason.as_str()))
            .collect::<Vec<_>>(),
        [(TriggerKind::Schedule, "regime_changed")]
    );

    // Position risk, non-strategy fills (e.g. options) and pre-close still fire.
    let held = [Held {
        instrument_key: "NSE_EQ|A".into(),
        entry: 100.0,
        mark: 95.0,
        stop: Some(95.0),
        target: None,
    }];
    o.held = &held;
    o.fills += 1;
    o.phase = Phase::PreClose;
    o.now += 5_000;
    assert_eq!(
        kinds(&engine.observe(&o)),
        [
            TriggerKind::PositionRisk,
            TriggerKind::PositionRisk,
            TriggerKind::Fill,
            TriggerKind::Schedule,
        ]
    );

    // Without an active strategy, the existing entry-candidate and hourly flow is unchanged.
    let mut baseline = TriggerEngine::new(TriggerSettings::default());
    o.strategy_active = false;
    o.strategy_supervision = false;
    o.phase = Phase::Open;
    o.minute = 555;
    o.held = &[];
    o.fills = 0;
    o.strategy_fills = 0;
    o.now = 1_000;
    o.ranked_at = 1_000;
    assert_eq!(kinds(&baseline.observe(&o)), [TriggerKind::Schedule]);
    o.now = 2_000;
    o.ranked_at = 2_000;
    assert_eq!(
        kinds(&baseline.observe(&o)),
        [TriggerKind::EntryCandidate, TriggerKind::EntryCandidate]
    );
    o.minute = 600;
    o.now = 3_000;
    o.ranked_at = 3_000;
    o.ranking = &ranking;
    assert_eq!(kinds(&baseline.observe(&o)), [TriggerKind::Schedule]);
}

#[test]
fn the_strategy_is_chosen_before_the_open_and_research_searches_when_none_is_accepted() {
    let observe = |engine: &mut TriggerEngine, phase, minute, accepted, now| {
        engine
            .observe(&Observation {
                now,
                minute,
                open_minute: 555,
                phase,
                held: &[],
                fills: 0,
                ranking: &[],
                ranked_at: 0,
                decision_spent: 0.0,
                research_budget_left: true,
                requests_queued: 0,
                rankers: &[],
                after_close_due: false,
                review_running: false,
                strategy_active: accepted,
                strategy_supervision: true,
                strategy_accepted: accepted,
                strategy_health: None,
                strategy_fills: 0,
                regime: None,
            })
            .iter()
            .map(|t| (t.kind, t.reason.clone()))
            .collect::<Vec<_>>()
    };
    let mut engine = TriggerEngine::new(TriggerSettings::default());
    // Before 09:00 nothing fires; at 09:00 the choice and the search fire once.
    assert!(observe(&mut engine, Phase::PreOpen, 539, false, 1).is_empty());
    assert_eq!(
        observe(&mut engine, Phase::PreOpen, 540, false, 2),
        [
            (TriggerKind::PreOpen, "choose_strategy".to_owned()),
            (
                TriggerKind::StrategySearch,
                "no_accepted_strategy".to_owned()
            ),
        ]
    );
    assert!(observe(&mut engine, Phase::PreOpen, 541, false, 3).is_empty());
    let (lane, batch) = engine.next().unwrap();
    assert_eq!((lane, batch.len()), (Lane::Decision, 1));
    assert_eq!(engine.next().unwrap().0, Lane::Research);
    // The open still re-evaluates; the search does not repeat.
    assert_eq!(
        observe(&mut engine, Phase::Open, 555, false, 4),
        [(TriggerKind::Schedule, "open".to_owned())]
    );
    // An accepted strategy needs no search; a closed or unprepared book
    // gets no pre-open turn; a late configured minute waits for it.
    let mut accepted = TriggerEngine::new(TriggerSettings::default());
    assert_eq!(
        observe(&mut accepted, Phase::PreOpen, 545, true, 1),
        [(TriggerKind::PreOpen, "choose_strategy".to_owned())]
    );
    let mut closed = TriggerEngine::new(TriggerSettings::default());
    assert!(observe(&mut closed, Phase::Closed, 545, false, 1).is_empty());
    let mut late = TriggerSettings::default();
    late.decision.pre_open_minute = 550;
    let mut late = TriggerEngine::new(late);
    assert!(observe(&mut late, Phase::PreOpen, 545, false, 1).is_empty());
    // A controller that starts after the open searches from the open turn.
    let mut restarted = TriggerEngine::new(TriggerSettings::default());
    assert_eq!(
        observe(&mut restarted, Phase::Open, 560, false, 1),
        [
            (TriggerKind::Schedule, "open".to_owned()),
            (
                TriggerKind::StrategySearch,
                "no_accepted_strategy".to_owned()
            ),
        ]
    );
}

/// A chain row: mid premium, open interest and whether it can be traded now.
fn row(key: &str, mid: f64, oi: u64, tradable: bool) -> crate::runtime::options::chain::ChainRow {
    crate::runtime::options::chain::ChainRow {
        instrument_key: key.into(),
        option: "call",
        strike: "26000".into(),
        offset: 0,
        lot_size: 65,
        bid: mid.to_string(),
        ask: mid.to_string(),
        spread_ticks: if tradable { 2 } else { 12 },
        depth_lots: 5,
        quote_age_ms: 100,
        open_interest: Some(oi),
        lot_cash: "6500".into(),
        affordable_lots: 1,
        round_trip_cost_bps: 80.0,
        mid,
    }
}

fn chain(
    spot: f64,
    expiry_day: bool,
    rows: Vec<crate::runtime::options::chain::ChainRow>,
) -> crate::runtime::options::chain::Chain {
    crate::runtime::options::chain::Chain {
        underlying_key: "NSE_INDEX|Nifty 50".into(),
        spot: spot.to_string(),
        spot_age_ms: 10,
        expiry: nivra_core::time::ExchangeDate::parse("2026-10-20").unwrap(),
        days_to_expiry: if expiry_day { 0 } else { 4 },
        expiry_day,
        atm_strike: "26000".into(),
        rows,
        spot_value: spot,
    }
}

fn fo(
    engine: &mut TriggerEngine,
    now: i64,
    chain: &crate::runtime::options::chain::Chain,
) -> Vec<(String, String)> {
    engine
        .observe_fo(&FoObservation {
            now,
            minute: 600,
            phase: Phase::Open,
            decision_spent: 0.0,
            chain,
        })
        .into_iter()
        .map(|t| {
            assert_eq!(t.kind, TriggerKind::FoSignal);
            (t.reason, t.instrument_key.unwrap())
        })
        .collect()
}

fn pair(reason: &str, key: &str) -> (String, String) {
    (reason.into(), key.into())
}

#[test]
fn option_chain_moves_fire_once_from_their_reference_on_tradable_contracts() {
    let mut engine = TriggerEngine::new(TriggerSettings::default());
    let (ce, thin) = ("NSE_FO|26000CE", "NSE_FO|26100CE");
    let start = chain(
        26_000.0,
        false,
        vec![
            row(ce, 100.0, 1_000_000, true),
            row(thin, 60.0, 500_000, false),
        ],
    );
    // The first chain only sets references.
    assert!(fo(&mut engine, 1_000, &start).is_empty());
    // Spot +40 bps, the liquid premium +20 % and its OI +15 %. The thin
    // contract moved as much but cannot be traded, so it stays quiet.
    let moved = chain(
        26_104.0,
        false,
        vec![
            row(ce, 120.0, 1_150_000, true),
            row(thin, 75.0, 800_000, false),
        ],
    );
    assert_eq!(
        fo(&mut engine, 2_000, &moved),
        [
            pair("spot_move", "NSE_INDEX|Nifty 50"),
            pair("premium_move", ce),
            pair("oi_shift", ce)
        ]
    );
    let (lane, batch) = engine.next().unwrap();
    assert_eq!((lane, batch.len()), (Lane::Decision, 3));
    assert_eq!(batch[1].score, Some(2_000.0));
    // The same chain again: each reference moved to the value it fired on.
    assert!(fo(&mut engine, 3_000, &moved).is_empty());
    // Another +20 % inside the cooldown waits; after it, it fires.
    let again = chain(26_104.0, false, vec![row(ce, 144.0, 1_150_000, true)]);
    assert!(fo(&mut engine, 4_000, &again).is_empty());
    assert_eq!(
        fo(&mut engine, 2_000 + 10 * 60_000, &again),
        [pair("premium_move", ce)]
    );
    // Once the thin contract tightens, its move since its first sight fires.
    let tight = chain(26_104.0, false, vec![row(thin, 75.0, 500_000, true)]);
    assert_eq!(
        fo(&mut engine, 2_000 + 10 * 60_000, &tight),
        [pair("premium_move", thin)]
    );
}

#[test]
fn option_triggers_respect_phase_expiry_day_budget_and_decision_turns() {
    let ce = "NSE_FO|26000CE";
    let start = chain(26_000.0, true, vec![row(ce, 100.0, 1_000_000, true)]);
    let moved = chain(26_104.0, true, vec![row(ce, 100.0, 1_000_000, true)]);
    let observe = |engine: &mut TriggerEngine, phase, minute, spent, chain| {
        engine
            .observe_fo(&FoObservation {
                now: 5_000,
                minute,
                phase,
                decision_spent: spent,
                chain,
            })
            .len()
    };
    let fresh = || {
        let mut engine = TriggerEngine::new(TriggerSettings::default());
        engine.observe_fo(&FoObservation {
            now: 1_000,
            minute: 600,
            phase: Phase::PreOpen,
            decision_spent: 0.0,
            chain: &start,
        });
        engine
    };
    assert_eq!(observe(&mut fresh(), Phase::PreClose, 600, 0.0, &moved), 0);
    // Expiry day stops raising triggers at 13:30.
    assert_eq!(observe(&mut fresh(), Phase::Open, 810, 0.0, &moved), 0);
    assert_eq!(observe(&mut fresh(), Phase::Open, 809, 0.0, &moved), 1);
    // A spent decision budget doubles the bar: +40 bps no longer clears 60.
    assert_eq!(observe(&mut fresh(), Phase::Open, 600, 1.0, &moved), 0);
    assert_eq!(TriggerSettings::default().fo.bar(0.5).spot_move_bps, 45.0);
    // With no contract to trade, a spot move is not worth a turn.
    let illiquid = chain(26_104.0, true, vec![row(ce, 100.0, 1_000_000, false)]);
    assert_eq!(observe(&mut fresh(), Phase::Open, 600, 0.0, &illiquid), 0);
    // A decision turn saw the chain: moves count from the next chain after it.
    let mut engine = fresh();
    engine.decision_started(2_000, &[], &[]);
    assert_eq!(observe(&mut engine, Phase::Open, 600, 0.0, &moved), 0);
    let back = chain(26_000.0, true, vec![row(ce, 100.0, 1_000_000, true)]);
    assert_eq!(observe(&mut engine, Phase::Open, 600, 0.0, &back), 1);
}

#[test]
fn option_trigger_settings_validate_and_stay_out_of_the_policy_at_their_defaults() {
    let defaults = TriggerSettings::default();
    assert!(defaults.validate());
    assert!(serde_json::to_value(&defaults).unwrap().get("fo").is_none());
    let custom: TriggerSettings =
        serde_json::from_value(serde_json::json!({"fo":{"spot_move_bps":45.0,"strikes_near":3}}))
            .unwrap();
    assert!(custom.validate());
    assert_eq!(
        serde_json::to_value(&custom).unwrap()["fo"]["strikes_near"],
        3
    );
    for bad in [
        serde_json::json!({"fo":{"strikes_near":0}}),
        serde_json::json!({"fo":{"premium_move_bps":0.0}}),
        serde_json::json!({"fo":{"expiry_day_last_minute":1440}}),
    ] {
        let settings: TriggerSettings = serde_json::from_value(bad).unwrap();
        assert!(!settings.validate());
    }
    assert!(serde_json::from_value::<TriggerSettings>(serde_json::json!({"fo":{"oi":1}})).is_err());
}

#[test]
fn strategy_health_has_a_decision_trigger_and_an_optional_policy_block() {
    let kind: TriggerKind = serde_json::from_str(r#""strategy_health""#).unwrap();
    assert_eq!(kind.lane(), Lane::Decision);
    let example: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../packaging/rust/strategy-health.fragment.json"
    ))
    .unwrap();
    let example: TriggerSettings =
        serde_json::from_value(example["agent"]["triggers"].clone()).unwrap();
    assert_eq!(example, TriggerSettings::default());
    let defaults = serde_json::to_value(example).unwrap();
    assert!(defaults["decision"].get("strategy_health").is_none());
    let settings: TriggerSettings = serde_json::from_value(serde_json::json!({
        "decision": {"strategy_health": {"idle_minutes": 9}}
    }))
    .unwrap();
    assert!(settings.validate());
    assert_eq!(
        serde_json::to_value(settings).unwrap()["decision"]["strategy_health"]["idle_minutes"],
        9
    );
}

#[test]
fn strategy_health_reasons_have_independent_cooldowns_and_require_session_budget() {
    use crate::runtime::wide::strategy_run::HealthBasis;

    let defaults = TriggerSettings::default();
    let cooldown = defaults.decision.strategy_health.cooldown_ms;
    let mut engine = TriggerEngine::new(defaults);
    let mut health = StrategyHealth {
        active_strategy_id: Some("strategy".into()),
        active_basis: HealthBasis::Accepted,
        fresh_prospects: true,
        idle_minutes: 15,
        session_net_pnl_bps: Some(-100.0),
        replay_expected_bps: Some(10.0),
        completed_calls: 5,
        failed_calls: 3,
        recent_actions: 6,
        refusals_by_code: BTreeMap::from([("price_above_max".into(), 3)]),
        ..StrategyHealth::default()
    };
    let start = 1_000_000;
    let mut observe = |health: &StrategyHealth, now, phase, decision_spent| {
        let fired = engine.observe(&Observation {
            now,
            minute: 600,
            open_minute: 555,
            phase,
            held: &[],
            fills: 0,
            ranking: &[],
            ranked_at: 0,
            decision_spent,
            research_budget_left: false,
            requests_queued: 0,
            rankers: &[],
            after_close_due: false,
            review_running: false,
            strategy_active: true,
            strategy_supervision: true,
            strategy_accepted: true,
            strategy_health: Some(health),
            strategy_fills: 0,
            regime: None,
        });
        while engine.next().is_some() {}
        fired
            .into_iter()
            .filter(|trigger| trigger.kind == TriggerKind::StrategyHealth)
            .map(|trigger| trigger.reason)
            .collect::<Vec<_>>()
    };
    const RUNNING_REASONS: [&str; 5] =
        ["idle", "losing", "below_expectation", "failing", "refusals"];
    for phase in [Phase::PreOpen, Phase::Closed, Phase::Sealed] {
        assert!(observe(&health, start, phase, 0.0).is_empty());
    }
    assert!(observe(&health, start, Phase::Open, 1.0).is_empty());
    assert_eq!(observe(&health, start, Phase::Open, 0.99), RUNNING_REASONS);

    let mut fallback = StrategyHealth {
        active_basis: HealthBasis::Fallback,
        eligible_strategy_id: Some("eligible".into()),
        ..StrategyHealth::default()
    };
    assert!(observe(&fallback, start + 1, Phase::Closed, 0.0).is_empty());
    assert!(observe(&fallback, start + 1, Phase::Open, 1.0).is_empty());
    assert_eq!(
        observe(&fallback, start + 1, Phase::Open, 0.0),
        ["fallback_while_eligible"]
    );
    assert!(observe(&health, start + cooldown - 1, Phase::Open, 0.0).is_empty());
    assert!(observe(&fallback, start + cooldown, Phase::Open, 0.0).is_empty());
    assert_eq!(
        observe(&health, start + cooldown, Phase::Open, 0.0),
        RUNNING_REASONS
    );

    fallback.active_basis = HealthBasis::Blocked;
    assert_eq!(
        observe(&fallback, start + cooldown + 1, Phase::Open, 0.0),
        ["fallback_while_eligible"]
    );
    fallback.active_basis = HealthBasis::StoodDown;
    assert!(observe(&fallback, start + 2 * cooldown + 1, Phase::Open, 0.0).is_empty());

    health.session_net_pnl_bps = Some(0.0);
    health.replay_expected_bps = None;
    health.fresh_prospects = false;
    health.completed_calls = 4;
    health.failed_calls = 4;
    health.recent_actions = 4;
    health.cash_drawdown_bps = Some(150.0);
    assert_eq!(
        observe(&health, start + 2 * cooldown + 1, Phase::PreClose, 0.0),
        ["losing"]
    );
}
