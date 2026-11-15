use super::*;
use sadr_cli::orchestrator::{Error as OrchestratorError, OrchestratorConfig};
use sadr_core::strategy::{Feature, FeatureRow};
use sadr_sandbox::orchestrator_policy::PolicyError;
use serde_json::json;
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const GOOD_SCORER: &str = "import json,sys\nwith open(sys.argv[1]) as handle:\n    payload = json.load(handle)\nparams = payload.pop('params')\nout = {}\nfor key, row in payload.items():\n    move = row.get('absolute_move_bps')\n    if move is not None:\n        out[key] = -float(move) + float(params.get('offset', 0.0))\nprint(json.dumps(out))\n";
const BROKEN_SCORER: &str = "raise RuntimeError('broken scorer')\n";

struct FakeModel {
    replies: VecDeque<String>,
    prompts: Vec<String>,
    called: bool,
}

impl FakeModel {
    fn new(reply: serde_json::Value) -> Self {
        Self::scripted(vec![reply])
    }

    fn scripted(replies: Vec<serde_json::Value>) -> Self {
        Self {
            replies: replies.into_iter().map(|reply| reply.to_string()).collect(),
            prompts: Vec::new(),
            called: false,
        }
    }
}

impl ScorerModel for FakeModel {
    fn complete(&mut self, system_prompt: &str) -> Result<String> {
        self.called = true;
        self.prompts.push(system_prompt.to_owned());
        self.replies
            .pop_front()
            .ok_or_else(|| "fixture_exhausted".into())
    }
}

fn settings() -> OrchestratorModelSettings {
    let prompt =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/rust/prompts/scorer.md");
    let mut settings = OrchestratorModelSettings {
        enabled: true,
        ..OrchestratorModelSettings::default()
    };
    settings.triggers.get_mut(&Trigger::Scorer).unwrap().prompt = prompt;
    settings
}

fn orchestrator(dir: &tempfile::TempDir) -> Orchestrator {
    Orchestrator::new(dir.path(), OrchestratorConfig::default()).unwrap()
}

fn replay_fixtures(dir: &tempfile::TempDir) {
    fs::write(
        dir.path().join("spec.json"),
        serde_json::to_vec(&StrategySpec::default()).unwrap(),
    )
    .unwrap();
    let snapshots = vec![FeatureSnapshot {
        observed_ms: 1000,
        lookback_samples: 20,
        rows: vec![],
    }];
    fs::write(
        dir.path().join("snapshots.json"),
        serde_json::to_vec(&snapshots).unwrap(),
    )
    .unwrap();
}

fn scorer_sandbox_ready() -> bool {
    let sandbox = ScorerSandboxConfig::default();
    sandbox.python_path.is_file()
        && sandbox.bwrap_path.is_file()
        && Command::new(&sandbox.bwrap_path)
            .args(["--unshare-all", "--ro-bind", "/", "/", "/usr/bin/true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

fn require_scorer_sandbox(fixture: &str) -> bool {
    if scorer_sandbox_ready() {
        return true;
    }
    assert!(
        std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
        "{fixture} requires python3 and bubblewrap"
    );
    eprintln!("skipping {fixture}: python3 or bwrap unavailable on host");
    false
}

fn scorer_replay_fixtures(dir: &tempfile::TempDir) {
    let fixtures = dir.path().join("proposals/fixtures");
    fs::create_dir_all(&fixtures).unwrap();
    fs::write(
        fixtures.join("spec.json"),
        serde_json::to_vec(&StrategySpec::default()).unwrap(),
    )
    .unwrap();
    let snapshots: Vec<FeatureSnapshot> = [0i64, 60_000, 120_000]
        .into_iter()
        .enumerate()
        .map(|(index, observed_ms)| FeatureSnapshot {
            observed_ms,
            lookback_samples: 20,
            rows: ["A", "B"]
                .into_iter()
                .enumerate()
                .map(|(row, key)| FeatureRow {
                    key: key.into(),
                    features: std::collections::BTreeMap::from([
                        (Feature::Price, 100.0 + index as f64 * 10.0),
                        (Feature::AbsoluteMoveBps, 300.0 + row as f64),
                        (Feature::TradedValue, 2_000_000.0),
                        (Feature::CircuitDistanceBps, 500.0),
                    ]),
                })
                .collect(),
        })
        .collect();
    fs::write(
        fixtures.join("snapshots.json"),
        serde_json::to_vec(&snapshots).unwrap(),
    )
    .unwrap();
}

fn scorer_host() -> HostContext {
    HostContext {
        session: "2026-10-16".into(),
        spec: SPEC_PATH.into(),
        snapshots: SNAPSHOTS_PATH.into(),
        scorer: SCORER_PATH.into(),
        params: PARAMS_PATH.into(),
        out: OUT_PATH.into(),
        publish_required: true,
    }
}

fn scorer_reply(scorer: &str, publish: bool) -> serde_json::Value {
    let mut replay = json!({
        "tool":"replay",
        "spec":SPEC_PATH,
        "snapshots":SNAPSHOTS_PATH,
        "out":OUT_PATH,
        "scorer":SCORER_PATH,
        "params":PARAMS_PATH
    });
    if publish {
        replay["publish"] = json!({"scorer":SCORER_PATH,"params":PARAMS_PATH});
    }
    json!({"actions":[
        {"tool":"sandbox_write","path":SCORER_PATH,"text":scorer},
        {"tool":"sandbox_write","path":PARAMS_PATH,"text":"{\"offset\":0}"},
        replay
    ]})
}

fn replay_settings() -> ScorerReplaySettings {
    ScorerReplaySettings {
        horizons_minutes: vec![1],
        min_valid_snapshots: 3,
        ..ScorerReplaySettings::default()
    }
}

fn assert_scorer_harness_archive() {
    let dir = tempfile::tempdir().unwrap();
    let c = crate::runtime::tests::config(dir.path());
    let role = planner::ModelRole {
        enabled: true,
        model_id: crate::model::LUNA.into(),
        transport: sadr_cli::native_transport::Transport::OpenaiCodex,
        daily_call_budget: 8,
        ..planner::ModelRole::default()
    };
    struct OneShot(Option<Value>);
    impl planner::Backend for OneShot {
        fn call(&mut self, _: &planner::ModelRole, _: &str) -> Result<String> {
            Ok(self.0.take().ok_or("fixture_exhausted")?.to_string())
        }
    }
    let response = json!({
        "usage":{"input_tokens":10,"output_tokens":3},
        "output":[{"type":"message","role":"assistant","content":[
            {"type":"output_text","text":"{\"actions\":[]}"}
        ]}]
    });
    let raw = crate::runtime::harness::run(
        &c,
        &PlannerConfig::default(),
        "scorer",
        &role,
        "Return {\"actions\":[]}",
        &json!({"trigger":{"kind":"post_close"}}),
        &mut OneShot(Some(response)),
        &planner::preview_guard(),
    )
    .unwrap();
    assert_eq!(raw, "{\"actions\":[]}");
    let index = fs::read_to_string(c.directory.join("model/runs.ndjson")).unwrap();
    let indexed: Value = serde_json::from_str(index.trim()).unwrap();
    assert_eq!(indexed["role"], "scorer");
    assert_eq!(indexed["trigger_kind"], "post_close");
    assert!(
        fs::read_dir(c.directory.join("model"))
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("harness-scorer-"))
    );
}

fn policy_error(error: &(dyn std::error::Error + 'static)) -> PolicyError {
    match error.downcast_ref::<OrchestratorError>() {
        Some(OrchestratorError::Policy(error)) => *error,
        other => panic!("expected orchestrator policy error, got {other:?}"),
    }
}

#[test]
fn writes_proposal_and_runs_replay_through_the_policy() {
    let dir = tempfile::tempdir().unwrap();
    replay_fixtures(&dir);
    let orchestrator = orchestrator(&dir);
    let mut model = FakeModel::new(json!({"actions": [
        {
            "tool": "sandbox_write",
            "path": "proposals/run/proposal.json",
            "text": "{\"kind\":\"proposal\"}"
        },
        {
            "tool": "replay",
            "spec": "spec.json",
            "snapshots": "snapshots.json",
            "out": "proposals/run/report.json"
        }
    ]}));

    let run = run_scorer(&settings(), &orchestrator, &mut model).unwrap();
    assert_eq!(run.actions_run, 2);
    assert_eq!(
        run.written,
        vec![PathBuf::from("proposals/run/proposal.json")]
    );
    assert_eq!(run.replays.len(), 1);
    assert_eq!(run.replays[0].ticks, 1);
    assert!(run.published.is_empty());
    assert!(dir.path().join("proposals/run/proposal.json").exists());
    assert!(dir.path().join("proposals/run/report.json").exists());
}

#[test]
fn policy_refusals_keep_the_original_policy_error() {
    let dir = tempfile::tempdir().unwrap();
    let orchestrator = orchestrator(&dir);

    let mut outside = FakeModel::new(json!({"actions": [{
        "tool": "sandbox_write",
        "path": "denied/x.json",
        "text": "denied"
    }]}));
    let error = run_scorer(&settings(), &orchestrator, &mut outside).unwrap_err();
    assert_eq!(
        policy_error(error.as_ref()),
        PolicyError::OutsideAllowedRoots
    );
    assert!(!dir.path().join("denied").exists());

    let mut traversal = FakeModel::new(json!({"actions": [{
        "tool": "sandbox_write",
        "path": "strategies/../escape.json",
        "text": "denied"
    }]}));
    let error = run_scorer(&settings(), &orchestrator, &mut traversal).unwrap_err();
    assert_eq!(policy_error(error.as_ref()), PolicyError::ParentTraversal);
    assert!(!dir.path().join("escape.json").exists());
}

#[test]
fn network_is_refused_before_the_model_runs() {
    let dir = tempfile::tempdir().unwrap();
    let orchestrator = orchestrator(&dir);
    let mut settings = settings();
    settings.sandbox.network = true;
    let mut model = FakeModel::new(json!({"actions": []}));

    let error = run_scorer(&settings, &orchestrator, &mut model).unwrap_err();
    assert_eq!(error.to_string(), "scorer_sandbox_network_forbidden");
    assert!(!model.called);

    // The post-close replay sandbox is refused too, even when the orchestrator
    // sandbox setting is left at its network-disabled default.
    let mut model = FakeModel::new(json!({"actions": []}));
    let error = run_scorer_with_replay(
        &self::settings(),
        &orchestrator,
        &mut model,
        None,
        None,
        &replay_settings(),
        &ScorerSandboxConfig {
            network: true,
            ..ScorerSandboxConfig::default()
        },
        None,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "scorer_sandbox_network_forbidden");
    assert!(!model.called);
}

#[test]
fn tool_missing_from_the_trigger_list_is_refused_before_execution() {
    let dir = tempfile::tempdir().unwrap();
    let orchestrator = orchestrator(&dir);
    let mut settings = settings();
    settings.triggers.get_mut(&Trigger::Scorer).unwrap().tools = Some(vec!["sandbox_write".into()]);
    let mut model = FakeModel::new(json!({"actions": [
        {
            "tool": "sandbox_write",
            "path": "proposals/run/proposal.json",
            "text": "should not be written"
        },
        {
            "tool": "replay",
            "spec": "spec.json",
            "snapshots": "snapshots.json",
            "out": "proposals/run/report.json"
        }
    ]}));

    let error = run_scorer(&settings, &orchestrator, &mut model).unwrap_err();
    assert_eq!(error.to_string(), "scorer_tool_not_allowed");
    assert!(!dir.path().join("proposals").exists());
}

#[test]
fn too_many_actions_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let orchestrator = orchestrator(&dir);
    let mut settings = settings();
    settings
        .triggers
        .get_mut(&Trigger::Scorer)
        .unwrap()
        .max_turns = Some(1);
    let mut model = FakeModel::new(json!({"actions": [
        {
            "tool": "sandbox_write",
            "path": "proposals/a.json",
            "text": "a"
        },
        {
            "tool": "sandbox_write",
            "path": "proposals/b.json",
            "text": "b"
        }
    ]}));

    let error = run_scorer(&settings, &orchestrator, &mut model).unwrap_err();
    assert_eq!(error.to_string(), "scorer_too_many_actions");
    assert!(!dir.path().join("proposals").exists());
}

#[test]
fn disabled_or_unconfigured_trigger_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let orchestrator = orchestrator(&dir);
    let mut disabled = settings();
    disabled.enabled = false;
    let mut model = FakeModel::new(json!({"actions": []}));
    assert_eq!(
        run_scorer(&disabled, &orchestrator, &mut model)
            .unwrap_err()
            .to_string(),
        "scorer_trigger_disabled"
    );
    assert!(!model.called);

    let mut unconfigured = settings();
    unconfigured.triggers.remove(&Trigger::Scorer);
    let mut model = FakeModel::new(json!({"actions": []}));
    assert_eq!(
        run_scorer(&unconfigured, &orchestrator, &mut model)
            .unwrap_err()
            .to_string(),
        "scorer_trigger_disabled"
    );
    assert!(!model.called);
}

fn due_requires_a_closed_session_and_the_configured_minute() {
    assert!(!scorer_due(935, false, 930));
    assert!(!scorer_due(929, true, 930));
    assert!(scorer_due(930, true, 930));
    assert!(scorer_due(1200, true, 930));
}

fn planner_settings(after_minute: Option<u32>) -> PlannerConfig {
    let mut orchestrator = settings();
    orchestrator
        .triggers
        .get_mut(&Trigger::Scorer)
        .unwrap()
        .after_minute = after_minute;
    PlannerConfig {
        enabled: true,
        orchestrator: Some(orchestrator),
        ..PlannerConfig::default()
    }
}

fn at(hour: u32, minute: u32) -> i64 {
    chrono::DateTime::parse_from_rfc3339(&format!("2026-10-16T{hour:02}:{minute:02}:00+05:30"))
        .unwrap()
        .timestamp_millis()
}

fn closed_book(dir: &Path) -> Portfolio {
    fs::write(dir.join("report.json"), "{}").unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    p.closed = true;
    p
}

#[test]
fn tick_skips_a_disabled_trigger_and_waits_before_the_close_minute() {
    let dir = tempfile::tempdir().unwrap();
    let c = crate::runtime::tests::config(dir.path());
    let p = closed_book(dir.path());
    let mut db = Connection::open_in_memory().unwrap();

    let mut disabled = planner_settings(Some(931));
    disabled
        .orchestrator
        .as_mut()
        .unwrap()
        .triggers
        .get_mut(&Trigger::Scorer)
        .unwrap()
        .enabled = false;
    let status = tick(&mut db, &c, Some(&disabled), &p, at(16, 0), &mut None).unwrap();
    assert_eq!(status["status"], "skipped");
    assert_eq!(status["reason"], "trigger_disabled");

    let mut db = Connection::open_in_memory().unwrap();
    let cfg = planner_settings(Some(931));
    let status = tick(&mut db, &c, Some(&cfg), &p, at(10, 0), &mut None).unwrap();
    assert_eq!(status["status"], "awaiting_close");
    assert_eq!(status["reason"], "not_due");
    assert!(!table_exists(&db).unwrap());
    due_requires_a_closed_session_and_the_configured_minute();
}

#[test]
fn admission_is_durable_and_a_restart_settles_as_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let c = crate::runtime::tests::config(dir.path());
    let p = closed_book(dir.path());
    let cfg = planner_settings(Some(931));
    let mut db = Connection::open_in_memory().unwrap();
    let now = at(15, 40);

    let Admitted::Run(admission) = admit(&mut db, &c, Some(&cfg), &p, now).unwrap() else {
        panic!("expected a due admission");
    };
    assert_eq!(admission.after_minute, 931);
    assert_eq!(status(&db, &c).unwrap()["status"], "reserved");

    let Admitted::Settled(settled) = admit(&mut db, &c, Some(&cfg), &p, now + 1).unwrap() else {
        panic!("restart must not reserve a second run");
    };
    assert_eq!(settled["status"], "interrupted");
    assert_eq!(settled["reason"], "process_restart_no_replay");
    let trials = super::experiment_ledger::list(&c).unwrap();
    assert_eq!(trials.len(), 1);
    assert_eq!(trials[0].state, "execution_failed");
    let encoded = serde_json::to_string(&trials[0]).unwrap();
    assert!(encoded.contains("\"state\":\"execution_failed\""));
    let historic: super::experiment_ledger::Trial = serde_json::from_str(&encoded).unwrap();
    assert_eq!(historic.state, "execution_failed");
    let old_shaped = encoded.replace(
        "\"state\":\"execution_failed\"",
        "\"state\":\"insufficient_evidence\"",
    );
    let old_row: super::experiment_ledger::Trial = serde_json::from_str(&old_shaped).unwrap();
    assert_eq!(old_row.state.as_str(), "insufficient_evidence");
    assert!(!trials[0].execution_ok);
    assert!(!trials[0].edge_qualified);
    assert!(!settled["experiment_edge_qualified"].as_bool().unwrap());
    let mut changed = trials[0].clone();
    changed.reason = "replaced".into();
    assert_eq!(
        super::experiment_ledger::record(&c, &changed)
            .unwrap_err()
            .to_string(),
        "experiment_trial_identity_changed"
    );
    let mut poor = trials[0].clone();
    poor.trial_id = super::experiment_ledger::trial_id(&["poor-candidate", &c.session.to_string()]);
    poor.state = "insufficient_evidence".into();
    poor.reason = "failed_snapshots".into();
    poor.source_sha256 = "a".repeat(64);
    poor.params_sha256 = "b".repeat(64);
    poor.dataset_hash = "c".repeat(64);
    poor.recorded_at_ms += 1;
    super::experiment_ledger::record(&c, &poor).unwrap();
    let retained = super::experiment_ledger::list(&c).unwrap();
    assert_eq!(retained.len(), 2);
    assert_eq!(retained[0].trial_id, trials[0].trial_id);
    assert_eq!(retained[1].state, "insufficient_evidence");
    poor.edge_qualified = true;
    assert_eq!(
        super::experiment_ledger::record(&c, &poor)
            .unwrap_err()
            .to_string(),
        "experiment_edge_not_established"
    );
    assert_eq!(
        db.query_row("SELECT status FROM scorer_control WHERE id=1", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "interrupted"
    );

    // A closed session with no scanner-purpose snapshot evidence ends in an
    // explicit terminal status carrying the window counts; it is not a
    // silent loss and no model call is made (#163).
    fs::write(
            dir.path().join("wide-universe.json"),
            serde_json::to_vec(&json!({
                "session":"2026-10-16",
                "instruments":[{"key":"NSE_EQ|INE001B01026","symbol":"KRBL","series":"EQ","tick":"0.05"}]
            }))
            .unwrap(),
        )
        .unwrap();
    super::open_book(&c).unwrap();
    let snapshot_error = run_real(
        &c,
        &cfg,
        &Admission {
            session: c.session,
            binding: super::book_binding(&c).unwrap(),
            configuration_hash: config_hash(&cfg).unwrap(),
            prompt_sha256: "a".repeat(64),
            model_id: "fixture-model".into(),
            after_minute: 931,
            admitted_ms: now,
            deadline_ms: now + 600_000,
        },
        &Arc::new(AtomicBool::new(false)),
    )
    .unwrap_err()
    .downcast::<SnapshotError>()
    .expect("snapshot failure");
    assert_eq!(snapshot_error.reason, "scorer_snapshots_unavailable");
    assert_eq!(snapshot_error.detail["window_rows"], 0);
    assert_eq!(snapshot_error.detail["scanner_rows"], 0);
    assert_eq!(
        snapshot_error.detail["source"],
        "session_paper_quotes_read_only"
    );
    // A streamed session's scanner batch is archived as scanner_metadata;
    // its full quote payload is valid screening evidence for the scorer.
    let metadata_at = at(15, 40);
    let quote_body = json!({
        "status":"success",
        "data":{"NSE_EQ:KRBL":{
            "instrument_token":"NSE_EQ|INE001B01026",
            "symbol":"KRBL",
            "timestamp":metadata_at,
            "depth":{"buy":[{"price":100,"quantity":100}],"sell":[{"price":100.05,"quantity":100}]},
            "lower_circuit_limit":80,
            "upper_circuit_limit":120,
            "volume":1_000_000
        }}
    });
    let archive = json!({
        "source":"upstox-readonly-v1",
        "purpose":"scanner_metadata",
        "requested_ms":metadata_at,
        "received_ms":metadata_at,
        "body":quote_body.to_string()
    });
    let db = super::open_book(&c).unwrap();
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,?,?,'scanner_metadata')",
        rusqlite::params![metadata_at, metadata_at, archive.to_string()],
    )
    .unwrap();
    drop(db);
    let snapshots =
        super::paper_replay::scorer_snapshots(&c.directory, c.session, 60, 2_000, 20).unwrap();
    assert_eq!(snapshots.len(), 1);
    let mut db = Connection::open_in_memory().unwrap();
    let Admitted::Run(admission) = admit(&mut db, &c, Some(&cfg), &p, now).unwrap() else {
        panic!("expected a due admission");
    };
    settle(
        &db,
        &c,
        &admission,
        Err((snapshot_error.reason, Some(snapshot_error.detail))),
        now,
    )
    .unwrap();
    let saved: Value = serde_json::from_str(
        &db.query_row("SELECT result FROM scorer_control WHERE id=1", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(saved["reason"], "scorer_snapshots_unavailable");
    assert_eq!(saved["detail"]["scanner_rows"], 0);
}

#[test]
fn trigger_after_minute_falls_back_to_the_top_level_setting() {
    let dir = tempfile::tempdir().unwrap();
    let c = crate::runtime::tests::config(dir.path());
    let p = closed_book(dir.path());
    let mut cfg = planner_settings(None);
    cfg.orchestrator.as_mut().unwrap().scorer_after_minute = Some(931);
    let mut db = Connection::open_in_memory().unwrap();
    let Admitted::Run(admission) = admit(&mut db, &c, Some(&cfg), &p, at(15, 40)).unwrap() else {
        panic!("expected the top-level due minute to admit");
    };
    assert_eq!(admission.after_minute, 931);

    missing_after_minute_is_retryable_after_the_config_is_corrected();
    absent_scorer_after_minute_does_not_change_serialized_settings();
}

fn missing_after_minute_is_retryable_after_the_config_is_corrected() {
    let dir = tempfile::tempdir().unwrap();
    let c = crate::runtime::tests::config(dir.path());
    let p = closed_book(dir.path());
    let mut db = Connection::open_in_memory().unwrap();
    let first = tick(
        &mut db,
        &c,
        Some(&planner_settings(None)),
        &p,
        at(15, 40),
        &mut None,
    )
    .unwrap();
    assert_eq!(first["status"], "configuration_error");
    assert_eq!(first["reason"], "after_minute_unset");
    assert!(table_exists(&db).unwrap());
    assert_eq!(status(&db, &c).unwrap()["status"], "configuration_error");
    // The unchanged bad config is not re-admitted or logged again.
    assert!(matches!(
        admit(&mut db, &c, Some(&planner_settings(None)), &p, at(15, 41)).unwrap(),
        Admitted::Settled(_)
    ));

    let Admitted::Run(admission) = admit(
        &mut db,
        &c,
        Some(&planner_settings(Some(931))),
        &p,
        at(15, 40),
    )
    .unwrap() else {
        panic!("a corrected config must admit the same session");
    };
    assert_eq!(admission.after_minute, 931);
    assert_eq!(status(&db, &c).unwrap()["status"], "reserved");

    // A journal written by an older release may already hold the terminal
    // configuration skip. It must not block the corrected config.
    let mut legacy = Connection::open_in_memory().unwrap();
    record_skip(
        &legacy,
        &c,
        Some(&planner_settings(None)),
        0,
        "after_minute_unset",
        at(15, 40),
    )
    .unwrap();
    assert_eq!(status(&legacy, &c).unwrap()["status"], "skipped");
    let Admitted::Run(admission) = admit(
        &mut legacy,
        &c,
        Some(&planner_settings(Some(931))),
        &p,
        at(15, 40),
    )
    .unwrap() else {
        panic!("a config-error skip must be re-admittable");
    };
    assert_eq!(admission.after_minute, 931);
    assert_eq!(status(&legacy, &c).unwrap()["status"], "reserved");
}
#[test]
fn exhausted_budget_is_recorded_without_admission() {
    let dir = tempfile::tempdir().unwrap();
    let c = crate::runtime::tests::config(dir.path());
    let p = closed_book(dir.path());
    let cfg = planner_settings(Some(931));
    let usage = c.directory.join("model/usage");
    fs::create_dir_all(&usage).unwrap();
    for index in 0..4 {
        fs::write(usage.join(format!("scorer-{index}.attempt.json")), "{}").unwrap();
    }
    let mut db = Connection::open_in_memory().unwrap();
    let status = tick(&mut db, &c, Some(&cfg), &p, at(15, 40), &mut None).unwrap();
    assert_eq!(status["status"], "skipped");
    assert_eq!(status["reason"], "budget_exhausted");
    assert_eq!(status["result"]["reason"], "budget_exhausted");
}

#[test]
fn successful_replay_publishes_only_written_workspace_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    replay_fixtures(&dir);
    let orchestrator = orchestrator(&dir);
    let published = tempfile::tempdir().unwrap();
    let targets = PublishTargets {
        scorer: published.path().join("shadow/scorer.json"),
        params: published.path().join("shadow/params.json"),
    };
    let mut model = FakeModel::new(json!({"actions": [
        {"tool":"sandbox_write","path":"proposals/run/scorer.json","text":"{\"score\":1}"},
        {"tool":"sandbox_write","path":"proposals/run/params.json","text":"{\"threshold\":2}"},
        {"tool":"replay","spec":"spec.json","snapshots":"snapshots.json","out":"proposals/run/report.json",
         "publish":{"scorer":"proposals/run/scorer.json","params":"proposals/run/params.json"}}
    ]}));

    let run = run_scorer_with_publish(&settings(), &orchestrator, &mut model, None, Some(&targets))
        .unwrap();
    assert_eq!(
        run.published,
        vec![targets.scorer.clone(), targets.params.clone()]
    );
    assert_eq!(fs::read(&targets.scorer).unwrap(), b"{\"score\":1}");
    assert_eq!(fs::read(&targets.params).unwrap(), b"{\"threshold\":2}");

    // A harness-level scorer call archives one run directory and one index
    // row, matching the supervisor contract (#134).
    assert_scorer_harness_archive();

    if !require_scorer_sandbox("scorer replay qualification") {
        return;
    }
    let scorer_dir = tempfile::tempdir().unwrap();
    scorer_replay_fixtures(&scorer_dir);
    let scorer_orchestrator = self::orchestrator(&scorer_dir);
    let scorer_published = tempfile::tempdir().unwrap();
    let scorer_targets = PublishTargets {
        scorer: scorer_published.path().join("shadow/scorer.py"),
        params: scorer_published.path().join("shadow/params.json"),
    };
    let mut model = FakeModel::new(scorer_reply(GOOD_SCORER, true));
    let run = run_scorer_with_replay(
        &settings(),
        &scorer_orchestrator,
        &mut model,
        Some(&scorer_host()),
        Some(&scorer_targets),
        &replay_settings(),
        &ScorerSandboxConfig::default(),
        None,
    )
    .unwrap();
    assert_eq!(run.scorer_replays.len(), 1);
    let report = &run.scorer_replays[0];
    assert!(report.qualified());
    assert!(report.mean_top_n_overlap.is_some());
    assert!(report.horizons[0].mean_forward_return_bps.is_some());
    assert_eq!(
        run.published,
        vec![scorer_targets.scorer.clone(), scorer_targets.params.clone()]
    );
    assert_eq!(
        fs::read(&scorer_targets.scorer).unwrap(),
        GOOD_SCORER.as_bytes()
    );
    let sidecar = qualification_sidecar_path(&scorer_targets.scorer).unwrap();
    let summary: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    assert_eq!(summary["qualified"], true);
    assert_eq!(
        summary["report_hash"].as_str(),
        Some(report.report_hash.as_str())
    );
    assert_eq!(
        summary["dataset_hash"].as_str(),
        Some(report.dataset_hash.as_str())
    );
    assert_eq!(
        summary["scorer_sha256"].as_str(),
        Some(report.scorer_sha256.as_str())
    );
    assert_eq!(
        summary["params_sha256"].as_str(),
        Some(report.params_sha256.as_str())
    );

    let broken_dir = tempfile::tempdir().unwrap();
    scorer_replay_fixtures(&broken_dir);
    let broken_orchestrator = self::orchestrator(&broken_dir);
    let broken_published = tempfile::tempdir().unwrap();
    let broken_targets = PublishTargets {
        scorer: broken_published.path().join("shadow/scorer.py"),
        params: broken_published.path().join("shadow/params.json"),
    };
    let mut model = FakeModel::new(scorer_reply(BROKEN_SCORER, true));
    let run = run_scorer_with_replay(
        &settings(),
        &broken_orchestrator,
        &mut model,
        Some(&scorer_host()),
        Some(&broken_targets),
        &ScorerReplaySettings {
            max_replays: 1,
            ..replay_settings()
        },
        &ScorerSandboxConfig::default(),
        None,
    )
    .unwrap();
    assert_eq!(run.scorer_replays.len(), 1);
    assert!(!run.scorer_replays[0].qualified());
    assert_eq!(run.scorer_replays[0].failed_snapshots, 3);
    assert!(run.published.is_empty());
    assert!(!broken_targets.scorer.exists());
    assert!(!broken_targets.params.exists());
    assert!(
        !qualification_sidecar_path(&broken_targets.scorer)
            .unwrap()
            .exists()
    );
}

#[test]
fn configured_publish_requires_one_replay_and_written_paths_before_any_write() {
    let dir = tempfile::tempdir().unwrap();
    let orchestrator = orchestrator(&dir);
    let published = tempfile::tempdir().unwrap();
    let targets = PublishTargets {
        scorer: published.path().join("scorer.json"),
        params: published.path().join("params.json"),
    };

    let mut no_replay = FakeModel::new(json!({"actions": [
        {"tool":"sandbox_write","path":"proposals/run/scorer.json","text":"x"}
    ]}));
    let error = run_scorer_with_publish(
        &settings(),
        &orchestrator,
        &mut no_replay,
        None,
        Some(&targets),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "scorer_replay_required");
    assert!(!dir.path().join("proposals").exists());

    let mut unwritten = FakeModel::new(json!({"actions": [
        {"tool":"sandbox_write","path":"proposals/run/scorer.json","text":"x"},
        {"tool":"replay","spec":"spec.json","snapshots":"snapshots.json","out":"proposals/run/report.json",
         "publish":{"scorer":"proposals/run/scorer.json","params":"proposals/run/params.json"}}
    ]}));
    let error = run_scorer_with_publish(
        &settings(),
        &orchestrator,
        &mut unwritten,
        None,
        Some(&targets),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "scorer_publish_unwritten");
    assert!(!dir.path().join("proposals").exists());
    assert!(!published.path().join("scorer.json").exists());

    if !require_scorer_sandbox("scorer feedback publication") {
        return;
    }
    // Broken first reply, fixed second reply: only the fixed script is
    // published and the next prompt carries the prior report summary.
    let feedback_dir = tempfile::tempdir().unwrap();
    scorer_replay_fixtures(&feedback_dir);
    let feedback_orchestrator = self::orchestrator(&feedback_dir);
    let feedback_published = tempfile::tempdir().unwrap();
    let feedback_targets = PublishTargets {
        scorer: feedback_published.path().join("shadow/scorer.py"),
        params: feedback_published.path().join("shadow/params.json"),
    };
    let mut model = FakeModel::scripted(vec![
        scorer_reply(BROKEN_SCORER, true),
        scorer_reply(GOOD_SCORER, true),
    ]);
    let run = run_scorer_with_replay(
        &settings(),
        &feedback_orchestrator,
        &mut model,
        Some(&scorer_host()),
        Some(&feedback_targets),
        &replay_settings(),
        &ScorerSandboxConfig::default(),
        None,
    )
    .unwrap();
    assert_eq!(model.prompts.len(), 2);
    assert!(model.prompts[1].contains("Previous scorer replay report"));
    assert!(model.prompts[1].contains("report_hash"));
    assert_eq!(run.scorer_replays.len(), 2);
    assert!(!run.scorer_replays[0].qualified());
    assert!(run.scorer_replays[1].qualified());
    assert_eq!(
        run.published,
        vec![
            feedback_targets.scorer.clone(),
            feedback_targets.params.clone()
        ]
    );
    assert_eq!(
        fs::read(&feedback_targets.scorer).unwrap(),
        GOOD_SCORER.as_bytes()
    );
    let sidecar = qualification_sidecar_path(&feedback_targets.scorer).unwrap();
    let summary: Value = serde_json::from_slice(&fs::read(&sidecar).unwrap()).unwrap();
    assert_eq!(
        summary["report_hash"].as_str(),
        Some(run.scorer_replays[1].report_hash.as_str())
    );
}

fn absent_scorer_after_minute_does_not_change_serialized_settings() {
    let default = serde_json::to_value(OrchestratorModelSettings::default()).unwrap();
    assert!(default.get("scorer_after_minute").is_none());

    let settings = OrchestratorModelSettings {
        scorer_after_minute: Some(935),
        ..OrchestratorModelSettings::default()
    };
    let encoded = serde_json::to_value(&settings).unwrap();
    assert_eq!(encoded["scorer_after_minute"], 935);

    let decoded: OrchestratorModelSettings = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.scorer_after_minute, Some(935));
}
