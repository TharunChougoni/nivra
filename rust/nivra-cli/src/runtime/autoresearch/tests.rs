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
    let mut loop_settings = AutoresearchSettings {
        enabled: true,
        ..Default::default()
    };
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

fn report(net: &str) -> JobEnd {
    let split = |net: &str| json!({"mark_to_market_pnl":net,"max_drawdown_bps":40,"closed_trades":12,"open_positions":0,"fill_sample":[]});
    JobEnd::Report(
        serde_json::to_vec(
            &json!({"abi":"archive_bars_v1","training":split("10"),"test":split(net)}),
        )
        .unwrap(),
    )
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
    let campaign = store.campaign("bars-v1").unwrap().unwrap();
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
fn after_the_seal_one_idea_runs_one_job_to_a_verdict_under_budget_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = loop_config(dir.path(), |s| {
        s.day.usd = 1.0;
        s.turn.usd = 0.4;
    });
    let now = Arc::new(AtomicI64::new(ist("2026-11-27", 23, 40)));
    let clock_now = now.clone();
    let clock = move || clock_now.load(Ordering::SeqCst);
    let agent_settings = settings(100, 10.0);
    let source = "def on_bars(at_ms, split, bars, params, state):\n    return []\n";
    let (client, peer) = connect(move |peer| {
        let (id, turn) = peer.turn();
        assert_eq!(
            names(&turn),
            [
                "research_history",
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
    let mut jobs = ScriptedJobs(vec![JobEnd::Stopped, report("120")]);
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
    let campaign = store.campaign("bars-v1").unwrap().unwrap();
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
