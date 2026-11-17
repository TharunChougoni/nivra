//! The trading lane's only path into the research lane (#239).
use super::*;
use serde_json::json;

fn lane_cfg(dir: &std::path::Path, lane: bool) -> (Config, PlannerConfig) {
    let (c, mut cfg) = super::super::decision_owner::owner_fixture_config(dir);
    cfg.owner_tasks = Some(super::super::owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: super::super::owner_tasks::ReviewSettings::default(),
        research: Some(super::super::owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: lane,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: lane.then(Default::default),
        }),
    });
    cfg.workspace = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
    (c, cfg)
}

fn offered(owner: OwnerTools) -> bool {
    call_tool_definitions(
        "supervisor",
        &[],
        false,
        &planner::ToolsSettings::default(),
        owner,
    )
    .unwrap()
    .iter()
    .any(|tool| tool["name"] == "request_research")
}

/// Failure modes: a research or digest turn queueing research for itself
/// (self-feeding), a request tool advertised while the lane is off, an
/// unbound turn posting requests, and one decision turn flooding the queue
/// with duplicate questions.
#[test]
fn only_a_bound_decision_turn_may_queue_lane_research() {
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = lane_cfg(dir.path(), true);
    let decision =
        json!({"trigger":{"kind":"decision"},"summary":{"host_decision":{"turn_id":"turn-1"}}});
    let research = json!({"trigger":{"kind":"research"},"summary":{"owner_task":{"kind":"research"},"turn_id":"turn-1"}});
    let bound = planner::CallGuard {
        invocation_id: Some("turn-1"),
        ..planner::preview_guard()
    };
    let unbound = planner::preview_guard();
    let tools = owner_tools_for_call(&cfg, "supervisor", &decision, &bound);
    assert!(tools.research_request);
    assert!(offered(tools));
    for (name, bundle, guard) in [
        ("supervisor", &research, &bound),
        ("supervisor", &decision, &unbound),
        ("digest", &decision, &bound),
    ] {
        let tools = owner_tools_for_call(&cfg, name, bundle, guard);
        assert!(!tools.research_request, "{name} {bundle}");
    }
    assert!(!offered(OwnerTools::default()));
    let (_, off) = lane_cfg(dir.path(), false);
    assert!(!owner_tools_for_call(&off, "supervisor", &decision, &bound).research_request);
    assert_eq!(
        super::super::lane_queue::post_request(&c, &off, "turn-1", "q", Vec::new(), 1_000)
            .unwrap_err()
            .to_string(),
        "research_lane_disabled"
    );

    let question = "Does the gap fade survive costs?";
    let first =
        super::super::lane_queue::post_request(&c, &cfg, "turn-1", question, Vec::new(), 1_000)
            .unwrap();
    assert_eq!(first["state"], "queued");
    assert_eq!(first["deduped"], false);
    let again =
        super::super::lane_queue::post_request(&c, &cfg, "turn-1", question, Vec::new(), 1_001)
            .unwrap();
    assert_eq!(again["deduped"], true);
    assert_eq!(again["request_id"], first["request_id"]);
    for n in 0..3 {
        super::super::lane_queue::post_request(
            &c,
            &cfg,
            "turn-1",
            &format!("question {n}"),
            Vec::new(),
            1_002,
        )
        .unwrap();
    }
    let flood = super::super::lane_queue::post_request(
        &c,
        &cfg,
        "turn-1",
        "one too many",
        Vec::new(),
        1_003,
    )
    .unwrap_err();
    assert_eq!(flood.to_string(), "lane_request_capacity");
}

/// Failure modes: research enabled without a capped lane profile (uncapped
/// research), out-of-range caps, and a shipped config that silently turns the
/// lane on.
#[test]
fn research_lane_profile_is_required_bounded_and_off_by_default() {
    use super::super::owner_tasks::ResearchLaneSettings;
    let dir = tempfile::tempdir().unwrap();
    let (_, mut cfg) = lane_cfg(dir.path(), true);
    let research =
        |cfg: &PlannerConfig| cfg.owner_tasks.as_ref().unwrap().research.clone().unwrap();
    assert!(research(&cfg).validate(&cfg).is_ok());
    assert!(super::super::lane_queue::lane_settings(&cfg).is_some());
    let mut missing = research(&cfg);
    missing.lane = None;
    assert_eq!(
        missing.validate(&cfg).unwrap_err().to_string(),
        "research_lane_required"
    );
    for bad in [
        ResearchLaneSettings {
            max_turns_per_session: 0,
            ..Default::default()
        },
        ResearchLaneSettings {
            max_tokens_per_session: 10,
            ..Default::default()
        },
        ResearchLaneSettings {
            max_calls_per_session: 0,
            ..Default::default()
        },
        ResearchLaneSettings {
            model_id: Some(String::new()),
            ..Default::default()
        },
        ResearchLaneSettings {
            max_tool_turns: 257,
            ..Default::default()
        },
        ResearchLaneSettings {
            timeout_ms: 21_600_001,
            ..Default::default()
        },
    ] {
        let mut invalid = research(&cfg);
        invalid.lane = Some(bad);
        assert_eq!(
            invalid.validate(&cfg).unwrap_err().to_string(),
            "research_lane_settings_invalid"
        );
    }
    // Long research runs fit (#242): 96..256 tool turns and up to six hours.
    let mut long = research(&cfg);
    long.lane = Some(ResearchLaneSettings {
        max_tool_turns: 256,
        timeout_ms: 21_600_000,
        max_calls_per_session: 150,
        max_turns_per_session: 12,
        max_tokens_per_session: 12_000_000,
        ..Default::default()
    });
    assert!(long.validate(&cfg).is_ok());
    // The idle trigger (#244) is off by default, bounded, and needs the lane.
    let parsed: super::super::owner_tasks::OwnerResearchSettings =
        serde_json::from_value(json!({"idle_trigger":{"enabled":true}})).unwrap();
    assert_eq!(parsed.idle_trigger.unwrap().quiet_minutes, 30);
    for (enabled, minutes, lane, ok) in [
        (true, 30, true, true),
        (false, 30, false, true),
        (true, 30, false, false),
        (true, 0, true, false),
        (false, 376, true, false),
    ] {
        let mut idle = research(&cfg);
        idle.idle_trigger = Some(super::super::owner_tasks::IdleTriggerSettings {
            enabled,
            quiet_minutes: minutes,
        });
        if !lane {
            idle.lane = None;
            idle.enabled = false;
        }
        assert_eq!(
            idle.validate(&cfg).is_ok(),
            ok,
            "{enabled} {minutes} {lane}"
        );
    }
    // The lane profile round-trips from config JSON with documented defaults.
    let parsed: ResearchLaneSettings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(parsed, ResearchLaneSettings::default());
    assert!(!parsed.during_market);
    // Absent owner tasks (every shipped config) keep the lane off.
    cfg.owner_tasks = None;
    assert!(super::super::lane_queue::lane_settings(&cfg).is_none());
    let shipped: Value = serde_json::from_str(include_str!(
        "../../../../../packaging/rust/planner-current.example.json"
    ))
    .unwrap();
    assert_ne!(shipped["owner_tasks"]["research"]["enabled"], true);
}

#[test]
fn registered_research_waits_only_for_the_book_and_a_pending_owner_decision() {
    // #236: the launch gate takes no model-worker input, so a busy scorer,
    // aside, review or supervisor turn can never starve registered research.
    let ready = ResearchLaunch::for_book(LaunchBook::default());
    assert_eq!(
        ready,
        ResearchLaunch {
            registered: true,
            candidate: true,
            reason: "ready",
        }
    );
    // A queued or in-flight owner decision still keeps registered research
    // out of the way; candidate compute is unaffected, as before.
    let owner = ResearchLaunch::for_book(LaunchBook {
        owner_decision_pending: true,
        ..Default::default()
    });
    assert!(!owner.registered && owner.candidate);
    assert_eq!(owner.reason, "owner_decision_pending");
    for (book, reason) in [
        (
            LaunchBook {
                halted: true,
                ..Default::default()
            },
            "book_halted",
        ),
        (
            LaunchBook {
                stopped: true,
                ..Default::default()
            },
            "book_stopped",
        ),
        (
            LaunchBook {
                context_unavailable: true,
                ..Default::default()
            },
            "controller_session_end_unavailable",
        ),
    ] {
        let launch = ResearchLaunch::for_book(book);
        assert!(!launch.registered && !launch.candidate, "{reason}");
        assert_eq!(launch.reason, reason);
    }
}
