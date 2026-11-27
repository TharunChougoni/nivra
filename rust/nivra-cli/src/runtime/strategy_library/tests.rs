use super::acceptance::{Reason, Thresholds, accept};
use super::review::{EntryReview, review_policy_sha256};
use super::*;

const SOURCE: &[u8] = b"def on_observation(obs):\n    return {'actions': []}\n";

fn review_for(source: &[u8], now: i64) -> EntryReview {
    EntryReview {
        verdict: "safe".into(),
        model: "fixture".into(),
        source_sha256: digest(source),
        runner_sha256: crate::runtime::strategy::runner_sha256(),
        policy_sha256: review_policy_sha256(),
        reasons: Vec::new(),
        reviewed_at_ms: now,
    }
}

/// A planner policy with these fallbacks; `tag` stands in for its digest.
fn policy(fallback: &[&str], tag: char) -> choice::ChoicePolicy {
    choice::ChoicePolicy {
        fallback: fallback.iter().map(|f| (*f).to_owned()).collect(),
        policy_sha256: tag.to_string().repeat(64),
        python_path: std::path::PathBuf::from("/usr/bin/python3"),
        allow_research_env: false,
    }
}

/// A report exactly at every default bound: 10 trades, net P&L just above
/// zero, a 300 bps drawdown on 5,000 capital, three tape dates and 1 failure in
/// 100 calls.
fn report(edit: impl FnOnce(&mut Value)) -> (Vec<u8>, String) {
    let mut value = json!({
        "abi":"strategy_actions_v1","report_version":1,"paper_only":true,
        "source_sha256":digest(SOURCE),"parameters":{"version":1,"params":{"n":1}},
        "capital":"5000",
        "tape":{"dates":["2026-11-23","2026-11-25","2026-11-26"],"first_at_ms":1,"last_at_ms":2,"events":200,
                "depth_events":100,"strategy_calls":100},
        "summary":{"trades":10,"wins":6,"win_rate":"0.6","net_pnl":"0.01","median_trade":"0.5",
                   "worst_drawdown":"150","fees":"9","turnover":"40000","ending_cash":"5000.01",
                   "marked_equity":"5000.01","open_positions":0,"unmarked_positions":[]},
        "strategy_failures":{"strategy_wall_timeout":1},
    });
    edit(&mut value);
    let bytes = artifact_bytes(&value).unwrap();
    let sha = digest(&bytes);
    (bytes, sha)
}

#[test]
fn acceptance_names_each_threshold_and_integrity_failure() {
    let defaults = Thresholds::default();
    type Edit = fn(&mut Value);
    let rows: [(&str, Edit, &Thresholds, &[Reason]); 16] = [
        ("at every bound", |_| {}, &defaults, &[]),
        (
            "trades above",
            |r| r["summary"]["trades"] = json!(11),
            &defaults,
            &[],
        ),
        (
            "trades below",
            |r| r["summary"]["trades"] = json!(9),
            &defaults,
            &[Reason::TooFewTrades],
        ),
        (
            "net at zero",
            |r| r["summary"]["net_pnl"] = json!("0"),
            &defaults,
            &[Reason::NetPnlTooLow],
        ),
        (
            "net below",
            |r| r["summary"]["net_pnl"] = json!("-4.27"),
            &defaults,
            &[Reason::NetPnlTooLow],
        ),
        (
            "drawdown below",
            |r| r["summary"]["worst_drawdown"] = json!("149.99"),
            &defaults,
            &[],
        ),
        (
            "drawdown above",
            |r| r["summary"]["worst_drawdown"] = json!("150.01"),
            &defaults,
            &[Reason::DrawdownTooDeep],
        ),
        (
            "duplicate dates count once",
            |r| r["tape"]["dates"] = json!(["2026-11-23", "2026-11-25", "2026-11-25"]),
            &defaults,
            &[Reason::TooFewTapeDates],
        ),
        (
            "three distinct dates with a duplicate",
            |r| {
                r["tape"]["dates"] = json!(["2026-11-23", "2026-11-25", "2026-11-26", "2026-11-26"])
            },
            &defaults,
            &[],
        ),
        (
            "failures above",
            |r| r["strategy_failures"]["strategy_exit"] = json!(1),
            &defaults,
            &[Reason::TooManyStrategyFailures],
        ),
        (
            "still holding",
            |r| r["summary"]["open_positions"] = json!(1),
            &defaults,
            &[Reason::OpenPositionsAtEnd],
        ),
        (
            "several misses",
            |r| {
                r["summary"]["trades"] = json!(2);
                r["summary"]["net_pnl"] = json!("-1");
            },
            &defaults,
            &[Reason::TooFewTrades, Reason::NetPnlTooLow],
        ),
        (
            "not paper",
            |r| r["paper_only"] = json!(false),
            &defaults,
            &[Reason::ReportNotPaperOnly],
        ),
        (
            "other abi",
            |r| r["abi"] = json!("cash_quote_action_tape_v1"),
            &defaults,
            &[Reason::ReportAbiUnsupported],
        ),
        (
            "other source",
            |r| r["source_sha256"] = json!(digest(b"other")),
            &defaults,
            &[Reason::SourceHashMismatch],
        ),
        (
            "no summary",
            |r| r["summary"] = json!(null),
            &defaults,
            &[Reason::ReportInvalid],
        ),
    ];
    for (name, edit, thresholds, expected) in rows {
        let (bytes, sha) = report(edit);
        let got = accept(&bytes, &sha, SOURCE, thresholds)
            .err()
            .unwrap_or_default();
        assert_eq!(got, expected, "{name}");
    }
    // Stored bytes that do not hash to the recorded report hash never pass.
    let (mut bytes, sha) = report(|_| {});
    bytes.push(b' ');
    assert_eq!(
        accept(&bytes, &sha, SOURCE, &defaults).err(),
        Some(vec![Reason::ReportHashMismatch])
    );
    // The planner block fills omitted fields with defaults and refuses a
    // net P&L bar below zero.
    let parsed = super::super::planner::PlannerConfig::parse(
        br#"{"enabled":false,"strategy_acceptance":{"min_trades":3}}"#,
    )
    .unwrap();
    assert_eq!(
        parsed.strategy_acceptance,
        Some(Thresholds {
            min_trades: 3,
            ..Thresholds::default()
        })
    );
    assert!(
        super::super::planner::PlannerConfig::parse(
            br#"{"enabled":false,"strategy_acceptance":{"min_net_pnl":"-1"}}"#
        )
        .is_err()
    );
}

#[test]
fn a_current_build_replay_refreshes_a_stale_acceptance_under_the_same_strategy_id() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::at(dir.path().join("strategy-library"));
    let register = |engine: &str, adapter: &str, now| {
        let (bytes, sha) = report(|r| {
            r["engine_sha256"] = json!(engine);
            r["adapter_sha256"] = json!(adapter);
        });
        let accepted = accept(&bytes, &sha, SOURCE, &Thresholds::default()).unwrap();
        let rev = review_for(SOURCE, now);
        library
            .register(
                &accepted,
                &rev,
                Author {
                    lane: "research".into(),
                    model: None,
                    model_revision: None,
                    turn_id: None,
                    session: None,
                },
                now,
            )
            .unwrap()
    };
    let old = register("old-engine", "old-adapter", 1);
    let id = &old.entry.strategy_id;
    library
        .append(ControlRecord {
            at_ms: 2,
            control: Control::Disable {
                strategy_id: id.clone(),
            },
        })
        .unwrap();
    let refreshed = register(
        &nivra_core::replay::engine_fingerprint(),
        &super::super::strategy::replay::adapter_sha256(),
        3,
    );
    assert_eq!(&refreshed.entry.strategy_id, id);
    assert_ne!(refreshed.entry.report_sha256, old.entry.report_sha256);
    assert!(library.resolve().unwrap().disabled.contains(id));
    assert!(library.report(&old.entry).is_ok());
    assert_eq!(library.get(id).unwrap().unwrap(), refreshed.entry);
    assert_eq!(
        register("old-engine", "old-adapter", 4).entry,
        refreshed.entry
    );
}

#[test]
fn registration_is_immutable_and_controls_change_the_active_strategy() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::at(dir.path().join("strategy-library"));
    let author = Author {
        lane: "research".into(),
        model: Some("grok-4.7".into()),
        model_revision: Some(3),
        turn_id: Some("turn-1".into()),
        session: NaiveDate::from_ymd_opt(2026, 11, 25),
    };
    let register = |n: u64, at: i64| {
        let (bytes, sha) = report(|r| {
            r["parameters"]["params"] = json!({"n": n});
            r["engine_sha256"] = json!(nivra_core::replay::engine_fingerprint());
            r["adapter_sha256"] = json!(super::super::strategy::replay::adapter_sha256());
        });
        let accepted = accept(&bytes, &sha, SOURCE, &Thresholds::default()).unwrap();
        let rev = review_for(SOURCE, at);
        library
            .register(&accepted, &rev, author.clone(), at)
            .unwrap()
    };
    let first = register(1, 1_000);
    assert_eq!(first.status, RegistrationStatus::Registered);
    let path = dir
        .path()
        .join("strategy-library/entries")
        .join(format!("{}.json", first.entry.strategy_id));
    let written = fs::read(&path).unwrap();
    // Re-registering the same (source, params) changes nothing, even later.
    let again = register(1, 9_000);
    assert_eq!(again.status, RegistrationStatus::AlreadyRegistered);
    assert_eq!(again.entry, first.entry);
    assert_eq!(fs::read(&path).unwrap(), written);
    let second = register(2, 2_000);
    assert_eq!(second.status, RegistrationStatus::Registered);
    assert_eq!(second.entry.source_sha256, first.entry.source_sha256);
    let (a, b) = (
        first.entry.strategy_id.clone(),
        second.entry.strategy_id.clone(),
    );
    let active = |command: Command| {
        let out = run(&library, command, 5_000).unwrap();
        out["active"].as_str().map(str::to_owned)
    };
    // The newest eligible version is active until the owner says otherwise.
    assert_eq!(active(Command::List), Some(b.clone()));
    assert_eq!(
        active(Command::Pin {
            strategy_id: a[..12].into()
        }),
        Some(a.clone())
    );
    assert_eq!(
        active(Command::Disable {
            strategy_id: a.clone()
        }),
        Some(b.clone())
    );
    assert!(
        run(
            &library,
            Command::Pin {
                strategy_id: a.clone()
            },
            5_000
        )
        .is_err()
    );
    assert_eq!(
        active(Command::Enable {
            strategy_id: a.clone()
        }),
        Some(a.clone())
    );
    assert_eq!(active(Command::Unpin), Some(b.clone()));
    // Rollback disables the active version and pins the one before it.
    assert_eq!(active(Command::Rollback { to: None }), Some(a.clone()));
    let resolved = library.resolve().unwrap();
    assert!(resolved.disabled.contains(&b));
    assert_eq!(resolved.pinned, Some(a.clone()));
    assert!(run(&library, Command::Rollback { to: None }, 5_000).is_err());

    // Decision-lane selections respect owner pin/disable precedence.
    assert_eq!(
        active(Command::Enable {
            strategy_id: b.clone()
        }),
        Some(a.clone())
    );
    // While owner pin `a` holds, lane selection records its reason without overriding `a`.
    let resolved = library
        .select_by_lane(
            Some(&b[..12]),
            &policy(&[], 'a'),
            "regime favors b",
            Some("decision-1"),
            6_000,
        )
        .unwrap();
    assert_eq!(resolved.active, Some(a.clone()));
    assert_eq!(
        resolved
            .lane_selection
            .as_ref()
            .and_then(|s| s.strategy_id.as_deref()),
        Some(b.as_str())
    );
    // Once owner unpins, lane selection takes effect.
    assert_eq!(active(Command::Unpin), Some(b.clone()));
    // Lane cannot select a disabled strategy.
    assert_eq!(
        active(Command::Disable {
            strategy_id: b.clone()
        }),
        Some(a.clone())
    );
    assert!(
        library
            .select_by_lane(Some(&b), &policy(&[], 'a'), "try disabled", None, 6_500)
            .is_err()
    );
    // Lane can stand down (`None`) when unpinned, and owner pin still overrides `None`.
    assert_eq!(
        active(Command::Enable {
            strategy_id: b.clone()
        }),
        Some(b.clone())
    );
    let stood_down = library
        .select_by_lane(
            None,
            &policy(&[], 'a'),
            "unknown regime",
            Some("decision-2"),
            7_000,
        )
        .unwrap();
    assert_eq!(stood_down.active, None);
    assert_eq!(
        active(Command::Pin {
            strategy_id: a.clone()
        }),
        Some(a.clone())
    );
    assert_eq!(active(Command::Unpin), None);

    // Controls never touch an entry, and a changed source fails its hash.
    assert_eq!(fs::read(&path).unwrap(), written);
    fs::write(
        dir.path()
            .join("strategy-library/sources")
            .join(format!("{}.py", first.entry.source_sha256)),
        b"print(1)\n",
    )
    .unwrap();
    assert!(library.resolve().is_err());
}

#[test]
fn a_configured_fallback_runs_only_until_an_accepted_version_can() {
    use super::choice::{Running, choose};
    let dir = tempfile::tempdir().unwrap();
    let library = Library::at(dir.path().join("strategy-library"));
    let fallback = policy(&["vwap_trend", "vwap_reversion"], 'a');
    let running = |library: &Library, fallback: &choice::ChoicePolicy| {
        let choice = choose(library, fallback).unwrap();
        (choice.running.map(|r| r.run_id()), choice.reason)
    };
    // Nothing accepted and nothing configured: nothing runs, and says why.
    assert_eq!(
        running(&library, &policy(&[], 'a')),
        (None, Some("no_active_strategy"))
    );
    // The first configured fallback runs, labelled, with its shipped params.
    let choice = choose(&library, &fallback).unwrap();
    let Some(Running::Fallback(reference)) = &choice.running else {
        panic!("fallback expected");
    };
    assert_eq!(reference.run_id(), "fallback:vwap_trend");
    assert_eq!(
        reference.source,
        &include_bytes!("../../../../../strategies/vwap_trend.py")[..]
    );
    assert_eq!(reference.params["max_positions"], 2);
    assert_eq!(choice.reason, Some("no_active_strategy"));
    // The lane may pick another configured fallback, never an unlisted one.
    let lane = |id: Option<&str>, at| library.select_by_lane(id, &fallback, "regime", None, at);
    lane(Some("fallback:vwap_reversion"), 1).unwrap();
    assert_eq!(
        running(&library, &fallback).0.as_deref(),
        Some("fallback:vwap_reversion")
    );
    assert!(lane(Some("fallback:circuit_magnet"), 2).is_err());
    // A version accepted on another build leaves the fallback running.
    let author = Author {
        lane: "research".into(),
        model: None,
        model_revision: None,
        turn_id: None,
        session: None,
    };
    let register = |n: u64, engine: String, at| {
        let (bytes, sha) = report(|r| {
            r["parameters"]["params"] = json!({"n": n});
            r["engine_sha256"] = json!(engine);
            r["adapter_sha256"] = json!(super::super::strategy::replay::adapter_sha256());
        });
        let accepted = accept(&bytes, &sha, SOURCE, &Thresholds::default()).unwrap();
        let rev = review_for(SOURCE, at);
        library
            .register(&accepted, &rev, author.clone(), at)
            .unwrap()
            .entry
            .strategy_id
    };
    let stale = register(1, "0".repeat(64), 3);
    assert_eq!(
        running(&library, &fallback),
        (
            Some("fallback:vwap_reversion".into()),
            Some("strategy_engine_changed")
        )
    );
    // An owner pin on a version that cannot run here runs nothing.
    library
        .append(ControlRecord {
            at_ms: 4,
            control: Control::Pin {
                strategy_id: stale.clone(),
            },
        })
        .unwrap();
    assert_eq!(
        running(&library, &fallback),
        (None, Some("strategy_engine_changed"))
    );
    library
        .append(ControlRecord {
            at_ms: 5,
            control: Control::Unpin,
        })
        .unwrap();
    // A runnable accepted version wins over the lane's fallback choice.
    let current = register(2, nivra_core::replay::engine_fingerprint(), 6);
    assert_eq!(running(&library, &fallback), (Some(current.clone()), None));
    // A lane stand-down runs nothing, fallback included.
    lane(None, 7).unwrap();
    assert_eq!(
        running(&library, &fallback),
        (None, Some("strategy_lane_stood_down"))
    );
    // An owner pin still wins.
    library
        .append(ControlRecord {
            at_ms: 8,
            control: Control::Pin {
                strategy_id: current.clone(),
            },
        })
        .unwrap();
    assert_eq!(running(&library, &fallback), (Some(current), None));
}

#[test]
fn fallback_config_names_only_shipped_action_references() {
    let parse = |json: &str| super::super::planner::PlannerConfig::parse(json.as_bytes());
    let ok = parse(r#"{"enabled":false,"strategy_execution":{"enabled":true,"fallback":["vwap_trend","opening_range_breakout"]}}"#).unwrap();
    assert_eq!(
        ok.strategy_fallback(),
        ["vwap_trend", "opening_range_breakout"]
    );
    for bad in [
        "rsi2_reversion",
        "check_actions",
        "missing",
        "costs/nse-intraday",
    ] {
        let json = format!(
            r#"{{"enabled":false,"strategy_execution":{{"enabled":true,"fallback":["{bad}"]}}}}"#
        );
        assert!(parse(&json).is_err(), "{bad}");
    }
    assert!(parse(r#"{"enabled":false,"strategy_execution":{"enabled":true,"fallback":["vwap_trend","vwap_trend"]}}"#).is_err());
    // Disabled execution runs no fallback.
    let off = parse(
        r#"{"enabled":false,"strategy_execution":{"enabled":false,"fallback":["vwap_trend"]}}"#,
    )
    .unwrap();
    assert!(off.strategy_fallback().is_empty());
}

#[test]
fn a_lane_choice_counts_only_under_the_policy_it_was_made_under() {
    use super::choice::choose;
    let dir = tempfile::tempdir().unwrap();
    let library = Library::at(dir.path().join("strategy-library"));
    let run = |policy: &choice::ChoicePolicy| {
        let choice = choose(&library, policy).unwrap();
        (choice.running.map(|r| r.run_id()), choice.reason)
    };
    // Under policy A, with nothing configured, the lane stands down.
    let a = policy(&[], 'a');
    library
        .select_by_lane(None, &a, "nothing eligible", None, 1)
        .unwrap();
    assert_eq!(run(&a), (None, Some("strategy_lane_stood_down")));
    // The owner applies policy B with a fallback: the old stand-down no
    // longer counts and the fallback runs.
    let b = policy(&["vwap_trend", "vwap_reversion"], 'b');
    assert_eq!(
        run(&b),
        (
            Some("fallback:vwap_trend".to_owned()),
            Some("no_active_strategy")
        )
    );
    // A fallback chosen under a policy that lists it is ignored under another.
    let a2 = policy(&["vwap_trend", "vwap_reversion"], 'c');
    library
        .select_by_lane(Some("fallback:vwap_reversion"), &a2, "regime", None, 2)
        .unwrap();
    assert_eq!(run(&a2).0.as_deref(), Some("fallback:vwap_reversion"));
    assert_eq!(run(&b).0.as_deref(), Some("fallback:vwap_trend"));
    // A record from before digests were kept counts under no policy.
    let legacy = Library::at(dir.path().join("legacy"));
    legacy
        .append(ControlRecord {
            at_ms: 3,
            control: Control::LaneSelect {
                strategy_id: None,
                reason: "stand down".into(),
                turn_id: None,
                policy_sha256: None,
            },
        })
        .unwrap();
    let choice = choose(&legacy, &b).unwrap();
    assert_eq!(
        choice.running.map(|r| r.run_id()).as_deref(),
        Some("fallback:vwap_trend")
    );
}

#[test]
fn unreviewed_or_stale_review_entries_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::at(dir.path().join("strategy-library"));
    let (bytes, sha) = report(|r| {
        r["engine_sha256"] = json!(nivra_core::replay::engine_fingerprint());
        r["adapter_sha256"] = json!(super::super::strategy::replay::adapter_sha256());
    });
    let accepted = accept(&bytes, &sha, SOURCE, &Thresholds::default()).unwrap();
    let author = Author {
        lane: "research".into(),
        model: None,
        model_revision: None,
        turn_id: None,
        session: None,
    };
    let mut bad_review = review_for(SOURCE, 1);
    bad_review.source_sha256 = digest(b"different source");
    assert!(
        library
            .register(&accepted, &bad_review, author.clone(), 1)
            .is_err()
    );

    let ok_review = review_for(SOURCE, 1);
    let reg = library.register(&accepted, &ok_review, author, 1).unwrap();
    let py = std::path::Path::new("/usr/bin/python3");
    assert_eq!(
        library
            .entry_review_mismatch(&reg.entry, py, false)
            .unwrap(),
        None
    );

    // Tampering with the stored entry's review policy version refuses execution in choice::choose.
    let mut entry = reg.entry.clone();
    entry.review.policy_sha256 = "0".repeat(64);
    assert_eq!(
        library.entry_review_mismatch(&entry, py, false).unwrap(),
        Some("strategy_review_changed")
    );
    let path = dir
        .path()
        .join("strategy-library/entries")
        .join(format!("{}.json", entry.strategy_id));
    fs::write(&path, artifact_bytes(&entry).unwrap()).unwrap();
    let chosen = super::choice::choose(&library, &policy(&[], 'a')).unwrap();
    assert!(chosen.running.is_none());
    assert_eq!(chosen.reason, Some("strategy_review_changed"));
}
