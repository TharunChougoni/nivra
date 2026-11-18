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
fn drift_and_seal_fire_once_and_an_agenda_runs_in_research() {
    let mut engine = TriggerEngine::new(TriggerSettings::default());
    let rankers = [RankerStats {
        name: "test-ranker".into(),
        live_hit_rate: 0.3,
        backtest_hit_rate: 0.6,
        samples: 20,
    }];
    let observation = Observation {
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
    };
    let fired = engine.observe(&observation);
    assert_eq!(kinds(&fired), [TriggerKind::Drift, TriggerKind::Seal]);
    assert_eq!(
        fired[0].instrument_key.as_deref(),
        Some("ranker:test-ranker")
    );
    assert!(engine.observe(&observation).is_empty());
    assert_eq!(engine.next().unwrap().0, Lane::Research);
    assert_eq!(engine.next().unwrap().0, Lane::Review);
    engine.push(Trigger::new(
        TriggerKind::Agenda,
        "review_agenda",
        observation.now,
    ));
    let (lane, batch) = engine.next().unwrap();
    assert_eq!(lane, Lane::Research);
    assert_eq!(kinds(&batch), [TriggerKind::Agenda]);
}
