//! Phase 1 and 2 proofs on the real lane runner, bridge, workspace, store and
//! queue. Only the agent (a scripted pipe) and the bar worker are stand-ins.
use super::cli::RunnerTurns;
use super::coordinator::{Step, StepReport, step};
use super::jobs::{JobEnd, Jobs};
use super::{AutoresearchSettings, HistoryArgs, MemoryOp, execute, open, research_root, skills};
use crate::runtime::Config;
use crate::runtime::agent::Lane;
use crate::runtime::agent::lanes::LaneRunner;
use crate::runtime::agent::tests::{all_tools, connect, context, fixture_agent, settings};
use crate::runtime::planner::PlannerConfig;
use crate::runtime::workspace::{self, WorkspaceOp, WorkspaceRoot, WorkspaceScope};
use chrono::NaiveDate;
use nivra_core::autoresearch::{Comparator, Regime, Rule, Score, Stage, keep, usd_micros};
use nivra_core::finance::Amount;
use nivra_store::research_memory::{NewIdea, Outcome, Reserved, Submission};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// IST wall time on a date, as epoch milliseconds.
fn ist(date: &str, hour: u32, minute: u32) -> i64 {
    date.parse::<NaiveDate>()
        .unwrap()
        .and_hms_opt(hour, minute, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
        - 19_800_000
}

fn loop_config(
    dir: &std::path::Path,
    edit: impl FnOnce(&mut AutoresearchSettings),
) -> (Config, PlannerConfig) {
    let (mut c, mut cfg) = all_tools(dir);
    // The shipped fragment turns the loop on with exactly the code defaults.
    let fragment: Value = serde_json::from_str(include_str!(
        "../../../../../packaging/rust/autoresearch.fragment.json"
    ))
    .unwrap();
    let mut loop_settings: AutoresearchSettings =
        serde_json::from_value(fragment["autoresearch"].clone()).unwrap();
    assert_eq!(
        loop_settings,
        AutoresearchSettings {
            enabled: true,
            ..Default::default()
        }
    );
    loop_settings.validate().unwrap();
    edit(&mut loop_settings);
    cfg.autoresearch = Some(loop_settings);
    std::fs::write(
        c.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&cfg).unwrap(),
    )
    .unwrap();
    c.market_archive = Some(crate::runtime::MarketArchiveSettings {
        url: "postgresql://nivra@127.0.0.1/nivra_market".into(),
        password_file: dir.join("archive-password"),
    });
    (c, cfg)
}

fn book(c: &Config, session: &str) -> Config {
    let mut book = c.clone();
    book.session = session.parse().unwrap();
    book.previous_session = book.session.pred_opt().unwrap();
    book.directory = c.sessions_root.as_ref().unwrap().join(session);
    std::fs::create_dir_all(&book.directory).unwrap();
    book
}

/// A bar worker stand-in: each run pops the next scripted end.
struct ScriptedJobs(Vec<JobEnd>);
impl Jobs for ScriptedJobs {
    fn run(&mut self, _: &str, _: u32, _: &dyn Fn() -> bool) -> crate::runtime::Result<JobEnd> {
        Ok(self.0.remove(0))
    }
}

fn report(net: &str, snapshot: &nivra_store::market_archive::BarSnapshot) -> JobEnd {
    let fold = |day: &str| {
        json!({"qualification_net_pnl":net,"max_drawdown_bps":40,
        "closed_trades":12,"open_positions":0,"daily_net_pnl":[[day,net]]})
    };
    JobEnd::Report(
        serde_json::to_vec(&json!({"abi":"archive_bars_v1","bar_snapshot":snapshot,
        "test":fold("2026-08-03"),"folds":[fold("2026-09-01"),fold("2026-10-01")]}))
        .unwrap(),
    )
}

fn seed_qualification_bars(
    c: &Config,
) -> crate::runtime::Result<nivra_store::market_archive::BarWindow> {
    use nivra_store::market_archive::{BarUnit, BarWindow, init};
    use sha2::{Digest, Sha256};
    let archive = c.market_archive.as_ref().ok_or("archive_config_missing")?;
    let mut db = crate::runtime::market_archive::connect(archive)?;
    init(&mut db)?;
    let mut tx = db.transaction()?;
    let key = format!(
        "NSE_EQ|QUAL-FIXTURE-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    );
    let instrument: i32 = tx
        .query_one(
            "INSERT INTO instruments(key) VALUES($1)
        ON CONFLICT(key) DO UPDATE SET key=EXCLUDED.key RETURNING id",
            &[&key],
        )?
        .get(0);
    let now = crate::runtime::millis();
    let hash = Sha256::digest(format!("{key}-{now}").as_bytes());
    let receipt: i64 = tx
        .query_one(
            "INSERT INTO receipts(kind, requested_ms, received_ms, sha256, body)
        VALUES(2,$1,$1,$2,$2) RETURNING id",
            &[&now, &hash.as_slice()],
        )?
        .get(0);
    let mut days = vec!["2025-12-01".parse()?];
    days.extend(super::qualification::exchange_dates(
        c,
        "2026-01-01".parse()?,
        "2026-10-31".parse()?,
    )?);
    tx.execute(
        "INSERT INTO daily_bars(instrument_id, day, o, h, l, c, volume, oi, receipt_id)
        SELECT $1, day, 10000, 20000, 10000, 18000, 10000, 0, $3
        FROM unnest($2::date[]) day
        ON CONFLICT(instrument_id,day) DO UPDATE SET c=EXCLUDED.c, receipt_id=EXCLUDED.receipt_id",
        &[&instrument, &days, &receipt],
    )?;
    tx.commit()?;
    Ok(BarWindow {
        unit: BarUnit::Day,
        from: "2025-12-01".parse()?,
        to: "2026-10-31".parse()?,
        instruments: Some(vec![key]),
    })
}

#[test]
#[ignore = "requires NIVRA_ARCHIVE_TEST_URL and bubblewrap"]
fn revised_archive_changes_identity_and_future_only_signal_cannot_fill()
-> crate::runtime::Result<()> {
    use nivra_core::autoresearch::{ExperimentIdentity, qualification};
    use nivra_store::market_archive::ist_midnight;
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir()?;
    let (mut c, cfg) = loop_config(dir.path(), |_| {});
    let archive = crate::runtime::market_archive::test_settings(dir.path())?;
    c.market_archive = Some(archive.clone());
    let window = seed_qualification_bars(&c)?;
    let snapshot = super::jobs::read_snapshot(&c, &window)?;
    let mut db = crate::runtime::market_archive::connect(&archive)?;
    let (_, candidate) = super::jobs::loop_settings(&c, &cfg)?;
    let date = |s: &str| ist_midnight(s.parse().unwrap());
    let last = [date("2026-08-31"), date("2026-09-30"), date("2026-10-30")];
    let mut request = crate::runtime::harness::research::bar_request(
        &archive,
        window.clone(),
        nivra_store::research_jobs::BacktestWindows {
            training_from_ms: date("2025-12-01"),
            training_to_ms: date("2026-08-01") - 1,
            selection_from_ms: date("2026-08-01"),
            selection_to_ms: date("2026-11-01") - 1,
        },
        c.sleeves.cash.capital,
        c.paper_costs.clone().unwrap(),
        json!({"last":last}),
        &candidate,
    )
    .unwrap();
    request.qualification = Some(nivra_research::archive_bars::Qualification {
        snapshot: snapshot.clone(),
        fold_boundaries_ms: Some([date("2026-09-01"), date("2026-10-01")]),
        embargo_sessions: 0,
    });
    // The terminal high looks profitable only if a fill can use the same
    // bar or cross into the next fold. Both paths must leave zero fills.
    let source = r#"def on_bars(at, split, rows, params, state):
    return [[0, 1]] if at in params['last'] and rows[0][2] > rows[0][1] else []
"#;
    let run =
        |db: &mut nivra_store::market_archive::ArchiveClient| -> crate::runtime::Result<Value> {
            Ok(serde_json::from_slice(&nivra_research::archive_bars::run(
                &request,
                source,
                db,
                std::time::Duration::from_secs(30),
            )?)?)
        };
    let result = run(&mut db)?;
    let mut parts = vec![result["test"].clone()];
    parts.extend(result["folds"].as_array().unwrap().clone());
    assert!(
        parts
            .iter()
            .all(|f| f["signals"] == 1 && f["fills"] == 0 && f["unfilled_signals"] == 1)
    );
    let folds: Vec<qualification::Fold> = parts
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<_, _>>()?;
    let score = qualification::pooled(&folds)?;
    let paired = qualification::paired(&folds, None, request.capital)?;
    assert_eq!(
        qualification::verdict(&score, paired.as_ref(), 300)
            .verdict
            .as_str(),
        "reject"
    );
    // The archive is append-only, so a correction is a new observation row.
    // A later backfill inside the frozen window changes the input digest.
    let key = window.instruments.as_ref().unwrap()[0].clone();
    let receipt: i64 = db
        .query_one(
            "INSERT INTO receipts(kind, requested_ms, received_ms, sha256, body)
             VALUES(2, 1, 1, $1, $1) RETURNING id",
            &[&Sha256::digest(format!("{key}-backfill").as_bytes()).as_slice()],
        )?
        .get(0);
    db.execute(
        "INSERT INTO daily_bars(instrument_id, day, o, h, l, c, volume, oi, receipt_id)
         SELECT id, '2025-12-15', 10000, 20000, 10000, 18000, 10000, 0, $2
         FROM instruments WHERE key=$1",
        &[&key, &receipt],
    )?;
    let revised = super::jobs::read_snapshot(&c, &window)?;
    let identity = |digest: &str| {
        ExperimentIdentity {
            stage: Stage::Bars,
            abi: "archive_bars_v1",
            source_sha256: "same-source",
            params_sha256: "same-params",
            manifest_sha256: digest,
            runner_sha256: "same-runner",
        }
        .key()
    };
    assert_ne!(
        identity(&snapshot.ordered_sha256),
        identity(&revised.ordered_sha256)
    );
    assert_eq!(run(&mut db)?["reason"], "archive_bars_snapshot_changed");
    Ok(())
}

struct Run<'a> {
    c: &'a Config,
    cfg: &'a PlannerConfig,
    now: &'a dyn Fn() -> i64,
    jobs: &'a mut ScriptedJobs,
    tape: bool,
}

fn run_step(runner: &mut LaneRunner<'_>, run: &mut Run<'_>) -> StepReport {
    let mut context = context("5000");
    let tape = run.tape;
    let mut covers = |_| Ok(tape);
    step(&mut Step {
        c: run.c,
        cfg: run.cfg,
        owner: "test:1",
        clock: run.now,
        turns: &mut RunnerTurns {
            runner,
            context: &mut context,
        },
        jobs: run.jobs,
        stop: &|| false,
        tape: &mut covers,
        trading_today: false,
        sealed_today: false,
        last_sealed: "2026-11-26".parse().unwrap(),
    })
    .unwrap()
}

fn brief(turn: &Value) -> Value {
    serde_json::from_str(turn["brief"].as_str().unwrap()).unwrap()
}

fn names(turn: &Value) -> Vec<String> {
    turn["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn two_dated_books_share_scripts_reviewed_skills_and_losing_experiments() {
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = loop_config(dir.path(), |_| {});
    let loop_settings = cfg.autoresearch.as_ref().unwrap();
    let manifest =
        super::jobs::campaign_manifest(&c, &cfg, loop_settings, "2026-11-26".parse().unwrap())
            .unwrap();
    open(&c)
        .unwrap()
        .ensure_campaign(&nivra_store::research_memory::NewCampaign {
            name: &loop_settings.campaign.name,
            segment: super::SEGMENT,
            manifest: &serde_json::to_vec(&manifest).unwrap(),
            development_from: manifest.window.from,
            development_to: manifest.window.to,
            selection_from: "2026-08-01".parse().unwrap(),
            trial_cap: loop_settings.campaign.trial_cap,
            now: ist("2026-11-26", 16, 0),
        })
        .unwrap();
    let day1 = book(&c, "2026-11-26");
    // Yesterday's book workspace: a script, a note and a skill written in-book.
    let old = day1.directory.join("workspace");
    for (path, text) in [
        ("scripts/old_ranker.py", "print('{}')\n"),
        ("notes/legacy.md", "gap fade lost to costs\n"),
        (
            "skills/fold-check/SKILL.md",
            "---\nname: fold-check\ndescription: Check every fold before trusting a score.\n---\nRun each fold.\n",
        ),
    ] {
        std::fs::create_dir_all(old.join(path).parent().unwrap()).unwrap();
        std::fs::write(old.join(path), text).unwrap();
    }
    let now = Arc::new(AtomicI64::new(ist("2026-11-26", 21, 0)));
    let clock_now = now.clone();
    let clock = move || clock_now.load(Ordering::SeqCst);
    let agent_settings = settings(100, 10.0);
    let (client, peer) = connect(|peer| {
        // Day 1: the imported draft goes to a review-lane turn first.
        let (id, turn) = peer.turn();
        assert_eq!(turn["lane"], "review");
        assert_eq!(names(&turn), ["research_history", "skill_verdict"]);
        let draft = brief(&turn)["drafts"][0].clone();
        assert_eq!(draft["name"], "fold-check");
        let verdict = peer.call(&id, "skill_verdict", json!({"name":"fold-check","digest":draft["digest"],
            "approve":true,"reason":"A reproducible procedure inside the research tools.","finding_ids":[]}));
        assert_eq!(verdict["ok"], true, "{verdict}");
        peer.finish(&id, "ok", "approved", 1, 0.01);
        // Day 2: an ordinary research turn reads yesterday's memory.
        let (id, turn) = peer.turn();
        let history = peer.call(
            &id,
            "research_history",
            json!({"family":null,"mechanism":null,
            "parent_experiment_id":null,"before_id":null,"limit":null}),
        );
        let script = peer.call(&id, "read", json!({"path":"scripts/old_ranker.py"}));
        peer.finish(&id, "ok", "done", 1, 0.01);
        (turn, history, script)
    });
    let mut agent = fixture_agent(client);
    let mut jobs = ScriptedJobs(Vec::new());
    {
        let mut runner = LaneRunner::new(&mut agent, &agent_settings, &day1, &cfg, &clock);
        let reviewed = run_step(
            &mut runner,
            &mut Run {
                c: &day1,
                cfg: &cfg,
                now: &clock,
                jobs: &mut jobs,
                tape: true,
            },
        );
        assert_eq!(reviewed.state, "skills_reviewed", "{reviewed:?}");
    }
    let research = WorkspaceRoot::research(&day1).unwrap();
    let skills_dir = research.path().join("skills");
    assert!(skills_dir.join("fold-check/SKILL.md").is_file());
    // A new draft is staged for review and stays out of discovery.
    let workspace_settings = cfg.workspace.as_ref().unwrap();
    workspace::execute_scoped_with_env(
        &research,
        workspace_settings,
        WorkspaceScope::ResearchSandbox,
        None,
        WorkspaceOp::Write {
            path: "skills-proposed/regime-gate/SKILL.md".into(),
            text: "---\nname: regime-gate\ndescription: Skip range days.\n---\n".into(),
        },
    )
    .unwrap();
    assert!(
        workspace::execute_scoped_with_env(
            &research,
            workspace_settings,
            WorkspaceScope::ResearchSandbox,
            None,
            WorkspaceOp::Write {
                path: "skills/regime-gate/SKILL.md".into(),
                text: "---\nname: regime-gate\n---\n".into(),
            },
        )
        .is_err(),
        "only Rust writes skills/"
    );
    let mut store = open(&day1).unwrap();
    skills::stage_proposals(research.path(), &mut store, 1).unwrap();
    skills::materialize(research.path(), &store).unwrap();
    assert!(!skills_dir.join("regime-gate").exists());
    assert_eq!(store.skills(Some("proposed")).unwrap().len(), 1);
    // Day 1 also records a losing experiment.
    let campaign = store.campaign("bars-v2").unwrap().unwrap();
    let (idea, _) = store
        .record_idea(&NewIdea {
            campaign_id: campaign.id,
            origin_request_key: None,
            family: "reversal",
            regime: Regime::Range,
            mechanism: "gap fade at the open",
            hypothesis: "Opening gaps fade by the close",
            falsifier: "Net after costs below cash",
            comparator: "cash",
            parent_experiment_id: None,
            now: 2,
        })
        .unwrap();
    let blob = store.put_object(b"source").unwrap();
    let Reserved::New(_, attempt) = store
        .reserve(&Submission {
            idea_id: idea.id,
            stage: Stage::Bars,
            quote_window: None,
            source_path: "scripts/gap_fade.py",
            source_blob: &blob,
            params_blob: &blob,
            manifest_blob: &campaign.manifest_digest,
            evaluator_digest: nivra_core::autoresearch::EVALUATOR,
            experiment_key: &"e".repeat(64),
            lease_generation: 1,
            deadline_ms: 0,
            day: "2026-11-26".parse().unwrap(),
            limits: cfg.autoresearch.as_ref().unwrap().day_limits(),
            now: 3,
        })
        .unwrap()
    else {
        panic!("reserved");
    };
    let score = Score {
        net_pnl: Amount::from_integer(-30),
        max_drawdown_bps: 90,
        closed_trades: 14,
        open_positions: 0,
    };
    let rule = Rule {
        max_drawdown_bps: 300,
        require_flat: false,
    };
    let generation = store
        .acquire_lease(super::LEASE, "test:1", 1, 4, 1_000, &|_| true)
        .unwrap()
        .unwrap()
        .generation;
    store
        .finish(
            attempt.id,
            generation,
            &Outcome::Scored {
                decision: keep(&score, &[Comparator::Cash], rule),
                score,
                report_blob: blob.clone(),
            },
            4,
        )
        .unwrap();
    store.release_lease(super::LEASE, "test:1").unwrap();
    drop(store);

    // Day 2: a new dated book resolves the same workspace, skill and loser.
    let day2 = book(&c, "2026-11-27");
    assert_eq!(WorkspaceRoot::research(&day2).unwrap(), research);
    let history = execute(
        &day2,
        &cfg,
        None,
        MemoryOp::History(
            serde_json::from_value::<HistoryArgs>(json!({"family":"reversal",
            "mechanism":null,"parent_experiment_id":null,"before_id":null,"limit":null}))
            .unwrap(),
        ),
        5,
        &|| false,
    )
    .unwrap();
    assert_eq!(history["history"]["experiments"][0]["verdict"], "reject");
    assert_eq!(history["history"]["skills"][0]["name"], "fold-check");
    let mut closed = cfg.clone();
    closed.autoresearch.as_mut().unwrap().family_stall = 1;
    let new_idea = super::parse_tool("idea_record", json!({"family":"reversal","regime":"range",
        "mechanism":"gap fade with narrower stops","hypothesis":"Price returns to yesterday's close",
        "falsifier":"Net after costs is negative","comparator":"cash","parent_experiment_id":null})).unwrap();
    assert_eq!(
        execute(&day2, &closed, None, new_idea, 5, &|| false)
            .unwrap_err()
            .to_string(),
        "research_family_closed"
    );

    let slot = crate::runtime::harness::research::compute_slot(&day1)
        .unwrap()
        .unwrap();
    assert!(
        crate::runtime::harness::research::compute_slot(&day2)
            .unwrap()
            .is_none()
    );
    drop(slot);
    assert!(
        crate::runtime::harness::research::compute_slot(&day2)
            .unwrap()
            .is_some()
    );
    now.store(ist("2026-11-27", 10, 0), Ordering::SeqCst);
    {
        let mut runner = LaneRunner::new(&mut agent, &agent_settings, &day2, &cfg, &clock);
        runner
            .turn(
                Lane::Research,
                json!({"kind":"test"}),
                json!("brief"),
                &mut context("5000"),
            )
            .unwrap();
    }
    let (turn, history, script) = peer.join().unwrap();
    // pi discovers skills from the persistent research workspace.
    assert_eq!(
        turn["session"]["skills_dir"],
        skills_dir.to_string_lossy().as_ref()
    );
    assert_eq!(history["ok"], true, "{history}");
    assert!(
        history["output"]["history"]["experiments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["verdict"] == "reject")
    );
    assert_eq!(script["output"]["text"], "print('{}')\n");
    // Shipped skill names keep precedence: the reserved list is the shipped set.
    let mut shipped: Vec<String> =
        std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../agent/skills"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
    shipped.sort();
    assert_eq!(shipped, skills::SHIPPED);
    // The old book is untouched and its note was imported once.
    assert!(old.join("notes/legacy.md").is_file());
    assert!(
        research
            .path()
            .join("notes/imported/2026-11-26/legacy.md")
            .is_file()
    );
}

#[test]
fn a_loop_job_yields_to_any_other_holder_of_the_shared_compute_slot() {
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = loop_config(dir.path(), |_| {});
    let held = crate::runtime::harness::research::compute_slot(&c)
        .unwrap()
        .expect("first slot holder");
    let mut jobs = super::jobs::QueueJobs::new(&c, &cfg).unwrap();
    assert_eq!(jobs.run("unused-task", 1, &|| false).unwrap(), JobEnd::Busy);
    drop(held);
    let mut jobs = super::jobs::QueueJobs::new(&c, &cfg).unwrap();
    // The slot is free now; the queue itself has no such task.
    assert!(matches!(
        jobs.run("unused-task", 1, &|| false).unwrap(),
        JobEnd::Failed(_)
    ));
}

#[test]
#[ignore = "requires NIVRA_ARCHIVE_TEST_URL for frozen bar identity"]
fn after_the_seal_one_idea_runs_one_job_to_a_verdict_under_budget_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (mut c, mut cfg) = loop_config(dir.path(), |s| {
        s.day.usd = 1.0;
        s.turn.usd = 0.4;
    });
    c.market_archive = Some(crate::runtime::market_archive::test_settings(dir.path()).unwrap());
    let window = seed_qualification_bars(&c).unwrap();
    let snapshot = super::jobs::read_snapshot(&c, &window).unwrap();
    cfg.autoresearch.as_mut().unwrap().campaign.instruments = window.instruments;
    let now = Arc::new(AtomicI64::new(ist("2026-11-27", 23, 40)));
    let clock_now = now.clone();
    let clock = move || clock_now.load(Ordering::SeqCst);
    // Debug builds hash the worker image on each admission; the loop's own
    // 120 s turn ceiling is the bound this test exercises, not the fixture's.
    let mut agent_settings = settings(100, 10.0);
    for lane in agent_settings.lanes.values_mut() {
        lane.turn_timeout_ms = 120_000;
    }
    let source = "def on_bars(at_ms, split, bars, params, state):\n    return []\n";
    let (client, peer) = connect(move |peer| {
        let (id, turn) = peer.turn();
        assert_eq!(
            names(&turn),
            [
                "research_history",
                "shadow_status",
                "idea_record",
                "list",
                "read",
                "write",
                "run",
                "backtest"
            ]
        );
        assert_eq!(brief(&turn)["kind"], "autoresearch_proposal");
        assert_eq!(brief(&turn)["slot"], "new_family");
        let idea = peer.call(&id, "idea_record", json!({"family":"breakout","regime":"trend",
            "mechanism":"volume breakout above the 20 day high","hypothesis":"Breakouts on volume continue",
            "falsifier":"Net after costs below cash","comparator":"cash","parent_experiment_id":null}));
        let idea_id = idea["output"]["idea_id"].clone();
        peer.call(
            &id,
            "write",
            json!({"path":"scripts/breakout.py","text":source}),
        );
        let submit = json!({"idea_id":idea_id,"source_path":"scripts/breakout.py",
            "params":"{\"lookback\":20}","stage":"bars"});
        let first = peer.call(&id, "backtest", submit.clone());
        // A duplicate submission spends nothing.
        let second = peer.call(&id, "backtest", submit);
        peer.finish(&id, "ok", "submitted", 3, 0.05);
        let (id, turn) = peer.turn();
        let interpretation = brief(&turn);
        let experiment = interpretation["experiment"]["experiment_id"].clone();
        let finding = peer.call(
            &id,
            "finding_record",
            json!({"experiment_id":experiment,"kind":"positive",
            "observation":"Volume breakouts beat cash after costs","scope":"daily bars, one window",
            "limitation":"one selection window","next_test":"tighten the exit"}),
        );
        peer.finish(&id, "ok", "noted", 2, 0.7);
        // After midnight a new research day allows the next proposal.
        let (id, turn) = peer.turn();
        peer.finish(&id, "ok", "nothing new", 1, 0.01);
        (first, second, interpretation, finding, turn)
    });
    let mut agent = fixture_agent(client);
    let mut jobs = ScriptedJobs(vec![JobEnd::Stopped, report("120", &snapshot)]);
    let mut states = Vec::new();
    {
        let mut runner = LaneRunner::new(&mut agent, &agent_settings, &c, &cfg, &clock);
        let mut run = Run {
            c: &c,
            cfg: &cfg,
            now: &clock,
            jobs: &mut jobs,
            tape: false,
        };
        let waiting = run_step(&mut runner, &mut run);
        assert_eq!(waiting.reason.as_deref(), Some("waiting_for_tape"));
        run.tape = true;
        // proposed, preempted, retrying, queued (the restart adopts the
        // queued job), scored, interpreted, then the day's money is spent.
        for _ in 0..7 {
            let report = run_step(&mut runner, &mut run);
            states.push(report.reason.clone().unwrap_or(report.state.into()));
        }
        now.store(ist("2026-11-28", 0, 5), Ordering::SeqCst);
        let report = run_step(&mut runner, &mut run);
        states.push(report.reason.clone().unwrap_or(report.state.into()));
    }
    assert_eq!(
        states,
        [
            "proposed",
            "preempted",
            "retrying",
            "queued",
            "scored",
            "interpreted",
            "day_usd_exhausted",
            "proposed"
        ]
    );
    let (first, second, interpretation, finding, last) = peer.join().unwrap();
    assert_eq!(first["output"]["status"], "queued", "{first}");
    assert_eq!(second["output"]["status"], "duplicate", "{second}");
    assert_eq!(interpretation["experiment"]["verdict"], "keep");
    assert_eq!(finding["ok"], true, "{finding}");
    // The next brief carries the kept incumbent; trial 2 takes the cycle's
    // next open slot (no failure to revisit yet).
    let next = brief(&last);
    assert_eq!(next["incumbents"][0]["verdict"], "keep");
    assert_eq!(next["campaign"]["trials_used"], 1);
    assert_eq!(next["slot"], "new_family");

    let store = open(&c).unwrap();
    let campaign = store.campaign("bars-v2").unwrap().unwrap();
    assert_eq!(
        store.trials(campaign.id).unwrap(),
        1,
        "one trial per variant"
    );
    let experiment = store.recent_experiments(campaign.id, 1).unwrap().remove(0);
    let attempt = store.attempt(experiment.id).unwrap().unwrap();
    assert_eq!(
        (attempt.attempt_no, attempt.state.as_str()),
        (2, "succeeded")
    );
    // The restarted attempt bound the job already queued: no second job.
    let (research, _) = super::jobs::loop_settings(&c, &cfg).unwrap();
    let mut queue = crate::runtime::harness::research::open_queue(
        &research,
        &research_root(&c).unwrap(),
        false,
    )
    .unwrap();
    let task_id = super::jobs::task_id(&store, &experiment).unwrap();
    let task = nivra_store::research_jobs::latest_scoped(&mut queue, &task_id)
        .unwrap()
        .unwrap();
    assert_eq!(task.spec.revision, 1);
    assert_eq!(attempt.job_id.as_deref(), Some(task_id.as_str()));
    // Each call was charged once; the reservation settled to actual usage.
    let day = store.day_spend("2026-11-27".parse().unwrap()).unwrap();
    assert_eq!(
        (day.variants, day.turns, day.calls, day.usd_micros),
        (1, 2, 5, usd_micros(0.75))
    );
    assert_eq!(
        store
            .day_spend("2026-11-28".parse().unwrap())
            .unwrap()
            .turns,
        1
    );
    // Bars evidence never registers a library entry.
    let entries = c
        .sessions_root
        .as_ref()
        .unwrap()
        .join("strategy-library/entries");
    assert!(std::fs::read_dir(&entries).map_or(true, |mut dir| dir.next().is_none()));
}

#[test]
fn previous_tape_fallback_is_bounded_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = loop_config(dir.path(), |_| {});
    let clock = || ist("2026-11-28", 12, 0);
    let agent_settings = settings(100, 10.0);

    // Case 1: Nothing archived within 5 trading sessions before 2026-11-27
    // (2026-11-24 is an NSE holiday, so 2026-11-18 is 6 trading sessions back)
    // -> stays waiting_for_tape.
    let (client, peer) = connect(|_| {});
    let mut agent = fixture_agent(client);
    let mut jobs = ScriptedJobs(Vec::new());
    {
        let mut runner = LaneRunner::new(&mut agent, &agent_settings, &c, &cfg, &clock);
        let mut ctx = context("5000");
        let mut covers = |day: NaiveDate| Ok(day <= "2026-11-18".parse::<NaiveDate>().unwrap());
        let report = step(&mut Step {
            c: &c,
            cfg: &cfg,
            owner: "test:1",
            clock: &clock,
            turns: &mut RunnerTurns {
                runner: &mut runner,
                context: &mut ctx,
            },
            jobs: &mut jobs,
            stop: &|| false,
            tape: &mut covers,
            trading_today: false,
            sealed_today: false,
            last_sealed: "2026-11-27".parse().unwrap(),
        })
        .unwrap();
        assert_eq!(report.state, "waiting");
        assert_eq!(report.reason.as_deref(), Some("waiting_for_tape"));
        let name = &cfg.autoresearch.as_ref().unwrap().campaign.name;
        assert!(open(&c).unwrap().campaign(name).unwrap().is_none());
    }
    peer.join().unwrap();

    // Case 2: resolve_tape skips missing 2026-11-27 (and holiday 2026-11-24)
    // within 5 trading sessions to return 2026-11-26, and stops at 6 sessions.
    let last_sealed: NaiveDate = "2026-11-27".parse().unwrap();
    let mut only_old = |day: NaiveDate| Ok(day <= "2026-11-18".parse::<NaiveDate>().unwrap());
    assert_eq!(
        super::coordinator::resolve_tape(&c, last_sealed, &mut only_old).unwrap(),
        None
    );
    let mut prev_ok = |day: NaiveDate| Ok(day <= "2026-11-26".parse::<NaiveDate>().unwrap());
    assert_eq!(
        super::coordinator::resolve_tape(&c, last_sealed, &mut prev_ok).unwrap(),
        Some("2026-11-26".parse().unwrap())
    );

    // Case 3: A campaign frozen on 2026-10-31 is current while last_sealed is
    // 2026-11-27, and reports using_previous_tape once last_sealed enters Dec.
    let loop_settings = cfg.autoresearch.as_ref().unwrap();
    let manifest =
        super::jobs::campaign_manifest(&c, &cfg, loop_settings, "2026-11-26".parse().unwrap())
            .unwrap();
    open(&c)
        .unwrap()
        .ensure_campaign(&nivra_store::research_memory::NewCampaign {
            name: &loop_settings.campaign.name,
            segment: super::SEGMENT,
            manifest: &serde_json::to_vec(&manifest).unwrap(),
            development_from: manifest.window.from,
            development_to: manifest.window.to,
            selection_from: "2026-08-01".parse().unwrap(),
            trial_cap: loop_settings.campaign.trial_cap,
            now: ist("2026-11-26", 16, 0),
        })
        .unwrap();

    let dec_clock = || ist("2026-12-02", 12, 0);
    let (client, peer) = connect(|peer| {
        let (id, turn) = peer.turn();
        assert_eq!(
            brief(&turn)["campaign"]["development_to"],
            json!("2026-10-31")
        );
        peer.finish(&id, "ok", "done", 1, 0.01);
    });
    let mut agent = fixture_agent(client);
    let mut jobs = ScriptedJobs(Vec::new());
    {
        let mut runner = LaneRunner::new(&mut agent, &agent_settings, &c, &cfg, &dec_clock);
        let mut ctx = context("5000");
        let mut covers = |_: NaiveDate| Ok(true);
        let report = step(&mut Step {
            c: &c,
            cfg: &cfg,
            owner: "test:1",
            clock: &dec_clock,
            turns: &mut RunnerTurns {
                runner: &mut runner,
                context: &mut ctx,
            },
            jobs: &mut jobs,
            stop: &|| false,
            tape: &mut covers,
            trading_today: false,
            sealed_today: false,
            last_sealed: "2026-12-01".parse().unwrap(),
        })
        .unwrap();
        assert_eq!(report.state, "proposed");
        let report_json = serde_json::to_value(&report).unwrap();
        assert_eq!(report_json["tape_state"], json!("using_previous_tape"));
        assert_eq!(report_json["tape_session"], json!("2026-10-31"));
        let status_json = super::status(&c, &cfg, dec_clock());
        assert_eq!(status_json["tape_state"], json!("using_previous_tape"));
        assert_eq!(status_json["tape_session"], json!("2026-10-31"));
        assert_eq!(
            status_json["campaign"]["development_to"],
            json!("2026-10-31")
        );
    }
    peer.join().unwrap();
}

#[test]
#[ignore = "requires NIVRA_ARCHIVE_TEST_URL and bubblewrap"]
fn confirmation_reads_real_bars_once_and_keeps_only_the_sealed_verdict()
-> crate::runtime::Result<()> {
    let dir = tempfile::tempdir()?;
    let (mut c, mut cfg) = loop_config(dir.path(), |_| {});
    c.market_archive = Some(crate::runtime::market_archive::test_settings(dir.path())?);
    let window = seed_qualification_bars(&c)?;
    let settings = cfg.autoresearch.as_mut().unwrap();
    settings.campaign.instruments = window.instruments;
    let settings = settings.clone();
    let mut store = open(&c)?;
    let mut manifest = super::jobs::campaign_manifest(&c, &cfg, &settings, "2026-07-31".parse()?)?;
    manifest.snapshot = Some(super::jobs::read_snapshot(&c, &manifest.window)?);
    let campaign = store.ensure_campaign(&nivra_store::research_memory::NewCampaign {
        name: &settings.campaign.name,
        segment: super::SEGMENT,
        manifest: &serde_json::to_vec(&manifest)?,
        development_from: manifest.window.from,
        development_to: manifest.window.to,
        selection_from: "2026-05-01".parse()?,
        trial_cap: settings.campaign.trial_cap,
        now: 1,
    })?;
    let (idea, _) = store.record_idea(&NewIdea {
        campaign_id: campaign.id,
        origin_request_key: None,
        family: "trend",
        regime: Regime::Trend,
        mechanism: "No orders during confirmation",
        hypothesis: "A bad finalist cannot borrow its discovery result",
        falsifier: "Confirmation must earn positive P&L",
        comparator: "cash",
        parent_experiment_id: None,
        now: 2,
    })?;
    let source_blob =
        store.put_object(b"def on_bars(at, split, rows, params, state): return []")?;
    let params_blob = store.put_object(b"{}")?;
    let generation = store
        .acquire_lease(super::LEASE, "test", 1, 3, 1000, &|_| true)?
        .unwrap()
        .generation;
    let Reserved::New(finalist, attempt) = store.reserve(&Submission {
        idea_id: idea.id,
        stage: Stage::Bars,
        quote_window: None,
        source_path: "scripts/finalist.py",
        source_blob: &source_blob,
        params_blob: &params_blob,
        manifest_blob: &campaign.manifest_digest,
        evaluator_digest: nivra_core::autoresearch::EVALUATOR,
        experiment_key: &"a".repeat(64),
        lease_generation: generation,
        deadline_ms: 5000,
        day: "2026-07-31".parse()?,
        limits: settings.day_limits(),
        now: 4,
    })?
    else {
        panic!("finalist fixture did not reserve");
    };
    let score = Score {
        net_pnl: Amount::from_integer(10),
        max_drawdown_bps: 5,
        closed_trades: 3,
        open_positions: 0,
    };
    let decision = keep(
        &score,
        &[Comparator::Cash],
        Rule {
            max_drawdown_bps: 300,
            require_flat: true,
        },
    );
    store.finish(
        attempt.id,
        generation,
        &Outcome::Scored {
            score,
            decision,
            report_blob: source_blob.clone(),
        },
        5,
    )?;
    assert!(store.freeze_finalist(
        campaign.id,
        finalist.id,
        "2026-08-01".parse()?,
        "2026-09-30".parse()?
    )?);
    let now = ist("2026-10-01", 20, 0);
    let through = "2026-10-01".parse()?;
    let confirm = |c: &Config, store: &mut nivra_store::research_memory::Store| {
        super::qualification::advance(c, &cfg, store, &campaign, through, now, &|| false)
    };
    assert_eq!(confirm(&c, &mut store)?, "holdout_finished");
    let sealed = store.holdout(campaign.id)?.unwrap();
    assert_eq!(sealed.verdict.as_deref(), Some("reject"), "{sealed:?}");
    let report: Value = serde_json::from_slice(&store.object(sealed.evidence.as_ref().unwrap())?)?;
    assert_eq!(report["reason"], "net_not_positive");
    assert!(report["scored_dates"].as_u64().unwrap() > 30);
    drop(store);
    c.market_archive = None;
    let mut store = open(&c)?;
    assert_eq!(confirm(&c, &mut store)?, "holdout_finished");
    assert_eq!(store.holdout(campaign.id)?, Some(sealed));
    assert!(!crate::runtime::strategy_library::root(&c)?.exists());
    Ok(())
}
