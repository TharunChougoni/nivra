//! Behavior tests for the research lane (#237-#240): research never blocks,
//! parks or charges the trading lane, and its queue survives crashes without
//! replaying a provider call. Also the owner agenda turns (#229) and bounded
//! decision prompt reconstruction (#230).
use super::super::lane_queue::{self, MessageState, TransitionReason};
use super::super::owner_tasks::{
    self, OwnerTask, ResearchEvidenceKind, ResearchEvidenceV1, ResearchIdentity,
    ResearchLaneSettings, ResearchOutputV1,
};
use super::*;
use serde_json::json;

const END: i64 = 100_000_000;

fn fixture(dir: &std::path::Path, lane: ResearchLaneSettings) -> (Config, PlannerConfig) {
    let (c, mut cfg) = super::super::decision_owner::owner_fixture_config(dir);
    cfg.owner_tasks = Some(owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: owner_tasks::ReviewSettings::default(),
        research: Some(owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: Some(lane),
        }),
    });
    cfg.workspace = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
    (c, cfg)
}

fn candidate(session: &str) -> ResearchEvidenceV1 {
    ResearchEvidenceV1 {
        version: 1,
        kind: ResearchEvidenceKind::CandidateResult,
        source_session: session.into(),
        task_id: "a".repeat(64),
        revision: 1,
        result_sha256: "b".repeat(64),
        lifecycle_candidate_id: Some("c".repeat(64)),
        maturity_state: None,
        workspace_refs: Vec::new(),
    }
}

fn requested(c: &Config, id: &str) -> ResearchEvidenceV1 {
    let db = lane_queue::open(&c.directory).unwrap();
    let message = lane_queue::load(&db, id).unwrap().unwrap();
    ResearchEvidenceV1 {
        version: 1,
        kind: ResearchEvidenceKind::TradingRequest,
        source_session: c.session.to_string(),
        task_id: id.into(),
        revision: 1,
        result_sha256: message.body.sha256().unwrap(),
        lifecycle_candidate_id: None,
        maturity_state: None,
        workspace_refs: Vec::new(),
    }
}

fn lane_turn(c: &Config, seq: u64, evidence: ResearchEvidenceV1, started: i64) -> session::Turn {
    let session = c.session.to_string();
    let identity = ResearchIdentity {
        version: 1,
        session: session.clone(),
        evidence_id: evidence.evidence_id().unwrap(),
        policy_sha256: "e".repeat(64),
        prompt_version: digest(owner_tasks::RESEARCH_PROMPT.as_bytes()),
    };
    let id = format!("research-{session}-{seq}");
    session::Turn {
        id: id.clone(),
        session,
        trigger: "research".into(),
        prompt_version: identity.prompt_version.clone(),
        notes_version: 0,
        fills_at_start: 0,
        started_at_ms: started,
        request: json!({
            "lane":"research",
            "owner_task":OwnerTask::Research { identity: identity.clone(), evidence },
            "turn_id":id,
            "research_id":identity.id().unwrap(),
            "model_id":"grok-4.7",
            "cutoff_ms":END * 2,
        }),
    }
}

fn claim(c: &Config, id: &str, turn_id: &str) {
    let mut db = lane_queue::open(&c.directory).unwrap();
    assert!(
        lane_queue::transition(
            &mut db,
            id,
            MessageState::Queued,
            MessageState::Running,
            TransitionReason::Claimed,
            Some(turn_id),
            1_500,
        )
        .unwrap()
    );
}

fn request_state(c: &Config, id: &str) -> (MessageState, Option<TransitionReason>) {
    let db = lane_queue::open(&c.directory).unwrap();
    let message = lane_queue::load(&db, id).unwrap().unwrap();
    (message.state, message.reason)
}

/// Failure modes: research holding the trading slot or log, a research
/// budget exhaustion parking trading, a research failure arming the trading
/// failure streak/backoff, uncapped research turns or tokens, research
/// running during market hours by default, and back-to-back research turns.
#[test]
fn research_lane_failures_and_caps_never_touch_trading() {
    let dir = tempfile::tempdir().unwrap();
    let lane = ResearchLaneSettings {
        min_interval_ms: 0,
        max_turns_per_session: 1,
        ..Default::default()
    };
    let (c, cfg) = fixture(dir.path(), lane);
    let root = c.directory.as_path();
    let session = c.session.to_string();
    with_state(root, &session, |_| {}).unwrap();
    // #233: an unreadable lane or trading log is never mistaken for idle.
    let session_dir = root.join(session::ROOT_PREFIX).join(&session);
    let garbage = session_dir.join("research-lane/log.bin");
    fs::create_dir_all(garbage.parent().unwrap()).unwrap();
    fs::write(&garbage, b"\xff\xffnot a session log frame").unwrap();
    assert_eq!(
        research::schedule(&c, &cfg, 2_000, END, true, None)
            .unwrap_err()
            .to_string(),
        "research_lane_log_unavailable"
    );
    let mut refused = research::Lane::default();
    research::tick_lane(&c, &cfg, &mut refused, 2_000, END, true, None);
    assert!(refused.worker.is_none());
    fs::remove_file(&garbage).unwrap();
    let trading_garbage = session_dir.join("log.bin");
    fs::write(&trading_garbage, b"\xff\xffnot a session log frame").unwrap();
    let corrupt = session::SessionLog::new(root, &session);
    assert!(review::schedule_input(&c, &corrupt, None, true, 2_000, END, END, None).busy);
    fs::remove_file(&trading_garbage).unwrap();
    let idle = session::SessionLog::new(root, &session);
    assert!(!review::schedule_input(&c, &idle, None, true, 2_000, END, END, None).busy);
    let turn = lane_turn(&c, 1, candidate(&session), 1_000);
    let lane_log = research::lane_log(root, &session);
    lane_log
        .append(1_000, session::Body::Turn { turn: turn.clone() })
        .unwrap();
    // In flight, research is pending in its own log; trading's log is free.
    let schedule = research::schedule(&c, &cfg, 2_000, END, true, None).unwrap();
    assert_eq!(schedule.research_state, "pending");
    assert!(session::SessionLog::new(root, &session).pending().is_none());

    let (sender, receiver) = mpsc::sync_channel(1);
    let mut slot = research::Lane::default();
    slot.worker = Some(test_worker_with(
        receiver,
        &session,
        &turn.id,
        RunKind::Research,
    ));
    assert!(!research::poll_lane(root, &mut slot, 2_000));
    sender.send(Err("planner_budget_exhausted".into())).unwrap();
    assert!(research::poll_lane(root, &mut slot, 3_000));
    assert!(slot.worker.is_none());
    let result = lane_log.result_for(&turn.id).unwrap();
    assert_eq!(result.outcome, "failed");
    assert_eq!(
        result.reason.as_deref(),
        Some("research_lane_call_budget_exhausted")
    );
    let failure: Value =
        serde_json::from_slice(&fs::read(root.join("model/research-lane-failure.json")).unwrap())
            .unwrap();
    assert_eq!(failure["reason"], "research_lane_call_budget_exhausted");
    assert_eq!(failure["turn_id"], turn.id.as_str());

    // Trading state, failure record and log are untouched.
    let state = super::state::load(root, &session).unwrap();
    assert!(state.parked.is_none());
    assert_eq!(state.failures, 0);
    assert_eq!(state.owner_failure_streak, None);
    assert_eq!(state.last_failure_ms, None);
    assert!(!root.join("model/supervisor-failure.json").exists());
    assert!(
        session::SessionLog::new(root, &session)
            .records()
            .is_empty()
    );

    // Caps park only the lane, with the lane's own reason.
    let capped = research::schedule(&c, &cfg, 4_000, END, true, None).unwrap();
    assert_eq!(
        (capped.research_state, capped.research_reason),
        (
            owner_tasks::ResearchState::Parked,
            Some("research_lane_turn_budget_exhausted")
        )
    );
    let halted = research::schedule(&c, &cfg, 4_000, END, false, None).unwrap();
    assert_eq!(
        halted.research_reason,
        Some("research_lane_book_not_runnable")
    );

    // Turn spacing and the market-hours window defer, never park.
    let spaced_dir = tempfile::tempdir().unwrap();
    let (spaced_c, spaced_cfg) = fixture(
        spaced_dir.path(),
        ResearchLaneSettings {
            min_interval_ms: 3_600_000,
            ..Default::default()
        },
    );
    let spaced_session = spaced_c.session.to_string();
    let spaced_turn = lane_turn(&spaced_c, 1, candidate(&spaced_session), 1_000);
    let spaced_log = research::lane_log(&spaced_c.directory, &spaced_session);
    spaced_log
        .append(
            1_000,
            session::Body::Turn {
                turn: spaced_turn.clone(),
            },
        )
        .unwrap();
    spaced_log
        .append(
            2_000,
            session::Body::Result {
                result: session::TurnResult {
                    turn_id: spaced_turn.id.clone(),
                    finished_at_ms: 2_000,
                    outcome: "failed".into(),
                    reason: Some("owner_research_failed".into()),
                    reply: String::new(),
                    actions: Vec::new(),
                },
            },
        )
        .unwrap();
    let spaced = research::schedule(&spaced_c, &spaced_cfg, 5_000, END, true, None).unwrap();
    assert_eq!(
        (
            spaced.research_state,
            spaced.research_reason,
            spaced.deferred_until_ms
        ),
        (
            owner_tasks::ResearchState::Deferred,
            Some("research_lane_min_interval"),
            Some(3_601_000)
        )
    );
    let market_dir = tempfile::tempdir().unwrap();
    let (market_c, market_cfg) = fixture(market_dir.path(), ResearchLaneSettings::default());
    let open = research::schedule(&market_c, &market_cfg, END - 1_000, END, true, None).unwrap();
    assert_eq!(
        (
            open.research_state,
            open.research_reason,
            open.deferred_until_ms
        ),
        (
            owner_tasks::ResearchState::Deferred,
            Some("research_lane_market_hours"),
            Some(END)
        )
    );
    let (_, opted_in_cfg) = fixture(
        market_dir.path(),
        ResearchLaneSettings {
            during_market: true,
            ..Default::default()
        },
    );
    let opted_in =
        research::schedule(&market_c, &opted_in_cfg, END - 1_000, END, true, None).unwrap();
    assert_eq!(opted_in.research_state, "awaiting_evidence");

    // Research tokens are counted from the research bucket only.
    let journal = dir.path().join("journal");
    fs::create_dir_all(&journal).unwrap();
    for (name, tokens_in, tokens_out) in [
        ("research-0001.result.json", 700, 300),
        ("research-0002.result.json", 1_000, 0),
        ("supervisor-0001.result.json", 50_000, 9_000),
    ] {
        fs::write(
            journal.join(name),
            json!({"transport_evidence":{"tokens_in":tokens_in,"tokens_out":tokens_out}})
                .to_string(),
        )
        .unwrap();
    }
    // A completed call written before its usage reached transport evidence
    // still counts from the provider envelope (#241).
    fs::write(
        journal.join("research-0004.result.json"),
        json!({"transport_evidence":{"stage":"output_validation"},
            "output":json!({"output":[],"usage":{"input_tokens":400,"output_tokens":100}}).to_string()})
        .to_string(),
    )
    .unwrap();
    fs::write(journal.join("research-0003.attempt.json"), "{}").unwrap();
    assert_eq!(planner::bucket_tokens(&journal, "research").unwrap(), 2_500);
    assert_eq!(
        planner::bucket_tokens(&journal, "supervisor").unwrap(),
        59_000
    );
}

/// Failure modes: a crash after claiming a request (with or without the
/// durable turn) leaving it running forever, a recovered turn re-calling the
/// provider, a re-posted request being served twice, results lost between the
/// lane log and the ledger, and a result delivered to trading more than once.
#[test]
fn research_lane_recovers_claims_without_replay_and_delivers_results_once() {
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = fixture(
        dir.path(),
        ResearchLaneSettings {
            min_interval_ms: 0,
            ..Default::default()
        },
    );
    let root = c.directory.as_path();
    let session = c.session.to_string();
    let lane_log = research::lane_log(root, &session);
    let post = |turn: &str, question: &str| {
        lane_queue::post_request(&c, &cfg, turn, question, Vec::new(), 1_000).unwrap()["request_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    // Request 1: claimed and its turn durable, then the process died.
    let first = post("turn-trading-1", "Backtest the gap fade on today's movers.");
    let crashed = lane_turn(&c, 1, requested(&c, &first), 1_500);
    claim(&c, &first, &crashed.id);
    lane_log
        .append(
            1_500,
            session::Body::Turn {
                turn: crashed.clone(),
            },
        )
        .unwrap();
    // Request 2: claimed by a turn that never became durable.
    let second = post("turn-trading-1", "Replay yesterday's exits.");
    claim(&c, &second, &format!("research-{session}-9"));

    let mut slot = research::Lane::default();
    research::tick_lane(&c, &cfg, &mut slot, 2_000, END, true, None);
    assert!(
        slot.worker.is_none(),
        "recovery never starts a provider call"
    );
    assert!(!root.join("model/usage").exists());
    assert_eq!(
        lane_log.result_for(&crashed.id).unwrap().outcome,
        "interrupted"
    );
    for id in [&first, &second] {
        assert_eq!(
            request_state(&c, id),
            (MessageState::Failed, Some(TransitionReason::Interrupted))
        );
    }
    research::tick_lane(&c, &cfg, &mut slot, 2_500, END, true, None);
    assert!(slot.worker.is_none());
    // Trading re-posting the same question is deduped, never served again.
    let replay = lane_queue::post_request(
        &c,
        &cfg,
        "turn-trading-1",
        "Backtest the gap fade on today's movers.",
        Vec::new(),
        2_600,
    )
    .unwrap();
    assert_eq!(replay["deduped"], true);
    assert_eq!(replay["state"], "failed");

    // Request 3 completed, but the process died before the ledger was settled.
    let third = post("turn-trading-2", "Is the opening-range edge real?");
    let evidence = requested(&c, &third);
    let output = ResearchOutputV1 {
        version: 1,
        evidence_id: evidence.evidence_id().unwrap(),
        summary: "Opening-range edge holds on 40 sessions.".into(),
        observations: vec!["raw trace stays in the artifact".into()],
        cited_ids: Vec::new(),
        workspace_refs: Vec::new(),
    };
    let done = lane_turn(&c, 3, evidence, 2_700);
    claim(&c, &third, &done.id);
    lane_log
        .append(2_700, session::Body::Turn { turn: done.clone() })
        .unwrap();
    lane_log
        .append(
            2_800,
            session::Body::Result {
                result: session::TurnResult {
                    turn_id: done.id.clone(),
                    finished_at_ms: 2_800,
                    outcome: "ok".into(),
                    reason: None,
                    reply: serde_json::to_string(&output).unwrap(),
                    actions: Vec::new(),
                },
            },
        )
        .unwrap();
    let mut restarted = research::Lane::default();
    research::tick_lane(&c, &cfg, &mut restarted, 3_000, END, true, None);
    assert!(restarted.worker.is_none());
    assert_eq!(
        request_state(&c, &third),
        (MessageState::Done, Some(TransitionReason::Completed))
    );
    let updates = lane_queue::trading_updates(&c, 3_000).unwrap();
    assert_eq!(updates.len(), 1);
    let fact = lane_queue::update_fact(&updates[0]);
    assert_eq!(fact["kind"], "research_result");
    assert_eq!(fact["request_id"], third.as_str());
    assert_eq!(fact["summary"], "Opening-range edge holds on 40 sessions.");
    assert!(
        fact["artifact_ref"]
            .as_str()
            .unwrap()
            .starts_with("model/research/")
    );
    assert!(!fact.to_string().contains("raw trace"));
    lane_queue::mark_delivered(&c, &[updates[0].id.clone()], "turn-trading-3", 3_100).unwrap();
    assert!(lane_queue::trading_updates(&c, 3_200).unwrap().is_empty());
    research::tick_lane(
        &c,
        &cfg,
        &mut research::Lane::default(),
        3_300,
        END,
        true,
        None,
    );
    assert!(
        lane_queue::trading_updates(&c, 3_300).unwrap().is_empty(),
        "settling again never re-delivers"
    );
}

#[test]
fn agenda_turns_are_typed_once_per_session_and_bounds_share_one_source() {
    use owner_tasks::{AgendaInput, AgendaSettings, AgendaState, AgendaTurn, project_agenda};
    // #229: pre-open and pre-close owner turns are admitted from one typed
    // projection, run once each and yield to blocked books.
    let both = AgendaSettings {
        pre_open: true,
        pre_close: true,
    };
    let base = AgendaInput {
        settings: Some(both),
        in_session: true,
        book_runnable: true,
        minute: 500,
        open_minute: 555,
        pre_close_minute: 910,
        open_done: false,
        pre_close_done: false,
        cooled: true,
        budget_left: true,
    };
    let project = |input: AgendaInput| project_agenda(&input);
    assert_eq!(
        project(AgendaInput {
            settings: None,
            ..base
        }),
        AgendaState::Disabled
    );
    assert_eq!(
        project(AgendaInput {
            settings: Some(AgendaSettings::default()),
            ..base
        }),
        AgendaState::Disabled
    );
    assert_eq!(project(base), AgendaState::Waiting);
    let open = AgendaInput {
        minute: 555,
        ..base
    };
    assert_eq!(project(open), AgendaState::Due(AgendaTurn::PreOpen));
    assert_eq!(AgendaTurn::PreOpen.reason(), "session_open");
    assert_eq!(AgendaTurn::PreClose.reason(), "pre_close");
    for blocked in [
        AgendaInput {
            in_session: false,
            ..open
        },
        AgendaInput {
            book_runnable: false,
            ..open
        },
        AgendaInput {
            cooled: false,
            ..open
        },
        AgendaInput {
            budget_left: false,
            ..open
        },
    ] {
        assert_eq!(project(blocked), AgendaState::Blocked);
    }
    let close_only = AgendaInput {
        settings: Some(AgendaSettings {
            pre_open: false,
            pre_close: true,
        }),
        ..open
    };
    assert_eq!(project(close_only), AgendaState::Waiting);
    // A missed open is skipped once pre-close is due.
    assert_eq!(
        project(AgendaInput {
            minute: 910,
            ..base
        }),
        AgendaState::Due(AgendaTurn::PreClose)
    );

    // Terminal outcome: only a successful agenda turn marks its slot done.
    let dir = tempfile::tempdir().unwrap();
    let (c, mut cfg) = fixture(dir.path(), ResearchLaneSettings::default());
    let root = c.directory.as_path();
    let session = c.session.to_string();
    with_state(root, &session, |_| {}).unwrap();
    let hashes = AppliedHashes::current(&cfg);
    let finish_agenda = |reason: &str, ok: bool| {
        finish(
            root,
            &session,
            RunKind::Supervisor,
            reason,
            1_000,
            ok,
            &hashes,
            None,
        );
        super::state::load(root, &session).unwrap()
    };
    let failed = finish_agenda("session_open", false);
    assert!(!failed.open_fired);
    let done = finish_agenda("session_open", true);
    assert!(done.open_fired && !done.pre_close_fired);
    let after_open = AgendaInput {
        minute: 600,
        open_done: done.open_fired,
        ..base
    };
    assert_eq!(project(after_open), AgendaState::Waiting);
    let closed = finish_agenda("pre_close", true);
    assert_eq!(
        project(AgendaInput {
            minute: 920,
            open_done: closed.open_fired,
            pre_close_done: closed.pre_close_fired,
            ..base
        }),
        AgendaState::Done
    );

    // Settings: absent keeps policy bytes, unknown keys and a non-owner policy
    // are refused.
    let tasks = cfg.owner_tasks.clone().unwrap();
    assert!(
        serde_json::to_value(&tasks)
            .unwrap()
            .get("agenda")
            .is_none()
    );
    assert!(serde_json::from_value::<AgendaSettings>(json!({"pre_open":true,"extra":1})).is_err());
    let mut agenda_tasks = tasks.clone();
    agenda_tasks.research = None;
    agenda_tasks.agenda = Some(both);
    assert!(agenda_tasks.validate(&cfg).is_ok());
    assert_eq!(
        agenda_tasks
            .validate(&PlannerConfig::default())
            .unwrap_err()
            .to_string(),
        "owner_agenda_requires_decision_owner"
    );

    // Typed schedule states serialize to the same public strings.
    for state in [
        owner_tasks::ReviewState::Disabled,
        owner_tasks::ReviewState::AwaitingSealedReport,
        owner_tasks::ReviewState::Due,
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(state.as_str()));
    }
    for state in [
        owner_tasks::ResearchState::AwaitingEvidence,
        owner_tasks::ResearchState::Pending,
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(state.as_str()));
    }

    // #234: cutoff floor, capture limits and liveness share named sources.
    assert_eq!(
        owner_tasks::MIN_CUTOFF_MINUTE,
        sadr_core::execution::DEFAULT_SESSION_END_MINUTE + 1
    );
    let research_settings = tasks.research.clone().unwrap();
    let mut early = research_settings.clone();
    early.cutoff_minute = sadr_core::execution::DEFAULT_SESSION_END_MINUTE;
    assert!(early.validate(&cfg).is_err());
    assert_eq!(
        research::settings_cutoff(&c, &cfg, END).unwrap(),
        END + i64::from(1020 - c.execution.session_end_minute) * 60_000
    );
    let mut late_close = c.clone();
    late_close.execution.session_end_minute = 1020;
    assert_eq!(
        research::settings_cutoff(&late_close, &cfg, END)
            .unwrap_err()
            .to_string(),
        "owner_research_cutoff_invalid"
    );
    let capture = owner_tasks::ControllerObservationCaptureSettings {
        max_rows: owner_tasks::OBSERVATION_CAPTURE_MAX_ROWS + 1,
        ..Default::default()
    };
    assert!(capture.validate().is_err());
    let capture = owner_tasks::ControllerObservationCaptureSettings {
        max_rows: owner_tasks::OBSERVATION_CAPTURE_MAX_ROWS,
        ..Default::default()
    };
    assert!(capture.validate().is_ok());
    assert!(c.validate().is_ok());
    let mut slow = c.clone();
    slow.idle_poll_interval_ms = super::super::config::MAX_IDLE_POLL_INTERVAL_MS + 1;
    assert!(slow.validate().is_err());
    cfg.owner_tasks = None;
}

#[test]
fn decision_prompts_reference_owner_task_turns_without_replaying_them() {
    // #230: review and research turns (including legacy research written to
    // the trading log) are kept as short references in decision prompts.
    let dir = tempfile::tempdir().unwrap();
    let (c, _cfg) = fixture(dir.path(), ResearchLaneSettings::default());
    let root = c.directory.as_path();
    let session = c.session.to_string();
    let log = session::SessionLog::new(root, &session);
    let bulky = "n".repeat(20_000);
    let mut at = 1_000;
    for seq in 0..4 {
        let mut turn = lane_turn(&c, seq + 1, candidate(&session), at);
        turn.id = format!("legacy-research-{seq}");
        turn.request["notes"] = json!(bulky);
        turn.request["evidence_payload"] = json!(bulky);
        log.append(at, session::Body::Turn { turn: turn.clone() })
            .unwrap();
        log.append(
            at + 1,
            session::Body::Result {
                result: session::TurnResult {
                    turn_id: turn.id.clone(),
                    finished_at_ms: at + 1,
                    outcome: "ok".into(),
                    reason: None,
                    reply: bulky.clone(),
                    actions: vec![session::action(
                        "research_artifact",
                        &format!("artifact-{seq}"),
                        &json!({"seq":seq}),
                    )],
                },
            },
        )
        .unwrap();
        at += 10;
    }
    let review = session::Turn {
        id: "review-1".into(),
        session: session.clone(),
        trigger: "review".into(),
        prompt_version: "p".into(),
        notes_version: 0,
        fills_at_start: 0,
        started_at_ms: at,
        request: json!({"owner_task":{"kind":"review","identity":{"session":session}},
            "sealed_evidence":bulky}),
    };
    log.append(at, session::Body::Turn { turn: review })
        .unwrap();
    log.append(
        at + 1,
        session::Body::Result {
            result: session::TurnResult {
                turn_id: "review-1".into(),
                finished_at_ms: at + 1,
                outcome: "ok".into(),
                reason: None,
                reply: bulky.clone(),
                actions: Vec::new(),
            },
        },
    )
    .unwrap();
    let conversation = log.conversation();
    let text = conversation.to_string();
    assert!(text.len() < 8_000, "conversation grew to {}", text.len());
    assert!(!text.contains(&"n".repeat(64)));
    let exchanges = conversation["exchanges"].as_array().unwrap();
    assert_eq!(exchanges.len(), 5);
    assert_eq!(exchanges[0]["turn"]["trigger"], "research");
    assert_eq!(
        exchanges[0]["turn"]["request"]["owner_task"]["kind"],
        "research"
    );
    assert_eq!(exchanges[0]["result"]["reply_omitted"], true);
    assert_eq!(exchanges[0]["result"]["reply_bytes"], 20_000);
    assert_eq!(exchanges[0]["result"]["actions"][0]["id"], "artifact-0");
    assert_eq!(exchanges[4]["turn"]["trigger"], "review");
    // The durable log still holds the full committed results.
    assert_eq!(log.result_for("review-1").unwrap().reply, bulky);
}
