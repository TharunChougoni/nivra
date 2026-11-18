use super::*;

fn registration(root: &Path, runtime: &Path) -> Registration {
    let code = root.join("analysis.py");
    let input = root.join("input.json");
    std::fs::write(&code, b"print('fixture')\n").unwrap();
    std::fs::write(&input, b"{\"fixture\":true}\n").unwrap();
    Registration {
        analysis_id: "shared_core_backtest".into(),
        dataset_id: "synthetic_no_quotes_fixture".into(),
        code_file: code,
        runtime_file: runtime.to_path_buf(),
        input_file: input,
        code_sha256: digest(b"print('fixture')\n"),
        runtime_sha256: digest(b"nivra-analysis-fixture"),
        input_sha256: digest(b"{\"fixture\":true}\n"),
        memory_bytes: 64 * 1024 * 1024,
        wall_millis: 5_000,
        limits: ResourceLimits {
            cpu_millis: 1_000,
            tasks: 2,
            workspace_bytes: 1024 * 1024,
            file_bytes: 65536,
        },
    }
}

fn settings(root: &Path, runtime: &Path) -> ResearchSettings {
    ResearchSettings {
        queue: root.join("research.sqlite"),
        stage_root: root.join("stage"),
        task_ttl_ms: default_task_ttl_ms(),
        termination_grace_ms: default_termination_grace_ms(),
        admission_limits: None,
        library_max_candidates: None,
        library_max_bytes: None,
        candidate_compute: None,
        registrations: vec![registration(root, runtime)],
    }
}

fn owner_fixture(root: &Path, source: &ResearchSettings) -> (Config, PlannerConfig) {
    let c = super::super::super::tests::config(root);
    let mut research = source.clone();
    research.queue = root.join("research.sqlite");
    research.stage_root = root.join("research-stage");
    let mut orchestrator =
        super::super::super::orchestrator_model::OrchestratorModelSettings::default();
    for trigger in super::super::super::orchestrator_model::Trigger::ALL {
        let path = root.join(format!("orchestrator-{}.md", trigger.as_str()));
        std::fs::write(&path, "owner research fixture\n").unwrap();
        orchestrator.triggers.get_mut(&trigger).unwrap().prompt = path;
    }
    let cfg = super::super::super::planner::PlannerConfig {
        orchestrator: Some(orchestrator),
        research_tools: Some(research),
        ..Default::default()
    };
    (c, cfg)
}

use std::sync::atomic::{AtomicBool, Ordering};

fn ensure_test_state(c: &Config) {
    let db = super::super::super::open_book(c).unwrap();
    let exists: i64 = db
        .query_row("SELECT count(*) FROM state", [], |row| row.get(0))
        .unwrap();
    if exists == 0 {
        let portfolio = nivra_core::execution::Portfolio::with_capital(
            nivra_core::finance::Amount::parse("5000").unwrap(),
        );
        db.execute(
            "INSERT INTO state VALUES(1,?,?)",
            rusqlite::params![
                super::super::super::book_binding(c).unwrap(),
                serde_json::to_string(&portfolio).unwrap()
            ],
        )
        .unwrap();
    }
}

fn candidate_at_on(day: &str, hour: u32, minute: u32) -> i64 {
    chrono::DateTime::parse_from_rfc3339(&format!("{day}T{hour:02}:{minute:02}:00+05:30"))
        .unwrap()
        .timestamp_millis()
}

fn candidate_at(hour: u32, minute: u32) -> i64 {
    candidate_at_on("2026-10-16", hour, minute)
}

const CANDIDATE_SOURCE: &str = r#"import json,sys
d=json.load(open(sys.argv[1]))
print(json.dumps({k:(1.0 if k=="NSE_EQ|INE000000001" else 0.0) for k in d if k!="params"}))
"#;
const CANDIDATE_SOURCE_ALT: &str = r#"import json,sys
d=json.load(open(sys.argv[1]))
print(json.dumps({k:(1.0 if k=="NSE_EQ|INF000000002" else 0.0) for k in d if k!="params"}))
"#;
const CANDIDATE_BAD_SOURCE: &str = "print('not json')";
const CANDIDATE_SLEEP_SOURCE: &str = r#"import json,sys,time
time.sleep(0.2)
d=json.load(open(sys.argv[1]))
print(json.dumps({k:(1.0 if k=="NSE_EQ|INE000000001" else 0.0) for k in d if k!="params"}))
"#;

fn candidate_quote_body(at: i64) -> Value {
    json!({
        "status":"success",
        "data":{
            "NSE_EQ:INE000000001":{"instrument_token":"NSE_EQ|INE000000001","symbol":"AAA","timestamp":at,"depth":{"buy":[{"price":100,"quantity":100}],"sell":[{"price":100.05,"quantity":100}]},"lower_circuit_limit":80,"upper_circuit_limit":120,"volume":1_000_000},
            "NSE_EQ:INF000000002":{"instrument_token":"NSE_EQ|INF000000002","symbol":"BBB","timestamp":at,"depth":{"buy":[{"price":200,"quantity":100}],"sell":[{"price":200.05,"quantity":100}]},"lower_circuit_limit":160,"upper_circuit_limit":240,"volume":1_000_000}
        }
    })
}

fn candidate_archive(at: i64) -> Value {
    json!({
        "source":"upstox-readonly-v1",
        "purpose":"scanner_metadata",
        "requested_ms":at,
        "received_ms":at,
        "body":candidate_quote_body(at).to_string()
    })
}

fn candidate_book(c: &Config) {
    candidate_book_on(c, "2026-10-16", "2026-10-15");
}

fn candidate_book_on(c: &Config, session: &str, file_date: &str) {
    write_verified_universe(c, session, file_date);
    let db = super::super::super::open_book(c).unwrap();
    for (hour, minute) in [(9, 20), (9, 21), (9, 22)] {
        let at = candidate_at_on(session, hour, minute);
        db.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?,?,?,'scanner_metadata')",
            rusqlite::params![at, at, candidate_archive(at).to_string()],
        )
        .unwrap();
    }
    drop(db);
}

/// A real synthetic verified universe: the three raw archives are written and
/// their exact hashes/mapping are recorded in the complete frozen manifest, so
/// the production `wide::universe::cached` reader accepts it. This is a
/// synthetic fixture, never a genuine market-instrument claim.
fn write_verified_universe(c: &Config, session: &str, file_date: &str) {
    use std::io::Write;
    let zip = |text: &str| {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap()
    };
    let security = zip(
        "TckrSymb,SctySrs,ISIN,NewBrdLotQty,ElgbltyNrmlMkt\nAAA,EQ,INE000000001,1,1\nBBB,EQ,INF000000002,1,1\n",
    );
    let broker = zip(
        &json!([
            {"segment":"NSE_EQ","exchange":"NSE","trading_symbol":"AAA","instrument_type":"EQ","isin":"INE000000001","instrument_key":"NSE_EQ|INE000000001","lot_size":1,"tick_size":5,"security_type":"NORMAL"},
            {"segment":"NSE_EQ","exchange":"NSE","trading_symbol":"BBB","instrument_type":"EQ","isin":"INF000000002","instrument_key":"NSE_EQ|INF000000002","lot_size":1,"tick_size":5,"security_type":"NORMAL"}
        ])
        .to_string(),
    );
    let suspended = zip("[]");
    let day: chrono::NaiveDate = file_date.parse().unwrap();
    fs::write(
        c.directory
            .join(format!("NSE_CM_security_{}.csv.gz", day.format("%d%m%Y"))),
        &security,
    )
    .unwrap();
    fs::write(c.directory.join("upstox-NSE.json.gz"), &broker).unwrap();
    fs::write(c.directory.join("upstox-suspended.json.gz"), &suspended).unwrap();
    let manifest = json!({
        "instruments":[
            {"key":"NSE_EQ|INE000000001","symbol":"AAA","series":"EQ","tick":"0.05"},
            {"key":"NSE_EQ|INF000000002","symbol":"BBB","series":"EQ","tick":"0.05"}
        ],
        "exclusions":{},
        "file_date":file_date,
        "session":session,
        "security_sha256":digest(&security),
        "broker_sha256":digest(&broker),
        "suspended_sha256":digest(&suspended),
        "received_ms":0
    });
    fs::write(
        c.directory.join("wide-universe.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

fn candidate_evidence() -> jobs::OwnerEvidence {
    jobs::OwnerEvidence {
        digest_id: "candidate-digest".into(),
        owner_work_id: digest(b"candidate-work"),
        facts_sha256: digest(b"candidate-facts"),
        profile_sha256: None,
        prompt_hashes_sha256: digest(b"candidate-prompts"),
        owner_claim_sha256: digest(b"candidate-claim"),
    }
}

fn candidate_settings(root: &Path, base: &ResearchSettings) -> ResearchSettings {
    let mut settings = base.clone();
    settings.queue = root.join("candidate-research.sqlite");
    settings.stage_root = root.join("candidate-stage");
    settings.admission_limits = Some(jobs::AdmissionLimits {
        max_trials_per_input: 40,
        max_session_trials: 100,
    });
    let helper_path = root.join("candidate-helper");
    fs::write(&helper_path, b"candidate-helper-fixture-v3").unwrap();
    settings.candidate_compute = Some(CandidateComputeSettings {
        enabled: true,
        abi: "scorer_replay_v1".into(),
        sandbox: nivra_sandbox::ScorerSandboxConfig {
            wall_ms: 2_000,
            ..Default::default()
        },
        replay: nivra_cli::scorer_compare::ScorerReplaySettings {
            max_snapshots: 10,
            min_valid_snapshots: 1,
            horizons_minutes: vec![5],
            top_n: 2,
            max_age_ms: 2_000,
            lookback_samples: 20,
            ..Default::default()
        },
        max_source_bytes: 262_144,
        max_params_bytes: 65_536,
        max_result_bytes: 1_048_576,
        wall_millis: 120_000,
        limits: ResourceLimits {
            cpu_millis: 120_000,
            tasks: 1,
            workspace_bytes: 1_048_576,
            file_bytes: 65_536,
        },
        helper_path: Some(helper_path),
        unit_tasks: 16,
        unit_memory_bytes: 512 * 1024 * 1024,
        unit_open_files: 128,
        unit_cpu_millis: 0,
        unit_wall_millis: 0,
        datasets: vec![CandidateDataset {
            dataset_id: "current_session_features_v1".into(),
            kind: CandidateDatasetKind::CurrentSessionFeatures,
        }],
        execution: None,
        observation: None,
    });
    settings
}

fn candidate_config(
    root: &Path,
    base: &PlannerConfig,
    settings: ResearchSettings,
) -> (Config, PlannerConfig) {
    let c = super::super::super::tests::config(root);
    let mut cfg = base.clone();
    cfg.research_tools = Some(settings);
    (c, cfg)
}

fn candidate_submit_args(
    task_key: &str,
    source: &str,
    parent: Option<&str>,
) -> CandidateSubmitArgs {
    CandidateSubmitArgs {
        task_key: task_key.into(),
        hypothesis: "AAA should rank above BBB on frozen current-session features".into(),
        source: source.into(),
        params: json!({}),
        dataset_id: "current_session_features_v1".into(),
        training_from_ms: candidate_at(9, 20),
        training_to_ms: candidate_at(9, 20),
        selection_from_ms: candidate_at(9, 21),
        selection_to_ms: candidate_at(9, 22),
        horizon_minutes: 5,
        parent_task_id: parent.map(str::to_owned),
        execution: None,
        parent_ref: None,
        observation: None,
    }
}

fn candidate_sandbox_ready(settings: &ResearchSettings) -> bool {
    let Some(candidate) = settings.candidate_compute.as_ref() else {
        return false;
    };
    let sandbox = &candidate.sandbox;
    sandbox.python_path.is_file()
        && sandbox.bwrap_path.is_file()
        && std::process::Command::new(&sandbox.bwrap_path)
            .args(["--unshare-all", "--ro-bind", "/", "/", "/usr/bin/true"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

fn candidate_queue(c: &Config, cfg: &PlannerConfig) -> Connection {
    open_queue_for_owner(
        cfg.research_tools.as_ref().unwrap(),
        &c.directory.join("model"),
        false,
    )
    .unwrap()
}

fn candidate_launch_count(db: &Connection, job_id: &str) -> i64 {
    let exists: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='registered_worker_receipts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if exists == 0 {
        return 0;
    }
    db.query_row(
        "SELECT count(*) FROM registered_worker_receipts WHERE job=? AND kind='candidate_launch'",
        rusqlite::params![job_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn candidate_record(c: &Config, cfg: &PlannerConfig, task_id: &str) -> jobs::CandidateTaskRecord {
    let mut db = candidate_queue(c, cfg);
    jobs::load_candidate(&mut db, task_id, 1).unwrap()
}

struct FakeCandidateUnit {
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

thread_local! {
    /// Last static helper refusal recorded by a fake unit on this test thread.
    /// It is diagnostic only: the runner contract still maps to `Ambiguous`.
    static LAST_HELPER_ERROR: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

fn record_helper_error(reason: &'static str) {
    LAST_HELPER_ERROR.with(|slot| slot.set(Some(reason)));
}

fn take_helper_error() -> Option<&'static str> {
    LAST_HELPER_ERROR.with(|slot| slot.take())
}

fn candidate_fake_proof(launch: &worker::CandidateLaunch, at: i64) -> worker::CandidateUnitProof {
    let mut proof = worker::CandidateUnitProof {
        version: 1,
        unit: launch.unit.clone(),
        lease: launch.lease.clone(),
        machine: launch.machine.clone(),
        policy_sha256: launch.unit_policy.policy_sha256(),
        active_state: "inactive".into(),
        main_pid: 0,
        tasks_current: 0,
        tasks_present: false,
        control_group: String::new(),
        cgroup_absent: true,
        cgroup_events: String::new(),
        cgroup_events_sha256: digest(b""),
        exit_code: 0,
        checked_at_ms: at,
        observation: worker::CandidateUnitObservation::default(),
        properties_observed: false,
        proof_origin: "fixture".into(),
        proof_sha256: String::new(),
    };
    proof.proof_sha256 = proof.hash().unwrap();
    proof
}

impl worker::CandidateUnitRunner for FakeCandidateUnit {
    fn start(
        &mut self,
        launch: &worker::CandidateLaunch,
    ) -> std::result::Result<(), worker::RunnerError> {
        let clock = Arc::clone(&self.clock);
        let output = if launch.observation.is_some() {
            run_observation_at(launch, || clock(), false)
        } else {
            run_helper_at(launch, || clock(), false)
        }
        .map_err(|reason| {
            record_helper_error(reason);
            worker::RunnerError::Ambiguous
        })?;
        write_helper_output(
            std::path::Path::new(&launch.stage_dir),
            &output,
            launch.unit_policy.max_result_bytes,
        )
        .map_err(|reason| {
            record_helper_error(reason);
            worker::RunnerError::Ambiguous
        })
    }

    fn poll(
        &mut self,
        launch: &worker::CandidateLaunch,
    ) -> std::result::Result<worker::CandidateUnitPoll, worker::RunnerError> {
        let at = (self.clock)();
        let result = fs::read(std::path::Path::new(&launch.stage_dir).join("result.json"))
            .unwrap_or_default();
        Ok(worker::CandidateUnitPoll::Exited(Box::new(
            worker::CandidateUnitExited {
                proof: candidate_fake_proof(launch, at),
                exit_code: 0,
                result,
            },
        )))
    }

    fn terminate(
        &mut self,
        launch: &worker::CandidateLaunch,
    ) -> std::result::Result<worker::CandidateUnitProof, worker::RunnerError> {
        Ok(candidate_fake_proof(launch, (self.clock)()))
    }
}

fn candidate_drive(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    clock: impl Fn() -> i64 + Send + Sync + 'static,
) {
    let settings = cfg.research_tools.clone().unwrap();
    let authority = c.directory.join("model");
    let clock: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(clock);
    let mut unit = FakeCandidateUnit {
        clock: Arc::clone(&clock),
    };
    let _ = take_helper_error();
    if let Err(reason) = drive_candidate_with_unit(
        settings,
        authority,
        task_id.to_owned(),
        1,
        Arc::new(AtomicBool::new(false)),
        || clock(),
        &mut unit,
    ) {
        panic!(
            "candidate drive failed for {task_id}: {reason} (helper: {})",
            take_helper_error().unwrap_or("none")
        );
    }
}

fn candidate_drive_result(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    clock: impl Fn() -> i64 + Send + Sync + 'static,
    unit: &mut dyn worker::CandidateUnitRunner,
) -> std::result::Result<(), &'static str> {
    let settings = cfg.research_tools.clone().unwrap();
    let authority = c.directory.join("model");
    let clock: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(clock);
    drive_candidate_with_unit(
        settings,
        authority,
        task_id.to_owned(),
        1,
        Arc::new(AtomicBool::new(false)),
        || clock(),
        unit,
    )
}

fn candidate_scripted_clock(times: Vec<i64>) -> impl Fn() -> i64 + Send + Sync + 'static {
    let times = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
        times,
    )));
    move || {
        let mut times = times.lock().unwrap();
        times.pop_front().unwrap_or(0)
    }
}

struct LateCandidateUnit {
    helper_at: i64,
    proof_at: i64,
    tamper_binding: bool,
}

impl worker::CandidateUnitRunner for LateCandidateUnit {
    fn start(
        &mut self,
        launch: &worker::CandidateLaunch,
    ) -> std::result::Result<(), worker::RunnerError> {
        let mut output = run_helper_at(launch, || self.helper_at, false)
            .map_err(|_| worker::RunnerError::Ambiguous)?;
        if self.tamper_binding {
            output.lease = "f".repeat(64);
        }
        write_helper_output(
            std::path::Path::new(&launch.stage_dir),
            &output,
            launch.unit_policy.max_result_bytes,
        )
        .map_err(|_| worker::RunnerError::Ambiguous)
    }

    fn poll(
        &mut self,
        launch: &worker::CandidateLaunch,
    ) -> std::result::Result<worker::CandidateUnitPoll, worker::RunnerError> {
        let result = fs::read(std::path::Path::new(&launch.stage_dir).join("result.json"))
            .unwrap_or_default();
        Ok(worker::CandidateUnitPoll::Exited(Box::new(
            worker::CandidateUnitExited {
                proof: candidate_fake_proof(launch, self.proof_at),
                exit_code: 0,
                result,
            },
        )))
    }

    fn terminate(
        &mut self,
        launch: &worker::CandidateLaunch,
    ) -> std::result::Result<worker::CandidateUnitProof, worker::RunnerError> {
        Ok(candidate_fake_proof(launch, self.proof_at))
    }
}

struct UnavailableCandidateUnit;

impl worker::CandidateUnitRunner for UnavailableCandidateUnit {
    fn start(
        &mut self,
        _: &worker::CandidateLaunch,
    ) -> std::result::Result<(), worker::RunnerError> {
        Err(worker::RunnerError::Unavailable)
    }

    fn poll(
        &mut self,
        _: &worker::CandidateLaunch,
    ) -> std::result::Result<worker::CandidateUnitPoll, worker::RunnerError> {
        Err(worker::RunnerError::Unavailable)
    }

    fn terminate(
        &mut self,
        _: &worker::CandidateLaunch,
    ) -> std::result::Result<worker::CandidateUnitProof, worker::RunnerError> {
        Err(worker::RunnerError::Unavailable)
    }
}

fn candidate_execution_book(c: &Config) {
    write_verified_universe(c, "2026-10-16", "2026-10-15");
    let db = super::super::super::open_book(c).unwrap();
    let body = |at: i64, bid: f64, ask: f64| {
        json!({
            "status":"success",
            "data":{
                "NSE_EQ:INE000000001":{"instrument_token":"NSE_EQ|INE000000001","symbol":"AAA","timestamp":at,
                    "depth":{"buy":[{"price":bid,"quantity":100}],"sell":[{"price":ask,"quantity":100}]},
                    "lower_circuit_limit":80,"upper_circuit_limit":120,"volume":987654321012u64},
                "NSE_EQ:INF000000002":{"instrument_token":"NSE_EQ|INF000000002","symbol":"BBB","timestamp":at,
                    "depth":{"buy":[{"price":200,"quantity":100}],"sell":[{"price":200.05,"quantity":100}]},
                    "lower_circuit_limit":160,"upper_circuit_limit":240,"volume":1_000_000}
            }
        })
    };
    for (at, purpose, bid, ask) in [
        (candidate_at(9, 20), "scanner_metadata", 99.0, 100.05),
        (
            candidate_at(9, 20) + 59_500,
            "execution_or_protection",
            99.0,
            100.05,
        ),
        (candidate_at(9, 21), "scanner_metadata", 99.0, 100.05),
        (
            candidate_at(9, 21) + 59_500,
            "execution_or_protection",
            110.0,
            110.05,
        ),
        (candidate_at(9, 22), "scanner_metadata", 110.0, 110.05),
        (
            candidate_at(9, 22) + 500,
            "execution_or_protection",
            110.0,
            110.05,
        ),
    ] {
        let archive = json!({
            "source":"upstox-readonly-v1",
            "purpose":purpose,
            "requested_ms":at,
            "received_ms":at,
            "body":body(at, bid, ask).to_string()
        });
        db.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?2,?3,CASE WHEN instr(?3,'\"purpose\":\"research_observation\"')>0 THEN 'research_quotes' ELSE 'wide_quotes' END)",
            rusqlite::params![at, at, archive.to_string()],
        )
        .unwrap();
    }
    drop(db);
}

fn candidate_execution_spec(
    hypothesis: &str,
    units: u64,
    max_entries: u32,
    top_n: u32,
    capital: &str,
    per_order: &str,
    stress: Vec<Value>,
) -> Box<nivra_cli::candidate_execution::CandidateExecutionSpecV1> {
    Box::new(serde_json::from_value(json!({
        "version":1,"segment":"cash",
        "parameters":{
            "version":1,"parent_candidate_id":null,"hypothesis":hypothesis,
            "training_from_ms":candidate_at(9,20),"training_to_ms":candidate_at(9,20),
            "selection_from_ms":candidate_at(9,21),"selection_to_ms":candidate_at(9,22),
            "holding_ms":60_000,"max_exit_delay_ms":60_000,
            "capital":capital,
            "costs":{"per_order":per_order,"buy_bps":"0","sell_bps":"0","slippage_bps":"0","rounding_increment":"0.01"}
        },
        "units":units,"max_entries":max_entries,"selection_top_n":top_n,
        "entry_price_offset_ticks":0,"entry_at":"signal","exit":"time",
        "first_signal_per_key":true,"cost_stress":stress
    }))
    .unwrap())
}

fn candidate_execution_settings(
    root: &Path,
    base: &ResearchSettings,
    max_result_bytes: u32,
) -> ResearchSettings {
    let mut settings = candidate_settings(root, base);
    let candidate = settings.candidate_compute.as_mut().unwrap();
    candidate.replay.horizons_minutes = vec![1];
    candidate.max_result_bytes = max_result_bytes;
    candidate.execution = Some(CandidateExecutionSettings {
        enabled: true,
        abi: "cash_quote_action_tape_v1".into(),
        max_quote_events: 100,
        max_observations: 1_000,
        max_tape_bytes: 1_048_576,
        max_entries: 10,
        max_capital: nivra_core::finance::Amount::parse("10000").unwrap(),
        max_cost_stress: 4,
    });
    settings
}

const CANDIDATE_EXEC_SENTINEL: &str = "987654321012";

/// Adversarial score source for the execution contract. It probes the actual
/// bwrap mount table, the allowed cwd/root, open file descriptors and a bounded
/// traversal of the real stage root for the execution-only sentinel. Any
/// unexpected mount point or any readable sentinel/stage-path hit suppresses the
/// score map, which makes the execution result unavailable and fails the
/// contract. A host path string in mount metadata is not a hit by itself: the
/// probe must actually open the path or file before it is treated as readable.
fn candidate_tape_peek_source(sentinel: &str, stage_root: &Path) -> String {
    const TEMPLATE: &str = r#"import json, os, stat, sys
SENTINEL = __SENTINEL__
STAGE_ROOT = __STAGE_ROOT__
leaked = False

def probe(data):
    global leaked
    if SENTINEL.encode() in data or STAGE_ROOT.encode() in data:
        leaked = True

# The mount table's root fields can legitimately contain the host stage path
# string without making that path readable from this namespace. Readability is
# proven by the mount artifact, fd and bounded traversal probes below, never by
# a metadata string alone.
for path in ("/proc/self/environ", "/proc/self/cmdline", "/proc/self/status", "/proc/self/maps"):
    try:
        with open(path, "rb") as handle:
            probe(handle.read(262144))
    except Exception:
        pass

try:
    mountinfo = open("/proc/self/mountinfo", "rb").read(262144).decode("utf-8", "replace")
    # Only fields[4] (mount point, field 5 in mountinfo) is a namespace path.
    # fields[3] (root, field 4) is a source-filesystem path and may name the
    # host stage without exposing it; an actual open below decides.
    for line in mountinfo.splitlines():
        fields = line.split()
        if len(fields) < 5:
            continue
        mount = fields[4].replace("\\040", " ")
        allowed = mount == "/" or mount == "/proc" or mount in ("/dev", "/sys", "/etc", "/usr", "/lib", "/lib64", "/scorer.py", "/input.json") or mount.startswith("/usr/") or mount.startswith("/dev/") or mount.startswith("/sys/") or mount.startswith("/etc/")
        if not allowed:
            leaked = True
        for name in ("execution_manifest", "execution_events", "execution_spec", "manifest.json", "events.ndjson", "result.json"):
            try:
                with open(os.path.join(mount, name), "rb") as handle:
                    probe(handle.read(65536))
            except Exception:
                pass
except Exception:
    pass

try:
    for name in os.listdir("/proc/self/fd"):
        path = "/proc/self/fd/" + name
        try:
            probe(os.readlink(path).encode())
            info = os.stat(path)
            if stat.S_ISREG(info.st_mode):
                fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
                try:
                    probe(os.read(fd, 65536))
                finally:
                    os.close(fd)
        except Exception:
            pass
except Exception:
    pass

seen = 0
for base in (STAGE_ROOT, "/proc/self/root" + STAGE_ROOT, "/proc/1/root" + STAGE_ROOT, "/", "/tmp", "/var/tmp", "/run"):
    try:
        for root, dirs, files in os.walk(base):
            depth = root[len(base):].count(os.sep)
            if depth > 2:
                dirs[:] = []
                continue
            dirs[:] = [name for name in dirs if name not in ("proc", "sys", "dev", "usr", "lib", "lib64")][:32]
            for name in files[:32]:
                if name in ("scorer.py", "input.json"):
                    continue
                seen += 1
                if seen > 200:
                    break
                path = os.path.join(root, name)
                try:
                    info = os.lstat(path)
                    if stat.S_ISREG(info.st_mode) and info.st_size <= 262144:
                        with open(path, "rb") as handle:
                            probe(handle.read(262144))
                except Exception:
                    pass
            if seen > 200:
                break
    except Exception:
        pass
    if seen > 200:
        break

if leaked:
    print("{}")
else:
    data = json.load(open(sys.argv[1]))
    print(json.dumps({key: (1.0 if key == "NSE_EQ|INE000000001" else 0.0) for key in data if key != "params"}))
"#;
    TEMPLATE
        .replace("__SENTINEL__", &format!("{sentinel:?}"))
        .replace(
            "__STAGE_ROOT__",
            &format!("{:?}", stage_root.to_string_lossy()),
        )
}

fn candidate_observation_settings(root: &Path, base: &ResearchSettings) -> ResearchSettings {
    let mut settings = candidate_settings(root, base);
    let candidate = settings.candidate_compute.as_mut().unwrap();
    candidate.replay.horizons_minutes = vec![1];
    candidate.observation = Some(CandidateObservationSettings {
        enabled: true,
        abi: "observation_single_entry_v1".into(),
        max_quotes: 64,
        max_quote_events: 100,
        max_observations: 1_000,
        max_tape_bytes: 1_048_576,
        max_capital: nivra_core::finance::Amount::parse("10000").unwrap(),
        max_units: 100,
        max_holding_ms: 3_600_000,
        max_decision_delay_ms: 5_000,
    });
    settings
}

fn candidate_observation_args(
    source_task_id: &str,
    window_from_ms: i64,
    window_to_ms: i64,
) -> CandidateObservationArgsV1 {
    CandidateObservationArgsV1 {
        source_task_id: source_task_id.into(),
        source_revision: 1,
        window_from_ms,
        window_to_ms,
        units: 10,
        capital: nivra_core::finance::Amount::parse("5000").unwrap(),
        costs: serde_json::from_value(json!({
            "per_order":"1","buy_bps":"0","sell_bps":"0","slippage_bps":"0","rounding_increment":"0.01"
        }))
        .unwrap(),
        holding_ms: 60_000,
        max_exit_delay_ms: 60_000,
        decision_delay_ms: 2_000,
        observation_projection: None,
    }
}

fn case_carry_contract(root: &Path, base_settings: &ResearchSettings, base_policy: &PlannerConfig) {
    let dir = root.join("candidate-carry-contract");
    fs::create_dir_all(&dir).unwrap();
    let settings = candidate_settings(&dir, base_settings);
    let (c, cfg) = candidate_config(&dir, base_policy, settings);
    // A malformed reference is refused before any predecessor read.
    let mut reference = jobs::CandidateParentRefV1 {
        version: 1,
        source_session: "2026-10-15".into(),
        task_id: "a".repeat(64),
        revision: 1,
        candidate_id: "b".repeat(64),
        lifecycle_candidate_id: "c".repeat(64),
        source_book_binding: "d".repeat(64),
        source_state_hash: "e".repeat(64),
        carry_sha256: "f".repeat(64),
        account: c.account.clone(),
        instrument_universe_sha256: "1".repeat(64),
        cohort_key: "2".repeat(64),
        source_sha256: "3".repeat(64),
        params_sha256: "4".repeat(64),
        dataset_hash: "5".repeat(64),
        content_key: "6".repeat(64),
        result_sha256: "7".repeat(64),
        runtime_hash: "8".repeat(64),
        result_kind: "ranking_diagnostic".into(),
        axes: vec!["retrospective".into()],
    };
    assert!(reference.validate());
    let mut cross_account = reference.clone();
    cross_account.account = "another-account".into();
    assert_eq!(
        super::carry::resolve_parent_ref(&c, &cfg, &cross_account)
            .unwrap_err()
            .to_string(),
        "source_continue_account_mismatch"
    );
    reference.task_id = "not-a-hash".into();
    assert!(!reference.validate());
    assert_eq!(
        super::carry::resolve_parent_ref(&c, &cfg, &reference)
            .unwrap_err()
            .to_string(),
        "source_continue_identity_mismatch"
    );
    // The predecessor's original bytes are never rewritten by a refused read.
    let predecessor = dir.join("sessions").join("2026-10-15");
    fs::create_dir_all(&predecessor).unwrap();
    let sentinel = predecessor.join("original.txt");
    fs::write(&sentinel, b"original predecessor bytes").unwrap();
    let before = digest(&fs::read(&sentinel).unwrap());
    let _ = super::carry::resolve_parent_ref(&c, &cfg, &cross_account);
    assert_eq!(digest(&fs::read(&sentinel).unwrap()), before);
    // The global fold is read-only and never resets the trial debt.
    let _ = open_queue_for_owner(
        cfg.research_tools.as_ref().unwrap(),
        &c.directory.join("model"),
        true,
    )
    .unwrap();
    let trials = super::carry::trial_fold(&c, &cfg);
    assert_eq!(trials["status"], "available");
    assert_eq!(trials["reset"], false);
    assert_eq!(trials["family_key_is_label_only"], true);
    assert!(trials["global_tested_trial_count"].is_u64());
}

fn insert_research_tick(c: &Config, at: i64) {
    let db = super::super::super::open_book(c).unwrap();
    let archive = json!({
        "source":"upstox-readonly-v1",
        "purpose":"research_observation",
        "requested_ms":at,
        "received_ms":at,
        "body":candidate_quote_body(at).to_string()
    });
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?2,?3,CASE WHEN instr(?3,'\"purpose\":\"research_observation\"')>0 THEN 'research_quotes' ELSE 'wide_quotes' END)",
        rusqlite::params![at, at, archive.to_string()],
    )
    .unwrap();
}

fn e2_item<'a>(value: &'a Value, id: &str) -> Option<&'a Value> {
    value["items"]
        .as_array()?
        .iter()
        .find(|item| item["candidate_id"] == id)
}

fn case_e2_bounds_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-e2-bounds");
    fs::create_dir_all(&dir).unwrap();
    let t0 = candidate_at(9, 23);
    let settings = candidate_observation_settings(&dir, base_settings);
    if !candidate_sandbox_ready(&settings) {
        assert!(
            std::env::var_os("NIVRA_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!("skipping candidate e2 bounds contract: python3 or bwrap unavailable");
        return;
    }
    let (c, cfg) = candidate_config(&dir, base_policy, settings.clone());
    candidate_book(&c);
    ensure_test_state(&c);
    insert_research_tick(&c, t0 + 500);
    let mut failures = Vec::new();
    macro_rules! check {
        ($cond:expr, $msg:expr) => {
            if !$cond {
                failures.push($msg.to_string());
            }
        };
    }
    let mut source_args = candidate_submit_args("candidate-e2b-source", CANDIDATE_SOURCE, None);
    source_args.horizon_minutes = 1;
    let source = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e2b",
        source_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    check!(source["status"] == "queued", format!("source: {source}"));
    let source_task = source["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &source_task, move || t0 + 10);
    {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &source_task, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
    }
    // A compacted potential first exit must be an explicit unknown gap with
    // its original archive SHA, never silently replaced by a later readable
    // receipt and never a zero PnL.
    insert_research_tick(&c, t0 + 419_500);
    let mut args = candidate_submit_args("candidate-e2b-compacted", CANDIDATE_SOURCE, None);
    args.observation = Some(Box::new(candidate_observation_args(
        &source_task,
        candidate_at(9, 21),
        candidate_at(9, 22),
    )));
    let submitted = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e2b",
        args,
        t0 + 420_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    check!(
        submitted["status"] == "queued",
        format!("compacted submit: {submitted}")
    );
    let task = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &task, move || t0 + 421_000);
    {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &task, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
    }
    let record = candidate_record(&c, &cfg, &task);
    let id = record.spec.lifecycle_candidate_id.clone();
    {
        let db = super::super::super::open_book(&c).unwrap();
        let compacted = json!({
            "kind":"compacted_quote_receipt",
            "sha256":"d".repeat(64),
            "original_bytes":1234
        });
        db.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?2,?3,CASE WHEN instr(?3,'\"purpose\":\"research_observation\"')>0 THEN 'research_quotes' ELSE 'wide_quotes' END)",
            rusqlite::params![t0 + 482_000, t0 + 482_000, compacted.to_string()],
        )
        .unwrap();
    }
    let matured = mature_candidates(&c, &cfg, t0 + 483_000).unwrap();
    let item = e2_item(&matured, &id);
    check!(
        item.is_some_and(|item| item["state"] == "unknown"),
        format!("compacted unknown: {matured}")
    );
    let measurement = item
        .map(|item| item["observation"].clone())
        .unwrap_or(Value::Null);
    check!(
        measurement["exit_status"] == "exit_first_receipt_unknown"
            && measurement["first_receipt_unknown"] == true,
        format!("compacted status: {measurement}")
    );
    check!(
        measurement["exit_gap"]["archive_sha256"] == "d".repeat(64)
            && measurement["net_pnl_if_flat"].is_null(),
        format!("compacted gap sha: {measurement}")
    );
    // Capture caps: the grant timeout is enforced on the request path, the row
    // budget is enforced before commit, and the verified-universe checks refuse
    // a wrong-session or over-budget universe without calling the provider.
    let capture_cfg = crate::runtime::owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: crate::runtime::owner_tasks::ReviewSettings::default(),
        research: Some(crate::runtime::owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: Some(
                crate::runtime::owner_tasks::ControllerObservationCaptureSettings {
                    enabled: true,
                    max_keys: 8,
                    max_calls_per_session: 8,
                    min_interval_ms: 1_000,
                    timeout_ms: 1_000,
                    max_rows: 1,
                },
            ),
            lane: Some(Default::default()),
        }),
    };
    let mut cfg = cfg.clone();
    cfg.owner_tasks = Some(capture_cfg);
    let mut db = super::super::super::open_book(&c).unwrap();
    let portfolio = nivra_core::execution::Portfolio::with_capital(
        nivra_core::finance::Amount::parse("5000").unwrap(),
    );
    let timeout = super::capture::tick_with(
        &mut db,
        &c,
        &cfg,
        &portfolio,
        t0 + 600_000,
        || t0 + 601_100,
        |_| {
            std::thread::sleep(std::time::Duration::from_millis(1_100));
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: t0 + 600_000,
                receipt_ms: t0 + 600_000,
                body: candidate_quote_body(t0 + 600_000).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    check!(
        timeout["status"] == "timeout" && timeout["provider_called"] == true,
        format!("capture timeout: {timeout}")
    );
    let rows = super::capture::tick_with(
        &mut db,
        &c,
        &cfg,
        &portfolio,
        t0 + 601_000,
        || t0 + 601_000,
        |_| {
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: t0 + 601_000,
                receipt_ms: t0 + 601_000,
                body: candidate_quote_body(t0 + 601_000).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    check!(
        rows["status"] == "refused" && rows["reason"] == "observation_capture_row_budget_exceeded",
        format!("capture row cap: {rows}")
    );
    let mut one_key = cfg.clone();
    if let Some(capture) = one_key
        .owner_tasks
        .as_mut()
        .and_then(|tasks| tasks.research.as_mut())
        .and_then(|research| research.observation_capture.as_mut())
    {
        capture.max_keys = 1;
    }
    let over_budget = super::capture::tick_with(
        &mut db,
        &c,
        &one_key,
        &portfolio,
        t0 + 602_000,
        || t0 + 602_000,
        |_| panic!("provider must not be called for an over-budget universe"),
    );
    check!(
        over_budget["status"] == "not_eligible" && over_budget["provider_called"] == false,
        format!("capture key budget: {over_budget}")
    );
    let universe_path = c.directory.join("wide-universe.json");
    let universe = fs::read(&universe_path).unwrap();
    let mut corrupt: Value = serde_json::from_slice(&universe).unwrap();
    corrupt["session"] = json!("2026-10-15");
    fs::write(&universe_path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
    let unverified = super::capture::tick_with(
        &mut db,
        &c,
        &cfg,
        &portfolio,
        t0 + 603_000,
        || t0 + 603_000,
        |_| panic!("provider must not be called for an unverified universe"),
    );
    check!(
        unverified["status"] == "not_eligible"
            && unverified["reason"] == "verified_universe_session_mismatch"
            && unverified["provider_called"] == false,
        format!("capture unverified universe: {unverified}")
    );
    fs::write(&universe_path, universe).unwrap();
    // A forged parent ref is refused against the host-derived source identity.
    let status = candidate_status(&c, &cfg, &source_task, 1, t0 + 100).unwrap();
    let reference: jobs::CandidateParentRefV1 =
        serde_json::from_value(status["parent_ref"]["ref"].clone()).unwrap();
    let mut forged = reference.clone();
    forged.cohort_key = "0".repeat(64);
    check!(
        super::carry::resolve_parent_ref(&c, &cfg, &forged).is_err(),
        "forged cohort refused"
    );
    let mut forged = reference.clone();
    forged.instrument_universe_sha256 = "0".repeat(64);
    check!(
        super::carry::resolve_parent_ref(&c, &cfg, &forged).is_err(),
        "forged universe refused"
    );
    let mut forged = reference.clone();
    forged.source_state_hash = "0".repeat(64);
    check!(
        super::carry::resolve_parent_ref(&c, &cfg, &forged).is_err(),
        "forged state refused"
    );
    let mut forged = reference.clone();
    forged.carry_sha256 = "0".repeat(64);
    check!(
        super::carry::resolve_parent_ref(&c, &cfg, &forged).is_err(),
        "forged carry refused"
    );
    let mut forged = reference.clone();
    forged.axes = vec!["edge_qualified".into()];
    check!(
        super::carry::resolve_parent_ref(&c, &cfg, &forged).is_err(),
        "forged axes refused"
    );
    let mut forged = reference.clone();
    forged.result_sha256 = "0".repeat(64);
    check!(
        super::carry::resolve_parent_ref(&c, &cfg, &forged).is_err(),
        "forged result refused"
    );
    // A corrupt carry chain makes the global fold unavailable, not zero.
    let broken = root.join("candidate-e2-broken-carry");
    fs::create_dir_all(broken.join("sessions").join("2026-10-16")).unwrap();
    fs::write(broken.join("carry-over.json"), b"{not-json").unwrap();
    let mut broken_config = c.clone();
    broken_config.directory = broken.clone();
    broken_config.sessions_root = Some(broken.join("sessions"));
    let broken_fold = super::carry::trial_fold(&broken_config, &cfg);
    check!(
        broken_fold["status"] == "unavailable"
            && broken_fold["global_tested_trial_count"].is_null(),
        format!("broken carry fold: {broken_fold}")
    );
    drop(db);
    assert!(failures.is_empty(), "E2 bounds failures: {failures:#?}");
}

fn candidate_execution_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-execution-contract");
    fs::create_dir_all(&dir).unwrap();
    let t0 = candidate_at(9, 23);
    let settings = candidate_execution_settings(&dir, base_settings, 1_048_576);
    if !candidate_sandbox_ready(&settings) {
        assert!(
            std::env::var_os("NIVRA_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!("skipping candidate execution contract: python3 or bwrap unavailable on host");
        return;
    }
    let (c, cfg) = candidate_config(&dir, base_policy, settings.clone());
    candidate_execution_book(&c);
    let tape_peek_source =
        candidate_tape_peek_source(CANDIDATE_EXEC_SENTINEL, &settings.stage_root);
    let hypothesis = "AAA should rank above BBB on frozen current-session features";
    let stress = json!({"per_order":"2","buy_bps":"0","sell_bps":"0","slippage_bps":"5","rounding_increment":"0.01"});
    let spec = candidate_execution_spec(hypothesis, 10, 1, 1, "5000", "1", vec![stress.clone()]);
    let mut args = candidate_submit_args("candidate-exec", &tape_peek_source, None);
    args.horizon_minutes = 1;
    args.execution = Some(spec.clone());
    let submitted = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(submitted["status"], "queued");
    assert_eq!(submitted["task"]["result_kind"], "executable_replay");
    let task_id = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    let record = candidate_record(&c, &cfg, &task_id);
    let binding = record.spec.execution.as_ref().unwrap();
    let execution_runtime_sha256 =
        jobs::candidate_execution_runtime_hash(&record.spec.runtime_binding, &record.spec.sandbox);
    assert_eq!(
        binding.spec_sha256,
        nivra_cli::candidate_execution::artifact_sha256(&spec).unwrap()
    );
    assert_eq!(
        record.spec.input_key,
        jobs::candidate_execution_trial_input_key(
            &record.spec.source_sha256,
            &record.spec.params_sha256,
            &record.spec.dataset_hash,
            &record.spec.abi,
            binding,
            &execution_runtime_sha256,
        )
    );
    assert_eq!(
        record.job.spec.input_hash,
        jobs::candidate_execution_input_hash(
            &record.spec.params_sha256,
            &record.spec.dataset_hash,
            binding,
            &execution_runtime_sha256,
        )
    );
    // Rewriting only the hypothesis prose changes the version identity but not
    // the computation input allowance, so it cannot reset the trial budget.
    let mut prose_args = candidate_submit_args("candidate-exec-prose", &tape_peek_source, None);
    prose_args.hypothesis = "A different prose description of the same execution rules".into();
    prose_args.horizon_minutes = 1;
    prose_args.execution = Some(candidate_execution_spec(
        &prose_args.hypothesis,
        10,
        1,
        1,
        "5000",
        "1",
        vec![stress],
    ));
    let prose = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        prose_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(prose["status"], "queued");
    let prose_task = prose["task"]["task_id"].as_str().unwrap().to_owned();
    let prose_record = candidate_record(&c, &cfg, &prose_task);
    assert_eq!(prose_record.spec.input_key, record.spec.input_key);
    assert_ne!(prose_record.spec.content_key, record.spec.content_key);

    let db = candidate_queue(&c, &cfg);
    for (hash, kind, cap) in [
        (&binding.spec_sha256, "execution_spec", 65_536),
        (&binding.manifest_sha256, "execution_manifest", 512 * 1024),
        (&binding.events_sha256, "execution_events", 16 * 1024 * 1024),
    ] {
        let bytes = jobs::read_candidate_artifact(&db, hash, kind, cap).unwrap();
        assert_eq!(&digest(&bytes), hash);
    }
    // The sentinel is a real execution-only manifest field (the instrument
    // symbol), never part of the scorer feature dataset. The adversarial source
    // above would suppress its scores if it could see either the sentinel or
    // the real stage path through a mount, fd, cwd or bounded traversal.
    let execution_events = jobs::read_candidate_artifact(
        &db,
        &binding.events_sha256,
        "execution_events",
        16 * 1024 * 1024,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&execution_events).contains(CANDIDATE_EXEC_SENTINEL));
    let dataset =
        jobs::read_candidate_artifact(&db, &record.spec.dataset_hash, "dataset", 4 * 1024 * 1024)
            .unwrap();
    assert!(!String::from_utf8_lossy(&dataset).contains(CANDIDATE_EXEC_SENTINEL));
    drop(db);

    candidate_drive(&c, &cfg, &task_id, move || t0 + 10);
    let result = candidate_result(&c, &cfg, &task_id, 1, t0 + 11).unwrap();
    assert_eq!(result["result_status"], "verified");
    let view = &result["result"];
    assert_eq!(view["result_kind"], "executable_replay");
    assert_eq!(view["qualified"], false);
    assert_eq!(view["analysis_status"], "insufficient_evidence");
    assert_eq!(view["promotion_authorized"], false);
    assert_eq!(view["activation_authorized"], false);
    assert_eq!(view["edge_qualified"], false);
    // The scorer report keeps its legitimate float metrics and self-hash; the
    // integer-only canonical encoding applies only to the generated execution
    // artifacts, never to the ranking report itself.
    assert!(view["report"]["report_hash"].as_str().is_some());
    assert!(!view["report"]["ticks"].as_array().unwrap().is_empty());
    assert!(view["report"]["ticks"][0].get("kendall").is_some());
    assert!(view["report"]["ticks"][0].get("spearman").is_some());
    let execution = &view["execution"];
    assert_eq!(execution["execution_status"], "executed");
    assert_eq!(execution["retrospective"], true);
    assert_eq!(execution["prospective"], false);
    assert_eq!(execution["promotion_authorized"], false);
    assert_eq!(execution["summary"]["cash"], "5097.5");
    assert_eq!(execution["summary"]["fees"], "2");
    assert_eq!(execution["summary"]["turnover"], "2100.5");
    assert_eq!(execution["summary"]["fills"], 2);
    assert_eq!(execution["summary"]["entry_fills"], 1);
    assert_eq!(execution["summary"]["exit_fills"], 1);
    assert_eq!(execution["summary"]["net_pnl_if_flat"], "97.5");
    assert_eq!(execution["summary"]["marked_equity"], "5097.5");
    assert_eq!(execution["summary"]["flat"], true);
    assert_eq!(execution["summary"]["accounting_complete"], true);
    assert_eq!(execution["cost_stress"][0]["summary"]["fills"], 0);
    assert_eq!(execution["cost_stress"][0]["summary"]["fees"], "0");
    assert_eq!(
        execution["cost_stress"][0]["summary"]["net_pnl_if_flat"],
        "0"
    );
    assert_eq!(
        execution["report_sha256"],
        nivra_cli::candidate_execution::artifact_sha256(&execution["report"]).unwrap()
    );
    assert_eq!(
        execution["action_tape_sha256"],
        nivra_cli::candidate_execution::artifact_sha256(&execution["action_tape"]).unwrap()
    );
    assert_eq!(
        execution["params_sha256"],
        nivra_cli::candidate_execution::artifact_sha256(&execution["execution_spec"]["parameters"])
            .unwrap()
    );
    assert_eq!(
        execution["scoring"]["scored_signals_sha256"],
        nivra_cli::candidate_execution::artifact_sha256(&execution["scored_signals"]).unwrap()
    );
    assert_eq!(
        execution["cost_stress"][0]["costs_sha256"],
        nivra_cli::candidate_execution::artifact_sha256(&execution["cost_stress"][0]["costs"])
            .unwrap()
    );
    assert_eq!(
        result["lifecycle"]["state"], "replayed",
        "retrospective execution must not open a prospective shadow"
    );
    let library =
        super::super::super::experiment_ledger::lifecycle::library(&c.directory, "2026-10-16")
            .unwrap();
    let item = library["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["candidate_id"] == result["lifecycle"]["candidate_id"])
        .unwrap();
    assert_eq!(item["report_kind"], "executable_replay");
    assert_eq!(item["state"], "replayed");

    // Historical read uses the recorded task/launch/proof, not the current
    // execution capability. Disabling it in config must not invalidate the
    // already verified result.
    let mut historical_cfg = cfg.clone();
    historical_cfg
        .research_tools
        .as_mut()
        .unwrap()
        .candidate_compute
        .as_mut()
        .unwrap()
        .execution = None;
    let historical = candidate_result(&c, &historical_cfg, &task_id, 1, t0 + 12).unwrap();
    assert_eq!(historical["result_status"], "verified");
    assert_eq!(
        historical["result"]["execution"]["execution_status"],
        "executed"
    );

    // Execution-only runtime identity: changing the actual Python executable
    // content and a containment cap under the same helper/source/params/
    // dataset/spec/tape must create a new version and a new input allowance,
    // never return the old task as "existing". The old result remains authentic
    // under its recorded runtime, and the old charge remains in the session.
    let mut runtime_cfg = cfg.clone();
    {
        let candidate = runtime_cfg
            .research_tools
            .as_mut()
            .unwrap()
            .candidate_compute
            .as_mut()
            .unwrap();
        candidate.sandbox.python_path = "/bin/true".into();
        candidate.sandbox.max_keys = 2048;
    }
    let mut runtime_args =
        candidate_submit_args("candidate-exec", &tape_peek_source, Some(&task_id));
    runtime_args.horizon_minutes = 1;
    runtime_args.execution = Some(spec.clone());
    let runtime_submitted = candidate_submit(
        &c,
        &runtime_cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        runtime_args,
        t0 + 15,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(runtime_submitted["status"], "queued", "{runtime_submitted}");
    let runtime_task = runtime_submitted["task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let runtime_record = candidate_record(&c, &runtime_cfg, &runtime_task);
    assert_ne!(runtime_record.spec.content_key, record.spec.content_key);
    assert_ne!(runtime_record.spec.input_key, record.spec.input_key);
    assert_eq!(
        runtime_record.spec.parent_task_id.as_deref(),
        Some(task_id.as_str())
    );
    assert_ne!(runtime_record.spec.sandbox, record.spec.sandbox);
    let runtime_identity = jobs::candidate_execution_runtime_hash(
        &runtime_record.spec.runtime_binding,
        &runtime_record.spec.sandbox,
    );
    assert_ne!(runtime_identity, execution_runtime_sha256);
    // The old successful result is still authentic under its recorded runtime;
    // current executable hashes are admission-only, never a historical reader.
    let old_result = candidate_result(&c, &cfg, &task_id, 1, t0 + 16).unwrap();
    assert_eq!(old_result["result_status"], "verified");
    let db = candidate_queue(&c, &cfg);
    let session = c.session.to_string();
    let total: i64 = db
        .query_row(
            "SELECT count(*) FROM candidate_tasks WHERE session=? AND json_extract(body,'$.book_binding')=?",
            rusqlite::params![session, record.spec.book_binding],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(total, 3, "the old trial charges must be retained");
    let old_input: i64 = db
        .query_row(
            "SELECT count(*) FROM candidate_tasks WHERE session=? AND input_key=?",
            rusqlite::params![session, record.spec.input_key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_input, 2);
    let new_input: i64 = db
        .query_row(
            "SELECT count(*) FROM candidate_tasks WHERE session=? AND input_key=?",
            rusqlite::params![session, runtime_record.spec.input_key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(new_input, 1);
    drop(db);
    // A cap-only change and a runtime-binding-only change are also distinct
    // identities: one canonical hash covers every runtime/sandbox field.
    let mut caps_only = record.spec.clone();
    caps_only.sandbox.max_keys += 1;
    let caps_identity =
        jobs::candidate_execution_runtime_hash(&caps_only.runtime_binding, &caps_only.sandbox);
    assert_ne!(caps_identity, execution_runtime_sha256);
    assert_ne!(
        jobs::candidate_execution_content_key(
            &record.spec.source_sha256,
            &record.spec.params_sha256,
            &record.spec.dataset_hash,
            &record.spec.windows,
            record.spec.horizon_minutes,
            &record.spec.abi,
            &record.spec.unit_policy.policy_sha256(),
            binding,
            &caps_identity,
        ),
        record.spec.content_key
    );
    let mut runtime_only = record.spec.clone();
    runtime_only.runtime_binding.runtime_hash = "0".repeat(64);
    assert_ne!(
        jobs::candidate_execution_runtime_hash(
            &runtime_only.runtime_binding,
            &runtime_only.sandbox
        ),
        execution_runtime_sha256
    );

    // A tampered stored execution input is refused before the result is
    // advertised as verified.
    let db = candidate_queue(&c, &cfg);
    let manifest = jobs::read_candidate_artifact(
        &db,
        &binding.manifest_sha256,
        "execution_manifest",
        512 * 1024,
    )
    .unwrap();
    db.execute(
        "UPDATE candidate_artifacts SET body=? WHERE hash=?",
        rusqlite::params![b"tampered".as_slice(), binding.manifest_sha256],
    )
    .unwrap();
    assert_eq!(
        candidate_result(&c, &cfg, &task_id, 1, t0 + 13)
            .unwrap_err()
            .to_string(),
        "candidate_result_unverified"
    );
    db.execute(
        "UPDATE candidate_artifacts SET body=? WHERE hash=?",
        rusqlite::params![manifest.as_slice(), binding.manifest_sha256],
    )
    .unwrap();
    // A tampered stored result payload is refused by the result hash.
    let result_record = candidate_record(&c, &cfg, &task_id);
    let result_hash = result_record.job.result_hash.clone().unwrap();
    let result_bytes =
        jobs::read_candidate_artifact(&db, &result_hash, "result", 1_048_576).unwrap();
    db.execute(
        "UPDATE candidate_artifacts SET body=? WHERE hash=?",
        rusqlite::params![b"tampered".as_slice(), result_hash],
    )
    .unwrap();
    assert_eq!(
        candidate_result(&c, &cfg, &task_id, 1, t0 + 14)
            .unwrap_err()
            .to_string(),
        "candidate_result_unverified"
    );
    db.execute(
        "UPDATE candidate_artifacts SET body=? WHERE hash=?",
        rusqlite::params![result_bytes.as_slice(), result_hash],
    )
    .unwrap();
    drop(db);

    // A request for execution without the reviewed capability is refused
    // before any queue or task row exists.
    let disabled_dir = root.join("candidate-execution-disabled");
    fs::create_dir_all(&disabled_dir).unwrap();
    let mut disabled_settings =
        candidate_execution_settings(&disabled_dir, base_settings, 1_048_576);
    disabled_settings
        .candidate_compute
        .as_mut()
        .unwrap()
        .execution = None;
    let (disabled_c, disabled_cfg) =
        candidate_config(&disabled_dir, base_policy, disabled_settings.clone());
    candidate_execution_book(&disabled_c);
    let mut disabled_args =
        candidate_submit_args("candidate-exec-disabled", CANDIDATE_SOURCE, None);
    disabled_args.horizon_minutes = 1;
    disabled_args.execution = Some(candidate_execution_spec(
        "AAA should rank above BBB on frozen current-session features",
        10,
        1,
        1,
        "5000",
        "1",
        vec![],
    ));
    let refused = candidate_submit(
        &disabled_c,
        &disabled_cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        disabled_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(refused["status"], "not_dispatched");
    assert_eq!(refused["reason"], "candidate_execution_disabled");
    assert!(
        !disabled_settings.queue.exists(),
        "a refused execution request must not create a queue or task row"
    );

    // Scanner-only evidence can rank but cannot fill; submission is refused
    // before a task row.
    let scanner_dir = root.join("candidate-execution-scanner");
    fs::create_dir_all(&scanner_dir).unwrap();
    let scanner_settings = candidate_execution_settings(&scanner_dir, base_settings, 1_048_576);
    let (scanner_c, scanner_cfg) =
        candidate_config(&scanner_dir, base_policy, scanner_settings.clone());
    candidate_book(&scanner_c);
    let mut scanner_args = candidate_submit_args("candidate-exec-scanner", CANDIDATE_SOURCE, None);
    scanner_args.horizon_minutes = 1;
    scanner_args.execution = Some(candidate_execution_spec(
        "AAA should rank above BBB on frozen current-session features",
        10,
        1,
        1,
        "5000",
        "1",
        vec![],
    ));
    let scanner_refused = candidate_submit(
        &scanner_c,
        &scanner_cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        scanner_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(scanner_refused["status"], "not_dispatched");
    assert_eq!(
        scanner_refused["reason"],
        "candidate_execution_quotes_unavailable"
    );
    assert!(!scanner_settings.queue.exists());

    // A pending execution cancellation never runs and never produces a result.
    let cancel_dir = root.join("candidate-execution-cancel");
    fs::create_dir_all(&cancel_dir).unwrap();
    let cancel_settings = candidate_execution_settings(&cancel_dir, base_settings, 1_048_576);
    let (cancel_c, cancel_cfg) =
        candidate_config(&cancel_dir, base_policy, cancel_settings.clone());
    candidate_execution_book(&cancel_c);
    let mut cancel_args = candidate_submit_args("candidate-exec-cancel", CANDIDATE_SOURCE, None);
    cancel_args.horizon_minutes = 1;
    cancel_args.execution = Some(candidate_execution_spec(
        "AAA should rank above BBB on frozen current-session features",
        10,
        1,
        1,
        "5000",
        "1",
        vec![],
    ));
    let pending = candidate_submit(
        &cancel_c,
        &cancel_cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        cancel_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    let pending_task = pending["task"]["task_id"].as_str().unwrap().to_owned();
    assert_eq!(
        candidate_cancel(&cancel_c, &cancel_cfg, &pending_task, 1, t0 + 1).unwrap()["task"]["status"],
        "cancelled"
    );
    let mut pending_unit = FakeCandidateUnit {
        clock: Arc::new(move || t0 + 2),
    };
    assert_eq!(
        drive_candidate_with_unit(
            cancel_settings,
            cancel_c.directory.join("model"),
            pending_task.clone(),
            1,
            Arc::new(AtomicBool::new(false)),
            || t0 + 2,
            &mut pending_unit,
        ),
        Err("candidate_recovery_required")
    );
    assert_eq!(
        candidate_result(&cancel_c, &cancel_cfg, &pending_task, 1, t0 + 3).unwrap()["result_status"],
        "not_verified"
    );

    // A genuine execution result produced after the compute deadline is
    // authentic late output, not a success.
    let late_dir = root.join("candidate-execution-late");
    fs::create_dir_all(&late_dir).unwrap();
    let late_settings = candidate_execution_settings(&late_dir, base_settings, 1_048_576);
    let (late_c, late_cfg) = candidate_config(&late_dir, base_policy, late_settings.clone());
    candidate_execution_book(&late_c);
    let mut late_args = candidate_submit_args("candidate-exec-late", CANDIDATE_SOURCE, None);
    late_args.horizon_minutes = 1;
    late_args.execution = Some(candidate_execution_spec(
        "AAA should rank above BBB on frozen current-session features",
        10,
        1,
        1,
        "5000",
        "1",
        vec![],
    ));
    let late = candidate_submit(
        &late_c,
        &late_cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        late_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    let late_task = late["task"]["task_id"].as_str().unwrap().to_owned();
    let late_head = candidate_record(&late_c, &late_cfg, &late_task);
    let candidate = late_settings.candidate_compute.as_ref().unwrap();
    let claim_at = t0 + 1;
    let compute_deadline =
        (claim_at + i64::from(candidate.wall_millis)).min(late_head.spec.expires_at_ms);
    let late_at = compute_deadline + 1;
    let ended_at = late_at + 10;
    let mut late_unit = LateCandidateUnit {
        helper_at: late_at,
        proof_at: ended_at,
        tamper_binding: false,
    };
    candidate_drive_result(
        &late_c,
        &late_cfg,
        &late_task,
        candidate_scripted_clock(vec![claim_at, compute_deadline - 1, ended_at]),
        &mut late_unit,
    )
    .unwrap();
    let late_record = candidate_record(&late_c, &late_cfg, &late_task);
    assert_eq!(late_record.status, jobs::TaskState::Terminated);
    let late_db = candidate_queue(&late_c, &late_cfg);
    let late_lease = late_record.job.lease.clone().unwrap();
    let late_stopped_bytes = worker::read_receipt(
        &late_db,
        &late_record.spec.job_id,
        &late_lease,
        "candidate_stopped",
        65_536,
    )
    .unwrap()
    .unwrap();
    let late_stopped: CandidateStopped = serde_json::from_slice(&late_stopped_bytes).unwrap();
    assert_eq!(
        late_stopped.cause,
        jobs::CandidateStopCause::DeadlineExpired
    );
    assert_eq!(
        late_stopped.outcome,
        CandidateStopOutcome::ResultAfterDeadline
    );
    assert_eq!(
        late_stopped.failure_code.as_deref(),
        Some("candidate_result_after_deadline")
    );
    drop(late_db);

    // The reviewed result cap remains authoritative; an oversized execution
    // envelope is a bounded failure, never an unverified success.
    let cap_dir = root.join("candidate-execution-cap");
    fs::create_dir_all(&cap_dir).unwrap();
    let cap_settings = candidate_execution_settings(&cap_dir, base_settings, 1_024);
    let (cap_c, cap_cfg) = candidate_config(&cap_dir, base_policy, cap_settings.clone());
    candidate_execution_book(&cap_c);
    let mut cap_args = candidate_submit_args("candidate-exec-cap", CANDIDATE_SOURCE, None);
    cap_args.horizon_minutes = 1;
    cap_args.execution = Some(candidate_execution_spec(
        "AAA should rank above BBB on frozen current-session features",
        10,
        1,
        1,
        "5000",
        "1",
        vec![],
    ));
    let capped = candidate_submit(
        &cap_c,
        &cap_cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        cap_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    let cap_task = capped["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&cap_c, &cap_cfg, &cap_task, move || t0 + 10);
    assert_eq!(
        candidate_status(&cap_c, &cap_cfg, &cap_task, 1, t0 + 11).unwrap()["task"]["status"],
        "terminated"
    );
    assert_eq!(
        candidate_result(&cap_c, &cap_cfg, &cap_task, 1, t0 + 11).unwrap()["result_status"],
        "not_verified"
    );
}

fn candidate_capacity_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-capacity");
    fs::create_dir_all(&dir).unwrap();
    let settings = candidate_settings(&dir, base_settings);
    let (c, cfg) = candidate_config(&dir, base_policy, settings);
    candidate_book(&c);
    let t0 = candidate_at(9, 23);
    let first = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-capacity",
        candidate_submit_args("capacity-one", CANDIDATE_SOURCE_ALT, None),
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    let second = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-capacity",
        candidate_submit_args("capacity-two", CANDIDATE_SOURCE, None),
        t0 + 1,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(first["status"], "queued");
    assert_eq!(second["status"], "queued");
    let first_task = first["task"]["task_id"].as_str().unwrap().to_owned();
    let second_task = second["task"]["task_id"].as_str().unwrap().to_owned();
    let first_record = candidate_record(&c, &cfg, &first_task);
    let lease = digest(b"capacity-fixture-lease");
    let mut db = candidate_queue(&c, &cfg);
    jobs::claim(&mut db, &first_record.spec.job_id, &lease, t0 + 1).unwrap();
    drop(db);
    // Capacity must leave the second task Pending with no launch receipt.
    candidate_drive(&c, &cfg, &second_task, move || t0 + 2);
    let second_record = candidate_record(&c, &cfg, &second_task);
    assert_eq!(second_record.status, jobs::TaskState::Pending);
    assert_eq!(second_record.job.attempts, 0);
    let db = candidate_queue(&c, &cfg);
    assert_eq!(candidate_launch_count(&db, &second_record.spec.job_id), 0);
    let queued: i64 = db
        .query_row("SELECT count(*) FROM candidate_tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(queued, 2);
    drop(db);
    let mut db = candidate_queue(&c, &cfg);
    jobs::record_candidate_termination(
        &mut db,
        &first_record.spec.job_id,
        &lease,
        &digest(b"capacity-fixture-stop"),
        jobs::CandidateStopCause::Failure,
        t0 + 2,
    )
    .unwrap();
    drop(db);
    candidate_drive(&c, &cfg, &second_task, move || t0 + 3);
    assert_eq!(
        candidate_record(&c, &cfg, &second_task).status,
        jobs::TaskState::Succeeded
    );
}

fn candidate_fairness_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-fairness");
    fs::create_dir_all(&dir).unwrap();
    let settings = candidate_settings(&dir, base_settings);
    let (c, cfg) = candidate_config(&dir, base_policy, settings.clone());
    candidate_book(&c);
    let t0 = candidate_at(9, 23);
    let registered = owner_task_enqueue_with_evidence(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-fairness",
        "fairness-registered",
        "Measure registered inputs",
        "shared_core_backtest",
        "synthetic_no_quotes_fixture",
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(registered["status"], "pending");
    let candidate = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-fairness",
        candidate_submit_args("fairness-candidate", CANDIDATE_SOURCE, None),
        t0 + 1,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(candidate["status"], "queued");
    let mut book = super::super::super::open_book(&c).unwrap();
    let mut runtime = OwnerResearchRuntime::default();
    let started = tick_owner_tasks_with_runner(
        &mut book,
        &c,
        &cfg,
        ResearchLaunch {
            registered: false,
            candidate: true,
            reason: "registered_owner_work_deferred",
        },
        &mut runtime,
        t0 + 2,
        move || {
            Box::new(FakeCandidateUnit {
                clock: Arc::new(move || t0 + 1),
            })
        },
    )
    .unwrap();
    assert_eq!(started["status"], "started");
    assert_eq!(started["kind"], "candidate");
    for _ in 0..200 {
        runtime.observe_channel();
        if runtime.active.is_none() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(runtime.active.is_none());
    let registered_task = scoped_task(
        &c,
        &cfg,
        registered["task_id"].as_str().unwrap(),
        registered["revision"].as_u64().unwrap() as u32,
    )
    .unwrap()
    .1;
    assert_eq!(registered_task.status, jobs::TaskState::Pending);

    // An ineligible predecessor on an old book binding must not starve a newer
    // eligible candidate. The selector skips it without mutating its row.
    let template = candidate_record(&c, &cfg, candidate["task"]["task_id"].as_str().unwrap());
    let mut old = template.clone();
    old.spec.book_binding = "e".repeat(64);
    old.spec.created_at_ms = t0;
    let mut new = template.clone();
    new.spec.book_binding = super::super::super::book_binding(&c).unwrap();
    new.spec.created_at_ms = t0 + 1;
    let selected = choose_active(
        None,
        vec![old.clone(), new.clone()],
        ResearchLaunch {
            registered: false,
            candidate: true,
            reason: "fixture",
        },
        &super::super::super::book_binding(&c).unwrap(),
        &c.session.to_string(),
        t0 + 2,
    )
    .unwrap();
    assert!(matches!(
        selected,
        ActiveChoice::Candidate(ref task) if task.spec.task_id == new.spec.task_id
    ));

    // The controller's in-process flag is operator intent only: a task TTL
    // expiry must not set it, while an explicit cancel receipt must. Use a
    // fresh book so no unrelated terminal row needs owner-event authority.
    let flag_dir = root.join("candidate-operator-flag");
    fs::create_dir_all(&flag_dir).unwrap();
    let (flag_c, flag_cfg) = candidate_config(
        &flag_dir,
        base_policy,
        candidate_settings(&flag_dir, base_settings),
    );
    candidate_book(&flag_c);
    let mut flag_book = super::super::super::open_book(&flag_c).unwrap();
    let flagged = candidate_submit(
        &flag_c,
        &flag_cfg,
        "grok-4.7",
        3,
        "turn-fairness",
        candidate_submit_args("fairness-operator-flag", CANDIDATE_SOURCE_ALT, None),
        t0 + 3,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(flagged["status"], "queued");
    let flagged_task = flagged["task"]["task_id"].as_str().unwrap().to_owned();
    let flagged_record = candidate_record(&flag_c, &flag_cfg, &flagged_task);
    let flagged_lease = digest(b"fairness-operator-flag-lease");
    let mut queue = candidate_queue(&flag_c, &flag_cfg);
    jobs::claim(
        &mut queue,
        &flagged_record.spec.job_id,
        &flagged_lease,
        t0 + 4,
    )
    .unwrap();
    let flagged_record = jobs::load_candidate(&mut queue, &flagged_task, 1).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut flag_runtime = OwnerResearchRuntime {
        active: Some((
            ResearchTaskKind::Candidate,
            flagged_task.clone(),
            1,
            Arc::clone(&cancel),
            receiver,
        )),
        ..OwnerResearchRuntime::default()
    };
    tick_owner_tasks_with_runner(
        &mut flag_book,
        &flag_c,
        &flag_cfg,
        ResearchLaunch {
            registered: false,
            candidate: true,
            reason: "fixture",
        },
        &mut flag_runtime,
        flagged_record.spec.expires_at_ms,
        move || {
            Box::new(FakeCandidateUnit {
                clock: Arc::new(move || t0 + 5),
            })
        },
    )
    .unwrap();
    assert!(
        !cancel.load(Ordering::Relaxed),
        "task TTL expiry must not set the operator-cancel flag"
    );
    candidate_cancel(&flag_c, &flag_cfg, &flagged_task, 1, t0 + 6).unwrap();
    tick_owner_tasks_with_runner(
        &mut flag_book,
        &flag_c,
        &flag_cfg,
        ResearchLaunch {
            registered: false,
            candidate: true,
            reason: "fixture",
        },
        &mut flag_runtime,
        t0 + 7,
        move || {
            Box::new(FakeCandidateUnit {
                clock: Arc::new(move || t0 + 7),
            })
        },
    )
    .unwrap();
    assert!(
        cancel.load(Ordering::Relaxed),
        "an explicit operator cancel receipt must set the flag"
    );
    drop(sender);
}

fn candidate_future_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-future");
    fs::create_dir_all(&dir).unwrap();
    let settings = candidate_settings(&dir, base_settings);
    let (c, cfg) = candidate_config(&dir, base_policy, settings);
    candidate_book(&c);
    let t0 = candidate_at(9, 21);
    let submitted = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-future",
        candidate_submit_args("future-freeze", CANDIDATE_SOURCE, None),
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(submitted["status"], "queued");
    let task_id = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    let record = candidate_record(&c, &cfg, &task_id);
    let db = candidate_queue(&c, &cfg);
    let dataset =
        jobs::read_candidate_artifact(&db, &record.spec.dataset_hash, "dataset", 4 * 1024 * 1024)
            .unwrap();
    let snapshots: Vec<nivra_core::strategy::FeatureSnapshot> =
        serde_json::from_slice(&dataset).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots.iter().all(|snapshot| snapshot.observed_ms <= t0));
    drop(db);
    candidate_drive(&c, &cfg, &task_id, move || t0 + 1);
    let status = candidate_status(&c, &cfg, &task_id, 1, t0 + 2).unwrap();
    assert_eq!(status["lifecycle"]["state"], "forward_shadow");
    let result = candidate_result(&c, &cfg, &task_id, 1, t0 + 2).unwrap();
    let entry_ms = result["result"]["report"]["ticks"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["observed_ms"]
        .as_i64()
        .unwrap();
    let due_ms = entry_ms + 5 * 60_000;
    let db = super::super::super::open_book(&c).unwrap();
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,?,?,'scanner_metadata')",
        rusqlite::params![due_ms, due_ms, candidate_archive(due_ms).to_string()],
    )
    .unwrap();
    drop(db);
    let early = mature_candidates(&c, &cfg, due_ms - 1).unwrap();
    assert_eq!(early["items"][0]["state"], "forward_shadow");
    let late = mature_candidates(&c, &cfg, due_ms + 1).unwrap();
    assert_eq!(late["items"][0]["state"], "eligible");
}

fn candidate_failed_report_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-failed-report");
    fs::create_dir_all(&dir).unwrap();
    let settings = candidate_settings(&dir, base_settings);
    let (c, cfg) = candidate_config(&dir, base_policy, settings);
    candidate_book(&c);
    let t0 = candidate_at(9, 23);
    let submitted = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-failed-report",
        candidate_submit_args("failed-report", CANDIDATE_BAD_SOURCE, None),
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    let task_id = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &task_id, move || t0 + 1);
    let result = candidate_result(&c, &cfg, &task_id, 1, t0 + 2).unwrap();
    assert_eq!(result["result_status"], "verified");
    assert_eq!(result["result"]["report"]["status"], "failed");
    assert_eq!(result["result"]["qualified"], false);
    assert_eq!(result["result"]["analysis_status"], "failed");
    assert_eq!(result["lifecycle"]["state"], "replayed");
    assert_ne!(result["lifecycle"]["state"], "forward_shadow");
}

fn candidate_missing_quote_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-missing-quote");
    fs::create_dir_all(&dir).unwrap();
    let settings = candidate_settings(&dir, base_settings);
    let (c, cfg) = candidate_config(&dir, base_policy, settings);
    candidate_book(&c);
    let t0 = candidate_at(9, 23);
    let submitted = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-missing-quote",
        candidate_submit_args("missing-quote", CANDIDATE_SOURCE, None),
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    let task_id = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &task_id, move || t0 + 1);
    let result = candidate_result(&c, &cfg, &task_id, 1, t0 + 2).unwrap();
    let entry_ms = result["result"]["report"]["ticks"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["observed_ms"]
        .as_i64()
        .unwrap();
    let due_ms = entry_ms + 5 * 60_000;
    let matured = mature_candidates(&c, &cfg, due_ms + 60_001).unwrap();
    assert_eq!(matured["items"][0]["state"], "insufficient");
    let marks = matured["items"][0]["marks"].as_array().unwrap();
    assert!(marks.iter().any(|mark| {
        mark["status"] == "unknown_gap" && mark["disposition"] == "source_ended_before_due"
    }));
}

fn candidate_dir_for_helper_self_test(launch: &worker::CandidateLaunch) -> PathBuf {
    Path::new(&launch.stage_dir).join("helper-self-test")
}

fn candidate_recovery_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-recovery");
    fs::create_dir_all(&dir).unwrap();
    let settings = candidate_settings(&dir, base_settings);
    if !candidate_sandbox_ready(&settings) {
        assert!(
            std::env::var_os("NIVRA_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!("skipping candidate recovery contract: python3 or bwrap unavailable on host");
        return;
    }
    let (c, cfg) = candidate_config(&dir, base_policy, settings.clone());
    candidate_book(&c);
    let candidate = settings.candidate_compute.as_ref().unwrap();
    let t0 = candidate_at(9, 23);
    drop(open_queue_for_owner(&settings, &c.directory.join("model"), true).unwrap());
    let mut db = candidate_queue(&c, &cfg);

    let submit = |db: &mut Connection, key: &str, source: &str, at: i64| {
        let submitted = candidate_submit(
            &c,
            &cfg,
            "grok-4.7",
            3,
            "turn-recovery",
            candidate_submit_args(key, source, None),
            at,
            Some(candidate_evidence()),
        )
        .unwrap();
        assert_eq!(submitted["status"], "queued", "{submitted}");
        let task_id = submitted["task"]["task_id"].as_str().unwrap().to_owned();
        let record = jobs::load_candidate(db, &task_id, 1).unwrap();
        (task_id, record)
    };
    let claim_and_launch =
        |db: &mut Connection, record: &jobs::CandidateTaskRecord, lease: &str, at: i64| {
            jobs::claim(db, &record.spec.job_id, lease, at).unwrap();
            let record = jobs::load_candidate(db, &record.spec.task_id, 1).unwrap();
            let launch = prepare_launch(&settings, &record, lease, at).unwrap();
            worker::save_receipt(
                db,
                &launch.job_id,
                &launch.lease,
                "candidate_launch",
                &serde_json::to_vec(&launch).unwrap(),
            )
            .unwrap();
            (record, launch)
        };

    let valid_result = |db: &mut Connection,
                        record: &jobs::CandidateTaskRecord,
                        launch: &worker::CandidateLaunch,
                        at: i64|
     -> (CandidateResultV1, String, u64) {
        let source =
            jobs::read_candidate_artifact(db, &record.spec.source_sha256, "source", 262_144)
                .unwrap();
        let params =
            jobs::read_candidate_artifact(db, &record.spec.params_sha256, "params", 65_536)
                .unwrap();
        let dataset = jobs::read_candidate_artifact(
            db,
            &record.spec.dataset_hash,
            "dataset",
            4 * 1024 * 1024,
        )
        .unwrap();
        stage_candidate(
            &settings,
            settings.candidate_compute.as_ref().unwrap(),
            launch,
            &source,
            &params,
            &dataset,
            None,
        )
        .unwrap();
        let output = run_helper_at(launch, || at, false).unwrap();
        write_helper_output(
            Path::new(&launch.stage_dir),
            &output,
            launch.unit_policy.max_result_bytes,
        )
        .unwrap();
        let proof = candidate_fake_proof(launch, at);
        let envelope = candidate_result_envelope(launch, record, output, proof).unwrap();
        let bytes = serde_json::to_vec(&envelope).unwrap();
        let hash = jobs::put_candidate_artifact_bounded(
            db,
            "result",
            &bytes,
            record.spec.unit_policy.max_result_bytes as usize,
            at,
        )
        .unwrap();
        (envelope, hash, bytes.len() as u64)
    };

    // (1) Durable stopped + result completes idempotently after a crash.
    let (task_id, record) = submit(&mut db, "recovery-result", CANDIDATE_SOURCE, t0 + 1);
    let lease = digest(format!("recovery-lease:{task_id}").as_bytes());
    let (record, launch) = claim_and_launch(&mut db, &record, &lease, t0 + 2);
    let source =
        jobs::read_candidate_artifact(&db, &record.spec.source_sha256, "source", 262_144).unwrap();
    let params =
        jobs::read_candidate_artifact(&db, &record.spec.params_sha256, "params", 65_536).unwrap();
    let dataset =
        jobs::read_candidate_artifact(&db, &record.spec.dataset_hash, "dataset", 4 * 1024 * 1024)
            .unwrap();
    stage_candidate(
        &settings, candidate, &launch, &source, &params, &dataset, None,
    )
    .unwrap();
    let output = run_helper_at(&launch, || t0 + 3, false).unwrap();
    write_helper_output(
        Path::new(&launch.stage_dir),
        &output,
        launch.unit_policy.max_result_bytes,
    )
    .unwrap();
    let proof = candidate_fake_proof(&launch, t0 + 3);
    let envelope = candidate_result_envelope(&launch, &record, output, proof.clone()).unwrap();
    let result_hash = jobs::put_candidate_artifact(
        &mut db,
        "result",
        &serde_json::to_vec(&envelope).unwrap(),
        t0 + 3,
    )
    .unwrap();
    let stopped = candidate_stopped(
        &launch,
        proof,
        jobs::CandidateStopCause::Succeeded,
        CandidateStopOutcome::Succeeded,
        None,
        Some(result_hash),
        Some(envelope.result_produced_at_ms),
        Some(serde_json::to_vec(&envelope).unwrap().len() as u64),
        t0 + 3,
    );
    write_stopped(&db, &launch, &stopped).unwrap();
    let mut fake = FakeCandidateUnit {
        clock: Arc::new(move || t0 + 4),
    };
    let reconciled =
        reconcile_candidate_attempt(&settings, &mut db, &record, &mut fake, || t0 + 4).unwrap();
    assert_eq!(reconciled["outcome"], "succeeded");
    assert_eq!(
        jobs::load_candidate(&mut db, &task_id, 1).unwrap().status,
        jobs::TaskState::Succeeded
    );
    let record = jobs::load_candidate(&mut db, &task_id, 1).unwrap();
    let again =
        reconcile_candidate_attempt(&settings, &mut db, &record, &mut fake, || t0 + 5).unwrap();
    assert_eq!(again["status"], "terminal");
    assert!(read_verified_candidate_result(&mut db, &task_id, 1).is_ok());

    // Unit proof negative cases: no fabricated absence and no PID/PGID proof.
    let mut bad = stopped.proof.clone();
    bad.active_state = "active".into();
    bad.proof_sha256 = bad.hash().unwrap();
    assert!(!bad.verify(&launch));
    let mut bad = stopped.proof.clone();
    bad.main_pid = 1;
    bad.proof_sha256 = bad.hash().unwrap();
    assert!(!bad.verify(&launch));
    let mut bad = stopped.proof.clone();
    bad.tasks_present = true;
    bad.tasks_current = 1;
    bad.proof_sha256 = bad.hash().unwrap();
    assert!(!bad.verify(&launch));
    let mut bad = stopped.proof.clone();
    bad.cgroup_absent = false;
    bad.control_group = "/user.slice/not-ours".into();
    bad.cgroup_events = String::new();
    bad.cgroup_events_sha256 = digest(b"");
    bad.proof_sha256 = bad.hash().unwrap();
    assert!(!bad.verify(&launch));
    let mut bad = stopped.proof.clone();
    bad.cgroup_absent = false;
    bad.control_group = "/user.slice/not-ours".into();
    bad.cgroup_events = "populated 1\n".into();
    bad.cgroup_events_sha256 = digest(b"populated 1\n");
    bad.proof_sha256 = bad.hash().unwrap();
    assert!(!bad.verify(&launch));
    let mut bad = stopped.proof.clone();
    bad.unit = "nivra-candidate-".to_owned() + &"f".repeat(64);
    bad.proof_sha256 = bad.hash().unwrap();
    assert!(!bad.verify(&launch));
    let mut bad = stopped.proof.clone();
    bad.checked_at_ms = launch.started_ms - 1;
    bad.proof_sha256 = bad.hash().unwrap();
    assert!(!bad.verify(&launch));
    let mut bad = stopped.proof.clone();
    bad.proof_sha256 = "0".repeat(64);
    assert!(!bad.verify(&launch));

    // (2) Durable stopped without a result is a trusted terminal failure.
    let (task_id, record) = submit(&mut db, "recovery-no-result", CANDIDATE_SOURCE_ALT, t0 + 10);
    let lease = digest(format!("recovery-lease:{task_id}").as_bytes());
    let (record, launch) = claim_and_launch(&mut db, &record, &lease, t0 + 11);
    let proof = candidate_fake_proof(&launch, t0 + 12);
    let stopped = candidate_stopped(
        &launch,
        proof,
        jobs::CandidateStopCause::Failure,
        CandidateStopOutcome::NoResult,
        None,
        None,
        None,
        None,
        t0 + 12,
    );
    write_stopped(&db, &launch, &stopped).unwrap();
    let reconciled =
        reconcile_candidate_attempt(&settings, &mut db, &record, &mut fake, || t0 + 13).unwrap();
    assert_eq!(reconciled["outcome"], "terminated");
    assert_eq!(
        jobs::load_candidate(&mut db, &task_id, 1).unwrap().status,
        jobs::TaskState::Terminated
    );

    // (3) A live attempt without a stop proof is stopped only through the
    // original owned unit, then proven absent; source is never relaunched.
    let (task_id, record) = submit(&mut db, "recovery-live", CANDIDATE_SLEEP_SOURCE, t0 + 20);
    let lease = digest(format!("recovery-lease:{task_id}").as_bytes());
    let (record, _launch) = claim_and_launch(&mut db, &record, &lease, t0 + 21);
    let reconciled =
        reconcile_candidate_attempt(&settings, &mut db, &record, &mut fake, || t0 + 22).unwrap();
    assert_eq!(reconciled["outcome"], "terminated");
    assert_eq!(
        jobs::load_candidate(&mut db, &task_id, 1).unwrap().status,
        jobs::TaskState::Terminated
    );

    // (5) A durable operator-cancel outcome is honored even when the mutable
    // job row is still Running. The stop receipt, not the head row, is the
    // recorded cause; repeating reconciliation is a no-op.
    let (cancel_task, cancel_record) = submit(
        &mut db,
        "recovery-durable-cancel",
        CANDIDATE_SLEEP_SOURCE,
        t0 + 30,
    );
    let cancel_lease = digest(format!("recovery-lease:{cancel_task}").as_bytes());
    let (cancel_record, cancel_launch) =
        claim_and_launch(&mut db, &cancel_record, &cancel_lease, t0 + 31);
    let cancel_stopped = candidate_stopped(
        &cancel_launch,
        candidate_fake_proof(&cancel_launch, t0 + 32),
        jobs::CandidateStopCause::OperatorCancel,
        CandidateStopOutcome::Cancelled,
        None,
        None,
        None,
        None,
        t0 + 32,
    );
    write_stopped(&db, &cancel_launch, &cancel_stopped).unwrap();
    let reconciled =
        reconcile_candidate_attempt(&settings, &mut db, &cancel_record, &mut fake, || t0 + 33)
            .unwrap();
    assert_eq!(reconciled["outcome"], "cancelled");
    assert_eq!(
        jobs::load_candidate(&mut db, &cancel_task, 1)
            .unwrap()
            .status,
        jobs::TaskState::Cancelled
    );
    let terminal_record = jobs::load_candidate(&mut db, &cancel_task, 1).unwrap();
    let again =
        reconcile_candidate_attempt(&settings, &mut db, &terminal_record, &mut fake, || t0 + 34)
            .unwrap();
    assert_eq!(again["status"], "terminal");
    assert_eq!(
        jobs::load_candidate(&mut db, &cancel_task, 1)
            .unwrap()
            .status,
        jobs::TaskState::Cancelled
    );

    // A durable deadline outcome on a Running row is Terminated, never a
    // fabricated operator cancellation.
    let (expired_task, expired_record) = submit(
        &mut db,
        "recovery-durable-expired",
        CANDIDATE_SLEEP_SOURCE,
        t0 + 40,
    );
    let expired_lease = digest(format!("recovery-lease:{expired_task}").as_bytes());
    let (expired_record, expired_launch) =
        claim_and_launch(&mut db, &expired_record, &expired_lease, t0 + 41);
    let expired_stopped = candidate_stopped(
        &expired_launch,
        candidate_fake_proof(&expired_launch, t0 + 42),
        jobs::CandidateStopCause::DeadlineExpired,
        CandidateStopOutcome::DeadlineExpired,
        None,
        None,
        None,
        None,
        t0 + 42,
    );
    write_stopped(&db, &expired_launch, &expired_stopped).unwrap();
    let reconciled =
        reconcile_candidate_attempt(&settings, &mut db, &expired_record, &mut fake, || t0 + 43)
            .unwrap();
    assert_eq!(reconciled["outcome"], "terminated");
    assert_eq!(
        jobs::load_candidate(&mut db, &expired_task, 1)
            .unwrap()
            .status,
        jobs::TaskState::Terminated
    );

    // (6) A timely result with a durable stop proof still reconciles after a
    // later deadline/failure termination; the later expiry does not erase the
    // authentic finished work.
    let (late_task, late_record) =
        submit(&mut db, "recovery-late-timely", CANDIDATE_SOURCE, t0 + 50);
    let late_lease = digest(format!("recovery-lease:{late_task}").as_bytes());
    let (late_record, late_launch) = claim_and_launch(&mut db, &late_record, &late_lease, t0 + 51);
    let (late_envelope, late_hash, late_bytes) =
        valid_result(&mut db, &late_record, &late_launch, t0 + 52);
    let late_stopped = candidate_stopped(
        &late_launch,
        late_envelope.unit_proof.clone(),
        jobs::CandidateStopCause::Succeeded,
        CandidateStopOutcome::Succeeded,
        None,
        Some(late_hash.clone()),
        Some(late_envelope.result_produced_at_ms),
        Some(late_bytes),
        t0 + 52,
    );
    write_stopped(&db, &late_launch, &late_stopped).unwrap();
    jobs::record_candidate_termination(
        &mut db,
        &late_launch.job_id,
        &late_lease,
        &digest(b"late-timely-deadline"),
        jobs::CandidateStopCause::DeadlineExpired,
        t0 + 53,
    )
    .unwrap();
    assert_eq!(
        jobs::load_candidate(&mut db, &late_task, 1).unwrap().status,
        jobs::TaskState::Terminated
    );
    let late_terminal = jobs::load_candidate(&mut db, &late_task, 1).unwrap();
    assert!(candidate_late_result_ready(&db, &late_terminal));
    let expired_terminal = jobs::load_candidate(&mut db, &expired_task, 1).unwrap();
    assert!(!candidate_late_result_ready(&db, &expired_terminal));
    let reconciled =
        reconcile_candidate_attempt(&settings, &mut db, &late_record, &mut fake, || t0 + 54)
            .unwrap();
    assert_eq!(reconciled["outcome"], "succeeded");
    assert_eq!(
        jobs::load_candidate(&mut db, &late_task, 1).unwrap().status,
        jobs::TaskState::Succeeded
    );
    assert!(read_verified_candidate_result(&mut db, &late_task, 1).is_ok());

    // (7) A corrupt, missing or mismatched stored payload is a trusted
    // terminal failure, never a Succeeded row that can never verify. The
    // artifact is preserved for audit.
    let (corrupt_task, corrupt_record) =
        submit(&mut db, "recovery-corrupt", CANDIDATE_SOURCE_ALT, t0 + 60);
    let corrupt_lease = digest(format!("recovery-lease:{corrupt_task}").as_bytes());
    let (corrupt_record, corrupt_launch) =
        claim_and_launch(&mut db, &corrupt_record, &corrupt_lease, t0 + 61);
    let corrupt_bytes = b"{\"version\":2,\"bad\":true}";
    let corrupt_hash =
        jobs::put_candidate_artifact(&mut db, "result", corrupt_bytes, t0 + 62).unwrap();
    let corrupt_stopped = candidate_stopped(
        &corrupt_launch,
        candidate_fake_proof(&corrupt_launch, t0 + 62),
        jobs::CandidateStopCause::Succeeded,
        CandidateStopOutcome::Succeeded,
        None,
        Some(corrupt_hash.clone()),
        Some(t0 + 62),
        Some(corrupt_bytes.len() as u64),
        t0 + 62,
    );
    write_stopped(&db, &corrupt_launch, &corrupt_stopped).unwrap();
    let reconciled =
        reconcile_candidate_attempt(&settings, &mut db, &corrupt_record, &mut fake, || t0 + 63)
            .unwrap();
    assert_eq!(reconciled["outcome"], "terminated");
    assert_eq!(
        jobs::load_candidate(&mut db, &corrupt_task, 1)
            .unwrap()
            .status,
        jobs::TaskState::Terminated
    );
    assert_eq!(
        jobs::read_candidate_artifact(&db, &corrupt_hash, "result", 1_048_576).unwrap(),
        corrupt_bytes
    );
    assert!(
        read_verified_candidate_result(&mut db, &corrupt_task, 1)
            .unwrap()
            .is_none()
    );

    let (missing_task, missing_record) = submit(
        &mut db,
        "recovery-missing-result",
        CANDIDATE_SOURCE_ALT,
        t0 + 70,
    );
    let missing_lease = digest(format!("recovery-lease:{missing_task}").as_bytes());
    let (missing_record, missing_launch) =
        claim_and_launch(&mut db, &missing_record, &missing_lease, t0 + 71);
    let missing_stopped = candidate_stopped(
        &missing_launch,
        candidate_fake_proof(&missing_launch, t0 + 72),
        jobs::CandidateStopCause::Succeeded,
        CandidateStopOutcome::Succeeded,
        None,
        Some("f".repeat(64)),
        Some(t0 + 72),
        Some(64),
        t0 + 72,
    );
    write_stopped(&db, &missing_launch, &missing_stopped).unwrap();
    let reconciled =
        reconcile_candidate_attempt(&settings, &mut db, &missing_record, &mut fake, || t0 + 73)
            .unwrap();
    assert_eq!(reconciled["outcome"], "terminated");
    assert_eq!(
        jobs::load_candidate(&mut db, &missing_task, 1)
            .unwrap()
            .status,
        jobs::TaskState::Terminated
    );

    let (mismatch_task, mismatch_record) = submit(
        &mut db,
        "recovery-mismatch-result",
        CANDIDATE_SOURCE_ALT,
        t0 + 80,
    );
    let mismatch_lease = digest(format!("recovery-lease:{mismatch_task}").as_bytes());
    let (mismatch_record, mismatch_launch) =
        claim_and_launch(&mut db, &mismatch_record, &mismatch_lease, t0 + 81);
    let mismatch_stopped = candidate_stopped(
        &mismatch_launch,
        candidate_fake_proof(&mismatch_launch, t0 + 82),
        jobs::CandidateStopCause::Succeeded,
        CandidateStopOutcome::Succeeded,
        None,
        Some(late_hash.clone()),
        Some(t0 + 82),
        Some(late_bytes),
        t0 + 82,
    );
    write_stopped(&db, &mismatch_launch, &mismatch_stopped).unwrap();
    let reconciled =
        reconcile_candidate_attempt(&settings, &mut db, &mismatch_record, &mut fake, || t0 + 83)
            .unwrap();
    assert_eq!(reconciled["outcome"], "terminated");
    assert_eq!(
        jobs::load_candidate(&mut db, &mismatch_task, 1)
            .unwrap()
            .status,
        jobs::TaskState::Terminated
    );

    // (8) A loaded unit with missing limit observations is not a valid proof;
    // an unloaded unit may retain an empty observation without inventing props.
    let mut loaded_missing = late_stopped.proof.clone();
    loaded_missing.cgroup_absent = false;
    loaded_missing.control_group = "/user.slice/ours.service".into();
    loaded_missing.cgroup_events = "populated 0\n".into();
    loaded_missing.cgroup_events_sha256 = digest(b"populated 0\n");
    loaded_missing.tasks_present = true;
    loaded_missing.tasks_current = 0;
    loaded_missing.observation = worker::CandidateUnitObservation::default();
    loaded_missing.properties_observed = false;
    loaded_missing.proof_sha256 = loaded_missing.hash().unwrap();
    assert!(!loaded_missing.verify(&late_launch));
    let mut loaded_complete = loaded_missing.clone();
    loaded_complete.observation = worker::CandidateUnitObservation {
        memory_max: Some(late_launch.unit_policy.unit_memory_bytes),
        memory_swap_max: Some(0),
        cpu_quota_usec: Some(
            u64::from(late_launch.unit_policy.unit_cpu_millis) * 1_000_000
                / u64::from(late_launch.unit_policy.unit_wall_millis.max(1)),
        ),
        tasks_max: Some(u64::from(late_launch.unit_policy.unit_tasks)),
        runtime_max_usec: Some(u64::from(late_launch.unit_policy.unit_wall_millis) * 1000),
        limit_nofile: Some(late_launch.unit_policy.unit_open_files),
        kill_mode: Some(late_launch.unit_policy.kill_mode.clone()),
        no_new_privileges: Some(true),
        timeout_stop_usec: Some(2_000_000),
        restart: Some(late_launch.unit_policy.restart.clone()),
    };
    loaded_complete.properties_observed = true;
    loaded_complete.proof_sha256 = loaded_complete.hash().unwrap();
    assert!(loaded_complete.verify(&late_launch));

    // (9) The helper's one-run marker refuses a second start and an existing
    // result, so a unit restart cannot execute untrusted source twice.
    // The late result already exists; a fresh stage is needed for the marker.
    let (marker_task, marker_record) =
        submit(&mut db, "recovery-run-once", CANDIDATE_SOURCE, t0 + 90);
    let marker_lease = digest(format!("recovery-lease:{marker_task}").as_bytes());
    let (marker_record, marker_launch) =
        claim_and_launch(&mut db, &marker_record, &marker_lease, t0 + 91);
    let marker_source =
        jobs::read_candidate_artifact(&db, &marker_record.spec.source_sha256, "source", 262_144)
            .unwrap();
    let marker_params =
        jobs::read_candidate_artifact(&db, &marker_record.spec.params_sha256, "params", 65_536)
            .unwrap();
    let marker_dataset = jobs::read_candidate_artifact(
        &db,
        &marker_record.spec.dataset_hash,
        "dataset",
        4 * 1024 * 1024,
    )
    .unwrap();
    stage_candidate(
        &settings,
        settings.candidate_compute.as_ref().unwrap(),
        &marker_launch,
        &marker_source,
        &marker_params,
        &marker_dataset,
        None,
    )
    .unwrap();
    claim_helper_run(&marker_launch).unwrap();
    assert_eq!(
        claim_helper_run(&marker_launch).unwrap_err(),
        "candidate_helper_run_already_started"
    );
    let marker_output = run_helper_at(&marker_launch, || t0 + 92, false).unwrap();
    write_helper_output(
        Path::new(&marker_launch.stage_dir),
        &marker_output,
        marker_launch.unit_policy.max_result_bytes,
    )
    .unwrap();
    assert_eq!(
        claim_helper_run(&marker_launch).unwrap_err(),
        "candidate_helper_result_exists"
    );
    jobs::record_candidate_termination(
        &mut db,
        &marker_launch.job_id,
        &marker_lease,
        &digest(b"run-once-marker-stop"),
        jobs::CandidateStopCause::Failure,
        t0 + 93,
    )
    .unwrap();
    // (9b) The helper's fd-based reader refuses a symlinked or oversized
    // staged input, and a changed helper self-hash is refused before scoring.
    let marker_source_path = Path::new(&marker_launch.stage_dir).join("source");
    fs::remove_file(&marker_source_path).unwrap();
    std::os::unix::fs::symlink(
        Path::new(&marker_launch.stage_dir).join("params"),
        &marker_source_path,
    )
    .unwrap();
    assert!(run_helper_at(&marker_launch, || t0 + 94, false).is_err());
    fs::remove_file(&marker_source_path).unwrap();
    fs::write(&marker_source_path, vec![b'x'; 262_145]).unwrap();
    fs::set_permissions(
        &marker_source_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(
        run_helper_at(&marker_launch, || t0 + 94, false).unwrap_err(),
        "candidate_helper_input_invalid"
    );
    fs::write(&marker_source_path, &marker_source).unwrap();
    fs::set_permissions(
        &marker_source_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .unwrap();
    let self_file = candidate_dir_for_helper_self_test(&marker_launch);
    fs::write(&self_file, b"helper-self-bytes").unwrap();
    verify_helper_file(&self_file, &digest(b"helper-self-bytes")).unwrap();
    assert_eq!(
        verify_helper_file(&self_file, &"0".repeat(64)).unwrap_err(),
        "candidate_helper_self_changed"
    );
    fs::remove_file(&self_file).unwrap();
    let mut bad_helper = marker_launch.clone();
    bad_helper.unit_policy.helper_sha256 = "0".repeat(64);
    assert!(run_helper_at(&bad_helper, || t0 + 95, true).is_err());

    // (10) A controlled operator cancel injected between the result snapshot
    // and the completion transaction wins; the artifact is preserved and the
    // transaction refuses to mark Succeeded. A host expiry cleanup that only
    // set CancelRequested (no operator receipt) does not erase a timely result.
    let (race_task, race_record) = submit(
        &mut db,
        "recovery-controlled-cancel",
        CANDIDATE_SOURCE,
        t0 + 100,
    );
    let race_lease = digest(format!("recovery-lease:{race_task}").as_bytes());
    jobs::claim(&mut db, &race_record.spec.job_id, &race_lease, t0 + 101).unwrap();
    let race_record = jobs::load_candidate(&mut db, &race_task, 1).unwrap();
    let race_launch = prepare_launch(&settings, &race_record, &race_lease, t0 + 101).unwrap();
    worker::save_receipt(
        &db,
        &race_launch.job_id,
        &race_launch.lease,
        "candidate_launch",
        &serde_json::to_vec(&race_launch).unwrap(),
    )
    .unwrap();
    let race_result = b"timely-result-preserved";
    let race_hash = jobs::put_candidate_artifact(&mut db, "result", race_result, t0 + 102).unwrap();
    candidate_cancel(&c, &cfg, &race_task, 1, t0 + 102).unwrap();
    // The in-process signal is operator-cancel intent only. A TTL/wall expiry
    // and a host cleanup without an operator receipt must not set it.
    let race_head = jobs::load_candidate(&mut db, &race_task, 1).unwrap();
    assert!(candidate_operator_cancel(&race_head));
    let mut host_head = race_head.clone();
    host_head.cancel_receipt = None;
    assert!(!candidate_operator_cancel(&host_head));
    let mut running_head = race_head.clone();
    running_head.status = jobs::TaskState::Running;
    assert!(!candidate_operator_cancel(&running_head));
    assert_eq!(
        jobs::complete_reconciled(
            &mut db,
            &race_launch.job_id,
            &race_launch.lease,
            &race_hash,
            t0 + 102,
            t0 + 103,
        )
        .unwrap_err(),
        jobs::Error::State
    );
    jobs::record_candidate_termination(
        &mut db,
        &race_launch.job_id,
        &race_lease,
        &digest(b"controlled-cancel-stop"),
        jobs::CandidateStopCause::OperatorCancel,
        t0 + 103,
    )
    .unwrap();
    assert_eq!(
        jobs::load(&mut db, &race_launch.job_id).unwrap().state,
        jobs::State::Cancelled
    );
    assert_eq!(
        jobs::read_candidate_artifact(&db, &race_hash, "result", 1_048_576).unwrap(),
        race_result
    );

    let (host_task, host_record) =
        submit(&mut db, "recovery-host-expiry", CANDIDATE_SOURCE, t0 + 110);
    let host_lease = digest(format!("recovery-lease:{host_task}").as_bytes());
    jobs::claim(&mut db, &host_record.spec.job_id, &host_lease, t0 + 111).unwrap();
    let host_record = jobs::load_candidate(&mut db, &host_task, 1).unwrap();
    let host_launch = prepare_launch(&settings, &host_record, &host_lease, t0 + 111).unwrap();
    let host_result = b"host-expiry-timely-result";
    let host_hash = jobs::put_candidate_artifact(&mut db, "result", host_result, t0 + 112).unwrap();
    db.execute(
        "UPDATE jobs SET state='CancelRequested' WHERE id=?",
        [&host_launch.job_id],
    )
    .unwrap();
    jobs::complete_reconciled(
        &mut db,
        &host_launch.job_id,
        &host_launch.lease,
        &host_hash,
        t0 + 112,
        t0 + 113,
    )
    .unwrap();
    assert_eq!(
        jobs::load(&mut db, &host_launch.job_id).unwrap().state,
        jobs::State::Succeeded
    );

    // (4) A legacy experimental candidate row fails closed with a reviewed
    // migration requirement, never an instruction to recreate the database.
    let legacy_id = "a".repeat(64);
    let legacy_job = "b".repeat(64);
    db.execute(
        "INSERT OR REPLACE INTO candidate_tasks(task_id,revision,job_id,session,parent_turn_id,content_key,input_key,body,created_ms) VALUES(?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            legacy_id,
            1,
            legacy_job,
            c.session.to_string(),
            "turn-legacy",
            "c".repeat(64),
            "d".repeat(64),
            "{\"version\":1}",
            t0
        ],
    )
    .unwrap();
    let error = candidate_status(&c, &cfg, &legacy_id, 1, t0 + 30).unwrap_err();
    assert_eq!(
        error.to_string(),
        "candidate_legacy_schema_migration_required"
    );

    // (5) Disabling the reviewed policy refuses submission without a task row.
    let mut off = cfg.clone();
    off.research_tools
        .as_mut()
        .unwrap()
        .candidate_compute
        .as_mut()
        .unwrap()
        .enabled = false;
    let before: i64 = db
        .query_row("SELECT count(*) FROM candidate_tasks", [], |row| row.get(0))
        .unwrap();
    let refused = candidate_submit(
        &c,
        &off,
        "grok-4.7",
        3,
        "turn-recovery",
        candidate_submit_args("recovery-policy-off", CANDIDATE_SOURCE, None),
        t0 + 40,
        Some(candidate_evidence()),
    )
    .unwrap_err();
    assert_eq!(refused.to_string(), "candidate_compute_disabled");
    let after: i64 = db
        .query_row("SELECT count(*) FROM candidate_tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(before, after);

    // (11) Machine identity is captured before the durable claim. An
    // unavailable identity refuses the still-Pending task with a static reason
    // and never writes a launch receipt or an empty-machine fallback.
    assert_eq!(
        machine_id_from(|_| Err(std::io::Error::from(std::io::ErrorKind::NotFound))).unwrap_err(),
        "candidate_machine_unavailable"
    );
    assert_eq!(
        machine_id_from(|_| Ok("  \n".into())).unwrap_err(),
        "candidate_machine_unavailable"
    );
    assert_eq!(
        machine_id_from(|_| Ok("machine-a\n".into())).unwrap(),
        "machine-a"
    );
    assert_eq!(
        machine_id_from(|_| Ok("bad\nid".into())).unwrap_err(),
        "candidate_machine_unavailable"
    );
    let (machine_task, _) = submit(
        &mut db,
        "recovery-machine-missing",
        CANDIDATE_SOURCE,
        t0 + 120,
    );
    let mut no_machine = UnavailableCandidateUnit;
    let machine_drive = drive_candidate_with_unit_and_machine(
        settings.clone(),
        c.directory.join("model"),
        machine_task.clone(),
        1,
        Arc::new(AtomicBool::new(false)),
        || t0 + 121,
        &mut no_machine,
        || Err("candidate_machine_unavailable"),
    );
    assert!(
        machine_drive.is_ok(),
        "an unavailable machine identity must be a handled Pending refusal: {machine_drive:?}"
    );
    let machine_record = jobs::load_candidate(&mut db, &machine_task, 1).unwrap();
    assert_eq!(machine_record.status, jobs::TaskState::Refused);
    assert_eq!(machine_record.job.attempts, 0);
    assert_eq!(
        machine_record
            .refusal_receipt
            .as_ref()
            .map(|receipt| receipt.reason),
        Some(jobs::RefusalReason::MachineIdentityUnavailable)
    );
    assert_eq!(candidate_launch_count(&db, &machine_record.spec.job_id), 0);

    // An existing claimed, unproven attempt stays ambiguous when the current
    // machine identity is unavailable; recovery never invents a release proof.
    let (recovery_task, recovery_head) = submit(
        &mut db,
        "recovery-machine-existing",
        CANDIDATE_SOURCE,
        t0 + 130,
    );
    let recovery_lease = digest(format!("recovery-lease:{recovery_task}").as_bytes());
    jobs::claim(
        &mut db,
        &recovery_head.spec.job_id,
        &recovery_lease,
        t0 + 131,
    )
    .unwrap();
    let recovery_head = jobs::load_candidate(&mut db, &recovery_task, 1).unwrap();
    let recovery_launch =
        prepare_launch(&settings, &recovery_head, &recovery_lease, t0 + 131).unwrap();
    assert!(!recovery_launch.machine.is_empty());
    worker::save_receipt(
        &db,
        &recovery_launch.job_id,
        &recovery_launch.lease,
        "candidate_launch",
        &serde_json::to_vec(&recovery_launch).unwrap(),
    )
    .unwrap();
    let mut unavailable = UnavailableCandidateUnit;
    let recovery =
        reconcile_candidate_attempt(&settings, &mut db, &recovery_head, &mut unavailable, || {
            t0 + 140
        });
    assert_eq!(
        recovery.unwrap_err().to_string(),
        "candidate_recovery_required"
    );
    assert_eq!(
        jobs::load_candidate(&mut db, &recovery_task, 1)
            .unwrap()
            .status,
        jobs::TaskState::Running
    );
}

fn candidate_execution_recovery_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-execution-recovery");
    fs::create_dir_all(&dir).unwrap();
    let settings = candidate_execution_settings(&dir, base_settings, 1_048_576);
    if !candidate_sandbox_ready(&settings) {
        assert!(
            std::env::var_os("NIVRA_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!(
            "skipping candidate execution recovery contract: python3 or bwrap unavailable on host"
        );
        return;
    }
    let (c, cfg) = candidate_config(&dir, base_policy, settings.clone());
    candidate_execution_book(&c);
    drop(open_queue_for_owner(&settings, &c.directory.join("model"), true).unwrap());
    let mut db = candidate_queue(&c, &cfg);
    let t0 = candidate_at(9, 23);
    let hypothesis = "AAA should rank above BBB on frozen current-session features";
    let spec = candidate_execution_spec(hypothesis, 10, 1, 1, "5000", "1", vec![]);
    let submit = |db: &mut Connection, key: &str, at: i64| -> jobs::CandidateTaskRecord {
        let mut args = candidate_submit_args(key, CANDIDATE_SOURCE, None);
        args.horizon_minutes = 1;
        args.execution = Some(spec.clone());
        let submitted = candidate_submit(
            &c,
            &cfg,
            "grok-4.7",
            3,
            "turn-exec-recovery",
            args,
            at,
            Some(candidate_evidence()),
        )
        .unwrap();
        assert_eq!(submitted["status"], "queued", "{submitted}");
        let task_id = submitted["task"]["task_id"].as_str().unwrap();
        jobs::load_candidate(db, task_id, 1).unwrap()
    };
    // Produce the exact durable state a crash leaves after the helper wrote its
    // result and the controller recorded the stop proof, but before the
    // completion transaction. The source is never run again during recovery.
    let prepare = |db: &mut Connection,
                   record: &jobs::CandidateTaskRecord,
                   at: i64|
     -> (worker::CandidateLaunch, CandidateResultV1, String, Vec<u8>) {
        let lease = digest(format!("exec-recovery-lease:{}", record.spec.task_id).as_bytes());
        jobs::claim(db, &record.spec.job_id, &lease, at).unwrap();
        let record = jobs::load_candidate(db, &record.spec.task_id, 1).unwrap();
        let launch = prepare_launch(&settings, &record, &lease, at).unwrap();
        worker::save_receipt(
            db,
            &launch.job_id,
            &launch.lease,
            "candidate_launch",
            &serde_json::to_vec(&launch).unwrap(),
        )
        .unwrap();
        let source =
            jobs::read_candidate_artifact(db, &record.spec.source_sha256, "source", 262_144)
                .unwrap();
        let params =
            jobs::read_candidate_artifact(db, &record.spec.params_sha256, "params", 65_536)
                .unwrap();
        let dataset = jobs::read_candidate_artifact(
            db,
            &record.spec.dataset_hash,
            "dataset",
            4 * 1024 * 1024,
        )
        .unwrap();
        let binding = record.spec.execution.as_ref().unwrap();
        let spec_bytes =
            jobs::read_candidate_artifact(db, &binding.spec_sha256, "execution_spec", 65_536)
                .unwrap();
        let manifest = jobs::read_candidate_artifact(
            db,
            &binding.manifest_sha256,
            "execution_manifest",
            512 * 1024,
        )
        .unwrap();
        let events = jobs::read_candidate_artifact(
            db,
            &binding.events_sha256,
            "execution_events",
            16 * 1024 * 1024,
        )
        .unwrap();
        stage_candidate(
            &settings,
            settings.candidate_compute.as_ref().unwrap(),
            &launch,
            &source,
            &params,
            &dataset,
            Some((&spec_bytes, &manifest, &events)),
        )
        .unwrap();
        let output = run_helper_at(&launch, || at + 1, false).unwrap();
        write_helper_output(
            Path::new(&launch.stage_dir),
            &output,
            launch.unit_policy.max_result_bytes,
        )
        .unwrap();
        let proof = candidate_fake_proof(&launch, at + 1);
        let envelope = candidate_result_envelope(&launch, &record, output, proof.clone()).unwrap();
        let bytes = serde_json::to_vec(&envelope).unwrap();
        let hash = jobs::put_candidate_artifact_bounded(
            db,
            "result",
            &bytes,
            record.spec.unit_policy.max_result_bytes as usize,
            at + 1,
        )
        .unwrap();
        let stopped = candidate_stopped(
            &launch,
            proof,
            jobs::CandidateStopCause::Succeeded,
            CandidateStopOutcome::Succeeded,
            None,
            Some(hash.clone()),
            Some(envelope.result_produced_at_ms),
            Some(bytes.len() as u64),
            at + 1,
        );
        write_stopped(db, &launch, &stopped).unwrap();
        (launch, envelope, hash, bytes)
    };
    let reconcile = |db: &mut Connection, record: &jobs::CandidateTaskRecord, at: i64| -> Value {
        // This runner cannot start, poll or terminate anything. A successful
        // reconcile therefore proves recovery used only the durable proof and
        // never re-ran source or touched a different unit.
        let mut unit = UnavailableCandidateUnit;
        reconcile_candidate_attempt(&settings, db, record, &mut unit, || at).unwrap()
    };

    // (1) Valid durable stop + result completes exactly once and repeats as a
    // terminal no-op.
    let record = submit(&mut db, "exec-recovery-valid", t0 + 1);
    let (launch, envelope, result_hash, result_bytes) = prepare(&mut db, &record, t0 + 2);
    assert_eq!(envelope.result_kind, "executable_replay");
    let record = jobs::load_candidate(&mut db, &record.spec.task_id, 1).unwrap();
    let reconciled = reconcile(&mut db, &record, t0 + 3);
    assert_eq!(reconciled["outcome"], "succeeded");
    assert_eq!(
        jobs::load_candidate(&mut db, &record.spec.task_id, 1)
            .unwrap()
            .status,
        jobs::TaskState::Succeeded
    );
    assert!(read_verified_candidate_result(&mut db, &record.spec.task_id, 1).is_ok());
    let terminal = jobs::load_candidate(&mut db, &record.spec.task_id, 1).unwrap();
    let again = reconcile(&mut db, &terminal, t0 + 4);
    assert_eq!(again["status"], "terminal");
    assert_eq!(
        jobs::load_candidate(&mut db, &record.spec.task_id, 1)
            .unwrap()
            .status,
        jobs::TaskState::Succeeded
    );
    let cap = record.spec.unit_policy.max_result_bytes as usize;
    assert_eq!(
        jobs::read_candidate_artifact(&db, &result_hash, "result", cap).unwrap(),
        result_bytes
    );
    assert_eq!(launch.lease, terminal.job.lease.clone().unwrap());

    // (2) A tampered stored execution spec is refused during recovery; the task
    // becomes a trusted terminal failure and the original result artifact is
    // retained for audit.
    let record = submit(&mut db, "exec-recovery-spec-tamper", t0 + 10);
    let (_launch, _envelope, result_hash, result_bytes) = prepare(&mut db, &record, t0 + 11);
    let record = jobs::load_candidate(&mut db, &record.spec.task_id, 1).unwrap();
    let binding = record.spec.execution.as_ref().unwrap();
    let original_spec =
        jobs::read_candidate_artifact(&db, &binding.spec_sha256, "execution_spec", 65_536).unwrap();
    let tampered_spec = b"{\"version\":1}";
    db.execute(
        "UPDATE candidate_artifacts SET body=? WHERE hash=? AND kind='execution_spec'",
        rusqlite::params![tampered_spec.as_slice(), binding.spec_sha256],
    )
    .unwrap();
    let reconciled = reconcile(&mut db, &record, t0 + 12);
    assert_eq!(reconciled["outcome"], "terminated");
    assert_eq!(
        jobs::load_candidate(&mut db, &record.spec.task_id, 1)
            .unwrap()
            .status,
        jobs::TaskState::Terminated
    );
    assert_eq!(
        jobs::read_candidate_artifact(&db, &binding.spec_sha256, "execution_spec", 65_536).unwrap(),
        tampered_spec
    );
    assert_eq!(
        jobs::read_candidate_artifact(&db, &result_hash, "result", cap).unwrap(),
        result_bytes
    );
    assert!(
        read_verified_candidate_result(&mut db, &record.spec.task_id, 1)
            .unwrap()
            .is_none()
    );
    db.execute(
        "UPDATE candidate_artifacts SET body=? WHERE hash=? AND kind='execution_spec'",
        rusqlite::params![original_spec.as_slice(), binding.spec_sha256],
    )
    .unwrap();

    // (3) A tampered stored execution manifest (the binding artifact) is also
    // refused, never reconciled to Succeeded.
    let record = submit(&mut db, "exec-recovery-manifest-tamper", t0 + 20);
    let (_launch, _envelope, result_hash, result_bytes) = prepare(&mut db, &record, t0 + 21);
    let record = jobs::load_candidate(&mut db, &record.spec.task_id, 1).unwrap();
    let binding = record.spec.execution.as_ref().unwrap();
    let original_manifest = jobs::read_candidate_artifact(
        &db,
        &binding.manifest_sha256,
        "execution_manifest",
        512 * 1024,
    )
    .unwrap();
    let tampered_manifest = b"{\"schema\":1}";
    db.execute(
        "UPDATE candidate_artifacts SET body=? WHERE hash=? AND kind='execution_manifest'",
        rusqlite::params![tampered_manifest.as_slice(), binding.manifest_sha256],
    )
    .unwrap();
    let reconciled = reconcile(&mut db, &record, t0 + 22);
    assert_eq!(reconciled["outcome"], "terminated");
    assert_eq!(
        jobs::load_candidate(&mut db, &record.spec.task_id, 1)
            .unwrap()
            .status,
        jobs::TaskState::Terminated
    );
    assert_eq!(
        jobs::read_candidate_artifact(
            &db,
            &binding.manifest_sha256,
            "execution_manifest",
            512 * 1024
        )
        .unwrap(),
        tampered_manifest
    );
    assert_eq!(
        jobs::read_candidate_artifact(&db, &result_hash, "result", cap).unwrap(),
        result_bytes
    );
    db.execute(
        "UPDATE candidate_artifacts SET body=? WHERE hash=? AND kind='execution_manifest'",
        rusqlite::params![original_manifest.as_slice(), binding.manifest_sha256],
    )
    .unwrap();

    // (4) A tampered stored result/report payload is refused by the recorded
    // hash, and the durable stop receipt is retained.
    let record = submit(&mut db, "exec-recovery-result-tamper", t0 + 30);
    let (launch, _envelope, result_hash, result_bytes) = prepare(&mut db, &record, t0 + 31);
    let record = jobs::load_candidate(&mut db, &record.spec.task_id, 1).unwrap();
    let tampered_result = b"{\"version\":2}";
    db.execute(
        "UPDATE candidate_artifacts SET body=? WHERE hash=? AND kind='result'",
        rusqlite::params![tampered_result.as_slice(), result_hash],
    )
    .unwrap();
    let reconciled = reconcile(&mut db, &record, t0 + 32);
    assert_eq!(reconciled["outcome"], "terminated");
    assert_eq!(
        jobs::load_candidate(&mut db, &record.spec.task_id, 1)
            .unwrap()
            .status,
        jobs::TaskState::Terminated
    );
    assert_eq!(
        jobs::read_candidate_artifact(&db, &result_hash, "result", cap).unwrap(),
        tampered_result
    );
    // The durable stop receipt and its recorded result binding are retained.
    let stopped_bytes = worker::read_receipt(
        &db,
        &launch.job_id,
        &launch.lease,
        "candidate_stopped",
        65_536,
    )
    .unwrap()
    .unwrap();
    let stopped: CandidateStopped = serde_json::from_slice(&stopped_bytes).unwrap();
    assert_eq!(stopped.result_sha256.as_deref(), Some(result_hash.as_str()));
    assert!(!result_bytes.is_empty());
}

// #231: the contracts below used to run as one chain inside
// `registered_file_failures_name_the_exact_artifact`, so the first failed
// assert hid every later scenario. Each group is its own test, every case gets
// a fresh fixture, and a failure names each scenario that broke.
type Contract = fn(&Path, &ResearchSettings, &PlannerConfig);

fn run_contracts(cases: &[(&str, Contract)], sandboxed: bool) {
    let mut failed = Vec::new();
    for (name, case) in cases {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("nivra-analysis");
        fs::write(&runtime, b"nivra-analysis-fixture").unwrap();
        let current = settings(root.path(), &runtime);
        if sandboxed && !candidate_sandbox_ready(&candidate_settings(root.path(), &current)) {
            assert!(
                std::env::var_os("NIVRA_REQUIRE_SCORER_SANDBOX").is_none(),
                "candidate sandbox required but python3 or bubblewrap is unavailable"
            );
            eprintln!("skipping {name}: python3 or bwrap unavailable on host");
            continue;
        }
        let owner_dir = tempfile::tempdir().unwrap();
        let (_owner_config, owner_policy) = owner_fixture(owner_dir.path(), &current);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            case(root.path(), &current, &owner_policy)
        }));
        if outcome.is_err() {
            failed.push(*name);
        }
    }
    assert!(failed.is_empty(), "failed research contracts: {failed:?}");
}

#[test]
fn candidate_capacity_fairness_and_failure_contracts() {
    run_contracts(
        &[
            ("capacity", candidate_capacity_contract),
            ("fairness", candidate_fairness_contract),
            ("future", candidate_future_contract),
            ("missing_quote", candidate_missing_quote_contract),
            ("failed_report", candidate_failed_report_contract),
        ],
        true,
    );
}

#[test]
fn candidate_recovery_and_execution_contracts() {
    run_contracts(
        &[
            ("recovery", candidate_recovery_contract),
            ("execution", candidate_execution_contract),
            ("execution_recovery", candidate_execution_recovery_contract),
        ],
        true,
    );
}

#[test]
fn observation_carry_and_evidence_contracts() {
    run_contracts(
        &[
            ("carry", case_carry_contract),
            ("e2_bounds", case_e2_bounds_contract),
        ],
        true,
    );
}

#[test]
fn continuation_and_corrective_contracts() {
    run_contracts(&[], true);
}
