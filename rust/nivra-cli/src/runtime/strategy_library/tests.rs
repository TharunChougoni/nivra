use super::acceptance::{Reason, Thresholds, accept};
use super::*;

const SOURCE: &[u8] = b"print('{\"actions\":[]}')\n";

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
        let (bytes, sha) = report(|r| r["parameters"]["params"] = json!({"n": n}));
        let accepted = accept(&bytes, &sha, SOURCE, &Thresholds::default()).unwrap();
        library.register(&accepted, author.clone(), at).unwrap()
    };
    let first = register(1, 1_000);
    assert!(first.created);
    let path = dir
        .path()
        .join("strategy-library/entries")
        .join(format!("{}.json", first.entry.strategy_id));
    let written = fs::read(&path).unwrap();
    // Re-registering the same (source, params) changes nothing, even later.
    let again = register(1, 9_000);
    assert!(!again.created);
    assert_eq!(again.entry, first.entry);
    assert_eq!(fs::read(&path).unwrap(), written);
    let second = register(2, 2_000);
    assert!(second.created);
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
        .select_by_lane(Some(&b[..12]), "regime favors b", Some("decision-1"), 6_000)
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
            .select_by_lane(Some(&b), "try disabled", None, 6_500)
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
        .select_by_lane(None, "unknown regime", Some("decision-2"), 7_000)
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
