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
