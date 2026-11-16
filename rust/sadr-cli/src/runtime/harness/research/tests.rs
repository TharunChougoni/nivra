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
        runtime_sha256: digest(b"sadr-analysis-fixture"),
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

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

fn write_test_carry(source: &Config, successor: &Config) {
    let source_db = super::super::super::open_book(source).unwrap();
    let mut portfolio = sadr_core::execution::Portfolio::with_capital(
        sadr_core::finance::Amount::parse("5000").unwrap(),
    );
    portfolio.costs = source.paper_costs.clone();
    portfolio.model_led = true;
    portfolio.closed = true;
    let body = serde_json::to_string(&portfolio).unwrap();
    source_db
        .execute(
            "INSERT INTO state VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
            rusqlite::params![super::super::super::book_binding(source).unwrap(), body],
        )
        .unwrap();
    drop(source_db);
    close_test_book_sidecars(source);
    drop(super::super::super::open_book(successor).unwrap());
    let mut carried = portfolio.clone();
    carried.closed = false;
    carried.stopped = false;
    carried.halted = false;
    carried.health = "carried_waiting_for_fresh_quotes".into();
    let carry = json!({
        "source_session":source.session.to_string(),
        "source_previous_session":source.previous_session.to_string(),
        "source_binding":super::super::super::book_binding(source).unwrap(),
        "source_state_hash":digest(body.as_bytes()),
        "destination_binding":super::super::super::book_binding(successor).unwrap(),
        "portfolio":carried,
        "halted_carry":null
    });
    std::fs::write(
        successor.directory.join("carry-over.json"),
        serde_json::to_vec(&carry).unwrap(),
    )
    .unwrap();
    std::fs::write(
        successor
            .sessions_root
            .as_ref()
            .unwrap()
            .join("current.json"),
        serde_json::to_vec(
            &json!({"account":successor.account,"session":successor.session.to_string()}),
        )
        .unwrap(),
    )
    .unwrap();
}

/// A cleanly closed predecessor book has no sidecar. The test fixtures open
/// the source book while authoring; remove a lingering zero-WAL `-shm` so the
/// immutable reader sees the same clean state a real rollover produces. A
/// non-empty WAL is left in place (and must be refused).
fn close_test_book_sidecars(c: &Config) {
    let wal = c.directory.join("paper.sqlite-wal");
    let non_empty_wal = fs::symlink_metadata(&wal)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);
    if !non_empty_wal {
        let _ = fs::remove_file(c.directory.join("paper.sqlite-shm"));
    }
}

fn ensure_test_state(c: &Config) {
    let db = super::super::super::open_book(c).unwrap();
    let exists: i64 = db
        .query_row("SELECT count(*) FROM state", [], |row| row.get(0))
        .unwrap();
    if exists == 0 {
        let portfolio = sadr_core::execution::Portfolio::with_capital(
            sadr_core::finance::Amount::parse("5000").unwrap(),
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

/// Recursive file manifest for the read-only predecessor assertion. It records
/// only relative paths and content hashes; no secret or account payload is read.
fn directory_manifest(directory: &Path) -> std::collections::BTreeMap<String, String> {
    fn walk(root: &Path, directory: &Path, out: &mut std::collections::BTreeMap<String, String>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                walk(root, &path, out);
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                let bytes = fs::read(&path).unwrap_or_default();
                out.insert(relative, digest(&bytes));
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(directory, directory, &mut out);
    out
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
        sandbox: sadr_sandbox::ScorerSandboxConfig {
            wall_ms: 2_000,
            ..Default::default()
        },
        replay: sadr_cli::scorer_compare::ScorerReplaySettings {
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

struct NoRunner;
impl worker::SandboxRunner for NoRunner {
    fn start(
        &mut self,
        _: &worker::Launch,
        _: &RegisteredRun,
    ) -> std::result::Result<(), worker::RunnerError> {
        panic!("candidate job must not start the registered runner")
    }
    fn poll(
        &mut self,
        _: &worker::ExecutionIdentity,
    ) -> std::result::Result<worker::SandboxPoll, worker::RunnerError> {
        Err(worker::RunnerError::Unsupported)
    }
    fn terminate(
        &mut self,
        _: &worker::ExecutionIdentity,
    ) -> std::result::Result<worker::StoppedReceipt, worker::RunnerError> {
        Err(worker::RunnerError::Unsupported)
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
) -> Box<sadr_cli::candidate_execution::CandidateExecutionSpecV1> {
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
        max_capital: sadr_core::finance::Amount::parse("10000").unwrap(),
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
        max_capital: sadr_core::finance::Amount::parse("10000").unwrap(),
        max_units: 100,
        max_holding_ms: 3_600_000,
        max_decision_delay_ms: 5_000,
    });
    settings
}

fn candidate_observation_book(c: &Config) {
    candidate_book(c);
    let db = super::super::super::open_book(c).unwrap();
    let body = |at: i64, bid: f64, ask: f64| {
        json!({
            "status":"success",
            "data":{
                "NSE_EQ:INE000000001":{"instrument_token":"NSE_EQ|INE000000001","symbol":"AAA","timestamp":at,
                    "depth":{"buy":[{"price":bid,"quantity":100}],"sell":[{"price":ask,"quantity":100}]},
                    "lower_circuit_limit":80,"upper_circuit_limit":120,"volume":1_000_000},
                "NSE_EQ:INF000000002":{"instrument_token":"NSE_EQ|INF000000002","symbol":"BBB","timestamp":at,
                    "depth":{"buy":[{"price":200,"quantity":100}],"sell":[{"price":200.05,"quantity":100}]},
                    "lower_circuit_limit":160,"upper_circuit_limit":240,"volume":1_000_000}
            }
        })
    };
    for (at, purpose, bid, ask) in [
        (
            candidate_at(9, 23) + 500,
            "research_observation",
            99.0,
            100.05,
        ),
        (
            candidate_at(9, 24) + 30_000,
            "execution_or_protection",
            110.0,
            110.05,
        ),
        (
            candidate_at(9, 26),
            "execution_or_protection",
            120.0,
            120.05,
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
        capital: sadr_core::finance::Amount::parse("5000").unwrap(),
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

fn e8_store_measurement(directory: &Path, id: &str, value: &Value) {
    let db = rusqlite::Connection::open(directory.join("model").join("experiment-trials.sqlite"))
        .unwrap();
    let projection: String = db
        .query_row(
            "SELECT projection FROM experiment_lifecycle_state WHERE candidate_id=?",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    let mut projection: Value = serde_json::from_str(&projection).unwrap();
    projection["observation_measurement"] = value.clone();
    db.execute(
        "UPDATE experiment_lifecycle_state SET projection=? WHERE candidate_id=?",
        rusqlite::params![projection.to_string(), id],
    )
    .unwrap();
}

fn e8_current_body_from_evaluation(
    base: &Value,
    evaluation: &super::ObservationExitEvaluation,
) -> Value {
    let executed = evaluation.exit_status == "executed";
    let (pnl, fees, turnover) = match (&evaluation.execution, executed) {
        (Some(summary), true) if summary.flat && summary.accounting_complete => (
            summary.net_pnl_if_flat,
            Some(summary.fees),
            Some(summary.turnover),
        ),
        (Some(summary), _) => (None, Some(summary.fees), Some(summary.turnover)),
        _ => (None, None, None),
    };
    let mut body = base.clone();
    body["exit_quote"] = serde_json::to_value(&evaluation.exit_quote).unwrap();
    body["exit_status"] = json!(evaluation.exit_status);
    body["exit_gap"] = serde_json::to_value(&evaluation.exit_gap).unwrap();
    body["first_receipt_unknown"] = json!(evaluation.first_receipt_unknown);
    body["execution"] = serde_json::to_value(&evaluation.execution).unwrap();
    body["net_pnl_if_flat"] = serde_json::to_value(pnl).unwrap();
    body["fees"] = serde_json::to_value(fees).unwrap();
    body["turnover"] = serde_json::to_value(turnover).unwrap();
    body["flat"] = json!(
        evaluation
            .execution
            .as_ref()
            .is_some_and(|summary| summary.flat)
    );
    body["accounting_complete"] = json!(
        evaluation
            .execution
            .as_ref()
            .is_some_and(|summary| summary.accounting_complete)
    );
    body["report_sha256"] = serde_json::to_value(&evaluation.report_sha256).unwrap();
    body["gaps"] = json!(evaluation.gaps);
    body
}

fn case_observation_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-observation-contract");
    fs::create_dir_all(&dir).unwrap();
    let t0 = candidate_at(9, 23);
    let settings = candidate_observation_settings(&dir, base_settings);
    if !candidate_sandbox_ready(&settings) {
        assert!(
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!("skipping candidate observation contract: python3 or bwrap unavailable");
        return;
    }
    let (c, cfg) = candidate_config(&dir, base_policy, settings.clone());
    candidate_observation_book(&c);
    // E8: a SECOND protection receipt inside the TRUE exit window, after the
    // first eligible exit. A body that forges a later due must not be allowed
    // to select this later receipt.
    {
        let db = super::super::super::open_book(&c).unwrap();
        let at = candidate_at(9, 24) + 50_000;
        let body = json!({
            "status":"success",
            "data":{
                "NSE_EQ:INE000000001":{"instrument_token":"NSE_EQ|INE000000001","symbol":"AAA","timestamp":at,
                    "depth":{"buy":[{"price":115.0,"quantity":100}],"sell":[{"price":115.05,"quantity":100}]},
                    "lower_circuit_limit":80,"upper_circuit_limit":120,"volume":1_000_000}
            }
        });
        let archive = json!({
            "source":"upstox-readonly-v1",
            "purpose":"execution_or_protection",
            "requested_ms":at,
            "received_ms":at,
            "body":body.to_string()
        });
        db.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?2,?3,CASE WHEN instr(?3,'\"purpose\":\"research_observation\"')>0 THEN 'research_quotes' ELSE 'wide_quotes' END)",
            rusqlite::params![at, at, archive.to_string()],
        )
        .unwrap();
    }
    // A verified original authoring candidate is the only eligible source.
    let mut original_args = candidate_submit_args("candidate-obs-source", CANDIDATE_SOURCE, None);
    original_args.horizon_minutes = 1;
    let original = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        original_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(original["status"], "queued");
    let source_task = original["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &source_task, move || t0 + 10);
    let source_record = candidate_record(&c, &cfg, &source_task);
    assert_eq!(source_record.status, jobs::TaskState::Succeeded);
    let mut args = candidate_submit_args("candidate-obs", CANDIDATE_SOURCE, None);
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
        "turn-candidate",
        args,
        t0 + 1_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(submitted["status"], "queued", "{submitted}");
    assert_eq!(submitted["purpose"], "observe");
    assert_eq!(
        submitted["observation_binding"]["quote_authority"],
        "research_observation_virtual_only"
    );
    assert_eq!(
        submitted["observation_binding"]["original_strategy_measurement"],
        false
    );
    let task_id = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    let record = candidate_record(&c, &cfg, &task_id);
    assert!(record.spec.purpose.is_some());
    assert_eq!(record.spec.execution, None);
    let jobs::CandidatePurposeV1::Observe { target } = record.spec.purpose.as_ref().unwrap();
    let target = target.as_ref().clone();
    assert_eq!(target.authoring_at_ms, source_record.spec.created_at_ms);
    assert_eq!(target.target_frozen_at_ms, t0 + 1_000);
    assert_ne!(target.authoring_at_ms, target.target_frozen_at_ms);
    // The frozen artifacts are real code/params/features/quotes/spec, not prose.
    let db = candidate_queue(&c, &cfg);
    for (hash, kind, cap) in [
        (&target.snapshot_sha256, "dataset", 4 * 1024 * 1024),
        (&target.spec_sha256, "observation_spec", 65_536),
        (&target.quotes_sha256, "observation_quotes", 2_000_000),
    ] {
        let bytes = jobs::read_candidate_artifact(&db, hash, kind, cap).unwrap();
        assert_eq!(digest(&bytes), *hash);
    }
    let bundle: Value = serde_json::from_slice(
        &jobs::read_candidate_artifact(&db, &target.quotes_sha256, "observation_quotes", 2_000_000)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(bundle["status"], "ok");
    assert!(
        bundle["events"]
            .as_str()
            .unwrap()
            .contains("virtual_observation")
    );
    assert!(!bundle["events"].as_str().unwrap().contains("screen"));
    drop(db);
    // The helper clock is the real post-scorer completion, not a replay time.
    candidate_drive(&c, &cfg, &task_id, move || t0 + 2_000);
    let result = {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &task_id, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
        read_verified_candidate_result(&mut db, &task_id, 1)
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        result.result_kind,
        sadr_cli::candidate_execution::RESULT_KIND_OBSERVATION
    );
    assert_eq!(
        result.evidence_scope,
        sadr_cli::candidate_execution::OBSERVATION_SCOPE
    );
    assert!(!result.qualified);
    let score = result.observation.as_ref().unwrap();
    assert_eq!(score.status, "scored_entry");
    assert_eq!(score.key.as_deref(), Some("NSE_EQ|INE000000001"));
    assert_eq!(score.decision_at_ms, t0 + 2_000);
    assert_eq!(score.score_produced_at_ms, t0 + 2_000);
    assert_eq!(score.authoring_at_ms, target.authoring_at_ms);
    assert_eq!(score.target_frozen_at_ms, target.target_frozen_at_ms);
    let entry = score.entry.as_ref().unwrap();
    assert!(entry.virtual_observation);
    assert_eq!(entry.quote.purpose, "research_observation");
    assert_eq!(entry.quote.source_ms, t0 + 500);
    assert!(entry.quote.source_age_ms <= 2_000);
    assert!(entry.quote.receipt_age_ms <= 2_000);
    // The same virtual tape is refused by the default paper authority, the
    // Task 8 execution adapter and the historical runner.
    let spec: sadr_cli::candidate_execution::ObservationExecutionSpecV1 = serde_json::from_slice(
        &jobs::read_candidate_artifact(
            &db_dummy(&c, &cfg),
            &target.spec_sha256,
            "observation_spec",
            65_536,
        )
        .unwrap(),
    )
    .unwrap();
    let manifest = bundle["manifest"].as_str().unwrap().as_bytes();
    let events = bundle["events"].as_str().unwrap().as_bytes();
    assert!(
        sadr_cli::strategy_actions::evaluate(
            &sadr_cli::candidate_execution::artifact_bytes(std::slice::from_ref(
                &sadr_cli::strategy_actions::Entry {
                    at_ms: entry.decision_at_ms,
                    key: entry.key.clone(),
                    units: entry.units,
                    max_price: entry.max_price,
                }
            ),)
            .unwrap(),
            &sadr_cli::candidate_execution::artifact_bytes(&spec.parameters).unwrap(),
            manifest,
            events,
        )
        .is_err()
    );
    assert!(
        sadr_cli::candidate_execution::execute(
            &[sadr_cli::candidate_execution::ScoredSignal {
                at_ms: entry.decision_at_ms,
                ordered_keys: vec![entry.key.clone()],
            }],
            &serde_json::from_value(json!({
                "version":1,"segment":"cash",
                "parameters":spec.parameters,
                "units":10,"max_entries":1,"selection_top_n":1,
                "entry_price_offset_ticks":0,"entry_at":"signal","exit":"time",
                "first_signal_per_key":true,"cost_stress":[]
            }))
            .unwrap(),
            manifest,
            events,
        )
        .is_err()
    );
    let historical_manifest = json!({
        "session":"2026-10-16",
        "warmup_start":"2026-10-15",
        "paper_costs":null,
        "policy":sadr_core::execution::POLICY,
        "split":"development",
        "receipt_clock":"virtual_historical",
        "bindings":{
            "engine":sadr_core::replay::engine_fingerprint(),
            "strategy":digest(b"strategy"),
            "data":digest(events),
            "calendar":digest(b"calendar"),
            "fees":digest(b"fees"),
            "slippage":digest(b"slippage")
        },
        "seed":1,
        "universe":[
            {"key":"NSE_EQ|INE000000001","symbol":"AAA","series":"EQ","tick":"0.05"},
            {"key":"NSE_EQ|INF000000002","symbol":"BBB","series":"EQ","tick":"0.05"}
        ]
    });
    let mut runner =
        sadr_core::historical::Runner::new(serde_json::from_value(historical_manifest).unwrap())
            .unwrap();
    let event: sadr_core::historical::Event =
        serde_json::from_slice(events.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert!(runner.apply(event).is_err());
    // Maturation uses the FIRST original protection receipt in the window.
    let mut db = candidate_queue(&c, &cfg);
    let maturation =
        match mature_observation(&c, &cfg, &mut db, &record, &result, t0 + 120_000).unwrap() {
            MaturationDecision::Matured(value) => value,
            other => panic!("expected matured observation: {other:?}"),
        };
    assert_eq!(maturation["exit_status"], "executed");
    assert_eq!(maturation["flat"], true);
    assert_eq!(maturation["accounting_complete"], true);
    assert!(maturation["net_pnl_if_flat"].is_string());
    assert_eq!(maturation["exit_quote"]["purpose"], "protection");
    assert_eq!(
        maturation["exit_quote"]["source_ms"],
        candidate_at(9, 24) + 30_000
    );
    assert_eq!(maturation["measurement_sha256"].as_str().unwrap().len(), 64);
    // A second maturation is idempotent at the lifecycle layer.
    let again = match mature_observation(&c, &cfg, &mut db, &record, &result, t0 + 120_000).unwrap()
    {
        MaturationDecision::Matured(value) => value,
        _ => panic!("expected matured observation"),
    };
    assert_eq!(
        again["measurement_sha256"],
        maturation["measurement_sha256"]
    );
    drop(db);
    // The measurement is an independent episode, never a portfolio sum.
    let measurement = super::measure::measure_observation(
        &c,
        &cfg,
        &record,
        &result,
        Some(&maturation),
        None,
        t0 + 120_000,
    );
    assert_eq!(measurement["evaluation_scope"], "independent_episodes");
    assert_eq!(measurement["strategy_portfolio_claimed"], false);
    assert!(measurement["portfolio_return"].is_null());
    assert!(measurement["aggregate_net_pnl"].is_null());
    assert_eq!(measurement["episode_count"], 1);
    assert_eq!(measurement["qualification"]["qualified"], false);
    assert_eq!(measurement["qualification"]["promotion_authorized"], false);
    assert_eq!(measurement["trials"]["multiple_testing"], "unadjusted");
    assert_eq!(measurement["trials"]["family_key_is_label_only"], true);
    assert!(
        measurement["trials"]["global_tested_trial_count"]
            .as_u64()
            .unwrap()
            >= 1
    );
    // A stale (>2 s) quote still produces a valid dynamic score but no entry.
    let mut stale_args = candidate_submit_args("candidate-obs-stale", CANDIDATE_SOURCE, None);
    stale_args.observation = Some(Box::new(candidate_observation_args(
        &source_task,
        candidate_at(9, 21),
        candidate_at(9, 22),
    )));
    let stale = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        stale_args,
        t0 + 1_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(stale["status"], "queued", "{stale}");
    let stale_task = stale["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &stale_task, move || t0 + 2_600);
    let stale_result = {
        let mut db = candidate_queue(&c, &cfg);
        read_verified_candidate_result(&mut db, &stale_task, 1)
            .unwrap()
            .unwrap()
    };
    let stale_score = stale_result.observation.as_ref().unwrap();
    assert_eq!(stale_score.status, "scored_missed_freshness");
    assert!(stale_score.entry.is_none());
    assert!(stale_score.quote.is_some());
    assert!(stale_score.score_sha256.len() == 64);
    assert!(!stale_score.gaps.is_empty());
    assert_eq!(stale_result.result_kind, "forward_observation_score");
    assert!(!stale_result.qualified);
    // The controller capture path is distinct and writes only a research tick.
    let capture_settings = crate::runtime::owner_tasks::ControllerObservationCaptureSettings {
        enabled: true,
        max_keys: 8,
        max_calls_per_session: 8,
        min_interval_ms: 1_000,
        timeout_ms: 5_000,
        max_rows: 64,
    };
    let mut capture_cfg = cfg.clone();
    capture_cfg.owner_tasks = Some(crate::runtime::owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: crate::runtime::owner_tasks::ReviewSettings::default(),
        research: Some(crate::runtime::owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: Some(capture_settings),
            lane: Some(Default::default()),
        }),
    });
    let mut capture_db = super::super::super::open_book(&c).unwrap();
    let state_rows_before: i64 = capture_db
        .query_row("SELECT count(*) FROM state", [], |row| row.get(0))
        .unwrap();
    let portfolio = sadr_core::execution::Portfolio::with_capital(
        sadr_core::finance::Amount::parse("5000").unwrap(),
    );
    let disabled = super::capture::tick_with(
        &mut capture_db,
        &c,
        &cfg,
        &portfolio,
        t0 + 3_000,
        || t0 + 3_000,
        |_| {
            Err(crate::runtime::data::BoundedQuoteError {
                code: "quote_keys".into(),
                request_attempted: false,
                provider_called: Some(false),
            })
        },
    );
    assert_eq!(disabled["status"], "disabled");
    let captured = super::capture::tick_with(
        &mut capture_db,
        &c,
        &capture_cfg,
        &portfolio,
        t0 + 4_000,
        || t0 + 4_000,
        |keys| {
            assert!(!keys.is_empty());
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: t0 + 4_000,
                receipt_ms: t0 + 4_000,
                body: candidate_quote_body(t0 + 4_000).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    assert_eq!(captured["status"], "captured", "{captured}");
    assert_eq!(captured["purpose"], "research_observation");
    assert_eq!(captured["state_written"], false);
    let state_rows_after: i64 = capture_db
        .query_row("SELECT count(*) FROM state", [], |row| row.get(0))
        .unwrap();
    assert_eq!(state_rows_before, state_rows_after);
    let research_rows: i64 = capture_db
        .query_row(
            "SELECT count(*) FROM ticks WHERE event='research_quotes'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(research_rows >= 2);

    // (E8) The PUBLIC current read must reject a stored body that chooses its
    // own later window/exit, a forged capital, numeric money, a fake executed
    // state, or an entry/scope status contradicted by the verified score.
    let mut failures: Vec<String> = Vec::new();
    macro_rules! check {
        ($cond:expr, $($arg:tt)*) => {
            if !($cond) {
                failures.push(format!($($arg)*));
            }
        };
    }
    let observation_id = record.spec.lifecycle_candidate_id.clone();
    let genuine = maturation.clone();
    let e8_now = t0 + 200_000;
    let forged_due = t0 + 95_000;
    let forged_deadline = t0 + 122_000;
    let later_tape = super::super::super::paper_replay::virtual_observation_tape_bounded(
        &c.directory,
        c.session,
        Some(c.previous_session),
        &BTreeSet::from([entry.key.clone()]),
        forged_due,
        forged_deadline,
        spec.parameters.capital,
        10_000,
        1_000,
        1_048_576,
        e8_now,
        super::super::super::paper_replay::TapeMode::ExitWindow,
    )
    .unwrap();
    let later_eval = match super::evaluate_observation_exit(score, &spec, &later_tape).unwrap() {
        super::ObservationExitOutcome::Evaluated(evaluation) => evaluation,
        _ => panic!("later fixture exit must evaluate"),
    };
    let mut later_window = e8_current_body_from_evaluation(&genuine, &later_eval);
    later_window["exit_due_ms"] = json!(forged_due);
    later_window["exit_deadline_ms"] = json!(forged_deadline);
    later_window["measurement_sha256"] =
        json!(super::measure::measurement_content_hash(&later_window).unwrap());
    e8_store_measurement(&c.directory, &observation_id, &later_window);
    if let Ok(response) = candidate_result(&c, &cfg, &task_id, 1, e8_now) {
        let later_refused = response["measurement"]["status"] == "unavailable"
            || response["measurement"]["episodes"][0]["after_cost"].is_null();
        check!(
            later_refused,
            "forged later current window/exit was consumed as PnL: {response}"
        );
    }
    e8_store_measurement(&c.directory, &observation_id, &genuine);

    for (label, mutate) in [
        (
            "capital",
            (|value: &mut Value| {
                value["capital"] = json!("999999");
            }) as fn(&mut Value),
        ),
        (
            "numeric-money",
            (|value: &mut Value| {
                value["execution"] = Value::Null;
                value["net_pnl_if_flat"] = json!(999.0);
                value["fees"] = json!(1.0);
                value["turnover"] = json!(1000.0);
                value["flat"] = json!(true);
                value["accounting_complete"] = json!(true);
                value["exit_status"] = json!("executed");
                value["report_sha256"] = Value::Null;
            }) as fn(&mut Value),
        ),
        (
            "all-null-executed",
            (|value: &mut Value| {
                value["execution"] = Value::Null;
                value["net_pnl_if_flat"] = Value::Null;
                value["fees"] = Value::Null;
                value["turnover"] = Value::Null;
                value["flat"] = json!(false);
                value["accounting_complete"] = json!(false);
                value["exit_status"] = json!("executed");
                value["report_sha256"] = Value::Null;
            }) as fn(&mut Value),
        ),
        (
            "entry-unavailable-with-entry",
            (|value: &mut Value| {
                value["exit_quote"] = Value::Null;
                value["exit_gap"] = Value::Null;
                value["first_receipt_unknown"] = json!(false);
                value["execution"] = Value::Null;
                value["net_pnl_if_flat"] = Value::Null;
                value["fees"] = Value::Null;
                value["turnover"] = Value::Null;
                value["flat"] = json!(false);
                value["accounting_complete"] = json!(false);
                value["report_sha256"] = Value::Null;
                value["exit_status"] = json!("entry_unavailable");
                value["gaps"] = json!([]);
            }) as fn(&mut Value),
        ),
        (
            "scope-unsupported-current",
            (|value: &mut Value| {
                value["exit_quote"] = Value::Null;
                value["exit_gap"] = json!({
                    "kind":"unsupported_session_scope",
                    "reason":"continuation_runtime_changed",
                    "source_runtime_hash":"a".repeat(64),
                    "recorded_runtime_hash":"b".repeat(64),
                    "source_cohort_key":"c".repeat(64),
                    "recorded_cohort_key":"d".repeat(64)
                });
                value["first_receipt_unknown"] = json!(false);
                value["execution"] = Value::Null;
                value["net_pnl_if_flat"] = Value::Null;
                value["fees"] = Value::Null;
                value["turnover"] = Value::Null;
                value["flat"] = json!(false);
                value["accounting_complete"] = json!(false);
                value["report_sha256"] = Value::Null;
                value["exit_status"] = json!("exit_scope_unsupported");
                value["gaps"] = json!([
                    "continuation_runtime_changed",
                    "no_fabricated_exit_price",
                    "sha_only_gap"
                ]);
            }) as fn(&mut Value),
        ),
    ] {
        let mut forged = genuine.clone();
        mutate(&mut forged);
        forged["measurement_sha256"] =
            json!(super::measure::measurement_content_hash(&forged).unwrap());
        e8_store_measurement(&c.directory, &observation_id, &forged);
        // An explicit read refusal is a valid rejection, not a crash.
        if let Ok(response) = candidate_result(&c, &cfg, &task_id, 1, e8_now) {
            let measurement = &response["measurement"];
            let refused = measurement["status"] == "unavailable"
                || measurement["episodes"][0]["after_cost"].is_null()
                    && measurement["episodes"][0]["exit_status"] != "executed"
                    && measurement["episodes"][0]["outcome_state"] != "evaluated";
            check!(
                refused,
                "forged current body {label} was published: {response}"
            );
        }
        // The operational status must never publish the raw forged body either.
        let status = observation_status(&c, &cfg, e8_now);
        if let Some(item) = status["lifecycle"]["milestones"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["candidate_id"] == observation_id.as_str())
            })
        {
            check!(
                item["measurement_status"] == "unavailable" || item["exit_status"] != "executed",
                "observation_status published forged current body {label}: {status}"
            );
        }
        e8_store_measurement(&c.directory, &observation_id, &genuine);
    }
    assert!(failures.is_empty(), "E8 current failures: {failures:#?}");
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
    // A verified predecessor ledger exists; the fold reads it without copying.
    experiment_ledger::lifecycle::admit_spec(
        &predecessor,
        experiment_ledger::lifecycle::ExperimentSpec {
            version: 1,
            parent_candidate_id: None,
            session: "2026-10-15".into(),
            source_sha256: digest(b"fold-source"),
            params_sha256: digest(b"fold-params"),
            dataset_hash: digest(b"fold-dataset"),
            feature_abi: "scorer_replay_v1".into(),
            hypothesis: "read-only predecessor fold fixture".into(),
            training_from_ms: 0,
            training_to_ms: 1,
            selection_from_ms: 2,
            selection_to_ms: 3,
            holding_ms: 60_000,
            max_exit_delay_ms: 2_000,
        },
        10,
    )
    .unwrap();
    let folded =
        experiment_ledger::lifecycle::fold_library(&dir, Some(&predecessor), "2026-10-16").unwrap();
    assert_eq!(folded["folded"], true);
    assert_eq!(folded["copied_rows"], false);
    assert_eq!(folded["items"].as_array().unwrap().len(), 1);
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

fn case_e2_contract(root: &Path, base_settings: &ResearchSettings, base_policy: &PlannerConfig) {
    let dir = root.join("candidate-e2-contract");
    fs::create_dir_all(&dir).unwrap();
    let t0 = candidate_at(9, 23);
    let settings = candidate_observation_settings(&dir, base_settings);
    if !candidate_sandbox_ready(&settings) {
        assert!(
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!("skipping candidate e2 contract: python3 or bwrap unavailable");
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
    let mut source_args = candidate_submit_args("candidate-e2-source", CANDIDATE_SOURCE, None);
    source_args.horizon_minutes = 1;
    let source = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e2",
        source_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    check!(source["status"] == "queued", format!("source: {source}"));
    let source_task = source["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &source_task, move || t0 + 10);
    let source_record = candidate_record(&c, &cfg, &source_task);
    check!(
        source_record.status == jobs::TaskState::Succeeded,
        "source succeeded"
    );
    // A host-built opaque parent ref is exposed by the read-only status tool.
    let status = candidate_status(&c, &cfg, &source_task, 1, t0 + 100).unwrap();
    check!(
        status["parent_ref"]["ref"].is_object(),
        format!("parent_ref exposed: {status}")
    );
    check!(
        status["parent_ref"]["account_payload"] == false && status["parent_ref"]["paths"] == false,
        "parent_ref opaque"
    );
    // A revised source/params continuation is admitted as a versioned
    // continuation, not rejected for not matching the parent algorithm.
    let mut continuation = candidate_submit_args("candidate-e2-cont", CANDIDATE_SOURCE_ALT, None);
    continuation.horizon_minutes = 1;
    continuation.params = json!({"revision":2});
    continuation.parent_ref = Some(Box::new(
        serde_json::from_value(status["parent_ref"]["ref"].clone()).unwrap(),
    ));
    let continued = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e2",
        continuation,
        t0 + 1_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    check!(
        continued["status"] == "queued",
        format!("parent continuation: {continued}")
    );
    if continued["status"] == "queued" {
        let continued_task = continued["task"]["task_id"].as_str().unwrap().to_owned();
        let continued_record = candidate_record(&c, &cfg, &continued_task);
        check!(
            continued_record.spec.parent_ref.is_some()
                && continued_record.spec.source_sha256 != source_record.spec.source_sha256
                && continued_record.spec.params_sha256 != source_record.spec.params_sha256,
            "continuation keeps parent provenance and new source/params"
        );
    }
    // A cancelled/failed admission is part of the global trial debt.
    let mut cancel_args = candidate_submit_args("candidate-e2-cancel", CANDIDATE_SOURCE, None);
    cancel_args.horizon_minutes = 1;
    let cancel = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e2",
        cancel_args,
        t0 + 2_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    check!(
        cancel["status"] == "queued",
        format!("cancel submit: {cancel}")
    );
    if cancel["status"] == "queued" {
        let cancel_task = cancel["task"]["task_id"].as_str().unwrap().to_owned();
        let _ = candidate_cancel(&c, &cfg, &cancel_task, 1, t0 + 3_000);
        let _ = candidate_status(&c, &cfg, &cancel_task, 1, t0 + 4_000);
    }
    let trials = super::carry::trial_fold(&c, &cfg);
    check!(
        trials["status"] == "available",
        format!("trial fold available: {trials}")
    );
    check!(
        trials["admitted_trial_count"].as_u64().unwrap_or(0) >= 3,
        format!("admitted debt: {trials}")
    );
    check!(
        trials["terminal_trial_count"].as_u64().unwrap_or(0) >= 1,
        format!("terminal debt: {trials}")
    );
    check!(
        trials["global_tested_trial_count"].as_u64().unwrap_or(0) >= 1,
        format!("tested debt: {trials}")
    );
    // Flat virtual book: only research-observation receipts exist. The entry
    // is virtual and the exit is the captured research receipt; no protection
    // receipt and no real position are inserted.
    let mut observation_args = candidate_submit_args("candidate-e2-obs", CANDIDATE_SOURCE, None);
    observation_args.observation = Some(Box::new(candidate_observation_args(
        &source_task,
        candidate_at(9, 21),
        candidate_at(9, 22),
    )));
    let submitted = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e2",
        observation_args,
        t0 + 1_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    check!(
        submitted["status"] == "queued",
        format!("observation: {submitted}")
    );
    let observation_task = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &observation_task, move || t0 + 2_000);
    {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &observation_task, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
    }
    let observation_record = candidate_record(&c, &cfg, &observation_task);
    let observation_result = {
        let mut db = candidate_queue(&c, &cfg);
        read_verified_candidate_result(&mut db, &observation_task, 1)
            .unwrap()
            .unwrap()
    };
    let observation_id = observation_record.spec.lifecycle_candidate_id.clone();
    check!(
        observation_result
            .observation
            .as_ref()
            .is_some_and(|score| score.entry.is_some()),
        "virtual entry scored"
    );
    // Before due: waiting, no terminal measurement or lifecycle row.
    let before = mature_candidates(&c, &cfg, t0 + 30_000).unwrap();
    let before_item = e2_item(&before, &observation_id);
    check!(
        before_item.is_some_and(|item| item["state"] == "waiting"),
        format!("before due waiting: {before}")
    );
    check!(
        !before_item.is_some_and(|item| item["observation"].is_object()),
        "before due has no measurement"
    );
    let library_before =
        experiment_ledger::lifecycle::library(&c.directory, &c.session.to_string()).unwrap();
    let lifecycle_before = library_before["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["candidate_id"] == observation_id)
        })
        .cloned()
        .unwrap_or(Value::Null);
    check!(
        lifecycle_before["observation_measurement"].is_null(),
        format!("before due lifecycle has no measurement: {lifecycle_before}")
    );
    // Capture the flat-book exit with the real controller capture path.
    let mut capture_cfg = cfg.clone();
    capture_cfg.owner_tasks = Some(crate::runtime::owner_tasks::OwnerTasksSettings {
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
                    timeout_ms: 5_000,
                    max_rows: 64,
                },
            ),
            lane: Some(Default::default()),
        }),
    });
    let mut capture_db = super::super::super::open_book(&c).unwrap();
    let portfolio = sadr_core::execution::Portfolio::with_capital(
        sadr_core::finance::Amount::parse("5000").unwrap(),
    );
    let captured = super::capture::tick_with(
        &mut capture_db,
        &c,
        &capture_cfg,
        &portfolio,
        t0 + 63_000,
        || t0 + 63_000,
        |_| {
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: t0 + 63_000,
                receipt_ms: t0 + 63_000,
                body: candidate_quote_body(t0 + 63_000).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    check!(
        captured["status"] == "captured",
        format!("capture: {captured}")
    );
    drop(capture_db);
    // Within grace: the captured research receipt is the FIRST eligible exit.
    let matured = mature_candidates(&c, &cfg, t0 + 65_000).unwrap();
    let matured_item = e2_item(&matured, &observation_id);
    check!(
        matured_item.is_some_and(|item| item["state"] == "evaluated"),
        format!("flatbook matured: {matured}")
    );
    let measurement = matured_item
        .map(|item| item["observation"].clone())
        .unwrap_or(Value::Null);
    check!(
        measurement["exit_status"] == "executed",
        format!("flatbook executed: {measurement}")
    );
    check!(
        measurement["exit_quote"]["purpose"] == "research_observation",
        format!("research exit purpose: {measurement}")
    );
    check!(
        measurement["exit_quote"]["archive_sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        format!("original archive sha: {measurement}")
    );
    check!(
        measurement["net_pnl_if_flat"].is_string(),
        format!("after-cost pnl: {measurement}")
    );
    let first_measurement_sha = measurement["measurement_sha256"].clone();
    // A later capture cannot replace the first eligible exit.
    let mut capture_db = super::super::super::open_book(&c).unwrap();
    let later = super::capture::tick_with(
        &mut capture_db,
        &c,
        &capture_cfg,
        &portfolio,
        t0 + 70_000,
        || t0 + 70_000,
        |_| {
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: t0 + 70_000,
                receipt_ms: t0 + 70_000,
                body: candidate_quote_body(t0 + 70_000).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    check!(
        later["status"] == "captured",
        format!("later capture: {later}")
    );
    drop(capture_db);
    let again = mature_candidates(&c, &cfg, t0 + 75_000).unwrap();
    let again_item = e2_item(&again, &observation_id);
    check!(
        again_item.is_none_or(|item| item["state"] != "evaluated")
            || again_item.is_some_and(
                |item| item["observation"]["measurement_sha256"] == first_measurement_sha
            ),
        format!("first exit wins: {again}")
    );
    // A second observation with no exit inside the grace window is pending,
    // then explicitly unknown after the deadline; never a fabricated zero.
    insert_research_tick(&c, t0 + 199_000);
    let mut pending_args = candidate_submit_args("candidate-e2-pending", CANDIDATE_SOURCE, None);
    pending_args.observation = Some(Box::new(candidate_observation_args(
        &source_task,
        candidate_at(9, 21),
        candidate_at(9, 22),
    )));
    let pending = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e2",
        pending_args,
        t0 + 199_500,
        Some(candidate_evidence()),
    )
    .unwrap();
    check!(
        pending["status"] == "queued",
        format!("pending submit: {pending}")
    );
    let pending_task = pending["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &pending_task, move || t0 + 200_000);
    {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &pending_task, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
    }
    let pending_record = candidate_record(&c, &cfg, &pending_task);
    let pending_id = pending_record.spec.lifecycle_candidate_id.clone();
    let pending_tick = mature_candidates(&c, &cfg, t0 + 270_000).unwrap();
    let pending_item = e2_item(&pending_tick, &pending_id);
    check!(
        pending_item.is_some_and(|item| item["state"] == "pending_capture"),
        format!("pending capture: {pending_tick}")
    );
    check!(
        !pending_item.is_some_and(|item| item["observation"].is_object()),
        "pending capture has no terminal measurement"
    );
    let unknown = mature_candidates(&c, &cfg, t0 + 330_000).unwrap();
    let unknown_item = e2_item(&unknown, &pending_id);
    check!(
        unknown_item.is_some_and(|item| item["state"] == "unknown"),
        format!("deadline unknown: {unknown}")
    );
    let unknown_measurement = unknown_item
        .map(|item| item["observation"].clone())
        .unwrap_or(Value::Null);
    check!(
        unknown_measurement["exit_status"] == "exit_quote_missing"
            && unknown_measurement["net_pnl_if_flat"].is_null(),
        format!("unknown has no pnl: {unknown_measurement}")
    );
    // Measurement binds the task/revision/observation hash and is stable across
    // reads; a tampered payload is unknown, not valid PnL.
    let measurement_a = super::measure::measure_observation(
        &c,
        &cfg,
        &observation_record,
        &observation_result,
        Some(&measurement),
        None,
        t0 + 100_000,
    );
    let measurement_b = super::measure::measure_observation(
        &c,
        &cfg,
        &observation_record,
        &observation_result,
        Some(&measurement),
        None,
        t0 + 200_000,
    );
    check!(
        measurement_a["projection_sha256"] == measurement_b["projection_sha256"],
        "measurement hash stable across reads"
    );
    check!(
        measurement_a["as_of_ms"] != measurement_b["as_of_ms"],
        "read as-of is outer metadata"
    );
    check!(
        measurement_a["counts"]["source_scope"] == "episode_key_only"
            && measurement_a["capital"]["research_capital_assumption"] == true,
        format!("measurement scope: {measurement_a}")
    );
    let mut tampered = measurement.clone();
    tampered["net_pnl_if_flat"] = json!("999999");
    let tampered_measurement = super::measure::measure_observation(
        &c,
        &cfg,
        &observation_record,
        &observation_result,
        Some(&tampered),
        None,
        t0 + 100_000,
    );
    check!(
        tampered_measurement["status"] == "unavailable",
        format!("tampered measurement refused: {tampered_measurement}")
    );
    // Nested Task 8 execution axes are copied exactly, not reported absent.
    let source_result = json!({
        "execution":{
            "retrospective":true,"prospective":false,"prospective_authoring":false,
            "no_lookahead_qualified":false,"holdout_qualified":false,
            "edge_qualified":false,"promotion_authorized":false,
            "activation_authorized":false,"scientific_state":"insufficient_evidence",
            "assumptions":[],"limitations":[],"report_sha256":"a".repeat(64)
        }
    });
    let axes_measurement = super::measure::measure_observation(
        &c,
        &cfg,
        &observation_record,
        &observation_result,
        Some(&measurement),
        Some(&source_result),
        t0 + 100_000,
    );
    check!(
        axes_measurement["replay"]["retrospective"] == true
            && axes_measurement["replay"]["copied_unchanged"] == true,
        format!("nested execution axes: {axes_measurement}")
    );
    // Typed non-finite score validation runs before serialization.
    let mut nonfinite = observation_result.observation.clone().unwrap();
    nonfinite.score = Some(f64::NAN);
    check!(
        sadr_cli::candidate_execution::validate_observation_score(&nonfinite).is_err(),
        "non-finite score refused"
    );
    // Ops projection uses the durable measurement field names and honest
    // running/unknown/cancel counts.
    let ops = observation_status(&c, &capture_cfg, t0 + 100_000);
    check!(
        ops["status"] == "available",
        format!("ops available: {ops}")
    );
    check!(
        ops["queue"]["unknown"].is_u64() && ops["queue"]["cancel_requested"].is_u64(),
        format!("ops lease states: {ops}")
    );
    check!(
        ops["lifecycle"]["milestones"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| item["measurement_scope"] == "episode_key_only")),
        format!("ops measurement scope: {ops}")
    );
    check!(
        ops["capture"]["enabled"] == true,
        format!("ops capture coverage: {ops}")
    );
    assert!(failures.is_empty(), "E2 failures: {failures:#?}");
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
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
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
    let portfolio = sadr_core::execution::Portfolio::with_capital(
        sadr_core::finance::Amount::parse("5000").unwrap(),
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

/// Mandatory P0: one REAL two-session rollover with different source and
/// successor configs/queues. The source candidate is authored, driven and
/// verified in the predecessor session; the successor submits a revised
/// source/params continuation through the host-built parent ref and the
/// successor's own research policy. The predecessor is read-only throughout.
fn case_e3_contract(root: &Path, base_settings: &ResearchSettings, base_policy: &PlannerConfig) {
    let dir = root.join("candidate-e3-contract");
    fs::create_dir_all(&dir).unwrap();
    let sessions = dir.join("sessions");
    let source_day = "2026-10-15";
    let successor_day = "2026-10-16";
    let source_dir = sessions.join(source_day);
    let successor_dir = sessions.join(successor_day);
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&successor_dir).unwrap();
    let t0 = candidate_at_on(source_day, 9, 23);
    let t1 = candidate_at_on(successor_day, 9, 23);
    let source_settings = candidate_settings(&source_dir, base_settings);
    if !candidate_sandbox_ready(&source_settings) {
        assert!(
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!("skipping candidate e3 contract: python3 or bwrap unavailable");
        return;
    }
    let successor_settings = candidate_settings(&successor_dir, base_settings);
    assert_ne!(
        source_settings.queue, successor_settings.queue,
        "the two sessions must have different research queues"
    );
    let mut source_c = super::super::super::tests::config(&dir);
    source_c.session = source_day.parse().unwrap();
    source_c.previous_session = "2026-10-14".parse().unwrap();
    source_c.directory = source_dir.clone();
    source_c.sessions_root = Some(sessions.clone());
    let mut successor_c = super::super::super::tests::config(&dir);
    successor_c.session = successor_day.parse().unwrap();
    successor_c.previous_session = source_day.parse().unwrap();
    successor_c.directory = successor_dir.clone();
    successor_c.sessions_root = Some(sessions.clone());
    // Dated planner policy: the source day resolves the ORIGINAL queue; the
    // successor day resolves the NEW queue. This is the real rollover shape.
    let mut source_policy = base_policy.clone();
    source_policy.research_tools = Some(source_settings.clone());
    source_policy.effective_from = None;
    source_policy.previous_config = None;
    let mut successor_policy = base_policy.clone();
    successor_policy.research_tools = Some(successor_settings.clone());
    successor_policy.effective_from = Some(successor_day.parse().unwrap());
    let source_planner = dir.join("planner-source.json");
    let successor_planner = dir.join("planner.json");
    successor_policy.previous_config = Some(source_planner.clone());
    fs::write(&source_planner, serde_json::to_vec(&source_policy).unwrap()).unwrap();
    fs::write(
        &successor_planner,
        serde_json::to_vec(&successor_policy).unwrap(),
    )
    .unwrap();
    source_c.planner_config = Some(source_planner.clone());
    successor_c.planner_config = Some(successor_planner.clone());
    let loaded_source =
        PlannerConfig::load_for_session(&successor_planner, source_day.parse().unwrap()).unwrap();
    assert_eq!(
        loaded_source.research_tools.as_ref().unwrap().queue,
        source_settings.queue,
        "the source day must resolve the original queue"
    );
    let loaded_successor =
        PlannerConfig::load_for_session(&successor_planner, successor_day.parse().unwrap())
            .unwrap();
    assert_eq!(
        loaded_successor.research_tools.as_ref().unwrap().queue,
        successor_settings.queue,
        "the successor day must resolve the new queue"
    );
    candidate_book_on(&source_c, source_day, "2026-10-14");
    candidate_book_on(&successor_c, successor_day, source_day);
    ensure_test_state(&source_c);
    ensure_test_state(&successor_c);
    write_test_carry(&source_c, &successor_c);
    // Author and complete the ORIGINAL candidate in the source session.
    let mut args = candidate_submit_args("e3-source", CANDIDATE_SOURCE, None);
    args.training_from_ms = candidate_at_on(source_day, 9, 20);
    args.training_to_ms = candidate_at_on(source_day, 9, 20);
    args.selection_from_ms = candidate_at_on(source_day, 9, 21);
    args.selection_to_ms = candidate_at_on(source_day, 9, 22);
    let source_submitted = candidate_submit(
        &source_c,
        &source_policy,
        "grok-4.7",
        3,
        "turn-e3-source",
        args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(source_submitted["status"], "queued", "{source_submitted}");
    let source_task = source_submitted["task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    candidate_drive(&source_c, &source_policy, &source_task, move || t0 + 10);
    let source_record = candidate_record(&source_c, &source_policy, &source_task);
    assert_eq!(source_record.status, jobs::TaskState::Succeeded);
    let source_status =
        candidate_status(&source_c, &source_policy, &source_task, 1, t0 + 100).unwrap();
    let reference: jobs::CandidateParentRefV1 =
        serde_json::from_value(source_status["parent_ref"]["ref"].clone()).unwrap();
    assert!(reference.validate());
    close_test_book_sidecars(&source_c);
    // The predecessor snapshot is taken after the source work and before any
    // successor read/write. Every later step must leave it byte-identical.
    let source_before = directory_manifest(&source_dir);
    // The successor policy has a DIFFERENT queue. Resolving through it must
    // load the ORIGINAL recorded source queue, not the successor queue.
    let resolved =
        super::carry::resolve_parent_ref(&successor_c, &successor_policy, &reference).unwrap();
    assert_eq!(resolved["status"], "resolved", "{resolved}");
    assert_eq!(resolved["source_session"], source_day);
    assert_eq!(resolved["task_id"], source_task);
    let carried = carry_state(&source_c, &source_policy);
    assert_eq!(carried["status"], "available", "{carried}");
    let carried_candidate = carried["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["task_id"] == source_task.as_str())
        .unwrap()
        .clone();
    let seed = json!({
        "source_session":source_day,
        "research_state":{
            "available":true,
            "tasks":[{"task_id":carried_candidate["task_id"],"revision":carried_candidate["revision"]}]
        }
    });
    close_test_book_sidecars(&source_c);
    let carried_resolved = resolve_carried_research(&successor_c, &successor_policy, &seed);
    assert_eq!(
        carried_resolved["status"], "resolved",
        "successor policy must resolve the original queue: {carried_resolved}"
    );
    assert_eq!(carried_resolved["tasks"][0]["resolution"], "matched");
    assert_eq!(carried_resolved["tasks"][0]["task_id"], source_task);
    // A revised source/params continuation through the host-built parent ref.
    let mut args = candidate_submit_args("e3-successor", CANDIDATE_SOURCE_ALT, None);
    args.params = json!({"revision":2});
    args.parent_ref = Some(Box::new(reference.clone()));
    let submitted = candidate_submit(
        &successor_c,
        &successor_policy,
        "grok-4.7",
        3,
        "turn-e3-successor",
        args,
        t1 + 1_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(submitted["status"], "queued", "{submitted}");
    assert_eq!(submitted["execution"], "not_started");
    assert_eq!(submitted["lifecycle"]["status"], "admitted", "{submitted}");
    assert_eq!(
        submitted["lifecycle"]["state"], "sandbox_abi",
        "{submitted}"
    );
    assert_eq!(
        submitted["task"]["parent_lifecycle_candidate_id"],
        reference.lifecycle_candidate_id
    );
    assert_eq!(
        submitted["task"]["parent_candidate_id"],
        reference.candidate_id
    );
    assert_ne!(submitted["task"]["candidate_id"], reference.candidate_id);
    let successor_task = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    // Status/result must bridge the recorded successor lifecycle row without a
    // copied predecessor row.
    let status = candidate_status(
        &successor_c,
        &successor_policy,
        &successor_task,
        1,
        t1 + 2_000,
    )
    .unwrap();
    assert_eq!(status["lifecycle"]["status"], "not_terminal", "{status}");
    assert_eq!(status["task"]["status"], "pending", "{status}");
    assert_eq!(
        experiment_ledger::lifecycle::state(
            &successor_dir,
            submitted["task"]["lifecycle_candidate_id"]
                .as_str()
                .unwrap()
        )
        .unwrap()
        .as_deref(),
        Some("sandbox_abi")
    );
    let result = candidate_result(
        &successor_c,
        &successor_policy,
        &successor_task,
        1,
        t1 + 2_000,
    )
    .unwrap();
    assert_eq!(result["result_status"], "not_verified");
    assert_eq!(result["lifecycle"]["status"], "not_terminal");
    // The global fold reads both original queues and both ledgers, and charges
    // the source admission as debt instead of resetting it at the handoff.
    let trials = trial_fold(&successor_c, &successor_policy);
    assert_eq!(trials["status"], "available", "{trials}");
    assert!(
        trials["admitted_trial_count"].as_u64().unwrap() >= 2,
        "{trials}"
    );
    assert!(
        trials["distinct_lifecycle_candidates"].as_u64().unwrap() >= 2,
        "{trials}"
    );
    assert_eq!(trials["distinct_sessions"], 2, "{trials}");
    assert_eq!(trials["reset"], false);
    assert_eq!(trials["counts_lower_bound"], false);
    // The predecessor is read-only: no file, row, queue, ledger or lease was
    // rewritten, copied or repaired by the cross-session continuation.
    assert_eq!(
        directory_manifest(&source_dir),
        source_before,
        "predecessor files changed during successor continuation"
    );
    let source_queue =
        super::carry::open_original_queue(&successor_c, &successor_policy, source_day).unwrap();
    let source_rows: i64 = source_queue
        .query_row(
            "SELECT count(*) FROM candidate_tasks WHERE session=?",
            [source_day],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source_rows, 1, "predecessor queue rows changed");
    drop(source_queue);
    // Wrong queue hard-fail: without the original recorded locator there is no
    // silent fallback to the successor queue.
    let mut no_locator = successor_c.clone();
    no_locator.planner_config = None;
    assert!(
        super::carry::open_original_queue(&no_locator, &successor_policy, source_day).is_err(),
        "a missing original queue locator must fail, not open the successor queue"
    );
    // A source policy whose recorded queue no longer exists is unavailable,
    // never silently redirected to the successor queue.
    let mut wrong_source = source_policy.clone();
    wrong_source.research_tools.as_mut().unwrap().queue = dir.join("missing-source.sqlite");
    let wrong_planner = dir.join("planner-wrong-source.json");
    fs::write(&wrong_planner, serde_json::to_vec(&wrong_source).unwrap()).unwrap();
    let mut wrong_successor = successor_policy.clone();
    wrong_successor.previous_config = Some(wrong_planner);
    let wrong_planner_path = dir.join("planner-wrong-successor.json");
    fs::write(
        &wrong_planner_path,
        serde_json::to_vec(&wrong_successor).unwrap(),
    )
    .unwrap();
    let mut wrong_c = successor_c.clone();
    wrong_c.planner_config = Some(wrong_planner_path);
    assert!(
        super::carry::open_original_queue(&wrong_c, &successor_policy, source_day).is_err(),
        "a missing original queue must not fall back to the successor queue"
    );
}

/// Build one source candidate plus one 5s-grace observation in its own session
/// directory. The caller decides whether to capture an exit and when to mature.
fn e3_grace_fixture(
    root: &Path,
    name: &str,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) -> Option<(Config, PlannerConfig, String, i64, Connection)> {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let t0 = candidate_at(9, 23);
    let settings = candidate_observation_settings(&dir, base_settings);
    if !candidate_sandbox_ready(&settings) {
        return None;
    }
    let (c, cfg) = candidate_config(&dir, base_policy, settings);
    candidate_book(&c);
    ensure_test_state(&c);
    insert_research_tick(&c, t0 + 500);
    let mut source_args = candidate_submit_args("e3b-source", CANDIDATE_SOURCE, None);
    source_args.horizon_minutes = 1;
    let source = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e3b",
        source_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(source["status"], "queued", "{source}");
    let source_task = source["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &source_task, move || t0 + 10);
    {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &source_task, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
    }
    let mut args = candidate_submit_args("e3b-grace5", CANDIDATE_SOURCE, None);
    let mut observation =
        candidate_observation_args(&source_task, candidate_at(9, 21), candidate_at(9, 22));
    observation.max_exit_delay_ms = 5_000;
    args.observation = Some(Box::new(observation));
    let submitted = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e3b",
        args,
        t0 + 1_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(submitted["status"], "queued", "{submitted}");
    let task = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    let record = candidate_record(&c, &cfg, &task);
    assert_eq!(
        record.spec.max_exit_delay_ms, 5_000,
        "the store must keep the declared 5s grace"
    );
    let id = record.spec.lifecycle_candidate_id.clone();
    candidate_drive(&c, &cfg, &task, move || t0 + 2_000);
    {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &task, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
    }
    let due = t0 + 2_000 + 60_000;
    let db = super::super::super::open_book(&c).unwrap();
    Some((c, cfg, id, due, db))
}

/// P0/C/F/G/H/I bounds: declared 5s grace end-to-end, actual completion-clock
/// capture, read-only book status, verified-universe refusal and structured
/// attempt provenance.
fn case_e3_bounds_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let Some((c, cfg, id, due, mut db)) =
        e3_grace_fixture(root, "candidate-e3-bounds", base_settings, base_policy)
    else {
        assert!(
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!("skipping candidate e3 bounds: python3 or bwrap unavailable");
        return;
    };
    let mut failures = Vec::new();
    macro_rules! check {
        ($cond:expr, $msg:expr) => {
            if !$cond {
                failures.push($msg.to_string());
            }
        };
    }
    check!(
        experiment_ledger::lifecycle::verified_parent(&c.directory, &id)
            .unwrap()
            .is_some_and(|(spec, _)| spec.max_exit_delay_ms == 5_000),
        "lifecycle spec keeps the declared 5s grace"
    );
    let mut capture_cfg = cfg.clone();
    capture_cfg.owner_tasks = Some(crate::runtime::owner_tasks::OwnerTasksSettings {
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
                    timeout_ms: 5_000,
                    max_rows: 64,
                },
            ),
            lane: Some(Default::default()),
        }),
    });
    let portfolio = sadr_core::execution::Portfolio::with_capital(
        sadr_core::finance::Amount::parse("5000").unwrap(),
    );
    // Actual completion clock T1 is after the original receipt T0+delta.
    let requested = due - 1_000;
    let completed = due + 3_000;
    // The HTTP response returned just before the completion sample; the
    // original data receipt is retained and is inside the 2s freshness gate.
    let receipt = completed - 100;
    let captured = super::capture::tick_with(
        &mut db,
        &c,
        &capture_cfg,
        &portfolio,
        due - 2_000,
        || completed,
        |_| {
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: requested,
                receipt_ms: receipt,
                body: candidate_quote_body(receipt).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    check!(
        captured["status"] == "captured",
        format!("completion-clock capture: {captured}")
    );
    check!(
        captured["event_at_ms"] == completed && captured["receipt_ms"] == receipt,
        format!("capture keeps original receipt and completion event: {captured}")
    );
    let (tick_at, tick_receipt, tick_body): (i64, i64, String) = db
        .query_row(
            "SELECT at,receipt,body FROM ticks WHERE event='research_quotes' ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let archive: Value = serde_json::from_str(&tick_body).unwrap();
    check!(
        tick_at == completed
            && tick_receipt == receipt
            && archive["received_ms"] == receipt
            && archive["requested_ms"] == requested,
        format!("tick eventAt/receipt: at={tick_at} receipt={tick_receipt} archive={archive}")
    );
    // A future receipt (after the completion sample) is refused, not renewed.
    let future = super::capture::tick_with(
        &mut db,
        &c,
        &capture_cfg,
        &portfolio,
        due + 4_000,
        || completed + 1_000,
        |_| {
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: due + 4_000,
                receipt_ms: completed + 2_000,
                body: candidate_quote_body(completed).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    check!(
        future["status"] == "refused" && future["reason"] == "observation_capture_clock_invalid",
        format!("future receipt refused: {future}")
    );
    // A wall clock that steps backwards is refused.
    let backwards = super::capture::tick_with(
        &mut db,
        &c,
        &capture_cfg,
        &portfolio,
        due + 5_000,
        || due + 4_000,
        |_| {
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: due + 5_000,
                receipt_ms: due + 5_000,
                body: candidate_quote_body(due + 5_000).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    check!(
        backwards["status"] == "refused"
            && backwards["reason"] == "observation_capture_clock_backwards",
        format!("wall backstep refused: {backwards}")
    );
    // The captured receipt is the first eligible exit inside the 5s window.
    let matured = mature_candidates(&c, &cfg, due + 3_000).unwrap();
    let item = e2_item(&matured, &id);
    check!(
        item.is_some_and(|item| item["state"] == "evaluated"),
        format!("5s grace matured at due+3s: {matured}")
    );
    check!(
        item.is_some_and(|item| {
            item["observation"]["exit_status"] == "executed"
                && item["observation"]["exit_deadline_ms"] == json!(due + 5_000)
        }),
        format!("5s deadline used: {matured}")
    );
    // P/Q: the immutable episode hash is stable across a NEW admission while
    // the live trial/cohort context legitimately changes; past financial fields
    // never mutate. Counts/capital/projection come from original evidence.
    let measurement = item
        .map(|item| item["observation"].clone())
        .unwrap_or(Value::Null);
    let (record, result) = {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::list_candidates_page(&mut db, &c.session.to_string(), None, None, 100)
            .unwrap()
            .into_iter()
            .find(|record| record.spec.lifecycle_candidate_id == id)
            .unwrap();
        let result =
            read_verified_candidate_result(&mut db, &record.spec.task_id, record.spec.revision)
                .unwrap()
                .unwrap();
        (record, result)
    };
    // The host never advertises an observation task as an authoring parent;
    // the resolver applies the same policy instead of accepting a forged one.
    let state_body: String = {
        let db = super::super::super::open_book(&c).unwrap();
        db.query_row("SELECT body FROM state WHERE id=1", [], |row| row.get(0))
            .unwrap()
    };
    let universe: Value =
        serde_json::from_slice(&fs::read(c.directory.join("wide-universe.json")).unwrap()).unwrap();
    let mut universe_keys = universe["instruments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|instrument| instrument["key"].as_str())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    universe_keys.sort();
    universe_keys.dedup();
    let observation_ref = jobs::CandidateParentRefV1 {
        version: 1,
        source_session: c.session.to_string(),
        task_id: record.spec.task_id.clone(),
        revision: 1,
        candidate_id: record.spec.candidate_id.clone(),
        lifecycle_candidate_id: record.spec.lifecycle_candidate_id.clone(),
        source_book_binding: record.spec.book_binding.clone(),
        source_state_hash: digest(state_body.as_bytes()),
        carry_sha256: digest(b"same_session"),
        account: c.account.clone(),
        instrument_universe_sha256: digest(&serde_json::to_vec(&universe_keys).unwrap()),
        cohort_key: super::carry::current_cohort_key(&c, &cfg).unwrap(),
        source_sha256: record.spec.source_sha256.clone(),
        params_sha256: record.spec.params_sha256.clone(),
        dataset_hash: record.spec.dataset_hash.clone(),
        content_key: record.spec.content_key.clone(),
        result_sha256: record.job.result_hash.clone().unwrap(),
        runtime_hash: record.spec.runtime_binding.runtime_hash.clone(),
        result_kind: result.result_kind.clone(),
        axes: Vec::new(),
    };
    check!(
        super::carry::resolve_parent_ref(&c, &cfg, &observation_ref)
            .unwrap_err()
            .to_string()
            == "source_continue_ineligible",
        "an observation task must not be an authoring parent"
    );
    let before = super::measure::measure_observation(
        &c,
        &cfg,
        &record,
        &result,
        Some(&measurement),
        None,
        due + 3_000,
    );
    check!(
        before["projection_scope"] == "immutable_episode_evidence"
            && before["counts"]["signals"] == 1
            && before["counts"]["raw_republications"].is_null()
            && before["counts"]["source_scope"] == "episode_key_only"
            && before["capital"]["amount"] == "5000"
            && before["capital"]["research_capital_assumption"] == true
            && before["authorship"]["observation_projection"] == "single_entry"
            && before["authorship"]["study_projection"]["selection_top_n"] == 1
            && before["cohort"]["cohort_matched"].is_null()
            && before["cohort"]["runtime_matched"] == true,
        format!("episode evidence fields: {before}")
    );
    let mut debt_args = candidate_submit_args("e3b-debt", CANDIDATE_SOURCE, None);
    debt_args.horizon_minutes = 1;
    let debt = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e3b",
        debt_args,
        due + 4_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    if let Some(task) = debt["task"]["task_id"].as_str() {
        let _ = candidate_cancel(&c, &cfg, task, 1, due + 5_000);
    }
    let after = super::measure::measure_observation(
        &c,
        &cfg,
        &record,
        &result,
        Some(&measurement),
        None,
        due + 6_000,
    );
    check!(
        before["projection_sha256"] == after["projection_sha256"],
        format!("episode hash changed by new admission: {before} -> {after}")
    );
    check!(
        before["live_context_sha256"] != after["live_context_sha256"],
        format!("live context did not track the new admission: {before} -> {after}")
    );
    check!(
        before["episodes"][0]["after_cost"] == after["episodes"][0]["after_cost"]
            && after["episodes"][0]["exit_status"] == "executed",
        format!("past financial fields mutated: {before} -> {after}")
    );
    check!(
        after["trials"]["admitted_trial_count"].as_u64()
            > before["trials"]["admitted_trial_count"].as_u64(),
        format!("new admission not charged: {before} -> {after}")
    );
    // Read-only capture status opens the CURRENT book, not the research queue.
    let status = super::capture::status(&c, &capture_cfg, completed + 2_000);
    check!(
        status["status"] == "journal_readable"
            && status["coverage_label"] == "journal_readability_only"
            && status["current_eligibility"] == "not_evaluated"
            && status["eligible_for_attempt"] == false
            && status["used"].as_u64().unwrap_or(0) >= 1
            && status["last_capture_ms"] == completed
            && status["last_capture_age_ms"] == 2_000,
        format!("capture status from book: {status}")
    );
    check!(
        status["remaining"].as_u64()
            == Some(8u64.saturating_sub(status["used"].as_u64().unwrap_or(0))),
        format!("capture remaining: {status}")
    );
    // An absent book is unknown with null counts, never an empty available.
    let empty_dir = root.join("candidate-e3-bounds-empty");
    fs::create_dir_all(&empty_dir).unwrap();
    let mut empty_c = c.clone();
    empty_c.directory = empty_dir;
    let absent = super::capture::status(&empty_c, &capture_cfg, completed);
    check!(
        absent["status"] == "unknown"
            && absent["coverage"] == "book_unavailable"
            && absent["used"].is_null()
            && absent["remaining"].is_null(),
        format!("absent book status: {absent}")
    );
    // A fabricated 64-hex manifest with missing raw archives never calls the
    // provider. The fixture is not a genuine market claim.
    let fabricated_dir = root.join("candidate-e3-bounds-fabricated");
    fs::create_dir_all(&fabricated_dir).unwrap();
    let mut fabricated_c = c.clone();
    fabricated_c.directory = fabricated_dir.clone();
    fs::write(
        fabricated_dir.join("wide-universe.json"),
        serde_json::to_vec(&json!({
            "session":fabricated_c.session.to_string(),
            "file_date":fabricated_c.previous_session.to_string(),
            "security_sha256":"a".repeat(64),
            "broker_sha256":"b".repeat(64),
            "suspended_sha256":"c".repeat(64),
            "instruments":[
                {"key":"NSE_EQ|INE000000001","symbol":"AAA","series":"EQ","tick":"0.05"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let fabricated = super::capture::tick_with(
        &mut db,
        &fabricated_c,
        &capture_cfg,
        &portfolio,
        completed + 10_000,
        || completed + 10_000,
        |_| panic!("provider must not be called for a fabricated universe"),
    );
    check!(
        fabricated["status"] == "not_eligible"
            && fabricated["reason"] == "verified_universe_unavailable"
            && fabricated["provider_called"] == false,
        format!("fabricated universe refused: {fabricated}")
    );
    // Preflight failure: no request attempted and the provider is provably not
    // called. A started network failure keeps the provider unproven (null).
    let preflight = super::capture::tick_with(
        &mut db,
        &c,
        &capture_cfg,
        &portfolio,
        due + 20_000,
        || due + 20_000,
        |_| {
            Err(crate::runtime::data::BoundedQuoteError {
                code: "token_file_unreadable".into(),
                request_attempted: false,
                provider_called: Some(false),
            })
        },
    );
    check!(
        preflight["status"] == "unavailable"
            && preflight["request_attempted"] == false
            && preflight["provider_called"] == false,
        format!("preflight attempt provenance: {preflight}")
    );
    let network = super::capture::tick_with(
        &mut db,
        &c,
        &capture_cfg,
        &portfolio,
        due + 21_000,
        || due + 21_000,
        |_| {
            Err(crate::runtime::data::BoundedQuoteError {
                code: "read_only_http_failed".into(),
                request_attempted: true,
                provider_called: None,
            })
        },
    );
    check!(
        network["status"] == "unavailable"
            && network["request_attempted"] == true
            && network["provider_called"].is_null(),
        format!("network attempt provenance: {network}")
    );
    // J: a two-key entry freeze must bind EACH key's original receipt SHA. The
    // chosen key's ref is its own archive, never the last key's global SHA.
    let t0 = due - 62_000;
    let key_a = "NSE_EQ|INE000000001";
    let key_b = "NSE_EQ|INF000000002";
    let single_body = |key: &str, at: i64| {
        json!({
            "status":"success",
            "data":{ key: {
                "instrument_token":key,
                "symbol":if key == key_a {"AAA"} else {"BBB"},
                "timestamp":at,
                "depth":{"buy":[{"price":100,"quantity":100}],"sell":[{"price":100.05,"quantity":100}]},
                "lower_circuit_limit":80,"upper_circuit_limit":120,"volume":1_000_000
            }}
        })
    };
    let archive_a = json!({"source":"upstox-readonly-v1","purpose":"research_observation","requested_ms":t0+600,"received_ms":t0+600,"body":single_body(key_a, t0+600).to_string()});
    let archive_b = json!({"source":"upstox-readonly-v1","purpose":"research_observation","requested_ms":t0+700,"received_ms":t0+700,"body":single_body(key_b, t0+700).to_string()});
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?2,?3,CASE WHEN instr(?3,'\"purpose\":\"research_observation\"')>0 THEN 'research_quotes' ELSE 'wide_quotes' END)",
        rusqlite::params![t0 + 600, t0 + 600, archive_a.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?2,?3,CASE WHEN instr(?3,'\"purpose\":\"research_observation\"')>0 THEN 'research_quotes' ELSE 'wide_quotes' END)",
        rusqlite::params![t0 + 700, t0 + 700, archive_b.to_string()],
    )
    .unwrap();
    let keys = std::collections::BTreeSet::from([key_a.to_owned(), key_b.to_owned()]);
    let tape = super::super::super::paper_replay::virtual_observation_tape_bounded(
        &c.directory,
        c.session,
        Some(c.previous_session),
        &keys,
        t0,
        t0 + 2_000,
        sadr_core::finance::Amount::parse("5000").unwrap(),
        100,
        1_000,
        1_048_576,
        t0 + 2_000,
        super::super::super::paper_replay::TapeMode::EntryFreeze,
    )
    .unwrap_or_else(|gap| panic!("two-key entry tape: {}", gap.reason));
    let manifest: Value = serde_json::from_slice(&tape.manifest).unwrap();
    let sha_a = manifest["provenance"]["per_key"][key_a]["archive_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let sha_b = manifest["provenance"]["per_key"][key_b]["archive_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    check!(
        sha_a != sha_b && sha_a == digest(archive_a.to_string().as_bytes()),
        format!("per-key archive SHAs: {sha_a} {sha_b}")
    );
    let signal = sadr_cli::candidate_execution::ScoredSignal {
        at_ms: t0 + 2_000,
        ordered_keys: vec![key_a.to_owned()],
    };
    let spec = sadr_cli::candidate_execution::ObservationExecutionSpecV1 {
        version: 1,
        abi: sadr_cli::candidate_execution::OBSERVATION_ABI.into(),
        parameters: sadr_cli::strategy_actions::Parameters {
            version: 1,
            parent_candidate_id: None,
            hypothesis: "two-key provenance study".into(),
            training_from_ms: t0 - 60_000,
            training_to_ms: t0 - 1,
            selection_from_ms: t0,
            selection_to_ms: t0 + 2_000,
            holding_ms: 60_000,
            max_exit_delay_ms: 5_000,
            capital: sadr_core::finance::Amount::parse("5000").unwrap(),
            costs: serde_json::from_value(json!({
                "per_order":"1","buy_bps":"0","sell_bps":"0","slippage_bps":"0","rounding_increment":"0.01"
            }))
            .unwrap(),
        },
        units: 10,
        max_entries: 1,
        selection_top_n: 1,
        entry_price_offset_ticks: 0,
        entry_at: sadr_cli::candidate_execution::EntryAt::Signal,
        exit: sadr_cli::candidate_execution::ExitRule::Time,
        first_signal_per_key: true,
        original_strategy_measurement: false,
        observation_projection: "single_entry".into(),
    };
    let action = sadr_cli::candidate_execution::observation_action_with_provenance(
        &signal,
        &spec,
        &tape.manifest,
        &tape.events,
        &tape.archive_sha256,
        Some(&tape.universe_sha256),
    )
    .unwrap();
    check!(
        action
            .quote
            .as_ref()
            .is_some_and(|quote| quote.archive_sha256 == sha_a),
        format!("entry ref uses its own archive: {action:?}")
    );
    // K: a NEWER compacted receipt hides the latest entry. The freeze must be an
    // explicit first-receipt-unknown gap, never the older readable quote.
    let compacted = json!({"kind":"compacted_quote_receipt","original_bytes":1234});
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?2,?3,CASE WHEN instr(?3,'\"purpose\":\"research_observation\"')>0 THEN 'research_quotes' ELSE 'wide_quotes' END)",
        rusqlite::params![t0 + 900, t0 + 900, compacted.to_string()],
    )
    .unwrap();
    let hidden = super::super::super::paper_replay::virtual_observation_tape_bounded(
        &c.directory,
        c.session,
        Some(c.previous_session),
        &keys,
        t0,
        t0 + 2_000,
        sadr_core::finance::Amount::parse("5000").unwrap(),
        100,
        1_000,
        1_048_576,
        t0 + 2_000,
        super::super::super::paper_replay::TapeMode::EntryFreeze,
    );
    check!(
        hidden.is_err_and(|gap| {
            gap.reason == "observation_quote_first_receipt_unknown"
                && gap.gap["archive_sha256"].is_null()
        }),
        "newer compacted entry must be an explicit unknown, not a fake first"
    );
    // O: a corrupt lifecycle ledger is unavailable, never an empty recorded 0.
    let corrupt_dir = root.join("candidate-e3-bounds-corrupt");
    fs::create_dir_all(corrupt_dir.join("model")).unwrap();
    fs::write(
        corrupt_dir.join("model").join("experiment-trials.sqlite"),
        b"not-a-sqlite-ledger",
    )
    .unwrap();
    check!(
        experiment_ledger::lifecycle::library(&corrupt_dir, "2026-10-16").is_err(),
        "corrupt lifecycle library must be unavailable"
    );
    let mut corrupt_c = c.clone();
    corrupt_c.directory = corrupt_dir.clone();
    let corrupt_mature = mature_candidates(&corrupt_c, &cfg, due + 3_000).unwrap();
    check!(
        corrupt_mature["status"] == "unavailable"
            && corrupt_mature["reason"] == "candidate_lifecycle_unavailable",
        format!("corrupt lifecycle maturation: {corrupt_mature}")
    );
    // A second session directory with the same 5s grace and no exit capture is
    // unknown at due+6s, never a permanent pending or a fabricated zero.
    let Some((c2, cfg2, id2, due2, _db2)) = e3_grace_fixture(
        root,
        "candidate-e3-bounds-missing",
        base_settings,
        base_policy,
    ) else {
        assert!(failures.is_empty(), "E3 bounds failures: {failures:#?}");
        return;
    };
    let unknown = mature_candidates(&c2, &cfg2, due2 + 6_000).unwrap();
    let unknown_item = e2_item(&unknown, &id2);
    check!(
        unknown_item.is_some_and(|item| item["state"] == "unknown"),
        format!("5s grace unknown at due+6s: {unknown}")
    );
    check!(
        unknown_item.is_some_and(|item| {
            item["observation"]["exit_status"] == "exit_quote_missing"
                && item["observation"]["net_pnl_if_flat"].is_null()
        }),
        format!("5s unknown has no pnl: {unknown}")
    );
    assert!(failures.is_empty(), "E3 bounds failures: {failures:#?}");
}

/// M/N: bounded queue paging (101st observation is matured) and an explicit
/// lineage limit instead of a silently complete 64-hop chain.
fn case_e3_paging_lineage_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-e3-paging");
    fs::create_dir_all(&dir).unwrap();
    let t0 = candidate_at(9, 23);
    let mut settings = candidate_observation_settings(&dir, base_settings);
    settings.admission_limits = Some(jobs::AdmissionLimits {
        max_trials_per_input: 10_000,
        max_session_trials: 10_000,
    });
    if !candidate_sandbox_ready(&settings) {
        assert!(
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!("skipping candidate e3 paging: python3 or bwrap unavailable");
        return;
    }
    let (c, cfg) = candidate_config(&dir, base_policy, settings);
    candidate_book(&c);
    ensure_test_state(&c);
    insert_research_tick(&c, t0 + 500);
    let mut source_args = candidate_submit_args("e3p-source", CANDIDATE_SOURCE, None);
    source_args.horizon_minutes = 1;
    let source = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e3p",
        source_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(source["status"], "queued", "{source}");
    let source_task = source["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &source_task, move || t0 + 10);
    {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &source_task, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
    }
    // 100 earlier queue rows are cancelled; the observation is row 101.
    for index in 0..100 {
        let mut args = candidate_submit_args(&format!("e3p-debt-{index}"), CANDIDATE_SOURCE, None);
        args.horizon_minutes = 1;
        let submitted = candidate_submit(
            &c,
            &cfg,
            "grok-4.7",
            3,
            "turn-e3p",
            args,
            t0 + 1_000 + index,
            Some(candidate_evidence()),
        )
        .unwrap();
        assert_eq!(submitted["status"], "queued", "{submitted}");
        let task = submitted["task"]["task_id"].as_str().unwrap().to_owned();
        let _ = candidate_cancel(&c, &cfg, &task, 1, t0 + 2_000 + index);
    }
    let mut args = candidate_submit_args("e3p-observation", CANDIDATE_SOURCE, None);
    let mut observation =
        candidate_observation_args(&source_task, candidate_at(9, 21), candidate_at(9, 22));
    observation.max_exit_delay_ms = 5_000;
    args.observation = Some(Box::new(observation));
    let submitted = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e3p",
        args,
        t0 + 1_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(submitted["status"], "queued", "{submitted}");
    let task = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    let record = candidate_record(&c, &cfg, &task);
    let id = record.spec.lifecycle_candidate_id.clone();
    candidate_drive(&c, &cfg, &task, move || t0 + 2_000);
    {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &task, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
    }
    let due = t0 + 2_000 + 60_000;
    let matured = mature_candidates(&c, &cfg, due + 3_000).unwrap();
    assert!(
        matured["scanned"].as_u64().unwrap_or(0) >= 101,
        "the whole bounded queue must be scanned: {matured}"
    );
    assert!(
        e2_item(&matured, &id).is_some_and(|item| item["state"] == "pending_capture"),
        "the 101st observation must be processed, not skipped: {matured}"
    );
    // N: 65 sessions exceed the 64-hop bound; the chain is an explicit limit.
    let family = root.join("candidate-e3-lineage");
    fs::create_dir_all(&family).unwrap();
    let sessions = family.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let day = |offset: i64| {
        (chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap() + chrono::Duration::days(offset))
            .to_string()
    };
    let mut previous: Option<Config> = None;
    for offset in 0..66 {
        let session = day(offset);
        let directory = sessions.join(&session);
        fs::create_dir_all(&directory).unwrap();
        let mut current = super::super::super::tests::config(&family);
        current.session = session.parse().unwrap();
        current.previous_session = day(offset.saturating_sub(1)).parse().unwrap();
        current.directory = directory;
        current.sessions_root = Some(sessions.clone());
        ensure_test_state(&current);
        if let Some(previous) = previous.as_ref() {
            write_test_carry(previous, &current);
        }
        previous = Some(current);
    }
    let last = previous.unwrap();
    assert_eq!(
        super::carry::predecessor_chain_verified(&last)
            .unwrap_err()
            .to_string(),
        "research_carry_lineage_limit"
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum E4ExitMode {
    AtDue,
    ProtectionAtDue,
    Missing,
    Compacted,
}

struct E4Sessions {
    source_c: Config,
    source_policy: PlannerConfig,
    successor_c: Config,
    successor_policy: PlannerConfig,
    source_day: String,
    successor_day: String,
    observation_id: String,
    observation_task: String,
    due: i64,
    source_before: std::collections::BTreeMap<String, String>,
    source_queue: std::path::PathBuf,
}

/// Build TWO real temp sessions with their own books, queues, accounts, carry
/// and dated planner chain. Session A authors and completes one original
/// candidate, then freezes and completes one prospective observation whose
/// original archived exit (if any) is saved before the rollover. The successor
/// B is created by the real carry-over and never reuses A's config or queue.
fn e4_continuation_fixture(
    root: &Path,
    name: &str,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
    exit_mode: E4ExitMode,
) -> Option<E4Sessions> {
    e4_continuation_fixture_with_source(
        root,
        name,
        base_settings,
        base_policy,
        exit_mode,
        CANDIDATE_SOURCE,
    )
}

fn e4_continuation_fixture_with_source(
    root: &Path,
    name: &str,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
    exit_mode: E4ExitMode,
    source_code: &str,
) -> Option<E4Sessions> {
    e4_continuation_fixture_with_hook(
        root,
        name,
        base_settings,
        base_policy,
        exit_mode,
        source_code,
        None,
    )
}

/// Same fixture with an optional pre-rollover hook. The hook runs while the
/// source session is still CURRENT, after the observation is terminal
/// `replayed` and before the carry closes the source book. Tests use it to
/// build an older stored record the successor must supersede. This is a
/// fixture-only seam; production code never calls it.
type E10PreRolloverHook<'a> = &'a dyn Fn(&Config, &PlannerConfig, &str, &str, i64);
type E10ValueMutation = Box<dyn Fn(&mut Value)>;

fn e4_continuation_fixture_with_hook(
    root: &Path,
    name: &str,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
    exit_mode: E4ExitMode,
    source_code: &str,
    hook: Option<E10PreRolloverHook<'_>>,
) -> Option<E4Sessions> {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let sessions = dir.join("sessions");
    let source_day = "2026-10-15";
    let successor_day = "2026-10-16";
    let source_dir = sessions.join(source_day);
    let successor_dir = sessions.join(successor_day);
    fs::create_dir_all(&source_dir).unwrap();
    fs::create_dir_all(&successor_dir).unwrap();
    let t0 = candidate_at_on(source_day, 9, 23);
    let source_settings = candidate_observation_settings(&source_dir, base_settings);
    if !candidate_sandbox_ready(&source_settings) {
        return None;
    }
    let successor_settings = candidate_observation_settings(&successor_dir, base_settings);
    assert_ne!(source_settings.queue, successor_settings.queue);
    let mut source_c = super::super::super::tests::config(&dir);
    source_c.session = source_day.parse().unwrap();
    source_c.previous_session = "2026-10-14".parse().unwrap();
    source_c.directory = source_dir.clone();
    source_c.sessions_root = Some(sessions.clone());
    let mut successor_c = super::super::super::tests::config(&dir);
    successor_c.session = successor_day.parse().unwrap();
    successor_c.previous_session = source_day.parse().unwrap();
    successor_c.directory = successor_dir.clone();
    successor_c.sessions_root = Some(sessions.clone());
    let mut source_policy = base_policy.clone();
    source_policy.research_tools = Some(source_settings.clone());
    source_policy.effective_from = None;
    source_policy.previous_config = None;
    let mut successor_policy = base_policy.clone();
    successor_policy.research_tools = Some(successor_settings.clone());
    successor_policy.effective_from = Some(successor_day.parse().unwrap());
    let source_planner = dir.join("planner-source.json");
    let successor_planner = dir.join("planner.json");
    successor_policy.previous_config = Some(source_planner.clone());
    fs::write(&source_planner, serde_json::to_vec(&source_policy).unwrap()).unwrap();
    fs::write(
        &successor_planner,
        serde_json::to_vec(&successor_policy).unwrap(),
    )
    .unwrap();
    source_c.planner_config = Some(source_planner);
    successor_c.planner_config = Some(successor_planner);
    candidate_book_on(&source_c, source_day, "2026-10-14");
    ensure_test_state(&source_c);
    // The original entry receipt at T0+500ms and the original archived exit at
    // DUE. No manual protection receipt and no real position exist.
    let due = t0 + 2_000 + 60_000;
    let insert_quote = |at: i64, purpose: &str, bid: f64, ask: f64| {
        let db = super::super::super::open_book(&source_c).unwrap();
        let mut body = candidate_quote_body(at);
        if let Some(row) = body["data"].get_mut("NSE_EQ:INE000000001") {
            row["depth"]["buy"][0]["price"] = json!(bid);
            row["depth"]["sell"][0]["price"] = json!(ask);
        }
        let archive = json!({
            "source":"upstox-readonly-v1",
            "purpose":purpose,
            "requested_ms":at,
            "received_ms":at,
            "body":body.to_string()
        });
        db.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?2,?3,CASE WHEN instr(?3,'\"purpose\":\"research_observation\"')>0 THEN 'research_quotes' ELSE 'wide_quotes' END)",
            rusqlite::params![at, at, archive.to_string()],
        )
        .unwrap();
    };
    insert_quote(t0 + 500, "research_observation", 99.0, 100.05);
    match exit_mode {
        E4ExitMode::AtDue => insert_quote(due, "research_observation", 100.0, 100.05),
        E4ExitMode::ProtectionAtDue => insert_quote(due, "execution_or_protection", 100.0, 100.05),
        E4ExitMode::Missing => {}
        E4ExitMode::Compacted => {
            let db = super::super::super::open_book(&source_c).unwrap();
            db.execute(
                "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?2,?3,CASE WHEN instr(?3,'\"purpose\":\"research_observation\"')>0 THEN 'research_quotes' ELSE 'wide_quotes' END)",
                rusqlite::params![
                    due,
                    due,
                    json!({"kind":"compacted_quote_receipt","original_bytes":1234}).to_string()
                ],
            )
            .unwrap();
        }
    }
    let mut source_args = candidate_submit_args("e4-source", source_code, None);
    source_args.horizon_minutes = 1;
    source_args.training_from_ms = candidate_at_on(source_day, 9, 20);
    source_args.training_to_ms = candidate_at_on(source_day, 9, 20);
    source_args.selection_from_ms = candidate_at_on(source_day, 9, 21);
    source_args.selection_to_ms = candidate_at_on(source_day, 9, 22);
    let source = candidate_submit(
        &source_c,
        &source_policy,
        "grok-4.7",
        3,
        "turn-e4",
        source_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(source["status"], "queued", "{source}");
    let source_task = source["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&source_c, &source_policy, &source_task, move || t0 + 10);
    {
        let mut db = candidate_queue(&source_c, &source_policy);
        let record = jobs::load_candidate(&mut db, &source_task, 1).unwrap();
        reconcile_candidate(&source_c, &source_policy, &mut db, &record).unwrap();
    }
    let mut args = candidate_submit_args("e4-observation", source_code, None);
    args.observation = Some(Box::new(candidate_observation_args(
        &source_task,
        candidate_at_on(source_day, 9, 21),
        candidate_at_on(source_day, 9, 22),
    )));
    let submitted = candidate_submit(
        &source_c,
        &source_policy,
        "grok-4.7",
        3,
        "turn-e4",
        args,
        t0 + 1_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(submitted["status"], "queued", "{submitted}");
    let observation_task = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    let observation_id = submitted["task"]["lifecycle_candidate_id"]
        .as_str()
        .unwrap()
        .to_owned();
    candidate_drive(&source_c, &source_policy, &observation_task, move || {
        t0 + 2_000
    });
    {
        let mut db = candidate_queue(&source_c, &source_policy);
        let record = jobs::load_candidate(&mut db, &observation_task, 1).unwrap();
        reconcile_candidate(&source_c, &source_policy, &mut db, &record).unwrap();
    }
    // The source episode is terminal Succeeded and recorded `replayed`; it is
    // NOT matured in the source session.
    assert_eq!(
        experiment_ledger::lifecycle::state(&source_dir, &observation_id)
            .unwrap()
            .as_deref(),
        Some("replayed")
    );
    // Real rollover: close the source book, write the carry and the successor
    // book. The source directory is snapshotted before any successor read.
    if let Some(hook) = hook {
        hook(
            &source_c,
            &source_policy,
            &observation_task,
            &observation_id,
            due,
        );
    }
    write_test_carry(&source_c, &successor_c);
    candidate_book_on(&successor_c, successor_day, source_day);
    ensure_test_state(&successor_c);
    let source_before = directory_manifest(&source_dir);
    Some(E4Sessions {
        source_c,
        source_policy,
        successor_c,
        successor_policy,
        source_day: source_day.to_owned(),
        successor_day: successor_day.to_owned(),
        observation_id,
        observation_task,
        due,
        source_before,
        source_queue: source_settings.queue,
    })
}

fn case_e4_continuation_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let Some(fixture) = e4_continuation_fixture(
        root,
        "candidate-e4-continuation",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        assert!(
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!("skipping candidate e4 continuation: python3 or bwrap unavailable");
        return;
    };
    let E4Sessions {
        source_c,
        source_policy,
        successor_c,
        successor_policy,
        source_day,
        successor_day,
        observation_id,
        observation_task,
        due,
        source_before,
        source_queue,
    } = fixture;
    assert_ne!(
        source_policy.research_tools.as_ref().unwrap().queue,
        successor_policy.research_tools.as_ref().unwrap().queue,
        "the two sessions must use their own queues"
    );
    let trials_before = trial_fold(&successor_c, &successor_policy);
    let source_attempts_before: u32 = {
        let db = rusqlite::Connection::open_with_flags(
            &source_queue,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        db.query_row(
            "SELECT j.attempts FROM candidate_tasks t JOIN jobs j ON j.id=t.job_id WHERE t.task_id=?",
            [&observation_task],
            |row| row.get(0),
        )
        .unwrap()
    };
    // Before DUE the successor must wait and record nothing.
    let early = mature_candidates(&successor_c, &successor_policy, due - 1).unwrap();
    assert!(
        e2_item(&early, &observation_id).is_some_and(|item| item["state"] == "waiting"),
        "before due must be waiting: {early}"
    );
    // At DUE+3s the archived original research exit is the FIRST eligible exit.
    let matured = mature_candidates(&successor_c, &successor_policy, due + 3_000).unwrap();
    let item = e2_item(&matured, &observation_id)
        .unwrap_or_else(|| panic!("successor did not mature the original episode: {matured}"));
    assert_eq!(item["state"], "evaluated", "{matured}");
    let observation = &item["observation"];
    assert_eq!(observation["exit_status"], "executed", "{observation}");
    assert_eq!(observation["flat"], true, "{observation}");
    assert_eq!(observation["accounting_complete"], true, "{observation}");
    assert!(observation["net_pnl_if_flat"].is_string(), "{observation}");
    assert_eq!(
        observation["exit_quote"]["purpose"], "research_observation",
        "{observation}"
    );
    assert_eq!(observation["exit_quote"]["receipt_ms"], json!(due));
    assert_eq!(observation["exit_at_ms"], json!(due));
    assert_eq!(observation["processed_at_ms"], json!(due + 3_000));
    assert_eq!(observation["source_session"], source_day);
    assert_eq!(observation["source_task_id"], observation_task);
    assert_eq!(observation["source_lifecycle_candidate_id"], observation_id);
    assert_eq!(observation["source_scope"], "continuation_original_episode");
    // The successor ledger records exactly one current continuation; the source
    // ledger row is untouched and still `replayed`.
    assert_eq!(
        experiment_ledger::lifecycle::state(&source_c.directory, &observation_id)
            .unwrap()
            .as_deref(),
        Some("replayed")
    );
    // Repeat tick: no new receipt, no conclusion error, no hash drift.
    let repeat = mature_candidates(&successor_c, &successor_policy, due + 4_000).unwrap();
    let repeated = e2_item(&repeat, &observation_id)
        .unwrap_or_else(|| panic!("repeat tick lost the continuation: {repeat}"));
    assert_eq!(repeated["state"], "evaluated", "{repeat}");
    assert_eq!(
        repeated["observation"]["measurement_sha256"],
        observation["measurement_sha256"]
    );
    assert_eq!(
        repeated["observation"]["processed_at_ms"],
        json!(due + 3_000)
    );
    // Source files and queue bytes are unchanged; no source rerun/probe.
    assert_eq!(
        directory_manifest(&source_c.directory),
        source_before,
        "predecessor files changed during successor continuation"
    );
    let source_attempts_after: u32 = {
        let db = rusqlite::Connection::open_with_flags(
            &source_queue,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        db.query_row(
            "SELECT j.attempts FROM candidate_tasks t JOIN jobs j ON j.id=t.job_id WHERE t.task_id=?",
            [&observation_task],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(source_attempts_after, source_attempts_before);
    let trials_after = trial_fold(&successor_c, &successor_policy);
    assert_eq!(
        trials_after["admitted_trial_count"], trials_before["admitted_trial_count"],
        "continuation must not admit a new trial: {trials_before} -> {trials_after}"
    );
    assert_eq!(
        trials_after["executed_trial_count"], trials_before["executed_trial_count"],
        "continuation must not re-execute a trial: {trials_before} -> {trials_after}"
    );
    assert_eq!(
        trials_after["distinct_lifecycle_candidates"],
        trials_before["distinct_lifecycle_candidates"],
        "continuation must not double-count the episode: {trials_before} -> {trials_after}"
    );
    // The CURRENT continuation is the latest verified state of the ORIGINAL
    // episode in the operational status and the read-only fold. It is not a
    // second candidate and is not counted twice.
    let status = observation_status(&successor_c, &successor_policy, due + 4_000);
    assert_eq!(status["status"], "available", "{status}");
    let milestone = status["lifecycle"]["milestones"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["candidate_id"] == observation_id.as_str())
        .unwrap_or_else(|| panic!("continuation milestone missing: {status}"));
    assert_eq!(milestone["continued"], true, "{status}");
    assert_eq!(milestone["continuation_state"], "evaluated", "{status}");
    assert_eq!(
        milestone["measurement_sha256"],
        observation["measurement_sha256"]
    );
    let folded = experiment_ledger::lifecycle::fold_library(
        &successor_c.directory,
        Some(&source_c.directory),
        &successor_day,
    )
    .unwrap();
    let folded_matches = folded["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["candidate_id"] == observation_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        folded_matches.len(),
        1,
        "continuation must not duplicate the episode: {folded}"
    );
    assert_eq!(
        folded_matches[0]["continuation"]["measurement_sha256"],
        observation["measurement_sha256"]
    );
    // One new terminal fact for the ORIGINAL episode, with a stable evidence id.
    let mut evidence_cfg = successor_policy.clone();
    {
        let orchestrator = evidence_cfg.orchestrator.as_mut().unwrap();
        orchestrator.enabled = true;
        orchestrator.triggers.insert(
            crate::runtime::orchestrator_model::Trigger::Supervisor,
            serde_json::from_value(json!({
                "enabled":true,
                "prompt":"prompts/supervisor.md",
                "tools":[],
                "max_turns":1,
                "daily_call_budget":1
            }))
            .unwrap(),
        );
    }
    evidence_cfg.owner_tasks = Some(crate::runtime::owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: crate::runtime::owner_tasks::ReviewSettings::default(),
        research: Some(crate::runtime::owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: Some(Default::default()),
        }),
    });
    let evidence =
        super::super::super::supervisor::research::due_evidence(&successor_c, &evidence_cfg)
            .unwrap();
    let continuation_evidence = evidence
        .evidence
        .iter()
        .filter(|item| item.maturity_state.as_deref() == Some("continuation_evaluated"))
        .collect::<Vec<_>>();
    assert_eq!(
        continuation_evidence.len(),
        1,
        "continuation evidence: {:?}",
        evidence.evidence
    );
    let evidence_id = continuation_evidence[0].evidence_id().unwrap();
    let evidence_again =
        super::super::super::supervisor::research::due_evidence(&successor_c, &evidence_cfg)
            .unwrap();
    let again = evidence_again
        .evidence
        .iter()
        .filter(|item| item.maturity_state.as_deref() == Some("continuation_evaluated"))
        .collect::<Vec<_>>();
    assert_eq!(again.len(), 1);
    assert_eq!(
        again[0].evidence_id().unwrap(),
        evidence_id,
        "continuation evidence id drifted"
    );

    // Secondary A: the original exit is missing. Before DUE the successor
    // waits; inside the grace it is pending capture (with the approved capture
    // scope, no model call) and records nothing; only after the deadline is it
    // a durable terminal UNKNOWN with SHA-only gaps and no fabricated price.
    let Some(missing) = e4_continuation_fixture(
        root,
        "candidate-e4-missing",
        base_settings,
        base_policy,
        E4ExitMode::Missing,
    ) else {
        panic!("second e4 fixture requires the sandbox");
    };
    let missing_due = missing.due;
    let missing_id = missing.observation_id.clone();
    let mut missing_cfg = missing.successor_policy.clone();
    missing_cfg.owner_tasks = Some(crate::runtime::owner_tasks::OwnerTasksSettings {
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
                    timeout_ms: 5_000,
                    max_rows: 64,
                },
            ),
            lane: Some(Default::default()),
        }),
    });
    let early = mature_candidates(&missing.successor_c, &missing_cfg, missing_due - 1).unwrap();
    assert_eq!(
        e2_item(&early, &missing_id).unwrap()["state"],
        "waiting",
        "{early}"
    );
    let grace = mature_candidates(&missing.successor_c, &missing_cfg, missing_due + 3_000).unwrap();
    let grace_item = e2_item(&grace, &missing_id).unwrap();
    assert_eq!(grace_item["state"], "pending_capture", "{grace}");
    // The CURRENT capture is configured and eligible in the current session,
    // but it is NOT authority for an exit in the ORIGINAL source window.
    assert_eq!(grace_item["capture"]["configured"], true, "{grace}");
    assert_eq!(
        grace_item["capture"]["eligible_for_attempt"], false,
        "{grace}"
    );
    assert_eq!(
        grace_item["capture"]["current_session"]["status"], "eligible",
        "{grace}"
    );
    assert_eq!(
        grace_item["capture"]["interest_set"][0],
        "NSE_EQ|INE000000001"
    );
    assert_eq!(grace_item["capture"]["new_model_call"], false);
    assert!(
        experiment_ledger::lifecycle::continuations(&missing.successor_c.directory)
            .unwrap()
            .is_empty(),
        "a grace-window pending capture must not record a terminal continuation"
    );
    let unknown =
        mature_candidates(&missing.successor_c, &missing_cfg, missing_due + 61_000).unwrap();
    let unknown_item = e2_item(&unknown, &missing_id).unwrap();
    assert_eq!(unknown_item["state"], "unknown", "{unknown}");
    let unknown_body = &unknown_item["observation"];
    assert_eq!(unknown_body["exit_status"], "exit_quote_missing");
    assert!(unknown_body["exit_quote"].is_null());
    assert!(unknown_body["net_pnl_if_flat"].is_null());
    assert!(unknown_body["fees"].is_null());
    assert_eq!(unknown_body["flat"], false);
    let unknown_gaps = unknown_body["gaps"].as_array().unwrap();
    assert!(unknown_gaps.iter().any(|gap| gap == "original_no_data"));
    assert!(
        unknown_gaps
            .iter()
            .any(|gap| gap == "no_fabricated_exit_price")
    );
    assert_eq!(
        directory_manifest(&missing.source_c.directory),
        missing.source_before,
        "missing-exit continuation wrote the predecessor"
    );

    // Secondary B: a compacted original receipt is an explicit first-receipt
    // unknown after the deadline, never a later readable substitute.
    let Some(compacted) = e4_continuation_fixture(
        root,
        "candidate-e4-compacted",
        base_settings,
        base_policy,
        E4ExitMode::Compacted,
    ) else {
        panic!("third e4 fixture requires the sandbox");
    };
    let compacted_due = compacted.due;
    let compacted_id = compacted.observation_id.clone();
    let grace = mature_candidates(
        &compacted.successor_c,
        &compacted.successor_policy,
        compacted_due + 3_000,
    )
    .unwrap();
    assert_eq!(
        e2_item(&grace, &compacted_id).unwrap()["state"],
        "pending_capture",
        "{grace}"
    );
    let unknown = mature_candidates(
        &compacted.successor_c,
        &compacted.successor_policy,
        compacted_due + 61_000,
    )
    .unwrap();
    let item = e2_item(&unknown, &compacted_id).unwrap();
    assert_eq!(item["state"], "unknown", "{unknown}");
    assert_eq!(
        item["observation"]["exit_status"],
        "exit_first_receipt_unknown"
    );
    assert_eq!(item["observation"]["first_receipt_unknown"], true);
    assert!(item["observation"]["exit_quote"].is_null());
    assert!(item["observation"]["exit_gap"]["archive_sha256"].is_null());
    assert_eq!(
        directory_manifest(&compacted.source_c.directory),
        compacted.source_before,
        "compacted continuation wrote the predecessor"
    );

    // Secondary C: a tampered CURRENT continuation body is refused, never
    // overwritten and never used to bypass the source authority.
    let Some(tamper) = e4_continuation_fixture(
        root,
        "candidate-e4-tamper",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        panic!("fourth e4 fixture requires the sandbox");
    };
    let tamper_due = tamper.due;
    let _ = mature_candidates(
        &tamper.successor_c,
        &tamper.successor_policy,
        tamper_due + 3_000,
    )
    .unwrap();
    let ledger = tamper
        .successor_c
        .directory
        .join("model")
        .join("experiment-trials.sqlite");
    let db = rusqlite::Connection::open(&ledger).unwrap();
    let stored: String = db
        .query_row(
            "SELECT body FROM experiment_continuations LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut tampered: Value = serde_json::from_str(&stored).unwrap();
    let continuation_id = tampered["continuation_id"].as_str().unwrap().to_owned();
    tampered["net_pnl_if_flat"] = json!("999.00");
    db.execute(
        "UPDATE experiment_continuations SET body=? WHERE continuation_id=?",
        rusqlite::params![tampered.to_string(), continuation_id],
    )
    .unwrap();
    drop(db);
    let refused = mature_candidates(
        &tamper.successor_c,
        &tamper.successor_policy,
        tamper_due + 4_000,
    )
    .unwrap();
    assert!(
        refused["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["reason"] == "candidate_predecessor_continuation_unavailable"),
        "tampered continuation must be refused: {refused}"
    );
    let db = rusqlite::Connection::open(&ledger).unwrap();
    let after: String = db
        .query_row(
            "SELECT body FROM experiment_continuations LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        after.contains("999.00"),
        "tampered continuation was silently overwritten"
    );
    drop(db);
    assert_eq!(
        directory_manifest(&tamper.source_c.directory),
        tamper.source_before,
        "tamper refusal wrote the predecessor"
    );

    // Secondary D: a forged carry hop is refused before any source read.
    let carry_path = successor_c.directory.join("carry-over.json");
    let mut forged_carry: Value = serde_json::from_slice(&fs::read(&carry_path).unwrap()).unwrap();
    forged_carry["source_state_hash"] = json!("0".repeat(64));
    fs::write(&carry_path, serde_json::to_vec(&forged_carry).unwrap()).unwrap();
    let forged = mature_candidates(&successor_c, &successor_policy, due + 5_000).unwrap();
    assert!(
        forged["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["reason"] == "candidate_predecessor_continuation_unavailable"),
        "forged carry must be refused: {forged}"
    );
    assert_eq!(
        directory_manifest(&source_c.directory),
        source_before,
        "forged carry wrote the predecessor"
    );

    // Secondary E: a changed successor runtime is not allowed to mature the
    // original episode. It stays pending inside the grace and becomes an
    // explicit unsupported-scope UNKNOWN only after the deadline.
    let Some(runtime_tamper) = e4_continuation_fixture(
        root,
        "candidate-e4-runtime",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        panic!("runtime e4 fixture requires the sandbox");
    };
    let runtime_due = runtime_tamper.due;
    let runtime_id = runtime_tamper.observation_id.clone();
    fs::write(
        runtime_tamper
            .successor_c
            .directory
            .join("candidate-helper"),
        b"changed-successor-runtime",
    )
    .unwrap();
    let pending = mature_candidates(
        &runtime_tamper.successor_c,
        &runtime_tamper.successor_policy,
        runtime_due + 3_000,
    )
    .unwrap();
    let pending_item = e2_item(&pending, &runtime_id).unwrap();
    assert_eq!(pending_item["state"], "pending_capture", "{pending}");
    assert_eq!(
        pending_item["gap"]["reason"], "continuation_runtime_changed",
        "{pending}"
    );
    let unknown = mature_candidates(
        &runtime_tamper.successor_c,
        &runtime_tamper.successor_policy,
        runtime_due + 61_000,
    )
    .unwrap();
    let unknown_item = e2_item(&unknown, &runtime_id).unwrap();
    assert_eq!(unknown_item["state"], "unknown", "{unknown}");
    assert_eq!(
        unknown_item["observation"]["exit_status"],
        "exit_scope_unsupported"
    );
    assert!(unknown_item["observation"]["exit_quote"].is_null());
    assert_eq!(
        directory_manifest(&runtime_tamper.source_c.directory),
        runtime_tamper.source_before,
        "runtime refusal wrote the predecessor"
    );

    // Secondary F: a changed successor cohort is a distinct refusal, not a
    // silent source-authority bypass.
    let Some(cohort_tamper) = e4_continuation_fixture(
        root,
        "candidate-e4-cohort",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        panic!("cohort e4 fixture requires the sandbox");
    };
    let cohort_due = cohort_tamper.due;
    let cohort_id = cohort_tamper.observation_id.clone();
    let universe_path = cohort_tamper
        .successor_c
        .directory
        .join("wide-universe.json");
    let mut universe: Value = serde_json::from_slice(&fs::read(&universe_path).unwrap()).unwrap();
    universe["instruments"]
        .as_array_mut()
        .unwrap()
        .push(json!({"key":"NSE_EQ|INE000000003","symbol":"CCC","series":"EQ","tick":"0.05"}));
    fs::write(&universe_path, serde_json::to_vec(&universe).unwrap()).unwrap();
    let pending = mature_candidates(
        &cohort_tamper.successor_c,
        &cohort_tamper.successor_policy,
        cohort_due + 3_000,
    )
    .unwrap();
    let pending_item = e2_item(&pending, &cohort_id).unwrap();
    assert_eq!(pending_item["state"], "pending_capture", "{pending}");
    assert_eq!(
        pending_item["gap"]["reason"], "continuation_cohort_changed",
        "{pending}"
    );
    let unknown = mature_candidates(
        &cohort_tamper.successor_c,
        &cohort_tamper.successor_policy,
        cohort_due + 61_000,
    )
    .unwrap();
    let unknown_item = e2_item(&unknown, &cohort_id).unwrap();
    assert_eq!(unknown_item["state"], "unknown", "{unknown}");
    assert_eq!(
        unknown_item["observation"]["exit_status"],
        "exit_scope_unsupported"
    );
    assert_eq!(
        directory_manifest(&cohort_tamper.source_c.directory),
        cohort_tamper.source_before,
        "cohort refusal wrote the predecessor"
    );
    assert_eq!(successor_day, successor_c.session.to_string());
    assert_ne!(source_c.directory, successor_c.directory);
    assert_ne!(source_c.session, successor_c.session);
}

/// Build a successor session C from B using the real dated planner chain,
/// carry-over and its own queue/book. No predecessor file is reused.
fn e5_successor(
    _root: &Path,
    sessions: &Path,
    day: &str,
    previous_day: &str,
    previous_planner: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) -> (Config, PlannerConfig) {
    let directory = sessions.join(day);
    fs::create_dir_all(&directory).unwrap();
    let settings = candidate_observation_settings(&directory, base_settings);
    let fixture_root = sessions.parent().unwrap();
    let mut c = super::super::super::tests::config(fixture_root);
    c.session = day.parse().unwrap();
    c.previous_session = previous_day.parse().unwrap();
    c.directory = directory;
    c.sessions_root = Some(sessions.to_path_buf());
    let mut policy = base_policy.clone();
    policy.research_tools = Some(settings);
    policy.effective_from = Some(day.parse().unwrap());
    policy.previous_config = Some(previous_planner.to_path_buf());
    let planner = fixture_root.join(format!("planner-{day}.json"));
    fs::write(&planner, serde_json::to_vec(&policy).unwrap()).unwrap();
    c.planner_config = Some(planner);
    (c, policy)
}

struct E5ThreeSessions {
    a: Config,
    b: Config,
    c: Config,
    c_policy: PlannerConfig,
    b_day: String,
    c_day: String,
    id: String,
    observation_task: String,
    due: i64,
    a_before: std::collections::BTreeMap<String, String>,
    b_before: std::collections::BTreeMap<String, String>,
    a_queue: std::path::PathBuf,
}

/// A -> B -> C with real rollover. B records the continuation of A's original
/// observation; C is a later successor that must SEE B's receipt without
/// minting a duplicate.
fn e5_three_sessions(
    root: &Path,
    name: &str,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) -> Option<E5ThreeSessions> {
    let fixture =
        e4_continuation_fixture(root, name, base_settings, base_policy, E4ExitMode::AtDue)?;
    let E4Sessions {
        source_c: a,
        successor_c: b,
        successor_policy: b_policy,
        successor_day: b_day,
        observation_id: id,
        observation_task,
        due,
        source_queue: a_queue,
        ..
    } = fixture;
    // B is the FIRST successor: it records the continuation of A's episode.
    let matured = mature_candidates(&b, &b_policy, due + 3_000).unwrap();
    let item =
        e2_item(&matured, &id).unwrap_or_else(|| panic!("B did not mature A's episode: {matured}"));
    assert_eq!(item["state"], "evaluated", "{matured}");
    let b_continuations = experiment_ledger::lifecycle::continuations(&b.directory).unwrap();
    assert_eq!(
        b_continuations.len(),
        1,
        "B must record exactly one continuation"
    );
    // C is a LATER successor. It inherits B's verified carry and has its own
    // config, queue, book and account.
    let sessions = root.join(name).join("sessions");
    let c_day = "2026-10-17";
    let b_planner = b.planner_config.clone().unwrap();
    let (c, c_policy) = e5_successor(
        root,
        &sessions,
        c_day,
        &b_day,
        &b_planner,
        base_settings,
        base_policy,
    );
    write_test_carry(&b, &c);
    candidate_book_on(&c, c_day, &b_day);
    ensure_test_state(&c);
    let a_before = directory_manifest(&a.directory);
    let b_before = directory_manifest(&b.directory);
    Some(E5ThreeSessions {
        a,
        b,
        c,
        c_policy,
        b_day,
        c_day: c_day.to_owned(),
        id,
        observation_task,
        due,
        a_before,
        b_before,
        a_queue,
    })
}

fn e5_set_book_flag(c: &Config, flag: &str, value: bool) {
    let db = super::super::super::open_book(c).unwrap();
    let body: String = db
        .query_row("SELECT body FROM state WHERE id=1", [], |row| row.get(0))
        .unwrap();
    let mut portfolio: sadr_core::execution::Portfolio = serde_json::from_str(&body).unwrap();
    match flag {
        "halted" => portfolio.halted = value,
        "stopped" => portfolio.stopped = value,
        "closed" => portfolio.closed = value,
        _ => panic!("unknown flag"),
    }
    db.execute(
        "UPDATE state SET body=? WHERE id=1",
        [serde_json::to_string(&portfolio).unwrap()],
    )
    .unwrap();
}

fn e5_corrupt_task_body(queue: &Path, task_id: &str) {
    let db = rusqlite::Connection::open(queue).unwrap();
    db.execute(
        "UPDATE candidate_tasks SET body=? WHERE task_id=?",
        rusqlite::params![b"{not-json", task_id],
    )
    .unwrap();
}

fn case_e5_corrective_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    // (1) THREE-SESSION VISIBILITY: C must see B's recorded continuation as the
    // latest verified state of A's episode, exactly once, without re-recording
    // it or changing A/B bytes.
    let Some(triple) = e5_three_sessions(
        root,
        "candidate-e5-three-session",
        base_settings,
        base_policy,
    ) else {
        assert!(
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bwrap unavailable"
        );
        eprintln!("skipping candidate e5 corrective: python3 or bwrap unavailable");
        return;
    };
    let b_measurement = {
        let continuations =
            experiment_ledger::lifecycle::continuations(&triple.b.directory).unwrap();
        continuations[0]["measurement_sha256"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let matured = mature_candidates(&triple.c, &triple.c_policy, triple.due + 5_000).unwrap();
    let item = e2_item(&matured, &triple.id)
        .unwrap_or_else(|| panic!("C silently dropped B's continuation of A's episode: {matured}"));
    assert_eq!(item["state"], "evaluated", "{matured}");
    assert_eq!(
        item["observation"]["measurement_sha256"],
        json!(b_measurement),
        "C must reuse B's recorded measurement: {matured}"
    );
    assert_eq!(
        item["recorded_in_session"],
        json!(triple.b_day),
        "C must preserve B's recording scope: {matured}"
    );
    assert!(
        experiment_ledger::lifecycle::continuations(&triple.c.directory)
            .unwrap()
            .is_empty(),
        "C must not mint a duplicate continuation receipt"
    );
    let status = observation_status(&triple.c, &triple.c_policy, triple.due + 5_000);
    assert_eq!(status["status"], "available", "{status}");
    let milestone = status["lifecycle"]["milestones"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["candidate_id"] == triple.id.as_str())
        .unwrap_or_else(|| panic!("C status lost B's continuation: {status}"));
    assert_eq!(milestone["continuation_state"], "evaluated", "{status}");
    assert_eq!(milestone["effective_state"], "evaluated", "{status}");
    assert_eq!(
        milestone["continuation_recorded_in_session"],
        json!(triple.b_day),
        "C status must name B as the recording session: {status}"
    );
    let folded = experiment_ledger::lifecycle::fold_library_chain(
        &triple.c,
        &triple.c_policy,
        &triple.c_day,
    )
    .unwrap();
    let folded_matches = folded["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["candidate_id"] == triple.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        folded_matches.len(),
        1,
        "C fold must show A's episode exactly once: {folded}"
    );
    assert_eq!(
        folded_matches[0]["continuation"]["measurement_sha256"],
        json!(b_measurement),
        "C fold must attach B's continuation: {folded}"
    );
    assert_eq!(
        directory_manifest(&triple.a.directory),
        triple.a_before,
        "C's read/mature changed A"
    );
    assert_eq!(
        directory_manifest(&triple.b.directory),
        triple.b_before,
        "C's read/mature changed B"
    );
    // (5) EXIT PROVENANCE: the recorded exit quote must carry the ORIGINAL
    // instrument identity and the FIRST original receipt SHA, not nulls or a
    // last-key/whole-tape digest.
    let b_continuation = experiment_ledger::lifecycle::continuations(&triple.b.directory)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let exit_quote = &b_continuation["exit_quote"];
    assert_eq!(exit_quote["symbol"], "AAA", "{b_continuation}");
    assert_eq!(exit_quote["series"], "EQ", "{b_continuation}");
    assert_eq!(exit_quote["tick"], "0.05", "{b_continuation}");
    assert_eq!(exit_quote["key"], "NSE_EQ|INE000000001", "{b_continuation}");
    assert_eq!(
        exit_quote["purpose"], "research_observation",
        "{b_continuation}"
    );
    let expected_exit_sha = {
        let db = super::super::super::open_book(&triple.a).unwrap();
        let raw: String = db
            .query_row(
                "SELECT body FROM ticks WHERE at=? AND event='research_quotes'",
                [triple.due],
                |row| row.get(0),
            )
            .unwrap();
        digest(raw.as_bytes())
    };
    assert_eq!(
        exit_quote["archive_sha256"], expected_exit_sha,
        "exit receipt SHA must be the FIRST original archived receipt: {b_continuation}"
    );
    assert_eq!(exit_quote["bid"], "100", "{b_continuation}");
    assert_eq!(exit_quote["ask"], "100.05", "{b_continuation}");
    assert_eq!(
        exit_quote["universe_sha256"], b_continuation["entry_quote"]["universe_sha256"],
        "{b_continuation}"
    );
    // (10) TRIAL DEBT: a corrupt original task body is an explicit partial
    // lower bound, never an available numeric zero.
    e5_corrupt_task_body(&triple.a_queue, &triple.observation_task);
    let trials = trial_fold(&triple.c, &triple.c_policy);
    assert_eq!(trials["status"], "partial", "{trials}");
    assert!(
        trials["global_tested_trial_count"].is_null(),
        "partial main count must be null, not a fake zero: {trials}"
    );
    assert!(
        trials["known_lower_bound_counts"].is_object(),
        "partial fold must expose explicit lower bounds: {trials}"
    );
    assert_eq!(trials["counts_lower_bound"], true, "{trials}");

    // (2) A MISSING B LEDGER MUST FAIL CLOSED: C must not re-mature A.
    let Some(missing) = e5_three_sessions(
        root,
        "candidate-e5-missing-b-ledger",
        base_settings,
        base_policy,
    ) else {
        panic!("missing-b fixture requires the sandbox");
    };
    let b_ledger = missing
        .b
        .directory
        .join("model")
        .join("experiment-trials.sqlite");
    fs::remove_file(&b_ledger).unwrap();
    let matured = mature_candidates(&missing.c, &missing.c_policy, missing.due + 5_000).unwrap();
    assert!(
        experiment_ledger::lifecycle::continuations(&missing.c.directory)
            .unwrap()
            .is_empty(),
        "C minted a new A receipt while B's ledger was unknown: {matured}"
    );
    assert!(
        matured["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["state"] == "unavailable"
                || item["reason"] == "candidate_predecessor_lifecycle_unavailable"),
        "C must report the unknown B prefix: {matured}"
    );
    assert_eq!(
        directory_manifest(&missing.a.directory),
        missing.a_before,
        "missing-B refusal wrote A"
    );

    // (1b) The lifecycle library pages past the old 256-row cap instead of
    // silently dropping later observations.
    let paging = root.join("candidate-e5-library-paging");
    fs::create_dir_all(&paging).unwrap();
    for index in 0..300u32 {
        let spec = experiment_ledger::lifecycle::ExperimentSpec {
            version: 1,
            parent_candidate_id: None,
            session: "2026-10-15".into(),
            source_sha256: digest(format!("e5-paging-source-{index}").as_bytes()),
            params_sha256: digest(b"e5-paging-params"),
            dataset_hash: digest(b"e5-paging-dataset"),
            feature_abi: "scorer_replay_v1".into(),
            hypothesis: format!("paging fixture {index}"),
            training_from_ms: 0,
            training_to_ms: 1,
            selection_from_ms: 2,
            selection_to_ms: 3,
            holding_ms: 1,
            max_exit_delay_ms: 0,
        };
        experiment_ledger::lifecycle::admit_spec(&paging, spec, i64::from(index)).unwrap();
    }
    let paged = experiment_ledger::lifecycle::library_bounded(&paging, "", 10_000).unwrap();
    assert_eq!(
        paged["items"].as_array().unwrap().len(),
        300,
        "the lifecycle library must page past 256 rows: {paged}"
    );
    assert_eq!(paged["truncated"], false, "{paged}");

    // (2b) A CORRUPT B LEDGER is also unknown: C must not re-mature A.
    let Some(corrupt) = e5_three_sessions(
        root,
        "candidate-e5-corrupt-b-ledger",
        base_settings,
        base_policy,
    ) else {
        panic!("corrupt-b fixture requires the sandbox");
    };
    let b_ledger = corrupt
        .b
        .directory
        .join("model")
        .join("experiment-trials.sqlite");
    fs::write(&b_ledger, b"not-a-sqlite-ledger").unwrap();
    let matured = mature_candidates(&corrupt.c, &corrupt.c_policy, corrupt.due + 5_000).unwrap();
    assert!(
        experiment_ledger::lifecycle::continuations(&corrupt.c.directory)
            .unwrap()
            .is_empty(),
        "C minted a new A receipt while B's ledger was corrupt: {matured}"
    );
    assert!(
        matured["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["state"] == "unavailable"),
        "C must report the corrupt B prefix as unavailable: {matured}"
    );

    // (2c) Fresh-vs-corrupt lifecycle ledger semantics: a missing ledger is a
    // known no-records projection; an existing corrupt ledger is unavailable.
    let fresh = root.join("candidate-e5-fresh-ledger");
    fs::create_dir_all(&fresh).unwrap();
    assert_eq!(
        experiment_ledger::lifecycle::library_optional(&fresh, "2026-10-18").unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let corrupt_file = fresh.join("model").join("experiment-trials.sqlite");
    fs::create_dir_all(corrupt_file.parent().unwrap()).unwrap();
    fs::write(&corrupt_file, b"not-a-sqlite-ledger").unwrap();
    assert!(
        experiment_ledger::lifecycle::library_optional(&fresh, "2026-10-18").is_err(),
        "an existing corrupt ledger must be unavailable, not empty"
    );

    // (2d) Concurrent continuation inserts are idempotent: one validated first
    // row wins; a stale inserter never overwrites it with a different body.
    let race_dir = root.join("candidate-e5-race");
    fs::create_dir_all(&race_dir).unwrap();
    let race_body = b_continuation.clone();
    let first_dir = race_dir.clone();
    let first_body = race_body.clone();
    let first = std::thread::spawn(move || {
        super::record_continuation_test(&first_dir, &first_body, 1_000)
            .map(|value| {
                value["measurement_sha256"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .map_err(|error| error.to_string())
    });
    let second_dir = race_dir.clone();
    let second_body = race_body.clone();
    let second = std::thread::spawn(move || {
        super::record_continuation_test(&second_dir, &second_body, 1_001)
            .map(|value| {
                value["measurement_sha256"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .map_err(|error| error.to_string())
    });
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    assert!(
        first.is_ok() || second.is_ok(),
        "at least one concurrent insert must succeed: {first:?} {second:?}"
    );
    let stored = experiment_ledger::lifecycle::continuations(&race_dir).unwrap();
    assert_eq!(stored.len(), 1, "concurrent inserts duplicated the receipt");
    assert_eq!(
        stored[0]["measurement_sha256"], race_body["measurement_sha256"],
        "concurrent insert replaced the first measurement"
    );

    // (3) UNBOUND TASK BOOK: a mutated original task book binding must refuse
    // before the source body is trusted, even with valid score artifacts.
    let Some(unbound) = e5_three_sessions(
        root,
        "candidate-e5-unbound-book",
        base_settings,
        base_policy,
    ) else {
        panic!("unbound-book fixture requires the sandbox");
    };
    {
        let db = rusqlite::Connection::open(&unbound.a_queue).unwrap();
        let body: String = db
            .query_row(
                "SELECT body FROM candidate_tasks WHERE task_id=?",
                [&unbound.observation_task],
                |row| row.get(0),
            )
            .unwrap();
        let mut task: Value = serde_json::from_str(&body).unwrap();
        task["book_binding"] = json!("0".repeat(64));
        db.execute(
            "UPDATE candidate_tasks SET body=? WHERE task_id=?",
            rusqlite::params![task.to_string(), &unbound.observation_task],
        )
        .unwrap();
    }
    let a_after_mutation = directory_manifest(&unbound.a.directory);
    let matured = mature_candidates(&unbound.c, &unbound.c_policy, unbound.due + 5_000).unwrap();
    assert!(
        experiment_ledger::lifecycle::continuations(&unbound.c.directory)
            .unwrap()
            .is_empty(),
        "C continued a task whose book binding did not match the verified carry: {matured}"
    );
    assert_eq!(
        directory_manifest(&unbound.a.directory),
        a_after_mutation,
        "unbound-book refusal wrote A"
    );

    // (6) CAPTURE SCOPE HONESTY: a halted CURRENT successor book is not an
    // eligible capture. The projection must not claim `available`.
    let Some(missing_exit) = e4_continuation_fixture(
        root,
        "candidate-e5-capture-halted",
        base_settings,
        base_policy,
        E4ExitMode::Missing,
    ) else {
        panic!("capture fixture requires the sandbox");
    };
    let mut capture_cfg = missing_exit.successor_policy.clone();
    capture_cfg.owner_tasks = Some(crate::runtime::owner_tasks::OwnerTasksSettings {
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
                    timeout_ms: 5_000,
                    max_rows: 64,
                },
            ),
            lane: Some(Default::default()),
        }),
    });
    e5_set_book_flag(&missing_exit.successor_c, "halted", true);
    let grace = mature_candidates(
        &missing_exit.successor_c,
        &capture_cfg,
        missing_exit.due + 3_000,
    )
    .unwrap();
    let grace_item = e2_item(&grace, &missing_exit.observation_id).unwrap();
    assert_eq!(grace_item["state"], "pending_capture", "{grace}");
    assert_ne!(
        grace_item["capture"]["status"], "available",
        "a halted current book must not claim capture availability: {grace}"
    );
    assert_eq!(
        grace_item["capture"]["eligible_for_attempt"], false,
        "a halted current book is not eligible: {grace}"
    );
    assert_eq!(
        directory_manifest(&missing_exit.source_c.directory),
        missing_exit.source_before,
        "capture projection wrote A"
    );
}

fn case_e6_corrective_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let capture_settings = crate::runtime::owner_tasks::ControllerObservationCaptureSettings {
        enabled: true,
        max_keys: 8,
        max_calls_per_session: 8,
        min_interval_ms: 1_000,
        timeout_ms: 5_000,
        max_rows: 64,
    };
    let capture_cfg = |policy: &PlannerConfig| {
        let mut cfg = policy.clone();
        cfg.owner_tasks = Some(crate::runtime::owner_tasks::OwnerTasksSettings {
            version: 1,
            agenda: None,
            review: crate::runtime::owner_tasks::ReviewSettings::default(),
            research: Some(crate::runtime::owner_tasks::OwnerResearchSettings {
                idle_trigger: None,
                enabled: true,
                cutoff_minute: 1020,
                max_evidence: 4,
                quote_capture: None,
                observation_capture: Some(capture_settings.clone()),
                lane: Some(Default::default()),
            }),
        });
        cfg
    };
    // (1) SESSION CLOCK: admission inside the window, completion after the
    // close. The completion must be refused and no quote tick may be inserted.
    let Some(cap) = e4_continuation_fixture(
        root,
        "candidate-e6-capture-clock",
        base_settings,
        base_policy,
        E4ExitMode::Missing,
    ) else {
        panic!("capture clock fixture requires the sandbox");
    };
    let cap_cfg = capture_cfg(&cap.successor_policy);
    let admission = candidate_at_on(&cap.successor_day, 9, 23);
    let close = candidate_at_on(&cap.successor_day, 15, 31);
    let mut db = super::super::super::open_book(&cap.successor_c).unwrap();
    let portfolio = sadr_core::execution::Portfolio::with_capital(
        sadr_core::finance::Amount::parse("5000").unwrap(),
    );
    let ticks_before: i64 = db
        .query_row(
            "SELECT count(*) FROM ticks WHERE event='research_quotes'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let refused = super::capture::tick_with(
        &mut db,
        &cap.successor_c,
        &cap_cfg,
        &portfolio,
        admission,
        || close,
        |_| {
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: admission,
                receipt_ms: admission,
                body: candidate_quote_body(admission).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    assert_eq!(refused["status"], "refused", "{refused}");
    assert_eq!(
        refused["reason"], "observation_capture_completed_outside_session",
        "{refused}"
    );
    let ticks_after: i64 = db
        .query_row(
            "SELECT count(*) FROM ticks WHERE event='research_quotes'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        ticks_before, ticks_after,
        "post-close completion wrote a tick"
    );
    drop(db);
    let late = mature_candidates(&cap.successor_c, &cap_cfg, cap.due + 3_000).unwrap();
    let late_item = e2_item(&late, &cap.observation_id).unwrap();
    assert_ne!(
        late_item["state"], "evaluated",
        "a refused post-close capture matured an exit: {late}"
    );
    // A genuine in-session T1 > T0 still captures and keeps the original
    // receipt timestamp.
    let Some(cap_ok) = e4_continuation_fixture(
        root,
        "candidate-e6-capture-ok",
        base_settings,
        base_policy,
        E4ExitMode::Missing,
    ) else {
        panic!("capture ok fixture requires the sandbox");
    };
    let cap_ok_cfg = capture_cfg(&cap_ok.successor_policy);
    let t0 = candidate_at_on(&cap_ok.successor_day, 9, 23);
    let t1 = t0 + 60_000;
    let mut db = super::super::super::open_book(&cap_ok.successor_c).unwrap();
    let captured = super::capture::tick_with(
        &mut db,
        &cap_ok.successor_c,
        &cap_ok_cfg,
        &portfolio,
        t0,
        || t1,
        |_| {
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: t0,
                receipt_ms: t0,
                body: candidate_quote_body(t0).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    assert_eq!(captured["status"], "captured", "{captured}");
    assert_eq!(captured["receipt_ms"], json!(t0), "{captured}");
    assert_eq!(captured["event_at_ms"], json!(t1), "{captured}");
    drop(db);

    // (5) A CURRENT live book with a non-empty WAL is read through a normal
    // read-only snapshot, not the closed-book immutable opener.
    let Some(cap_live) = e4_continuation_fixture(
        root,
        "candidate-e6-capture-live-wal",
        base_settings,
        base_policy,
        E4ExitMode::Missing,
    ) else {
        panic!("live WAL fixture requires the sandbox");
    };
    e5_set_book_flag(&cap_live.successor_c, "halted", true);
    let live = super::super::super::open_book(&cap_live.successor_c).unwrap();
    let body: String = live
        .query_row("SELECT body FROM state WHERE id=1", [], |row| row.get(0))
        .unwrap();
    live.execute("UPDATE state SET body=? WHERE id=1", [body])
        .unwrap();
    let live_cfg = capture_cfg(&cap_live.successor_policy);
    let grace = mature_candidates(&cap_live.successor_c, &live_cfg, cap_live.due + 3_000).unwrap();
    let grace_item = e2_item(&grace, &cap_live.observation_id).unwrap();
    assert_eq!(grace_item["state"], "pending_capture", "{grace}");
    assert_eq!(
        grace_item["capture"]["current_session"]["reason"], "book_halted",
        "a live WAL hid the real halted book: {grace}"
    );
    assert_eq!(
        grace_item["capture"]["current_session"]["status"], "not_eligible",
        "{grace}"
    );
    drop(live);

    // (2)/(3) MISSING QUEUE / MISSING LIFECYCLE LEDGER are unknown, never a
    // numeric zero. The prefix is checked for trial-fold consumers too.
    let Some(missing_queue) = e5_three_sessions(
        root,
        "candidate-e6-missing-queue",
        base_settings,
        base_policy,
    ) else {
        panic!("missing-queue fixture requires the sandbox");
    };
    fs::remove_file(&missing_queue.a_queue).unwrap();
    let trials = trial_fold(&missing_queue.c, &missing_queue.c_policy);
    assert_eq!(trials["status"], "partial", "{trials}");
    assert!(
        trials["global_tested_trial_count"].is_null(),
        "a deleted queue must not report a numeric zero: {trials}"
    );
    assert!(
        trials["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap == "trial_queue_missing"),
        "{trials}"
    );
    let queue_status = observation_status(
        &missing_queue.c,
        &missing_queue.c_policy,
        missing_queue.due + 3_000,
    );
    assert_eq!(
        queue_status["trials"]["status"], "partial",
        "{queue_status}"
    );
    assert!(
        queue_status["trials"]["global_tested_trial_count"].is_null(),
        "the operational projection hid the partial trial fold: {queue_status}"
    );
    let Some(missing_ledger) = e5_three_sessions(
        root,
        "candidate-e6-missing-ledger",
        base_settings,
        base_policy,
    ) else {
        panic!("missing-ledger fixture requires the sandbox");
    };
    fs::remove_file(
        missing_ledger
            .a
            .directory
            .join("model")
            .join("experiment-trials.sqlite"),
    )
    .unwrap();
    let trials = trial_fold(&missing_ledger.c, &missing_ledger.c_policy);
    assert!(
        matches!(trials["status"].as_str(), Some("partial" | "unavailable")),
        "{trials}"
    );
    assert!(
        trials["global_tested_trial_count"].is_null(),
        "a deleted expected ledger must not report a numeric zero: {trials}"
    );
    // Earliest hop with BOTH queue and ledger missing is not a valid no-records
    // projection; the whole prefix must fail closed and C must not re-mature A.
    let Some(both_missing) = e5_three_sessions(
        root,
        "candidate-e6-both-missing",
        base_settings,
        base_policy,
    ) else {
        panic!("both-missing fixture requires the sandbox");
    };
    fs::remove_file(&both_missing.a_queue).unwrap();
    fs::remove_file(
        both_missing
            .a
            .directory
            .join("model")
            .join("experiment-trials.sqlite"),
    )
    .unwrap();
    let both = mature_candidates(
        &both_missing.c,
        &both_missing.c_policy,
        both_missing.due + 5_000,
    )
    .unwrap();
    assert!(
        both["items"].as_array().unwrap().iter().any(|item| {
            item["state"] == "unavailable"
                && item["detail"] == "candidate_predecessor_lifecycle_missing"
        }),
        "a missing earliest queue+ledger was treated as no records: {both}"
    );
    assert!(
        experiment_ledger::lifecycle::continuations(&both_missing.c.directory)
            .unwrap()
            .is_empty(),
        "C minted a duplicate A receipt with an unknown prefix"
    );

    // (6) SELF-HASH IS NOT IDENTITY: a tampered body with a recomputed
    // measurement/id/current_session/recordedAt is refused by the ledger's own
    // session binding and by the reader.
    let Some(tamper) = e5_three_sessions(
        root,
        "candidate-e6-continuation-identity",
        base_settings,
        base_policy,
    ) else {
        panic!("identity fixture requires the sandbox");
    };
    let original = experiment_ledger::lifecycle::continuations(&tamper.b.directory)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let mut forged = original.clone();
    forged["current_scope"]["current_session"] = json!(tamper.c_day);
    forged["recorded_at_ms"] = json!(original["recorded_at_ms"].as_i64().unwrap_or(0) + 1);
    forged["processed_at_ms"] = forged["recorded_at_ms"].clone();
    forged["continuation_id"] = json!(super::continuation_identity_sha256(&forged).unwrap());
    forged["measurement_sha256"] =
        json!(experiment_ledger::lifecycle::continuation_content_sha256(&forged).unwrap());
    let a_before = directory_manifest(&tamper.a.directory);
    assert_eq!(
        super::record_continuation_test(
            &tamper.b.directory,
            &forged,
            original["recorded_at_ms"].as_i64().unwrap_or(0) + 2,
        )
        .unwrap_err()
        .to_string(),
        "candidate_continuation_identity_mismatch"
    );
    assert_eq!(
        experiment_ledger::lifecycle::continuations(&tamper.b.directory)
            .unwrap()
            .len(),
        1,
        "a refused forged body overwrote the original receipt"
    );
    {
        let db = rusqlite::Connection::open(
            tamper
                .b
                .directory
                .join("model")
                .join("experiment-trials.sqlite"),
        )
        .unwrap();
        db.execute(
            "INSERT OR REPLACE INTO experiment_continuations(continuation_id,body,recorded_at_ms) VALUES(?,?,?)",
            rusqlite::params![
                forged["continuation_id"].as_str().unwrap(),
                forged.to_string(),
                forged["recorded_at_ms"].as_i64().unwrap()
            ],
        )
        .unwrap();
    }
    assert!(
        experiment_ledger::lifecycle::continuations(&tamper.b.directory).is_err(),
        "the reader accepted a forged recording session"
    );
    assert_eq!(
        directory_manifest(&tamper.a.directory),
        a_before,
        "identity verification wrote the original episode"
    );

    // (7) SOURCE AUTHORITY REFUSAL: a missing source instrument is an authority
    // refusal before and after the deadline, never a terminal unknown receipt.
    let Some(authority) = e5_three_sessions(
        root,
        "candidate-e6-authority-universe",
        base_settings,
        base_policy,
    ) else {
        panic!("authority fixture requires the sandbox");
    };
    // Remove B's already-recorded continuation so C exercises the per-episode
    // source-authority path rather than the lineage resolver.
    {
        let db = rusqlite::Connection::open(
            authority
                .b
                .directory
                .join("model")
                .join("experiment-trials.sqlite"),
        )
        .unwrap();
        db.execute("DELETE FROM experiment_continuations", [])
            .unwrap();
    }
    let universe_path = authority.a.directory.join("wide-universe.json");
    let mut universe: Value = serde_json::from_slice(&fs::read(&universe_path).unwrap()).unwrap();
    universe["instruments"] = json!([]);
    fs::write(&universe_path, serde_json::to_vec(&universe).unwrap()).unwrap();
    for at in [authority.due + 3_000, authority.due + 61_000] {
        let result = mature_candidates(&authority.c, &authority.c_policy, at).unwrap();
        let item = e2_item(&result, &authority.id).unwrap();
        assert_eq!(item["state"], "unavailable", "{result}");
        assert_eq!(
            item["reason"], "candidate_continuation_source_authority_refused",
            "{result}"
        );
        assert!(
            matches!(
                item["detail"].as_str(),
                Some("continuation_entry_unverified" | "continuation_source_instrument_scope")
            ),
            "{result}"
        );
        assert!(
            experiment_ledger::lifecycle::continuations(&authority.c.directory)
                .unwrap()
                .is_empty(),
            "authority refusal minted a CURRENT receipt: {result}"
        );
    }

    // (4) A SOURCE RESULT READ ERROR is UNAVAILABLE, not the authoritative
    // "not present" projection. The measurement must not claim a ranking-only
    // source when the original result could not be read.
    let Some(measure_fixture) = e4_continuation_fixture(
        root,
        "candidate-e6-measure-unavailable",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        panic!("measure fixture requires the sandbox");
    };
    let mut measure_queue =
        candidate_queue(&measure_fixture.source_c, &measure_fixture.source_policy);
    let measure_record =
        jobs::load_candidate(&mut measure_queue, &measure_fixture.observation_task, 1).unwrap();
    let measure_result =
        read_verified_candidate_result(&mut measure_queue, &measure_record.spec.task_id, 1)
            .unwrap()
            .unwrap();
    drop(measure_queue);
    let unavailable_measurement = super::measure::measure_observation_with_status(
        &measure_fixture.source_c,
        &measure_fixture.source_policy,
        &measure_record,
        &measure_result,
        None,
        None,
        "unavailable",
        Some("candidate_source_result_unavailable"),
        false,
        measure_fixture.due + 3_000,
    );
    assert_eq!(
        unavailable_measurement["replay"]["status"], "unavailable",
        "{unavailable_measurement}"
    );
    assert_eq!(
        unavailable_measurement["replay"]["reason"], "candidate_source_result_unavailable",
        "{unavailable_measurement}"
    );

    // (7c) A tampered original entry archive and an unverified carry are
    // authority refusals, not terminal evidence gaps.
    let Some(entry_tamper) = e5_three_sessions(
        root,
        "candidate-e6-authority-entry",
        base_settings,
        base_policy,
    ) else {
        panic!("entry tamper fixture requires the sandbox");
    };
    {
        let db = rusqlite::Connection::open(
            entry_tamper
                .b
                .directory
                .join("model")
                .join("experiment-trials.sqlite"),
        )
        .unwrap();
        db.execute("DELETE FROM experiment_continuations", [])
            .unwrap();
    }
    let mut entry_queue = rusqlite::Connection::open(&entry_tamper.a_queue).unwrap();
    let entry_record =
        jobs::load_candidate(&mut entry_queue, &entry_tamper.observation_task, 1).unwrap();
    let quotes_hash = match &entry_record.spec.purpose {
        Some(jobs::CandidatePurposeV1::Observe { target }) => target.quotes_sha256.clone(),
        _ => panic!("observation purpose missing"),
    };
    entry_queue
        .execute(
            "UPDATE candidate_artifacts SET body=? WHERE hash=?",
            rusqlite::params![b"{}".as_slice(), quotes_hash],
        )
        .unwrap();
    drop(entry_queue);
    let entry_result = mature_candidates(
        &entry_tamper.c,
        &entry_tamper.c_policy,
        entry_tamper.due + 3_000,
    )
    .unwrap();
    let entry_item = e2_item(&entry_result, &entry_tamper.id).unwrap();
    assert_eq!(entry_item["state"], "unavailable", "{entry_result}");
    assert_eq!(
        entry_item["reason"], "candidate_continuation_source_authority_refused",
        "{entry_result}"
    );
    assert!(
        matches!(
            entry_item["detail"].as_str(),
            Some("continuation_entry_unverified" | "candidate_result_unverified")
        ),
        "{entry_result}"
    );
    assert!(
        experiment_ledger::lifecycle::continuations(&entry_tamper.c.directory)
            .unwrap()
            .is_empty(),
        "a tampered entry archive minted a CURRENT receipt: {entry_result}"
    );
    let Some(carry_tamper) = e5_three_sessions(
        root,
        "candidate-e6-authority-carry",
        base_settings,
        base_policy,
    ) else {
        panic!("carry tamper fixture requires the sandbox");
    };
    let carry_path = carry_tamper.b.directory.join("carry-over.json");
    let mut carry: Value = serde_json::from_slice(&fs::read(&carry_path).unwrap()).unwrap();
    carry["source_state_hash"] = json!("0".repeat(64));
    fs::write(&carry_path, serde_json::to_vec(&carry).unwrap()).unwrap();
    let carry_result = mature_candidates(
        &carry_tamper.c,
        &carry_tamper.c_policy,
        carry_tamper.due + 3_000,
    )
    .unwrap();
    assert!(
        carry_result["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item["state"] == "unavailable" && item["detail"] == "research_carry_unverified"
            }),
        "an unverified carry was not refused: {carry_result}"
    );
    assert!(
        experiment_ledger::lifecycle::continuations(&carry_tamper.c.directory)
            .unwrap()
            .is_empty(),
        "an unverified carry minted a CURRENT receipt: {carry_result}"
    );

    // (4b) A source-result read error is UNAVAILABLE, while the candidate's own
    // verified result stays verified. An authoritative absence is not the same
    // fact and must not be collapsed into one `None`.
    let Some(context) = e4_continuation_fixture(
        root,
        "candidate-e6-candidate-context",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        panic!("candidate context fixture requires the sandbox");
    };
    let mut context_queue = rusqlite::Connection::open(&context.source_queue).unwrap();
    let observation_record =
        jobs::load_candidate(&mut context_queue, &context.observation_task, 1).unwrap();
    let authoring_task = match &observation_record.spec.purpose {
        Some(jobs::CandidatePurposeV1::Observe { target }) => target.source_task_id.clone(),
        _ => panic!("observation purpose missing"),
    };
    let authoring = jobs::load_candidate(&mut context_queue, &authoring_task, 1).unwrap();
    let authoring_result = authoring.job.result_hash.clone().unwrap();
    context_queue
        .execute(
            "UPDATE candidate_artifacts SET body=? WHERE hash=?",
            rusqlite::params![b"{}".as_slice(), authoring_result],
        )
        .unwrap();
    drop(context_queue);
    let response = candidate_result(
        &context.source_c,
        &context.source_policy,
        &context.observation_task,
        1,
        context.due + 3_000,
    )
    .unwrap();
    assert_eq!(response["result_status"], "verified", "{response}");
    assert_eq!(
        response["source_result_status"], "unavailable",
        "a source read error was projected as an authoritative absence: {response}"
    );
    assert_eq!(
        response["parent_continuation_status"], "not_recorded",
        "{response}"
    );
    assert_eq!(
        response["measurement"]["replay"]["status"], "unavailable",
        "{response}"
    );
    assert_eq!(
        response["measurement"]["replay"]["reason"], "candidate_result_unverified",
        "{response}"
    );

    // (8) An unknown continuation pins the EXACT observation task result hash,
    // never the ORIGINAL authoring candidate's result SHA carried by the score.
    let Some(unknown_fixture) = e4_continuation_fixture(
        root,
        "candidate-e6-unknown-result-hash",
        base_settings,
        base_policy,
        E4ExitMode::Missing,
    ) else {
        panic!("unknown result fixture requires the sandbox");
    };
    let unknown = mature_candidates(
        &unknown_fixture.successor_c,
        &unknown_fixture.successor_policy,
        unknown_fixture.due + 61_000,
    )
    .unwrap();
    let unknown_item = e2_item(&unknown, &unknown_fixture.observation_id).unwrap();
    assert_eq!(unknown_item["state"], "unknown", "{unknown}");
    let continuation =
        experiment_ledger::lifecycle::continuations(&unknown_fixture.successor_c.directory)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
    let mut source_queue = rusqlite::Connection::open(&unknown_fixture.source_queue).unwrap();
    let observation_record =
        jobs::load_candidate(&mut source_queue, &unknown_fixture.observation_task, 1).unwrap();
    let observation_result = observation_record.job.result_hash.clone();
    let authoring_result = match &observation_record.spec.purpose {
        Some(jobs::CandidatePurposeV1::Observe { target }) => target.source_result_sha256.clone(),
        _ => panic!("observation task purpose missing"),
    };
    drop(source_queue);
    assert_eq!(
        continuation["source_result_sha256"],
        json!(observation_result.unwrap()),
        "{continuation}"
    );
    assert_eq!(
        continuation["source_result_status"], "verified",
        "{continuation}"
    );
    assert_eq!(
        continuation["source_task_id"], unknown_fixture.observation_task,
        "{continuation}"
    );
    assert_ne!(
        continuation["source_result_sha256"],
        json!(authoring_result),
        "the unknown continuation reused the authoring result SHA: {continuation}"
    );

    // (9) A direct bounded lookup finds an item outside a truncated library
    // page; the page truncation is not a false not_matured.
    let direct = root.join("candidate-e6-direct-lookup");
    fs::create_dir_all(&direct).unwrap();
    let mut direct_ids = Vec::new();
    for index in 0..3u32 {
        let spec = experiment_ledger::lifecycle::ExperimentSpec {
            version: 1,
            parent_candidate_id: None,
            session: "2026-10-15".into(),
            source_sha256: digest(format!("e6-direct-source-{index}").as_bytes()),
            params_sha256: digest(b"e6-direct-params"),
            dataset_hash: digest(b"e6-direct-dataset"),
            feature_abi: "scorer_replay_v1".into(),
            hypothesis: format!("direct fixture {index}"),
            training_from_ms: 0,
            training_to_ms: 1,
            selection_from_ms: 2,
            selection_to_ms: 3,
            holding_ms: 1,
            max_exit_delay_ms: 0,
        };
        let admitted =
            experiment_ledger::lifecycle::admit_spec(&direct, spec, i64::from(index)).unwrap();
        direct_ids.push(admitted["candidate_id"].as_str().unwrap().to_owned());
    }
    let page = experiment_ledger::lifecycle::library_bounded(&direct, "", 1).unwrap();
    assert_eq!(page["truncated"], true, "{page}");
    let target = direct_ids.last().unwrap();
    let direct_item = experiment_ledger::lifecycle::observation_item(&direct, target)
        .unwrap()
        .unwrap();
    assert_eq!(direct_item["candidate_id"], target.as_str());

    // (7b) A missing original queue ownership anchor is an authority refusal,
    // not a terminal evidence gap.
    let Some(pin) = e5_three_sessions(
        root,
        "candidate-e6-authority-pin",
        base_settings,
        base_policy,
    ) else {
        panic!("pin fixture requires the sandbox");
    };
    {
        let db = rusqlite::Connection::open(
            pin.b
                .directory
                .join("model")
                .join("experiment-trials.sqlite"),
        )
        .unwrap();
        db.execute("DELETE FROM experiment_continuations", [])
            .unwrap();
    }
    let anchor = pin.a.directory.join("model").join(format!(
        "research-queue-{}.json",
        digest(pin.a_queue.as_os_str().as_encoded_bytes())
    ));
    fs::remove_file(&anchor).unwrap();
    let pin_result = mature_candidates(&pin.c, &pin.c_policy, pin.due + 3_000).unwrap();
    let pin_item = e2_item(&pin_result, &pin.id).unwrap();
    assert_eq!(pin_item["state"], "unavailable", "{pin_result}");
    assert_eq!(
        pin_item["reason"], "candidate_continuation_source_authority_refused",
        "{pin_result}"
    );
    assert_eq!(
        pin_item["detail"], "research_original_binding_missing",
        "{pin_result}"
    );
    assert!(
        experiment_ledger::lifecycle::continuations(&pin.c.directory)
            .unwrap()
            .is_empty(),
        "a missing queue anchor minted a CURRENT receipt: {pin_result}"
    );

    // (5) A non-empty predecessor WAL is an explicit unavailable through the
    // status/carry/trial readers, and no sidecar or source byte is touched.
    let Some(wal) = e5_three_sessions(
        root,
        "candidate-e6-source-book-wal",
        base_settings,
        base_policy,
    ) else {
        panic!("book WAL fixture requires the sandbox");
    };
    fs::write(wal.a.directory.join("paper.sqlite-wal"), b"nonempty-wal").unwrap();
    let before = directory_manifest(&wal.a.directory);
    let status = observation_status(&wal.c, &wal.c_policy, wal.due + 3_000);
    assert_eq!(status["status"], "unavailable", "{status}");
    assert_eq!(
        status["reason"], "source_book_wal_uncheckpointed",
        "{status}"
    );
    assert!(
        !wal.a.directory.join("paper.sqlite-shm").exists(),
        "an immutable predecessor read created a sidecar"
    );
    assert_eq!(
        directory_manifest(&wal.a.directory),
        before,
        "a predecessor WAL read changed source bytes"
    );
    let Some(ledger_wal) = e5_three_sessions(
        root,
        "candidate-e6-source-ledger-wal",
        base_settings,
        base_policy,
    ) else {
        panic!("ledger WAL fixture requires the sandbox");
    };
    fs::write(
        ledger_wal
            .a
            .directory
            .join("model")
            .join("experiment-trials.sqlite-wal"),
        b"nonempty-wal",
    )
    .unwrap();
    let before = directory_manifest(&ledger_wal.a.directory);
    let trials = trial_fold(&ledger_wal.c, &ledger_wal.c_policy);
    assert_eq!(trials["status"], "unavailable", "{trials}");
    assert_eq!(
        trials["reason"], "source_lifecycle_wal_uncheckpointed",
        "{trials}"
    );
    assert!(
        !ledger_wal
            .a
            .directory
            .join("model")
            .join("experiment-trials.sqlite-shm")
            .exists(),
        "an immutable predecessor ledger read created a sidecar"
    );
    assert_eq!(
        directory_manifest(&ledger_wal.a.directory),
        before,
        "a predecessor ledger WAL read changed source bytes"
    );

    // (10) ONE shared pure quote binding for EVERY status, including a
    // non-entry quote. A stale/unavailable quote cannot ride along unverified.
    let quote = json!({
        "key":"NSE_EQ|INE000000001",
        "source_ms":1_000,
        "receipt_ms":1_000,
        "event_at_ms":1_000,
        "available_at_ms":1_000,
        "bid":"100",
        "ask":"100.05",
        "bid_units":10,
        "ask_units":10,
        "lower_circuit_limit":"0",
        "upper_circuit_limit":"0",
        "volume":10,
        "purpose":"research_observation",
        "source_age_ms":0,
        "receipt_age_ms":0,
        "evidence_status":"ok",
        "archive_sha256":"a".repeat(64)
    });
    let score: sadr_cli::candidate_execution::ObservationScoreV1 = serde_json::from_value(json!({
        "version":1,
        "abi":sadr_cli::candidate_execution::OBSERVATION_ABI,
        "candidate_id":"b".repeat(64),
        "task_id":"obs-task",
        "revision":1,
        "source_task_id":"source-task",
        "source_revision":1,
        "source_candidate_id":"c".repeat(64),
        "source_result_sha256":"d".repeat(64),
        "key":"NSE_EQ|INE000000001",
        "authoring_at_ms":1,
        "target_frozen_at_ms":2,
        "features_at_ms":1_000,
        "quote_at_ms":1_000,
        "score_produced_at_ms":1_000,
        "decision_at_ms":1_000,
        "exit_due_ms":2_000,
        "quote":quote,
        "entry":null,
        "status":"entry_quote_unavailable",
        "score":1.0,
        "score_metric":"top_n_overlap",
        "score_sha256":"e".repeat(64),
        "observation_sha256":"f".repeat(64),
        "observation_scope":sadr_cli::candidate_execution::OBSERVATION_SCOPE,
        "policy_sha256":"0".repeat(64),
        "gaps":["entry_quote_unavailable"]
    }))
    .unwrap();
    let action: sadr_cli::candidate_execution::ObservationActionV1 =
        serde_json::from_value(json!({
            "key":"NSE_EQ|INE000000001",
            "entry":null,
            "quote":quote,
            "status":"entry_quote_unavailable",
            "rejection":"entry_quote_unavailable"
        }))
        .unwrap();
    assert!(
        sadr_cli::candidate_execution::validate_observation_action_binding(&score, &action).is_ok()
    );
    let mut stale = score.clone();
    stale.quote.as_mut().unwrap().bid = sadr_core::finance::Price::parse("99").unwrap();
    assert!(
        sadr_cli::candidate_execution::validate_observation_action_binding(&stale, &action)
            .is_err(),
        "a changed non-entry quote passed provenance validation"
    );
    let mut wrong_status = score.clone();
    wrong_status.status = "scored_late".into();
    assert!(
        sadr_cli::candidate_execution::validate_observation_action_binding(&wrong_status, &action)
            .is_err(),
        "a status-only match passed quote provenance validation"
    );
}

fn e7_continuation(directory: &Path) -> Value {
    experiment_ledger::lifecycle::continuations(directory)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
}

fn e7_rehash_continuation(value: &mut Value) {
    value["measurement_sha256"] =
        json!(experiment_ledger::lifecycle::continuation_content_sha256(value).unwrap());
}

fn e7_store_continuation(directory: &Path, value: &Value) {
    let db = rusqlite::Connection::open(directory.join("model").join("experiment-trials.sqlite"))
        .unwrap();
    db.execute(
        "UPDATE experiment_continuations SET body=? WHERE continuation_id=?",
        rusqlite::params![
            value.to_string(),
            value["continuation_id"].as_str().unwrap()
        ],
    )
    .unwrap();
}

fn e7_forge_observation_result(
    source_c: &Config,
    source_policy: &PlannerConfig,
    observation_task: &str,
    no_score: bool,
    fabricate_quote: bool,
) -> (Vec<u8>, String, String) {
    let mut db = candidate_queue(source_c, source_policy);
    let record = jobs::load_candidate(&mut db, observation_task, 1).unwrap();
    let result_hash = record.job.result_hash.clone().unwrap();
    let original_bytes =
        jobs::read_candidate_artifact(&db, &result_hash, "result", 1_048_576).unwrap();
    let mut result: CandidateResultV1 = serde_json::from_slice(&original_bytes).unwrap();
    let mut score = result.observation.clone().unwrap();
    let mut report = result.report.clone();
    for tick in &mut report.ticks {
        tick.candidate_top_n.clear();
    }
    if no_score {
        report.mean_top_n_overlap = None;
    }
    report.report_hash.clear();
    report.report_hash = digest(&serde_json::to_vec(&report).unwrap());
    result.report = report;
    result.report_sha256 = result.report.report_hash.clone();
    score.key = None;
    score.entry = None;
    if fabricate_quote {
        score.quote = Some(
            serde_json::from_value(json!({
                "key":"NSE_EQ|INE000000001",
                "source_ms":1_000,
                "receipt_ms":1_000,
                "event_at_ms":1_000,
                "available_at_ms":1_000,
                "bid":"100",
                "ask":"100.05",
                "bid_units":10,
                "ask_units":10,
                "lower_circuit_limit":"0",
                "upper_circuit_limit":"0",
                "volume":10,
                "purpose":"research_observation",
                "source_age_ms":0,
                "receipt_age_ms":0,
                "evidence_status":"ok",
                "archive_sha256":"a".repeat(64)
            }))
            .unwrap(),
        );
        score.quote_at_ms = Some(1_000);
    } else {
        score.quote = None;
        score.quote_at_ms = None;
    }
    score.score_sha256 = result.report.report_hash.clone();
    if no_score {
        score.status = "scored_no_score".into();
        score.score = None;
        score.gaps = vec!["scorer_score_missing_or_nonfinite".into()];
    } else {
        score.status = "scored_no_ranked_key".into();
        score.gaps = vec!["no_ranked_key".into()];
    }
    let mut score_content = score.clone();
    score_content.observation_sha256.clear();
    score.observation_sha256 =
        sadr_cli::candidate_execution::observation_artifact_sha256(&score_content).unwrap();
    result.observation = Some(score);
    let forged_bytes = serde_json::to_vec(&result).unwrap();
    let forged_hash =
        jobs::put_candidate_artifact(&mut db, "result", &forged_bytes, 10_000).unwrap();
    db.execute(
        "UPDATE jobs SET result=? WHERE id=?",
        rusqlite::params![forged_hash, record.spec.job_id],
    )
    .unwrap();
    let lease = record.job.lease.clone().unwrap();
    let stopped_bytes = worker::read_receipt(
        &db,
        &record.spec.job_id,
        &lease,
        "candidate_stopped",
        65_536,
    )
    .unwrap()
    .unwrap();
    let mut stopped: CandidateStopped = serde_json::from_slice(&stopped_bytes).unwrap();
    stopped.result_sha256 = Some(forged_hash.clone());
    db.execute(
        "UPDATE registered_worker_receipts SET body=? WHERE job=? AND lease=? AND kind='candidate_stopped'",
        rusqlite::params![
            serde_json::to_vec(&stopped).unwrap(),
            record.spec.job_id,
            lease
        ],
    )
    .unwrap();
    (original_bytes, result_hash, forged_hash)
}

fn e7_restore_observation_result(
    source_c: &Config,
    source_policy: &PlannerConfig,
    observation_task: &str,
    original_bytes: &[u8],
    original_hash: &str,
) {
    let mut db = candidate_queue(source_c, source_policy);
    let record = jobs::load_candidate(&mut db, observation_task, 1).unwrap();
    let restored = jobs::put_candidate_artifact(&mut db, "result", original_bytes, 10_000).unwrap();
    db.execute(
        "UPDATE jobs SET result=? WHERE id=?",
        rusqlite::params![restored, record.spec.job_id],
    )
    .unwrap();
    let lease = record.job.lease.clone().unwrap();
    let stopped_bytes = worker::read_receipt(
        &db,
        &record.spec.job_id,
        &lease,
        "candidate_stopped",
        65_536,
    )
    .unwrap()
    .unwrap();
    let mut stopped: CandidateStopped = serde_json::from_slice(&stopped_bytes).unwrap();
    stopped.result_sha256 = Some(original_hash.to_owned());
    db.execute(
        "UPDATE registered_worker_receipts SET body=? WHERE job=? AND lease=? AND kind='candidate_stopped'",
        rusqlite::params![
            serde_json::to_vec(&stopped).unwrap(),
            record.spec.job_id,
            lease
        ],
    )
    .unwrap();
}

fn case_e7_corrective_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let mut failures: Vec<String> = Vec::new();
    macro_rules! check {
        ($cond:expr, $($arg:tt)*) => {
            if !($cond) {
                failures.push(format!($($arg)*));
            }
        };
    }

    // (B1) A runtime change after the deadline must record a VERIFIED UNKNOWN
    // with the full pinned original source facts, and that receipt must remain
    // readable by every public reader (no lineage poison).
    let runtime_name = "candidate-e7-runtime-unknown";
    let Some(runtime) = e4_continuation_fixture(
        root,
        runtime_name,
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        assert!(
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bwrap is unavailable"
        );
        return;
    };
    let runtime_due = runtime.due;
    let runtime_id = runtime.observation_id.clone();
    let runtime_task = runtime.observation_task.clone();
    fs::write(
        runtime.successor_c.directory.join("candidate-helper"),
        b"e7-changed-successor-runtime",
    )
    .unwrap();
    let runtime_unknown = mature_candidates(
        &runtime.successor_c,
        &runtime.successor_policy,
        runtime_due + 61_000,
    )
    .unwrap();
    let runtime_item = e2_item(&runtime_unknown, &runtime_id).unwrap();
    check!(
        runtime_item["state"] == "unknown",
        "runtime change did not record unknown: {runtime_unknown}"
    );
    let runtime_cont = e7_continuation(&runtime.successor_c.directory);
    for field in [
        "source_spec_sha256",
        "source_quotes_sha256",
        "source_dataset_sha256",
        "source_book_binding",
        "source_state_hash",
        "source_universe_sha256",
        "source_cohort_key",
    ] {
        check!(
            runtime_cont[field].as_str().is_some(),
            "runtime unknown dropped verified source field {field}: {runtime_cont}"
        );
    }
    check!(
        runtime_cont["source_runtime"].is_object(),
        "runtime unknown dropped source runtime: {runtime_cont}"
    );
    check!(
        runtime_cont["current_scope"]["runtime_hash"]
            != runtime_cont["source_runtime"]["runtime_hash"],
        "runtime unknown did not record the actual current/source difference: {runtime_cont}"
    );
    check!(
        runtime_cont["exit_gap"]["kind"] == "unsupported_session_scope",
        "runtime unknown gap kind: {runtime_cont}"
    );
    check!(
        runtime_cont["exit_gap"]["reason"] == "continuation_runtime_changed",
        "runtime unknown gap reason: {runtime_cont}"
    );
    check!(
        runtime_cont["execution"].is_null()
            && runtime_cont["net_pnl_if_flat"].is_null()
            && runtime_cont["fees"].is_null()
            && runtime_cont["turnover"].is_null()
            && runtime_cont["report_sha256"].is_null(),
        "runtime unknown made a financial claim: {runtime_cont}"
    );
    let runtime_hash = runtime_cont["measurement_sha256"].clone();
    match verified_continuations(&runtime.successor_c, &runtime.successor_policy) {
        Ok(values) => {
            check!(
                values.len() == 1 && values[0]["measurement_sha256"] == runtime_hash,
                "verified_continuations lost/poisoned the runtime unknown: {values:?}"
            );
        }
        Err(error) => failures.push(format!(
            "verified_continuations poisoned by runtime unknown: {error}"
        )),
    }
    let runtime_status = observation_status(
        &runtime.successor_c,
        &runtime.successor_policy,
        runtime_due + 62_000,
    );
    check!(
        runtime_status["status"] == "available",
        "observation_status unavailable after runtime unknown: {runtime_status}"
    );
    check!(
        runtime_status["lifecycle"]["milestones"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| {
                item["candidate_id"] == runtime_id.as_str()
                    && item["continuation_state"] == "unknown"
            })),
        "observation_status lost the runtime unknown: {runtime_status}"
    );
    let source_status = observation_status(
        &runtime.source_c,
        &runtime.source_policy,
        runtime_due + 62_000,
    );
    check!(
        source_status["lifecycle"]["milestones"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| {
                item["candidate_id"] == runtime_id.as_str()
                    && item["source_observation_ref"].is_object()
            })),
        "observation_status did not expose the exact source observation ref: {source_status}"
    );
    let runtime_repeat = mature_candidates(
        &runtime.successor_c,
        &runtime.successor_policy,
        runtime_due + 62_000,
    )
    .unwrap();
    match e2_item(&runtime_repeat, &runtime_id) {
        Some(item) => check!(
            item["observation"]["measurement_sha256"] == runtime_hash,
            "repeat tick changed the runtime unknown hash: {runtime_repeat}"
        ),
        None => failures.push(format!(
            "repeat tick lost the runtime unknown item: {runtime_repeat}"
        )),
    }
    check!(
        experiment_ledger::lifecycle::continuations(&runtime.successor_c.directory)
            .unwrap()
            .len()
            == 1,
        "repeat tick duplicated the runtime unknown"
    );
    check!(
        directory_manifest(&runtime.source_c.directory) == runtime.source_before,
        "runtime unknown wrote the original episode"
    );
    // C rollover must reuse B's unknown receipt and never mint a duplicate.
    let runtime_sessions = root.join(runtime_name).join("sessions");
    let runtime_b_day = runtime.successor_day.clone();
    let runtime_c_day = "2026-10-17";
    let runtime_b_planner = runtime.successor_c.planner_config.clone().unwrap();
    let (runtime_c, runtime_c_policy) = e5_successor(
        root,
        &runtime_sessions,
        runtime_c_day,
        &runtime_b_day,
        &runtime_b_planner,
        base_settings,
        base_policy,
    );
    write_test_carry(&runtime.successor_c, &runtime_c);
    candidate_book_on(&runtime_c, runtime_c_day, &runtime_b_day);
    ensure_test_state(&runtime_c);
    let runtime_c_mature = mature_candidates(&runtime_c, &runtime_c_policy, runtime_due + 63_000);
    match runtime_c_mature {
        Ok(value) => {
            let item = e2_item(&value, &runtime_id);
            check!(
                item.is_some_and(|item| item["state"] == "unknown"
                    && item["observation"]["measurement_sha256"] == runtime_hash),
                "C lost B's runtime unknown: {value}"
            );
            check!(
                item.is_some_and(|item| item["recorded_in_session"] == runtime_b_day.as_str()),
                "C did not preserve B's recording scope: {value}"
            );
        }
        Err(error) => failures.push(format!("C could not reuse B's runtime unknown: {error}")),
    }
    check!(
        experiment_ledger::lifecycle::continuations(&runtime_c.directory)
            .unwrap()
            .is_empty(),
        "C minted a duplicate runtime unknown receipt"
    );
    let runtime_ref = match candidate_result(
        &runtime.source_c,
        &runtime.source_policy,
        &runtime_task,
        1,
        runtime_due + 62_000,
    ) {
        Ok(response) => response["source_observation_ref"].clone(),
        Err(error) => {
            failures.push(format!(
                "source candidate_result did not expose the observation ref: {error}"
            ));
            Value::Null
        }
    };
    if runtime_ref.is_object() {
        match super::candidate_result_for_observation_ref(
            &runtime.successor_c,
            &runtime.successor_policy,
            &runtime_ref,
            runtime_due + 62_000,
        ) {
            Ok(response) => {
                check!(
                    response["parent_continuation_status"] == "verified"
                        && response["parent_continuation"]["measurement_sha256"] == runtime_hash,
                    "cross-session ref did not attach the exact observation continuation: {response}"
                );
            }
            Err(error) => failures.push(format!(
                "cross-session ref failed for the exact observation episode: {error}"
            )),
        }
        match super::candidate_result_for_observation_ref(
            &runtime_c,
            &runtime_c_policy,
            &runtime_ref,
            runtime_due + 63_000,
        ) {
            Ok(response) => {
                check!(
                    response["parent_continuation_status"] == "verified"
                        && response["parent_continuation_recorded_in_session"]
                            == runtime_b_day.as_str(),
                    "C ref query lost B's recording scope: {response}"
                );
            }
            Err(error) => failures.push(format!(
                "C ref query failed for the exact observation episode: {error}"
            )),
        }
    }

    // (B1b) A forged NULL source field or a changed reason/scope must be
    // refused even with a recomputed self-hash.
    for (label, mutate) in [
        (
            "null-source",
            (|value: &mut Value| {
                value["source_spec_sha256"] = Value::Null;
            }) as fn(&mut Value),
        ),
        (
            "changed-reason",
            (|value: &mut Value| {
                value["exit_gap"]["reason"] = json!("original_no_data");
                value["gaps"] = json!(["original_no_data"]);
            }) as fn(&mut Value),
        ),
        (
            "changed-scope",
            (|value: &mut Value| {
                value["current_scope"]["runtime_hash"] =
                    value["source_runtime"]["runtime_hash"].clone();
                value["continuation_id"] =
                    json!(super::continuation_identity_sha256(value).unwrap());
            }) as fn(&mut Value),
        ),
        (
            "bad-row-timestamp",
            (|value: &mut Value| {
                let recorded = value["recorded_at_ms"].as_i64().unwrap_or(0);
                value["recorded_at_ms"] = json!(recorded + 1);
            }) as fn(&mut Value),
        ),
    ] {
        let Some(forged_fixture) = e4_continuation_fixture(
            root,
            &format!("candidate-e7-runtime-forged-{label}"),
            base_settings,
            base_policy,
            E4ExitMode::AtDue,
        ) else {
            panic!("forged runtime fixture requires the sandbox");
        };
        fs::write(
            forged_fixture
                .successor_c
                .directory
                .join("candidate-helper"),
            b"e7-changed-successor-runtime",
        )
        .unwrap();
        let _ = mature_candidates(
            &forged_fixture.successor_c,
            &forged_fixture.successor_policy,
            forged_fixture.due + 61_000,
        )
        .unwrap();
        let mut forged = e7_continuation(&forged_fixture.successor_c.directory);
        let original_id = forged["continuation_id"].as_str().unwrap().to_owned();
        mutate(&mut forged);
        e7_rehash_continuation(&mut forged);
        if label == "changed-scope" {
            // A smarter forger recomputes the identity and inserts a NEW row.
            let db = rusqlite::Connection::open(
                forged_fixture
                    .successor_c
                    .directory
                    .join("model")
                    .join("experiment-trials.sqlite"),
            )
            .unwrap();
            db.execute(
                "DELETE FROM experiment_continuations WHERE continuation_id=?",
                [&original_id],
            )
            .unwrap();
            db.execute(
                "INSERT INTO experiment_continuations(continuation_id,body,recorded_at_ms) VALUES(?,?,?)",
                rusqlite::params![
                    forged["continuation_id"].as_str().unwrap(),
                    forged.to_string(),
                    forged["recorded_at_ms"].as_i64().unwrap()
                ],
            )
            .unwrap();
        } else {
            e7_store_continuation(&forged_fixture.successor_c.directory, &forged);
        }
        check!(
            verified_continuations(
                &forged_fixture.successor_c,
                &forged_fixture.successor_policy
            )
            .is_err(),
            "forged {label} runtime receipt was accepted"
        );
    }

    // (B1c) A cohort change is a distinct verified UNKNOWN with pinned source
    // facts, not a poisoned or refused receipt.
    let Some(cohort) = e4_continuation_fixture(
        root,
        "candidate-e7-cohort-unknown",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        panic!("cohort fixture requires the sandbox");
    };
    let cohort_due = cohort.due;
    let cohort_id = cohort.observation_id.clone();
    let cohort_universe_path = cohort.successor_c.directory.join("wide-universe.json");
    let mut cohort_universe: Value =
        serde_json::from_slice(&fs::read(&cohort_universe_path).unwrap()).unwrap();
    cohort_universe["instruments"]
        .as_array_mut()
        .unwrap()
        .push(json!({"key":"NSE_EQ|INE000000003","symbol":"CCC","series":"EQ","tick":"0.05"}));
    fs::write(
        &cohort_universe_path,
        serde_json::to_vec(&cohort_universe).unwrap(),
    )
    .unwrap();
    let cohort_unknown = mature_candidates(
        &cohort.successor_c,
        &cohort.successor_policy,
        cohort_due + 61_000,
    )
    .unwrap();
    check!(
        e2_item(&cohort_unknown, &cohort_id).is_some_and(|item| item["state"] == "unknown"),
        "cohort change did not record unknown: {cohort_unknown}"
    );
    let cohort_cont = e7_continuation(&cohort.successor_c.directory);
    check!(
        cohort_cont["source_universe_sha256"].as_str().is_some()
            && cohort_cont["source_cohort_key"].as_str().is_some()
            && cohort_cont["source_runtime"].is_object(),
        "cohort unknown dropped verified source facts: {cohort_cont}"
    );
    check!(
        cohort_cont["exit_gap"]["reason"] == "continuation_cohort_changed",
        "cohort unknown gap reason: {cohort_cont}"
    );
    check!(
        cohort_cont["current_scope"]["cohort_key"] != cohort_cont["source_cohort_key"],
        "cohort unknown did not record the actual difference: {cohort_cont}"
    );
    match verified_continuations(&cohort.successor_c, &cohort.successor_policy) {
        Ok(values) => check!(
            values.len() == 1,
            "cohort unknown was not readable: {values:?}"
        ),
        Err(error) => failures.push(format!("cohort unknown poisoned lineage: {error}")),
    }
    check!(
        directory_manifest(&cohort.source_c.directory) == cohort.source_before,
        "cohort unknown wrote the original episode"
    );

    // (B2) A valid Protection exit is explicit authority, not a research-only
    // rejection. The full read path must verify it.
    let Some(protection) = e4_continuation_fixture(
        root,
        "candidate-e7-protection-exit",
        base_settings,
        base_policy,
        E4ExitMode::ProtectionAtDue,
    ) else {
        panic!("protection fixture requires the sandbox");
    };
    let protection_due = protection.due;
    let protection_id = protection.observation_id.clone();
    let protection_task = protection.observation_task.clone();
    let protection_mature = mature_candidates(
        &protection.successor_c,
        &protection.successor_policy,
        protection_due + 3_000,
    )
    .unwrap();
    match e2_item(&protection_mature, &protection_id) {
        Some(item) => {
            check!(
                item["state"] == "evaluated",
                "valid protection exit was not evaluated: {protection_mature}"
            );
            check!(
                item["observation"]["exit_quote"]["purpose"] == "protection",
                "protection exit purpose was not preserved: {protection_mature}"
            );
            check!(
                item["observation"]["execution"].is_object()
                    && item["observation"]["net_pnl_if_flat"].is_string(),
                "valid protection exit did not produce a verified financial measurement: {protection_mature}"
            );
        }
        None => failures.push(format!(
            "valid protection exit item missing: {protection_mature}"
        )),
    }
    match verified_continuations(&protection.successor_c, &protection.successor_policy) {
        Ok(values) => check!(
            values.len() == 1 && values[0]["exit_quote"]["purpose"] == "protection",
            "verified_continuations rejected/rewrote the protection exit: {values:?}"
        ),
        Err(error) => failures.push(format!("valid protection exit was refused: {error}")),
    }
    let protection_ref = match candidate_result(
        &protection.source_c,
        &protection.source_policy,
        &protection_task,
        1,
        protection_due + 3_000,
    ) {
        Ok(response) => response["source_observation_ref"].clone(),
        Err(error) => {
            failures.push(format!(
                "source candidate_result did not expose the protection ref: {error}"
            ));
            Value::Null
        }
    };
    if protection_ref.is_object() {
        match super::candidate_result_for_observation_ref(
            &protection.successor_c,
            &protection.successor_policy,
            &protection_ref,
            protection_due + 3_000,
        ) {
            Ok(response) => check!(
                response["parent_continuation_status"] == "verified"
                    && response["parent_continuation"]["exit_quote"]["purpose"] == "protection",
                "cross-session ref lost the protection continuation: {response}"
            ),
            Err(error) => failures.push(format!(
                "cross-session ref failed for the protection episode: {error}"
            )),
        }
    }
    check!(
        directory_manifest(&protection.source_c.directory) == protection.source_before,
        "protection read wrote the original episode"
    );

    // (B2b) Financial self-attestation: forge ONLY a financial field, keep the
    // source refs/scope/identity and recompute the measurement self-hash. The
    // shared engine re-derivation must refuse it.
    let Some(financial) = e4_continuation_fixture(
        root,
        "candidate-e7-financial-forgery",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        panic!("financial fixture requires the sandbox");
    };
    let financial_due = financial.due;
    let financial_task = financial.observation_task.clone();
    let _ = mature_candidates(
        &financial.successor_c,
        &financial.successor_policy,
        financial_due + 3_000,
    )
    .unwrap();
    let financial_original = e7_continuation(&financial.successor_c.directory);
    for (label, mutate) in [
        (
            "net-pnl",
            (|value: &mut Value| {
                value["net_pnl_if_flat"] = json!("999.00");
            }) as fn(&mut Value),
        ),
        (
            "fees",
            (|value: &mut Value| {
                value["fees"] = json!("999.00");
            }) as fn(&mut Value),
        ),
        (
            "turnover",
            (|value: &mut Value| {
                value["turnover"] = json!("999.00");
            }) as fn(&mut Value),
        ),
        (
            "execution",
            (|value: &mut Value| {
                value["execution"]["fees"] = json!("999.00");
            }) as fn(&mut Value),
        ),
        (
            "report-sha",
            (|value: &mut Value| {
                value["report_sha256"] = json!("a".repeat(64));
            }) as fn(&mut Value),
        ),
    ] {
        let mut forged = financial_original.clone();
        mutate(&mut forged);
        e7_rehash_continuation(&mut forged);
        e7_store_continuation(&financial.successor_c.directory, &forged);
        check!(
            verified_continuations(&financial.successor_c, &financial.successor_policy).is_err(),
            "forged financial field {label} passed verified_continuations"
        );
        if let Ok(response) = candidate_result(
            &financial.successor_c,
            &financial.successor_policy,
            &financial_task,
            1,
            financial_due + 4_000,
        ) {
            check!(
                response["measurement"]["episodes"][0]["after_cost"].is_null(),
                "forged financial field {label} was consumed as PnL: {response}"
            );
        }
        e7_store_continuation(&financial.successor_c.directory, &financial_original);
    }
    // The public writer refuses a different body for the same identity.
    let mut writer_forged = financial_original.clone();
    writer_forged["net_pnl_if_flat"] = json!("999.00");
    e7_rehash_continuation(&mut writer_forged);
    check!(
        super::record_continuation_checked(
            &financial.successor_c,
            &financial.successor_policy,
            &super::predecessor_chain_verified_immutable(&financial.successor_c).unwrap(),
            &writer_forged,
            financial_due + 4_000,
        )
        .is_err(),
        "record_continuation accepted a forged financial body"
    );
    check!(
        e7_continuation(&financial.successor_c.directory)["net_pnl_if_flat"]
            == financial_original["net_pnl_if_flat"],
        "record_continuation overwrote the verified financial body"
    );

    // (B2c) Exit provenance: forge one typed exit field while keeping the
    // original archive SHA and recomputed self-hash. The full re-derivation
    // must refuse every field.
    for (label, mutate) in [
        (
            "bid",
            (|value: &mut Value| {
                value["exit_quote"]["bid"] = json!("101");
            }) as fn(&mut Value),
        ),
        (
            "ask",
            (|value: &mut Value| {
                value["exit_quote"]["ask"] = json!("101.05");
            }) as fn(&mut Value),
        ),
        (
            "bid-units",
            (|value: &mut Value| {
                value["exit_quote"]["bid_units"] = json!(99);
            }) as fn(&mut Value),
        ),
        (
            "ask-units",
            (|value: &mut Value| {
                value["exit_quote"]["ask_units"] = json!(99);
            }) as fn(&mut Value),
        ),
        (
            "lower-circuit",
            (|value: &mut Value| {
                value["exit_quote"]["lower_circuit_limit"] = json!("79");
            }) as fn(&mut Value),
        ),
        (
            "upper-circuit",
            (|value: &mut Value| {
                value["exit_quote"]["upper_circuit_limit"] = json!("121");
            }) as fn(&mut Value),
        ),
        (
            "volume",
            (|value: &mut Value| {
                value["exit_quote"]["volume"] = json!(999_999);
            }) as fn(&mut Value),
        ),
        (
            "source-ms",
            (|value: &mut Value| {
                let at = value["exit_quote"]["source_ms"].as_i64().unwrap();
                value["exit_quote"]["source_ms"] = json!(at + 1);
            }) as fn(&mut Value),
        ),
        (
            "receipt-ms",
            (|value: &mut Value| {
                let at = value["exit_quote"]["receipt_ms"].as_i64().unwrap();
                value["exit_quote"]["receipt_ms"] = json!(at + 1);
            }) as fn(&mut Value),
        ),
        (
            "event-at",
            (|value: &mut Value| {
                let at = value["exit_quote"]["event_at_ms"].as_i64().unwrap();
                value["exit_quote"]["event_at_ms"] = json!(at + 1);
            }) as fn(&mut Value),
        ),
        (
            "available-at",
            (|value: &mut Value| {
                let at = value["exit_quote"]["available_at_ms"].as_i64().unwrap();
                value["exit_quote"]["available_at_ms"] = json!(at + 1);
            }) as fn(&mut Value),
        ),
        (
            "symbol",
            (|value: &mut Value| {
                value["exit_quote"]["symbol"] = json!("ZZZ");
            }) as fn(&mut Value),
        ),
        (
            "series",
            (|value: &mut Value| {
                value["exit_quote"]["series"] = json!("BE");
            }) as fn(&mut Value),
        ),
        (
            "tick",
            (|value: &mut Value| {
                value["exit_quote"]["tick"] = json!("0.10");
            }) as fn(&mut Value),
        ),
        (
            "universe",
            (|value: &mut Value| {
                value["exit_quote"]["universe_sha256"] = json!("a".repeat(64));
            }) as fn(&mut Value),
        ),
        (
            "purpose",
            (|value: &mut Value| {
                value["exit_quote"]["purpose"] = json!("protection");
            }) as fn(&mut Value),
        ),
        (
            "key",
            (|value: &mut Value| {
                value["exit_quote"]["key"] = json!("NSE_EQ|INF000000002");
            }) as fn(&mut Value),
        ),
    ] {
        let mut forged = financial_original.clone();
        mutate(&mut forged);
        e7_rehash_continuation(&mut forged);
        e7_store_continuation(&financial.successor_c.directory, &forged);
        check!(
            verified_continuations(&financial.successor_c, &financial.successor_policy).is_err(),
            "forged exit field {label} passed verified_continuations"
        );
        e7_store_continuation(&financial.successor_c.directory, &financial_original);
    }

    // (B5) A quote-query failure is an explicit unavailable/deferred unknown,
    // never original_no_data.
    let Some(query) = e4_continuation_fixture(
        root,
        "candidate-e7-query-failure",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        panic!("query fixture requires the sandbox");
    };
    let query_due = query.due;
    let query_id = query.observation_id.clone();
    {
        let db = super::super::super::open_book(&query.source_c).unwrap();
        db.execute(
            "UPDATE ticks SET body=? WHERE at=? AND event='research_quotes'",
            rusqlite::params![
                br#"{"purpose":"research_observation","bad":"#.as_slice(),
                query_due
            ],
        )
        .unwrap();
    }
    let query_before = directory_manifest(&query.source_c.directory);
    let query_grace = mature_candidates(
        &query.successor_c,
        &query.successor_policy,
        query_due + 3_000,
    )
    .unwrap();
    match e2_item(&query_grace, &query_id) {
        Some(item) => check!(
            item["state"] == "pending_capture" && item["gap"]["kind"] == "quote_query_unavailable",
            "query failure was not deferred with its typed cause: {query_grace}"
        ),
        None => failures.push(format!("query grace item missing: {query_grace}")),
    }
    let query_unknown = mature_candidates(
        &query.successor_c,
        &query.successor_policy,
        query_due + 61_000,
    )
    .unwrap();
    match e2_item(&query_unknown, &query_id) {
        Some(item) => {
            check!(
                item["state"] == "unknown",
                "query failure did not record unknown: {query_unknown}"
            );
            check!(
                item["observation"]["exit_status"] == "exit_measurement_unavailable",
                "query failure exit status: {query_unknown}"
            );
            check!(
                item["observation"]["exit_gap"]["kind"] == "quote_query_unavailable"
                    && item["observation"]["gaps"].as_array().is_some_and(|gaps| {
                        gaps.iter().any(|gap| gap == "original_query_unavailable")
                    }),
                "query failure collapsed into a NoData gap: {query_unknown}"
            );
            check!(
                !item["observation"]["gaps"]
                    .as_array()
                    .is_some_and(|gaps| gaps.iter().any(|gap| gap == "original_no_data")),
                "query failure asserted original_no_data: {query_unknown}"
            );
        }
        None => failures.push(format!("query unknown item missing: {query_unknown}")),
    }
    match verified_continuations(&query.successor_c, &query.successor_policy) {
        Ok(values) => check!(
            values.len() == 1,
            "query unknown was not readable: {values:?}"
        ),
        Err(error) => failures.push(format!("query unknown poisoned lineage: {error}")),
    }
    check!(
        directory_manifest(&query.source_c.directory) == query_before,
        "query unknown read wrote the original episode"
    );

    // (MED) A forged no-entry artifact with a fabricated quote and a recomputed
    // self-hash must be refused by the public verified-result reader.
    let Some(no_entry) = e4_continuation_fixture(
        root,
        "candidate-e7-no-entry-forgery",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        panic!("no-entry fixture requires the sandbox");
    };
    for no_score in [false, true] {
        let (original_bytes, original_hash, _forged_hash) = e7_forge_observation_result(
            &no_entry.source_c,
            &no_entry.source_policy,
            &no_entry.observation_task,
            no_score,
            true,
        );
        let mut db = candidate_queue(&no_entry.source_c, &no_entry.source_policy);
        check!(
            read_verified_candidate_result(&mut db, &no_entry.observation_task, 1).is_err(),
            "forged no-entry artifact (no_score={no_score}) passed read_verified_candidate_result"
        );
        drop(db);
        check!(
            candidate_result(
                &no_entry.source_c,
                &no_entry.source_policy,
                &no_entry.observation_task,
                1,
                no_entry.due + 3_000,
            )
            .is_err(),
            "forged no-entry artifact (no_score={no_score}) passed candidate_result"
        );
        e7_restore_observation_result(
            &no_entry.source_c,
            &no_entry.source_policy,
            &no_entry.observation_task,
            &original_bytes,
            &original_hash,
        );
    }
    // A legitimate no-entry score is still valid and must not fabricate a zero.
    for no_score in [false, true] {
        let (legit_bytes, legit_hash, _) = e7_forge_observation_result(
            &no_entry.source_c,
            &no_entry.source_policy,
            &no_entry.observation_task,
            no_score,
            false,
        );
        let mut db = candidate_queue(&no_entry.source_c, &no_entry.source_policy);
        match read_verified_candidate_result(&mut db, &no_entry.observation_task, 1) {
            Ok(_) => {}
            Err(error) => failures.push(format!(
                "legitimate no-entry score (no_score={no_score}) was falsely refused: {error}"
            )),
        }
        drop(db);
        e7_restore_observation_result(
            &no_entry.source_c,
            &no_entry.source_policy,
            &no_entry.observation_task,
            &legit_bytes,
            &legit_hash,
        );
    }

    // (LOW) Capture admission must check the local session DATE before any
    // provider call, and an empty readable journal is not "present".
    let Some(capture) = e4_continuation_fixture(
        root,
        "candidate-e7-capture-admission",
        base_settings,
        base_policy,
        E4ExitMode::Missing,
    ) else {
        panic!("capture fixture requires the sandbox");
    };
    let capture_cfg = {
        let mut cfg = capture.successor_policy.clone();
        cfg.owner_tasks = Some(crate::runtime::owner_tasks::OwnerTasksSettings {
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
                        timeout_ms: 5_000,
                        max_rows: 64,
                    },
                ),
                lane: Some(Default::default()),
            }),
        });
        cfg
    };
    let stale_day = "2026-10-18";
    let stale_admission = candidate_at_on(stale_day, 9, 23);
    let called = std::cell::Cell::new(false);
    let mut db = super::super::super::open_book(&capture.successor_c).unwrap();
    let portfolio = sadr_core::execution::Portfolio::with_capital(
        sadr_core::finance::Amount::parse("5000").unwrap(),
    );
    let stale = super::capture::tick_with(
        &mut db,
        &capture.successor_c,
        &capture_cfg,
        &portfolio,
        stale_admission,
        || stale_admission,
        |_| {
            called.set(true);
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: stale_admission,
                receipt_ms: stale_admission,
                body: candidate_quote_body(stale_admission).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    check!(
        stale["status"] == "not_eligible"
            && stale["reason"] == "observation_capture_admission_session_date_mismatch",
        "stale-day admission was not refused before the provider: {stale}"
    );
    check!(
        !called.get(),
        "stale-day admission called the provider before the date check"
    );
    drop(db);
    let boundary_admission = candidate_at_on(&capture.successor_day, 15, 29) + 58_000;
    let boundary_receipt = candidate_at_on(&capture.successor_day, 15, 30) + 3_000;
    let mut db = super::super::super::open_book(&capture.successor_c).unwrap();
    let ticks_before_boundary: i64 = db
        .query_row(
            "SELECT count(*) FROM ticks WHERE event='research_quotes'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let boundary = super::capture::tick_with(
        &mut db,
        &capture.successor_c,
        &capture_cfg,
        &portfolio,
        boundary_admission,
        || boundary_receipt,
        |_| {
            Ok(crate::runtime::data::BoundedQuote {
                requested_ms: boundary_admission,
                receipt_ms: boundary_receipt,
                body: candidate_quote_body(boundary_receipt).to_string(),
                request_attempted: true,
                provider_called: Some(true),
            })
        },
    );
    check!(
        boundary["status"] == "refused"
            && boundary["reason"] == "observation_capture_receipt_outside_session"
            && boundary["reason"] != "timeout",
        "a post-close receipt was not a session refusal: {boundary}"
    );
    let ticks_after_boundary: i64 = db
        .query_row(
            "SELECT count(*) FROM ticks WHERE event='research_quotes'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    check!(
        ticks_before_boundary == ticks_after_boundary,
        "a post-close receipt wrote a tick"
    );
    drop(db);
    let empty_status = super::capture::status(&capture.successor_c, &capture_cfg, capture.due);
    check!(
        empty_status["receipts"] == "none",
        "an empty readable journal claimed receipts present: {empty_status}"
    );
    check!(
        empty_status["coverage"] == "journal_readable"
            && empty_status["current_eligibility"] == "not_evaluated",
        "capture status conflated journal readability with eligibility: {empty_status}"
    );

    // (LOW) Measurement trial projection must copy the actual known gaps,
    // sessions and coverage scopes, not drop them.
    let mut measure_db = candidate_queue(&no_entry.source_c, &no_entry.source_policy);
    let measure_record =
        jobs::load_candidate(&mut measure_db, &no_entry.observation_task, 1).unwrap();
    match read_verified_candidate_result(&mut measure_db, &no_entry.observation_task, 1) {
        Ok(Some(measure_result)) => {
            drop(measure_db);
            let measurement = super::measure::measure_observation(
                &no_entry.source_c,
                &no_entry.source_policy,
                &measure_record,
                &measure_result,
                None,
                None,
                no_entry.due + 3_000,
            );
            for field in [
                "gaps",
                "reason",
                "distinct_source_coverage",
                "distinct_lifecycle_coverage",
                "sessions",
            ] {
                check!(
                    measurement["trials"].get(field).is_some(),
                    "measure.trials dropped {field}: {measurement}"
                );
            }
        }
        Ok(None) => {
            drop(measure_db);
            failures.push("legitimate observation result disappeared".into());
        }
        Err(error) => {
            drop(measure_db);
            failures.push(format!(
                "legitimate observation result was refused: {error}"
            ));
        }
    }

    // (LOW) A session-filtered bounded library must not report truncation for
    // rows belonging to other sessions.
    let filtered = root.join("candidate-e7-library-filter");
    fs::create_dir_all(&filtered).unwrap();
    let target_session = "2026-10-16";
    let other_session = "2026-10-15";
    let spec_for = |session: &str, source: &str| experiment_ledger::lifecycle::ExperimentSpec {
        version: 1,
        parent_candidate_id: None,
        session: session.into(),
        source_sha256: digest(source.as_bytes()),
        params_sha256: digest(b"e7-filter-params"),
        dataset_hash: digest(b"e7-filter-dataset"),
        feature_abi: "scorer_replay_v1".into(),
        hypothesis: "e7 filter fixture".into(),
        training_from_ms: 0,
        training_to_ms: 1,
        selection_from_ms: 2,
        selection_to_ms: 3,
        holding_ms: 1,
        max_exit_delay_ms: 0,
    };
    experiment_ledger::lifecycle::admit_spec(
        &filtered,
        spec_for(target_session, "e7-filter-target"),
        1,
    )
    .unwrap();
    {
        let db =
            rusqlite::Connection::open(filtered.join("model").join("experiment-trials.sqlite"))
                .unwrap();
        db.execute("DELETE FROM experiment_lifecycle_specs", [])
            .unwrap();
        db.execute("DELETE FROM experiment_lifecycle_state", [])
            .unwrap();
        let target_id = "a".repeat(64);
        let other_id = "f".repeat(64);
        let projection = json!({
            "version":1,
            "marks":[],
            "edge_qualified":false,
            "activation_authorized":false
        })
        .to_string();
        for (id, session, source) in [
            (&target_id, target_session, "e7-filter-target"),
            (&other_id, other_session, "e7-filter-other"),
        ] {
            db.execute(
                "INSERT OR REPLACE INTO experiment_lifecycle_specs(candidate_id,body,recorded_at_ms) VALUES(?,?,?)",
                rusqlite::params![id, serde_json::to_string(&spec_for(session, source)).unwrap(), 1],
            )
            .unwrap();
            db.execute(
                "INSERT OR REPLACE INTO experiment_lifecycle_state(candidate_id,state,projection,updated_at_ms) VALUES(?,?,?,?)",
                rusqlite::params![id, "proposed", projection, 1],
            )
            .unwrap();
        }
    }
    let filtered_page =
        experiment_ledger::lifecycle::library_bounded(&filtered, target_session, 1).unwrap();
    check!(
        filtered_page["items"].as_array().map(Vec::len) == Some(1)
            && filtered_page["truncated"] == false,
        "session-filtered library falsely reported truncation: {filtered_page}"
    );

    // (B4) A LEGACY ledger without lifecycle tables is an EMPTY library only
    // when the queue proves no admissions. With real admissions it is
    // ambiguous: no earlier episode may be re-matured and every public reader
    // must stay conservative.
    let Some(legacy) = e5_three_sessions(
        root,
        "candidate-e7-legacy-ambiguous",
        base_settings,
        base_policy,
    ) else {
        panic!("legacy fixture requires the sandbox");
    };
    let legacy_ledger = legacy
        .a
        .directory
        .join("model")
        .join("experiment-trials.sqlite");
    fs::remove_file(&legacy_ledger).unwrap();
    {
        let db = rusqlite::Connection::open(&legacy_ledger).unwrap();
        db.execute_batch(
            "CREATE TABLE experiment_trials(
               trial_id TEXT PRIMARY KEY,
               body TEXT NOT NULL,
               recorded_at_ms INTEGER NOT NULL
             );
             CREATE TABLE experiment_trial_events(
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               trial_id TEXT NOT NULL,
               event TEXT NOT NULL,
               at_ms INTEGER NOT NULL
             );",
        )
        .unwrap();
        db.execute(
            "INSERT INTO experiment_trials(trial_id,body,recorded_at_ms) VALUES(?,?,?)",
            rusqlite::params!["legacy-trial", "{}", 1],
        )
        .unwrap();
    }
    let legacy_a_before = directory_manifest(&legacy.a.directory);
    let legacy_mature = mature_candidates(&legacy.c, &legacy.c_policy, legacy.due + 5_000).unwrap();
    check!(
        legacy_mature["items"].as_array().is_some_and(|items| items
            .iter()
            .any(|item| item["detail"] == "candidate_predecessor_lifecycle_legacy_ambiguous")),
        "legacy ledger with admissions was treated as a known zero: {legacy_mature}"
    );
    check!(
        experiment_ledger::lifecycle::continuations(&legacy.c.directory)
            .unwrap()
            .is_empty(),
        "C re-matured an older episode behind an ambiguous legacy ledger"
    );
    let legacy_trials = trial_fold(&legacy.c, &legacy.c_policy);
    check!(
        legacy_trials["status"] == "partial"
            && legacy_trials["global_tested_trial_count"].is_null()
            && legacy_trials["gaps"].as_array().is_some_and(|gaps| gaps
                .iter()
                .any(|gap| gap == "trial_lifecycle_legacy_ambiguous")),
        "trial fold skipped the ambiguous legacy ledger: {legacy_trials}"
    );
    let legacy_status = observation_status(&legacy.c, &legacy.c_policy, legacy.due + 5_000);
    check!(
        legacy_status["status"] == "unavailable"
            && legacy_status["reason"] == "candidate_predecessor_lifecycle_legacy_ambiguous",
        "observation status skipped the ambiguous legacy ledger: {legacy_status}"
    );
    check!(
        verified_continuations(&legacy.c, &legacy.c_policy).is_err(),
        "verified_continuations accepted an ambiguous legacy prefix"
    );
    check!(
        directory_manifest(&legacy.a.directory) == legacy_a_before,
        "ambiguous legacy prefix read changed A"
    );

    // (B3) An arbitrary sibling observation id must never be substituted for
    // the exact recorded observation episode.
    if runtime_ref.is_object() {
        let mut wrong = runtime_ref.clone();
        wrong["observation_lifecycle_candidate_id"] = json!("b".repeat(64));
        check!(
            super::candidate_result_for_observation_ref(
                &runtime.successor_c,
                &runtime.successor_policy,
                &wrong,
                runtime_due + 62_000,
            )
            .is_err(),
            "an arbitrary sibling observation id was accepted"
        );
    }

    // (LOW) Predecessor WAL is an explicit refusal for every public reader and
    // never creates a sidecar or changes source bytes.
    let Some(wal) = e5_three_sessions(root, "candidate-e7-wal-readers", base_settings, base_policy)
    else {
        panic!("wal fixture requires the sandbox");
    };
    let wal_ref = {
        let mut db = rusqlite::Connection::open(&wal.a_queue).unwrap();
        let task = jobs::load_candidate(&mut db, &wal.observation_task, 1).unwrap();
        let authoring_task = match &task.spec.purpose {
            Some(jobs::CandidatePurposeV1::Observe { target }) => target.source_task_id.clone(),
            _ => panic!("observation purpose missing"),
        };
        let authoring = jobs::load_candidate(&mut db, &authoring_task, 1).unwrap();
        let reference =
            super::carry::host_parent_ref(&wal.a, base_policy, &mut db, &authoring).unwrap();
        serde_json::from_value::<jobs::CandidateParentRefV1>(reference["ref"].clone()).unwrap()
    };
    // The ref-building read is a CURRENT same-session read of A; it may create
    // an SQLite -shm sidecar. Remove that test-created sidecar before taking
    // the immutable WAL snapshot so the assertion measures the readers below.
    let _ = fs::remove_file(wal.a.directory.join("paper.sqlite-shm"));
    let b_policy: PlannerConfig =
        serde_json::from_slice(&fs::read(wal.b.planner_config.as_ref().unwrap()).unwrap()).unwrap();
    let _ = super::open_queue_for_owner(
        b_policy.research_tools.as_ref().unwrap(),
        &wal.b.directory.join("model"),
        true,
    )
    .unwrap();
    // A CURRENT candidate with an explicit cross-session parent forces
    // carry_state to verify the original source book. The source WAL must be
    // refused there, not silently read through a normal RO connection.
    let mut wal_task_id: Option<String> = None;
    let mut wal_args = candidate_submit_args("e7-wal-candidate", CANDIDATE_SOURCE, None);
    wal_args.parent_ref = Some(Box::new(wal_ref.clone()));
    wal_args.horizon_minutes = 1;
    wal_args.training_from_ms = candidate_at_on(&wal.b_day, 9, 20);
    wal_args.training_to_ms = candidate_at_on(&wal.b_day, 9, 20);
    wal_args.selection_from_ms = candidate_at_on(&wal.b_day, 9, 21);
    wal_args.selection_to_ms = candidate_at_on(&wal.b_day, 9, 22);
    let wal_submit = candidate_submit(
        &wal.b,
        &b_policy,
        "grok-4.7",
        3,
        "turn-e7-wal",
        wal_args,
        candidate_at_on(&wal.b_day, 9, 23),
        Some(candidate_evidence()),
    )
    .unwrap();
    check!(
        wal_submit["status"] == "queued",
        "WAL parent candidate was not queued: {wal_submit}"
    );
    if wal_submit["status"] == "queued" {
        let wal_task = wal_submit["task"]["task_id"].as_str().unwrap().to_owned();
        let wal_at = candidate_at_on(&wal.b_day, 9, 23);
        candidate_drive(&wal.b, &b_policy, &wal_task, move || wal_at + 10);
        let mut db = candidate_queue(&wal.b, &b_policy);
        let record = jobs::load_candidate(&mut db, &wal_task, 1).unwrap();
        let _ = reconcile_candidate(&wal.b, &b_policy, &mut db, &record);
        wal_task_id = Some(wal_task);
    }
    fs::write(wal.a.directory.join("paper.sqlite-wal"), b"e7-nonempty-wal").unwrap();
    let wal_before = directory_manifest(&wal.a.directory);
    let carry = super::carry::carry_state(&wal.b, &b_policy);
    check!(
        carry["status"] == "unavailable" && carry["reason"] == "source_book_wal_uncheckpointed"
            || carry["candidates"].as_array().is_some_and(|candidates| {
                candidates.iter().any(|candidate| {
                    candidate["parent_ref_status"] == "unavailable"
                        && candidate["parent_ref_reason"] == "source_book_wal_uncheckpointed"
                })
            }),
        "carry_state did not refuse the source WAL: {carry}"
    );
    check!(
        super::carry::resolve_parent_ref(&wal.b, &b_policy, &wal_ref).is_err(),
        "resolve_parent_ref accepted a source with a non-empty WAL"
    );
    check!(
        super::carry::verify_source_lifecycle(&wal.b, &wal_ref).is_err(),
        "verify_source_lifecycle accepted a source with a non-empty WAL"
    );
    if let Some(task_id) = &wal_task_id {
        match candidate_status(
            &wal.b,
            &b_policy,
            task_id,
            1,
            candidate_at_on(&wal.b_day, 9, 24),
        ) {
            Ok(status) => check!(
                status["parent_ref_status"] == "unavailable"
                    && status["parent_ref_reason"] == "source_book_wal_uncheckpointed",
                "candidate_status silently omitted the parent-ref error: {status}"
            ),
            Err(error) => failures.push(format!(
                "candidate_status failed instead of exposing the parent-ref error: {error}"
            )),
        }
    }
    let wal_observation = observation_status(&wal.b, &b_policy, candidate_at_on(&wal.b_day, 9, 24));
    check!(
        wal_observation["status"] == "unavailable"
            && wal_observation["reason"] == "source_book_wal_uncheckpointed",
        "observation_status did not refuse the source WAL: {wal_observation}"
    );
    check!(
        !wal.a.directory.join("paper.sqlite-shm").exists(),
        "a predecessor WAL read created a sidecar"
    );
    check!(
        directory_manifest(&wal.a.directory) == wal_before,
        "a predecessor WAL read changed source bytes"
    );

    assert!(
        failures.is_empty(),
        "E7 corrective contract failures ({}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn case_e8_corrective_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let mut failures: Vec<String> = Vec::new();
    macro_rules! check {
        ($cond:expr, $($arg:tt)*) => {
            if !($cond) {
                failures.push(format!($($arg)*));
            }
        };
    }

    // (E8-1) FIRST INSERT: an empty continuation table must refuse a forged
    // body with the correct identity/self-hash and a valid source exit quote.
    let Some(writer) = e4_continuation_fixture(
        root,
        "candidate-e8-writer",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        assert!(
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bwrap is unavailable"
        );
        return;
    };
    let writer_due = writer.due;
    let writer_id = writer.observation_id.clone();
    let writer_chain = super::predecessor_chain_verified_immutable(&writer.successor_c).unwrap();
    let _ = mature_candidates(
        &writer.successor_c,
        &writer.successor_policy,
        writer_due + 3_000,
    )
    .unwrap();
    let genuine = e7_continuation(&writer.successor_c.directory);
    let genuine_hash = genuine["measurement_sha256"].clone();
    {
        let db = rusqlite::Connection::open(
            writer
                .successor_c
                .directory
                .join("model")
                .join("experiment-trials.sqlite"),
        )
        .unwrap();
        db.execute("DELETE FROM experiment_continuations", [])
            .unwrap();
    }
    check!(
        experiment_ledger::lifecycle::continuations(&writer.successor_c.directory)
            .unwrap()
            .is_empty(),
        "test setup did not empty the continuation table"
    );
    let mut forged = genuine.clone();
    forged["net_pnl_if_flat"] = json!("999.00");
    e7_rehash_continuation(&mut forged);
    check!(
        super::record_continuation_checked(
            &writer.successor_c,
            &writer.successor_policy,
            &writer_chain,
            &forged,
            writer_due + 4_000,
        )
        .is_err(),
        "first-insert forged PnL body was accepted"
    );
    check!(
        experiment_ledger::lifecycle::continuations(&writer.successor_c.directory)
            .unwrap()
            .is_empty(),
        "forged first insert minted a continuation row"
    );
    let inserted = super::record_continuation_checked(
        &writer.successor_c,
        &writer.successor_policy,
        &writer_chain,
        &genuine,
        writer_due + 4_000,
    )
    .unwrap();
    check!(
        inserted["measurement_sha256"] == genuine_hash,
        "valid first insert changed the receipt: {inserted}"
    );
    let again = super::record_continuation_checked(
        &writer.successor_c,
        &writer.successor_policy,
        &writer_chain,
        &genuine,
        writer_due + 5_000,
    )
    .unwrap();
    check!(
        again["measurement_sha256"] == genuine_hash
            && experiment_ledger::lifecycle::continuations(&writer.successor_c.directory)
                .unwrap()
                .len()
                == 1,
        "idempotent first receipt was not preserved: {again}"
    );
    let mut changed = genuine.clone();
    changed["net_pnl_if_flat"] = json!("1.00");
    e7_rehash_continuation(&mut changed);
    check!(
        super::record_continuation_checked(
            &writer.successor_c,
            &writer.successor_policy,
            &writer_chain,
            &changed,
            writer_due + 6_000,
        )
        .is_err(),
        "a different body for the same continuation identity was accepted"
    );
    check!(
        e7_continuation(&writer.successor_c.directory)["measurement_sha256"] == genuine_hash,
        "a refused different body overwrote the first receipt"
    );
    check!(
        directory_manifest(&writer.source_c.directory) == writer.source_before,
        "the first-insert writer wrote the original source"
    );

    // (E8-2) NATIVE DISCOVERY in C: the host enumerates the exact A.O ref from
    // the verified lineage, the native parser accepts the source-only selector,
    // and the native dispatcher loads B's verified continuation.
    let Some(three) = e5_three_sessions(root, "candidate-e8-native", base_settings, base_policy)
    else {
        assert!(failures.is_empty(), "E8 failures: {failures:#?}");
        return;
    };
    let native_now = three.due + 5_000;
    // C has its own queue; create the empty current queue schema so the
    // native carry/status enumerator can run in the successor session.
    open_queue_for_owner(
        three.c_policy.research_tools.as_ref().unwrap(),
        &three.c.directory.join("model"),
        true,
    )
    .unwrap();
    close_test_book_sidecars(&three.a);
    close_test_book_sidecars(&three.b);
    let carry = carry_state(&three.c, &three.c_policy);
    let refs = carry["observation_refs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    check!(
        carry["observation_refs_status"] == "available" && !refs.is_empty(),
        "C carry_state did not enumerate verified observation refs: {carry}"
    );
    let source_ref = refs
        .iter()
        .find(|reference| reference["candidate_id"] == three.id.as_str())
        .map(|reference| reference["source_observation_ref"].clone());
    let Some(source_ref) = source_ref.filter(Value::is_object) else {
        failures.push(format!(
            "C carry_state did not expose the exact A observation ref: {carry}"
        ));
        assert!(failures.is_empty(), "E8 failures: {failures:#?}");
        return;
    };
    let status = observation_status(&three.c, &three.c_policy, native_now);
    check!(
        status["observation_refs"]
            .as_array()
            .is_some_and(|refs| refs.iter().any(|reference| {
                reference["candidate_id"] == three.id.as_str()
                    && reference["source_observation_ref"] == source_ref
            })),
        "observation_status did not expose the exact A observation ref: {status}"
    );
    // Schema: the observation capability advertises nullable legacy fields and
    // requires all three; the default schema is byte-compatible with the old
    // non-null task pair and has no observation_ref.
    let observation_defs = super::super::owner_candidate_tool_definitions(false, true, true);
    let observation_result = observation_defs
        .iter()
        .find(|definition| definition["name"] == "candidate_result")
        .unwrap();
    let observation_properties = &observation_result["parameters"]["properties"];
    let nullable = |name: &str| {
        observation_properties[name]["type"]
            .as_array()
            .is_some_and(|types| types.iter().any(|value| value == "null"))
    };
    check!(
        nullable("task_id") && nullable("revision") && nullable("observation_ref"),
        "observation schema did not make the legacy/source selectors nullable: {observation_result}"
    );
    check!(
        observation_result["parameters"]["required"]
            == json!(["task_id", "revision", "observation_ref"]),
        "observation schema did not require all selector fields: {observation_result}"
    );
    let default_defs = super::super::owner_candidate_tool_definitions(false, false, true);
    let default_result = default_defs
        .iter()
        .find(|definition| definition["name"] == "candidate_result")
        .unwrap();
    check!(
        default_result["parameters"]["properties"]
            .get("observation_ref")
            .is_none()
            && default_result["parameters"]["properties"]["task_id"]["type"] == "string"
            && default_result["parameters"]["properties"]["revision"]["type"] == "integer"
            && default_result["parameters"]["required"] == json!(["task_id", "revision"]),
        "default candidate_result schema changed: {default_result}"
    );
    let ref_json = source_ref.to_string();
    let call = |arguments: Value| {
        super::super::checked_call(
            &json!({
                "call_id":"e8-native",
                "name":"candidate_result",
                "arguments":arguments.to_string()
            }),
            &observation_defs,
        )
    };
    let source_only = call(json!({
        "task_id":Value::Null,
        "revision":Value::Null,
        "observation_ref":ref_json
    }));
    check!(
        source_only.is_ok(),
        "native parser rejected the source-only selector"
    );
    let mixed = call(json!({
        "task_id":three.observation_task.clone(),
        "revision":1,
        "observation_ref":ref_json
    }));
    check!(
        mixed.is_err(),
        "native parser accepted a mixed legacy+source selector"
    );
    let both_none = call(json!({
        "task_id":Value::Null,
        "revision":Value::Null,
        "observation_ref":Value::Null
    }));
    check!(
        both_none.is_err(),
        "native parser accepted a both-None selector"
    );
    let partial = call(json!({
        "task_id":three.observation_task.clone(),
        "revision":Value::Null,
        "observation_ref":Value::Null
    }));
    check!(
        partial.is_err(),
        "native parser accepted a partial legacy pair"
    );
    let legacy = call(json!({
        "task_id":three.observation_task.clone(),
        "revision":1,
        "observation_ref":Value::Null
    }));
    check!(
        legacy.is_ok(),
        "native parser rejected the complete legacy selector"
    );
    if let Ok((_, tool)) = source_only {
        let super::super::Tool::CandidateResult(task_id, revision, observation_ref) = tool else {
            panic!("source-only candidate_result parsed to the wrong tool");
        };
        let mut tool_cfg = three.c_policy.clone();
        let mut orchestrator =
            super::super::super::orchestrator_model::OrchestratorModelSettings::default();
        for trigger in super::super::super::orchestrator_model::Trigger::ALL {
            let path = three
                .c
                .directory
                .join(format!("e8-orchestrator-{}.md", trigger.as_str()));
            fs::write(&path, "e8 native dispatch fixture\n").unwrap();
            orchestrator.triggers.get_mut(&trigger).unwrap().prompt = path;
        }
        tool_cfg.orchestrator = Some(orchestrator);
        tool_cfg.owner_tasks = Some(crate::runtime::owner_tasks::OwnerTasksSettings {
            version: 1,
            agenda: None,
            review: crate::runtime::owner_tasks::ReviewSettings::default(),
            research: Some(crate::runtime::owner_tasks::OwnerResearchSettings {
                idle_trigger: None,
                enabled: true,
                cutoff_minute: 1020,
                max_evidence: 4,
                quote_capture: None,
                observation_capture: None,
                lane: Some(Default::default()),
            }),
        });
        let role = crate::runtime::planner::ModelRole {
            model_id: "grok-4.7".into(),
            ..Default::default()
        };
        let bundle = json!({
            "trigger":{"kind":"research"},
            "summary":{
                "owner_task":{"kind":"research"},
                "turn_id":"turn-e8",
                "research_id":"research-e8",
                "model_selection":Value::Null
            }
        });
        let clock = || native_now;
        let guard = crate::runtime::planner::CallGuard {
            cancel: None,
            deadline: None,
            not_before: None,
            configuration: Some((&three.c, &tool_cfg)),
            journal_required: std::cell::Cell::new(false),
            minimum_prior_calls: 0,
            invocation_id: Some("turn-e8"),
            context_hash: None,
            clock: &clock,
            duration: None,
            started_wall_ms: None,
        };
        let operation = super::super::OwnerResearchOperation::CandidateResult(
            task_id,
            revision,
            observation_ref,
        );
        match super::super::owner_candidate_operation(
            &three.c,
            &tool_cfg,
            "supervisor",
            &role,
            &bundle,
            &guard,
            operation,
            native_now,
        ) {
            Ok(response) => {
                let b_receipt = experiment_ledger::lifecycle::continuations(&three.b.directory)
                    .unwrap()
                    .into_iter()
                    .next()
                    .unwrap();
                check!(
                    response["parent_continuation_status"] == "verified"
                        && response["parent_continuation"]["measurement_sha256"]
                            == b_receipt["measurement_sha256"]
                        && response["parent_continuation_recorded_in_session"]
                            == three.b_day.as_str(),
                    "native C dispatch did not load B's verified continuation: {response}"
                );
            }
            Err(error) => failures.push(format!(
                "native C dispatch failed for the exact A observation ref: {error}"
            )),
        }
    }
    check!(
        directory_manifest(&three.a.directory) == three.a_before
            && directory_manifest(&three.b.directory) == three.b_before,
        "native C discovery wrote a predecessor"
    );

    // (E8-3) SOURCE AXIS PIN: a different valid result for the same source
    // task id/revision is UNVERIFIED context, never copied as the pinned source.
    let observation_task = writer.observation_task.clone();
    let mut source_db = candidate_queue(&writer.source_c, &writer.source_policy);
    let observation_result = jobs::load_candidate(&mut source_db, &observation_task, 1)
        .unwrap()
        .job
        .result_hash
        .clone()
        .unwrap();
    let source_task_id = match jobs::load_candidate(&mut source_db, &observation_task, 1)
        .unwrap()
        .spec
        .purpose
    {
        Some(jobs::CandidatePurposeV1::Observe { target }) => target.source_task_id.clone(),
        _ => panic!("observation target"),
    };
    let original_source_result: String = source_db
        .query_row(
            "SELECT result FROM jobs WHERE id=(SELECT job_id FROM candidate_tasks WHERE task_id=? AND revision=1)",
            [&source_task_id],
            |row| row.get(0),
        )
        .unwrap();
    // A DIFFERENT valid result artifact now stands in for the pinned source
    // result. The read must report UNVERIFIED, not copy it.
    source_db
        .execute(
            "UPDATE jobs SET result=? WHERE id=(SELECT job_id FROM candidate_tasks WHERE task_id=? AND revision=1)",
            rusqlite::params![observation_result, source_task_id],
        )
        .unwrap();
    drop(source_db);
    match candidate_result(
        &writer.source_c,
        &writer.source_policy,
        &observation_task,
        1,
        writer_due + 7_000,
    ) {
        Ok(response) => {
            check!(
                response["source_result_status"] == "unverified"
                    && response["context_status"] == "partial"
                    && response["source_result"].is_null(),
                "a changed source result hash was copied as the pinned source: {response}"
            );
            check!(
                response["measurement"]["replay"]["status"] == "unavailable",
                "a pinned-source mismatch was projected as not_present: {response}"
            );
        }
        Err(_) => failures.push("pinned-source mismatch crashed the public read".into()),
    }
    let source_db = candidate_queue(&writer.source_c, &writer.source_policy);
    source_db
        .execute(
            "UPDATE jobs SET result=? WHERE id=(SELECT job_id FROM candidate_tasks WHERE task_id=? AND revision=1)",
            rusqlite::params![original_source_result, source_task_id],
        )
        .unwrap();
    drop(source_db);

    // (E8-4) STRICT GAP SET: a query-unavailable continuation with an extra
    // contradictory original_no_data gap must be refused on every read path.
    let Some(gap_fixture) = e4_continuation_fixture(
        root,
        "candidate-e8-gap-set",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        assert!(failures.is_empty(), "E8 failures: {failures:#?}");
        return;
    };
    let gap_due = gap_fixture.due;
    {
        let db = super::super::super::open_book(&gap_fixture.source_c).unwrap();
        db.execute(
            "UPDATE ticks SET body=? WHERE at=? AND event='research_quotes'",
            rusqlite::params![
                br#"{"purpose":"research_observation","bad":"#.as_slice(),
                gap_due
            ],
        )
        .unwrap();
    }
    let _ = mature_candidates(
        &gap_fixture.successor_c,
        &gap_fixture.successor_policy,
        gap_due + 61_000,
    )
    .unwrap();
    let gap_body = e7_continuation(&gap_fixture.successor_c.directory);
    check!(
        gap_body["exit_gap"]["kind"] == "quote_query_unavailable",
        "gap fixture did not record the typed query cause: {gap_body}"
    );
    let mut contradictory = gap_body.clone();
    let mut gaps = contradictory["gaps"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    gaps.push(json!("original_no_data"));
    contradictory["gaps"] = json!(gaps);
    e7_rehash_continuation(&mut contradictory);
    e7_store_continuation(&gap_fixture.successor_c.directory, &contradictory);
    check!(
        verified_continuations(&gap_fixture.successor_c, &gap_fixture.successor_policy).is_err(),
        "a query-unavailable continuation with original_no_data passed verified_continuations"
    );
    if let Ok(response) = candidate_result(
        &gap_fixture.successor_c,
        &gap_fixture.successor_policy,
        &gap_fixture.observation_task,
        1,
        gap_due + 62_000,
    ) {
        check!(
            response["measurement"]["episodes"][0]["after_cost"].is_null(),
            "a contradictory NoData gap was consumed as PnL: {response}"
        );
    }
    e7_store_continuation(&gap_fixture.successor_c.directory, &gap_body);
    check!(
        verified_continuations(&gap_fixture.successor_c, &gap_fixture.successor_policy).is_ok(),
        "the genuine query-unavailable continuation was not readable after restore"
    );

    // (E8-5) SIDECAR: a non-empty SHM with no WAL on a closed predecessor is a
    // conservative refusal; the current live book keeps its normal read.
    open_queue_for_owner(
        writer.successor_policy.research_tools.as_ref().unwrap(),
        &writer.successor_c.directory.join("model"),
        true,
    )
    .unwrap();
    let shm_path = writer.source_c.directory.join("paper.sqlite-shm");
    let before_shm = directory_manifest(&writer.source_c.directory);
    fs::write(&shm_path, b"non-empty-shm-without-wal").unwrap();
    let shm_carry = carry_state(&writer.successor_c, &writer.successor_policy);
    check!(
        shm_carry["observation_refs_status"] == "unavailable",
        "carry_state read a closed source with a non-empty SHM: {shm_carry}"
    );
    let shm_status = observation_status(
        &writer.successor_c,
        &writer.successor_policy,
        writer_due + 8_000,
    );
    check!(
        shm_status["status"] == "unavailable",
        "observation_status read a closed source with a non-empty SHM: {shm_status}"
    );
    let shm_mature = mature_candidates(
        &writer.successor_c,
        &writer.successor_policy,
        writer_due + 9_000,
    )
    .unwrap();
    check!(
        !shm_mature["items"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["candidate_id"] == writer_id.as_str() && item["state"] == "evaluated"
            })
        }),
        "mature_candidates evaluated a closed source with a non-empty SHM: {shm_mature}"
    );
    check!(
        shm_path.is_file() && !writer.source_c.directory.join("paper.sqlite-wal").exists(),
        "the SHM refusal created or deleted a source sidecar"
    );
    fs::remove_file(&shm_path).unwrap();
    check!(
        directory_manifest(&writer.source_c.directory) == before_shm,
        "the SHM refusal changed the closed source bytes"
    );
    check!(
        observation_status(
            &writer.successor_c,
            &writer.successor_policy,
            writer_due + 10_000
        )["status"]
            == "available",
        "normal read did not recover after the SHM was removed"
    );

    assert!(
        failures.is_empty(),
        "E8 continuation failures: {failures:#?}"
    );
}

/// E9 corrective contract: selector presence modes, closed stored-body keys,
/// explicit source-result status, scalar-measurement unavailability, wrong-
/// purpose authoring refusal and native A-result pinning. Every check goes
/// through the public read/parse path where one exists; no production revert or
/// copied RED is used.
fn case_e9_corrective_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let mut failures: Vec<String> = Vec::new();
    macro_rules! check {
        ($cond:expr, $($arg:tt)*) => {
            if !($cond) {
                failures.push(format!($($arg)*));
            }
        };
    }

    // (E9-1) SELECTOR PRESENCE: a partial legacy pair with a valid ref must be
    // refused, not silently treated as source-only. The complete legacy pair
    // and the source-only ref remain valid.
    let Some(three) = e5_three_sessions(root, "candidate-e9-selector", base_settings, base_policy)
    else {
        assert!(failures.is_empty(), "E9 selector failures: {failures:#?}");
        return;
    };
    open_queue_for_owner(
        three.c_policy.research_tools.as_ref().unwrap(),
        &three.c.directory.join("model"),
        true,
    )
    .unwrap();
    close_test_book_sidecars(&three.a);
    close_test_book_sidecars(&three.b);
    let carry = carry_state(&three.c, &three.c_policy);
    let reference = carry["observation_refs"]
        .as_array()
        .and_then(|refs| refs.first())
        .map(|reference| reference["source_observation_ref"].clone());
    let Some(reference) = reference.filter(Value::is_object) else {
        failures.push(format!(
            "E9 selector fixture did not expose an observation ref: {carry}"
        ));
        assert!(failures.is_empty(), "E9 selector failures: {failures:#?}");
        return;
    };
    let ref_json = reference.to_string();
    let definitions = super::super::owner_candidate_tool_definitions(false, true, true);
    let call = |arguments: Value| {
        super::super::checked_call(
            &json!({
                "call_id":"e9-selector",
                "name":"candidate_result",
                "arguments":arguments.to_string()
            }),
            &definitions,
        )
    };
    check!(
        call(json!({
            "task_id":three.observation_task,
            "revision":Value::Null,
            "observation_ref":ref_json
        }))
        .is_err(),
        "partial legacy pair + valid ref was accepted (id silently ignored)"
    );
    check!(
        call(json!({
            "task_id":three.observation_task,
            "revision":1,
            "observation_ref":ref_json
        }))
        .is_err(),
        "complete legacy pair + ref was accepted"
    );
    check!(
        call(json!({
            "task_id":Value::Null,
            "revision":1,
            "observation_ref":Value::Null
        }))
        .is_err(),
        "revision-only selector was accepted"
    );
    check!(
        call(json!({
            "task_id":Value::Null,
            "revision":Value::Null,
            "observation_ref":Value::Null
        }))
        .is_err(),
        "both-None selector was accepted"
    );
    check!(
        call(json!({
            "task_id":123,
            "revision":Value::Null,
            "observation_ref":ref_json
        }))
        .is_err(),
        "malformed legacy field + ref was accepted"
    );
    check!(
        call(json!({
            "task_id":Value::Null,
            "revision":Value::Null,
            "observation_ref":ref_json
        }))
        .is_ok(),
        "source-only selector was rejected"
    );
    check!(
        call(json!({
            "task_id":three.observation_task,
            "revision":1,
            "observation_ref":Value::Null
        }))
        .is_ok(),
        "complete legacy selector was rejected"
    );

    // (E9-2) CLOSED CONTINUATION BODY: extra top-level/nested keys, a missing
    // explicit source_result_status and a wrong financial ABI are refused even
    // with a recomputed self-hash. The genuine unsupported-scope receipt still
    // passes through the ONE canonical validator.
    let Some(cont) = e4_continuation_fixture(
        root,
        "candidate-e9-continuation",
        base_settings,
        base_policy,
        E4ExitMode::AtDue,
    ) else {
        assert!(
            failures.is_empty(),
            "E9 continuation failures: {failures:#?}"
        );
        return;
    };
    let _ = mature_candidates(&cont.successor_c, &cont.successor_policy, cont.due + 3_000).unwrap();
    let genuine = e7_continuation(&cont.successor_c.directory);
    check!(
        verified_continuations(&cont.successor_c, &cont.successor_policy).is_ok(),
        "the genuine continuation was refused"
    );
    type BodyMutation = fn(&mut Value);
    let mutations: Vec<(&str, BodyMutation)> = vec![
        ("extra top-level pnl", |value: &mut Value| {
            value["top_pnl"] = json!("999");
        }),
        ("extra private path", |value: &mut Value| {
            value["private_path"] = json!("/secret");
        }),
        ("extra nested runtime key", |value: &mut Value| {
            value["source_runtime"]["private_path"] = json!("/secret");
        }),
        ("missing source_result_status", |value: &mut Value| {
            value
                .as_object_mut()
                .unwrap()
                .remove("source_result_status");
        }),
        ("wrong financial_abi", |value: &mut Value| {
            value["financial_abi"] = json!("other_engine_v9");
        }),
        ("extra contradictory gap", |value: &mut Value| {
            value["gaps"]
                .as_array_mut()
                .unwrap()
                .push(json!("original_no_data"));
        }),
    ];
    for (label, mutate) in mutations {
        let mut forged = genuine.clone();
        mutate(&mut forged);
        e7_rehash_continuation(&mut forged);
        e7_store_continuation(&cont.successor_c.directory, &forged);
        check!(
            verified_continuations(&cont.successor_c, &cont.successor_policy).is_err(),
            "continuation with {label} passed verified_continuations"
        );
    }
    e7_store_continuation(&cont.successor_c.directory, &genuine);
    check!(
        verified_continuations(&cont.successor_c, &cont.successor_policy).is_ok(),
        "the genuine continuation was not readable after restore"
    );

    // (E9-3) STORED SCALAR / EXTRA KEY: a present non-object measurement or a
    // top-level extra key is UNAVAILABLE, never not_matured and never raw PnL.
    let mut current_db = candidate_queue(&cont.source_c, &cont.source_policy);
    let current_record = jobs::load_candidate(&mut current_db, &cont.observation_task, 1).unwrap();
    let current_result = read_verified_candidate_result(&mut current_db, &cont.observation_task, 1)
        .unwrap()
        .unwrap();
    let genuine_measurement = match mature_observation(
        &cont.source_c,
        &cont.source_policy,
        &mut current_db,
        &current_record,
        &current_result,
        cont.due + 3_000,
    )
    .unwrap()
    {
        MaturationDecision::Matured(value) => value,
        other => {
            failures.push(format!("E9 current maturation did not mature: {other:?}"));
            assert!(failures.is_empty(), "E9 current failures: {failures:#?}");
            return;
        }
    };
    drop(current_db);
    check!(
        genuine_measurement.is_object(),
        "E9 current maturation was not an object: {genuine_measurement}"
    );
    let mut extra = genuine_measurement.clone();
    extra["top_pnl"] = json!("999");
    extra["private_path"] = json!("/secret");
    extra["measurement_sha256"] = json!(super::measure::measurement_content_hash(&extra).unwrap());
    e8_store_measurement(&cont.source_c.directory, &cont.observation_id, &extra);
    if let Ok(response) = candidate_result(
        &cont.source_c,
        &cont.source_policy,
        &cont.observation_task,
        1,
        cont.due + 4_000,
    ) {
        check!(
            response["measurement"]["status"] == "unavailable",
            "extra-key current body was not unavailable: {response}"
        );
        let rendered = response.to_string();
        check!(
            !rendered.contains("top_pnl") && !rendered.contains("private_path"),
            "extra-key current body leaked unknown fields: {response}"
        );
    } else if let Err(error) = candidate_result(
        &cont.source_c,
        &cont.source_policy,
        &cont.observation_task,
        1,
        cont.due + 4_000,
    ) {
        failures.push(format!(
            "extra-key current body crashed the public read: {error}"
        ));
    }
    e8_store_measurement(
        &cont.source_c.directory,
        &cont.observation_id,
        &json!("forged-scalar"),
    );
    if let Ok(response) = candidate_result(
        &cont.source_c,
        &cont.source_policy,
        &cont.observation_task,
        1,
        cont.due + 4_000,
    ) {
        check!(
            response["measurement"]["status"] == "unavailable",
            "scalar measurement was not unavailable: {response}"
        );
    } else if let Err(error) = candidate_result(
        &cont.source_c,
        &cont.source_policy,
        &cont.observation_task,
        1,
        cont.due + 4_000,
    ) {
        failures.push(format!(
            "scalar measurement crashed the public read: {error}"
        ));
    }
    let status = observation_status(&cont.source_c, &cont.source_policy, cont.due + 4_000);
    check!(
        status["lifecycle"]["matured"] == 0,
        "scalar measurement was counted as matured: {status}"
    );
    check!(
        status["observation_refs_status"] == "available"
            && status["observation_refs_limit"] == 16
            && status["observation_refs_truncated"] == false,
        "observation_refs enumeration flags were not structured: {status}"
    );
    check!(
        status["lifecycle"]["milestones"][0]["measurement_status"] == "unavailable",
        "scalar measurement was not unavailable in observation_status: {status}"
    );
    e8_store_measurement(
        &cont.source_c.directory,
        &cont.observation_id,
        &genuine_measurement,
    );

    // (E9-4) WRONG-PURPOSE AUTHORING PARENT: an Observation task can never be
    // the authoring parent of another observation, even when every pin is
    // internally consistent.
    let mut queue = candidate_queue(&cont.source_c, &cont.source_policy);
    let observation = jobs::load_candidate(&mut queue, &cont.observation_task, 1).unwrap();
    let target = match &observation.spec.purpose {
        Some(jobs::CandidatePurposeV1::Observe { target }) => target.as_ref().clone(),
        _ => panic!("E9 observation target missing"),
    };
    let mut wrong = target.clone();
    wrong.source_task_id = observation.spec.task_id.clone();
    wrong.source_revision = observation.spec.revision;
    wrong.source_candidate_id = observation.spec.candidate_id.clone();
    wrong.source_lifecycle_candidate_id = observation.spec.lifecycle_candidate_id.clone();
    wrong.source_result_sha256 = observation.job.result_hash.clone().unwrap();
    wrong.authoring_at_ms = observation.spec.created_at_ms;
    wrong.policy_sha256 = observation.spec.policy_sha256.clone();
    wrong.source_runtime_hash = observation.spec.runtime_binding.runtime_hash.clone();
    wrong.runtime.source_sha256 = observation.spec.source_sha256.clone();
    wrong.runtime.params_sha256 = observation.spec.params_sha256.clone();
    wrong.runtime.runtime_hash = observation.spec.runtime_binding.runtime_hash.clone();
    wrong.runtime.helper_sha256 = observation.spec.runtime_binding.helper_sha256.clone();
    wrong.runtime.unit_policy_sha256 = observation.spec.runtime_binding.unit_policy_sha256.clone();
    wrong.runtime.scorer_compare_sha256 = observation
        .spec
        .runtime_binding
        .scorer_compare_sha256
        .clone();
    wrong.runtime.sandbox_source_sha256 = observation
        .spec
        .runtime_binding
        .sandbox_source_sha256
        .clone();
    check!(
        super::load_verified_authoring_parent(
            &cont.source_c,
            &[],
            &mut queue,
            &cont.source_day,
            &wrong,
        )
        .is_err(),
        "an Observation task was accepted as an authoring parent"
    );
    // A legitimate Observation task is NOT_APPLICABLE as an authoring parent;
    // it must not make carry_state's candidate projection partial.
    let source_carry = carry_state(&cont.source_c, &cont.source_policy);
    check!(
        source_carry["candidates_status"] == "available",
        "an Observation task made carry_state partial: {source_carry}"
    );
    check!(
        source_carry["candidates"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| {
                row["lifecycle_candidate_id"] == cont.observation_id.as_str()
                    && row["parent_ref_status"] == "not_applicable"
            })),
        "an Observation task's authoring-parent status was not not_applicable: {source_carry}"
    );

    // (E9-5) NATIVE A-RESULT PIN: a different valid result for A is UNVERIFIED
    // context, never the pinned authoring source and never an authoritative
    // absence.
    let native_now = three.due + 5_000;
    let original_a_result: String = {
        let mut a_queue = rusqlite::Connection::open(&three.a_queue).unwrap();
        let observation = jobs::load_candidate(&mut a_queue, &three.observation_task, 1).unwrap();
        let authoring_task = match &observation.spec.purpose {
            Some(jobs::CandidatePurposeV1::Observe { target }) => target.source_task_id.clone(),
            _ => panic!("E9 native observation target missing"),
        };
        let observation_result = observation.job.result_hash.clone().unwrap();
        let original: String = a_queue
            .query_row(
                "SELECT result FROM jobs WHERE id=(SELECT job_id FROM candidate_tasks WHERE task_id=? AND revision=1)",
                [&authoring_task],
                |row| row.get(0),
            )
            .unwrap();
        a_queue
            .execute(
                "UPDATE jobs SET result=? WHERE id=(SELECT job_id FROM candidate_tasks WHERE task_id=? AND revision=1)",
                rusqlite::params![observation_result, authoring_task],
            )
            .unwrap();
        original
    };
    match super::candidate_result_for_observation_ref(
        &three.c,
        &three.c_policy,
        &reference,
        native_now,
    ) {
        Ok(response) => {
            check!(
                response["source_result_status"] == "unverified"
                    && response["context_status"] == "partial"
                    && response["source_result"].is_null(),
                "native read copied a changed A result: {response}"
            );
            check!(
                response["replay"]["status"] == "unavailable",
                "native read projected a changed A result as available: {response}"
            );
        }
        Err(error) => failures.push(format!("native A-result pin crashed: {error}")),
    }
    {
        let mut a_queue = rusqlite::Connection::open(&three.a_queue).unwrap();
        let observation = jobs::load_candidate(&mut a_queue, &three.observation_task, 1).unwrap();
        let authoring_task = match &observation.spec.purpose {
            Some(jobs::CandidatePurposeV1::Observe { target }) => target.source_task_id.clone(),
            _ => panic!("E9 native observation target missing"),
        };
        a_queue
            .execute(
                "UPDATE jobs SET result=? WHERE id=(SELECT job_id FROM candidate_tasks WHERE task_id=? AND revision=1)",
                rusqlite::params![original_a_result, authoring_task],
            )
            .unwrap();
    }

    assert!(
        failures.is_empty(),
        "E9 corrective contract failures ({}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Insert one synthetic ORIGINAL archived quote into a fixture book. The
/// event/receipt clocks are the fixture's original times; no later clock or
/// fabricated OHLC series is used.
fn e10_insert_quote(c: &Config, at: i64, purpose: &str, bid: f64, ask: f64) {
    let db = super::super::super::open_book(c).unwrap();
    let mut body = candidate_quote_body(at);
    if let Some(row) = body["data"].get_mut("NSE_EQ:INE000000001") {
        row["depth"]["buy"][0]["price"] = json!(bid);
        row["depth"]["sell"][0]["price"] = json!(ask);
    }
    let archive = json!({
        "source":"upstox-readonly-v1",
        "purpose":purpose,
        "requested_ms":at,
        "received_ms":at,
        "body":body.to_string()
    });
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?2,?3,CASE WHEN instr(?3,'\"purpose\":\"research_observation\"')>0 THEN 'research_quotes' ELSE 'wide_quotes' END)",
        rusqlite::params![at, at, archive.to_string()],
    )
    .unwrap();
}

/// Pre-rollover hook: while the source session is still CURRENT, record a
/// genuine terminal UNKNOWN for the observation (the original exit is absent
/// at that clock) and then archive the original exit at DUE. The stored body
/// is the OLDER current record the successor must supersede; the source bytes
/// are never rewritten after the rollover.
fn e10_unknown_predecessor_hook(
    a: &Config,
    a_policy: &PlannerConfig,
    task: &str,
    id: &str,
    due: i64,
) {
    let mut db = candidate_queue(a, a_policy);
    let record = jobs::load_candidate(&mut db, task, 1).unwrap();
    let result = read_verified_candidate_result(&mut db, task, 1)
        .unwrap()
        .unwrap();
    let measurement =
        match mature_observation(a, a_policy, &mut db, &record, &result, due + 61_000).unwrap() {
            MaturationDecision::Unknown(value) => value,
            other => panic!("E10 unknown fixture did not produce an unknown: {other:?}"),
        };
    drop(db);
    assert_eq!(measurement["exit_status"], "exit_quote_missing");
    // Projection-only tamper while the source is CURRENT: the genuine unknown
    // body is retained while the lifecycle state is reset to `replayed` so the
    // successor is still eligible to continue the ORIGINAL episode. This is a
    // fixture-only projection tamper; no source artifact is rewritten.
    e8_store_measurement(&a.directory, id, &measurement);
    e10_force_state(&a.directory, id, "replayed");
    e10_insert_quote(a, due, "research_observation", 100.0, 100.05);
}

fn e10_force_state(directory: &Path, id: &str, state: &str) {
    let db = rusqlite::Connection::open(directory.join("model").join("experiment-trials.sqlite"))
        .unwrap();
    db.execute(
        "UPDATE experiment_lifecycle_state SET state=? WHERE candidate_id=?",
        rusqlite::params![state, id],
    )
    .unwrap();
}

/// A -> B -> C where A holds an OLDER stored UNKNOWN and B records the genuine
/// EXECUTED continuation of the same episode. C must prefer B's verified
/// continuation exactly once while preserving A's original recorded state.
fn e10_unknown_continuation_fixture(
    root: &Path,
    name: &str,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) -> Option<E5ThreeSessions> {
    let fixture = e4_continuation_fixture_with_hook(
        root,
        name,
        base_settings,
        base_policy,
        E4ExitMode::Missing,
        CANDIDATE_SOURCE,
        Some(&e10_unknown_predecessor_hook),
    )?;
    let E4Sessions {
        source_c: a,
        successor_c: b,
        successor_policy: b_policy,
        successor_day: b_day,
        observation_id: id,
        observation_task,
        due,
        source_queue: a_queue,
        ..
    } = fixture;
    let matured = mature_candidates(&b, &b_policy, due + 62_000).unwrap();
    let item = e2_item(&matured, &id)
        .unwrap_or_else(|| panic!("B did not record the executed continuation: {matured}"));
    assert_eq!(item["state"], "evaluated", "{matured}");
    let continuations = experiment_ledger::lifecycle::continuations(&b.directory).unwrap();
    assert_eq!(continuations.len(), 1, "B must record exactly one receipt");
    assert_eq!(continuations[0]["exit_status"], "executed");
    let sessions = root.join(name).join("sessions");
    let c_day = "2026-10-17";
    let b_planner = b.planner_config.clone().unwrap();
    let (c, c_policy) = e5_successor(
        root,
        &sessions,
        c_day,
        &b_day,
        &b_planner,
        base_settings,
        base_policy,
    );
    write_test_carry(&b, &c);
    candidate_book_on(&c, c_day, &b_day);
    ensure_test_state(&c);
    let a_before = directory_manifest(&a.directory);
    let b_before = directory_manifest(&b.directory);
    Some(E5ThreeSessions {
        a,
        b,
        c,
        c_policy,
        b_day,
        c_day: c_day.to_owned(),
        id,
        observation_task,
        due,
        a_before,
        b_before,
        a_queue,
    })
}

struct E10ObservationFixture {
    c: Config,
    cfg: PlannerConfig,
    source_task: String,
    observation_task: String,
    observation_id: String,
    now: i64,
}

/// One CURRENT-session verified authoring source plus one verified observation
/// with an original executed exit. Used by the identity/metrics/parent/book
/// corrective checks.
fn e10_observation_fixture(
    root: &Path,
    name: &str,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) -> Option<E10ObservationFixture> {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let t0 = candidate_at(9, 23);
    let settings = candidate_observation_settings(&dir, base_settings);
    if !candidate_sandbox_ready(&settings) {
        return None;
    }
    let (c, cfg) = candidate_config(&dir, base_policy, settings);
    candidate_observation_book(&c);
    let mut source_args = candidate_submit_args("e10-source", CANDIDATE_SOURCE, None);
    source_args.horizon_minutes = 1;
    let source = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e10",
        source_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(source["status"], "queued", "{source}");
    let source_task = source["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &source_task, move || t0 + 10);
    {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &source_task, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
    }
    let mut args = candidate_submit_args("e10-observation", CANDIDATE_SOURCE, None);
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
        "turn-e10",
        args,
        t0 + 1_000,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(submitted["status"], "queued", "{submitted}");
    let observation_task = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    let observation_id = submitted["task"]["lifecycle_candidate_id"]
        .as_str()
        .unwrap()
        .to_owned();
    candidate_drive(&c, &cfg, &observation_task, move || t0 + 2_000);
    {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &observation_task, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
    }
    Some(E10ObservationFixture {
        c,
        cfg,
        source_task,
        observation_task,
        observation_id,
        now: candidate_at(9, 26),
    })
}

/// Rewrite one stored observation task's target while the session is CURRENT.
/// The task id/content key are unchanged; only the frozen metric fields move.
fn e10_tamper_target(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    mutate: impl FnOnce(&mut Value),
) -> String {
    let mut db = candidate_queue(c, cfg);
    let record = jobs::load_candidate(&mut db, task_id, 1).unwrap();
    let original = serde_json::to_string(&record.spec).unwrap();
    let mut body = serde_json::to_value(&record.spec).unwrap();
    mutate(&mut body["purpose"]["target"]);
    db.execute(
        "UPDATE candidate_tasks SET body=? WHERE task_id=? AND revision=1",
        rusqlite::params![serde_json::to_string(&body).unwrap(), task_id],
    )
    .unwrap();
    original
}

fn e10_restore_task(c: &Config, cfg: &PlannerConfig, task_id: &str, body: &str) {
    let db = candidate_queue(c, cfg);
    db.execute(
        "UPDATE candidate_tasks SET body=? WHERE task_id=? AND revision=1",
        rusqlite::params![body, task_id],
    )
    .unwrap();
}

fn e10_milestone<'a>(status: &'a Value, id: &str) -> Option<&'a Value> {
    status["lifecycle"]["milestones"]
        .as_array()?
        .iter()
        .find(|item| item["candidate_id"] == id)
}

fn case_e10_corrective_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let mut failures: Vec<String> = Vec::new();
    macro_rules! check {
        ($cond:expr, $($arg:tt)*) => {
            if !($cond) {
                failures.push(format!($($arg)*));
            }
        };
    }

    // (E10-1) WRONG EPISODE LIBRARY CONTINUATION: a genuine X continuation body
    // copied into a Y lifecycle projection must be UNAVAILABLE under Y. It is
    // never X's verified financials, never a second matured count and never a
    // foreign source ref. X itself stays verified exactly once.
    let Some(three) = e5_three_sessions(root, "candidate-e10-copy", base_settings, base_policy)
    else {
        assert!(failures.is_empty(), "E10 copy failures: {failures:#?}");
        return;
    };
    let copy_now = three.due + 5_000;
    open_queue_for_owner(
        three.c_policy.research_tools.as_ref().unwrap(),
        &three.c.directory.join("model"),
        true,
    )
    .unwrap();
    close_test_book_sidecars(&three.a);
    close_test_book_sidecars(&three.b);
    let b_receipt = experiment_ledger::lifecycle::continuations(&three.b.directory)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let b_sha = b_receipt["measurement_sha256"].as_str().unwrap().to_owned();
    let a_library =
        experiment_ledger::lifecycle::library_bounded_immutable(&three.a.directory, "", 10_000)
            .unwrap();
    let a_item = a_library["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["candidate_id"] == three.id.as_str())
        .unwrap()
        .clone();
    let x_score = a_item["observation_score"].clone();
    let y_spec = experiment_ledger::lifecycle::ExperimentSpec {
        version: 1,
        parent_candidate_id: None,
        session: three.c.session.to_string(),
        source_sha256: "a".repeat(64),
        params_sha256: "b".repeat(64),
        dataset_hash: "c".repeat(64),
        feature_abi: sadr_cli::candidate_execution::OBSERVATION_ABI.into(),
        hypothesis: "E10 copied-body projection fixture".into(),
        training_from_ms: 0,
        training_to_ms: 0,
        selection_from_ms: 1,
        selection_to_ms: 2,
        holding_ms: 60_000,
        max_exit_delay_ms: 60_000,
    };
    let y_id = experiment_ledger::lifecycle::candidate_id(&y_spec);
    experiment_ledger::lifecycle::admit_spec(&three.c.directory, y_spec, copy_now).unwrap();
    experiment_ledger::lifecycle::record_abi(&three.c.directory, &y_id, copy_now).unwrap();
    experiment_ledger::lifecycle::record_observation(&three.c.directory, &y_id, &x_score, copy_now)
        .unwrap();
    e8_store_measurement(&three.c.directory, &y_id, &b_receipt);
    let chain = super::carry::predecessor_chain_verified_immutable(&three.c).unwrap();
    let synthetic = json!({
        "candidate_id":y_id,
        "session":three.c.session.to_string(),
        "feature_abi":sadr_cli::candidate_execution::OBSERVATION_ABI,
        "observation_score":x_score,
        "observation_measurement":b_receipt,
        "observation_measurement_present":true
    });
    let checked =
        check_library_observation(&three.c, &three.c_policy, &chain, &synthetic, copy_now);
    check!(
        checked.status == "unavailable"
            && checked.measurement.is_none()
            && checked.source_ref.is_none()
            && !checked.matured,
        "copied X body verified under Y: status={} measurement={:?} ref={:?}",
        checked.status,
        checked.measurement.is_some(),
        checked.source_ref.is_some()
    );
    let status = observation_status(&three.c, &three.c_policy, copy_now);
    check!(
        status["lifecycle"]["matured"] == 1,
        "copied body changed the matured count: {status}"
    );
    if let Some(milestone) = e10_milestone(&status, &three.id) {
        check!(
            milestone["measurement_status"] == "verified"
                && milestone["measurement_sha256"] == json!(b_sha),
            "X lost its verified continuation: {milestone}"
        );
    } else {
        failures.push("X milestone missing after copied body".into());
    }
    if let Some(milestone) = e10_milestone(&status, &y_id) {
        check!(
            milestone["measurement_status"] == "unavailable"
                && milestone["measurement_sha256"].is_null()
                && milestone["exit_status"].is_null(),
            "Y published copied financials: {milestone}"
        );
    } else {
        failures.push("Y milestone missing after copied body".into());
    }
    let library = checked_lifecycle_library(&three.c, &three.c_policy, copy_now).unwrap();
    if let Some(item) = library["items"].as_array().and_then(|items| {
        items
            .iter()
            .find(|item| item["candidate_id"] == y_id.as_str())
    }) {
        check!(
            item["measurement_status"] == "unavailable"
                && item["observation_measurement"].is_null(),
            "checked library published the copied body: {item}"
        );
    } else {
        failures.push("checked library lost the Y item".into());
    }
    let carry = carry_state(&three.c, &three.c_policy);
    if let Some(reference) = carry["observation_refs"]
        .as_array()
        .and_then(|refs| {
            refs.iter()
                .find(|reference| reference["candidate_id"] == three.id.as_str())
        })
        .map(|reference| reference["source_observation_ref"].clone())
    {
        match candidate_result_for_observation_ref(&three.c, &three.c_policy, &reference, copy_now)
        {
            Ok(native) => check!(
                native["parent_continuation_status"] == "verified"
                    && native["parent_continuation"]["measurement_sha256"] == json!(b_sha),
                "X native ref lost its continuation: {native}"
            ),
            Err(error) => failures.push(format!("X native ref was refused: {error}")),
        }
    } else {
        failures.push("X native ref missing after copied body".into());
    }
    check!(
        directory_manifest(&three.a.directory) == three.a_before,
        "copied-body read changed the original source A"
    );

    // (E10-2) VERIFIED CONTINUATION PRECEDENCE: A stored an OLDER valid UNKNOWN
    // before the rollover; B recorded the genuine EXECUTED continuation. C must
    // report B's hash/PnL/process/matured exactly once, keep A's ORIGINAL
    // recorded state separate and never relabel old A.
    let Some(triple) = e10_unknown_continuation_fixture(
        root,
        "candidate-e10-unknown-cont",
        base_settings,
        base_policy,
    ) else {
        assert!(
            failures.is_empty(),
            "E10 continuation failures: {failures:#?}"
        );
        return;
    };
    let cont_now = triple.due + 63_000;
    open_queue_for_owner(
        triple.c_policy.research_tools.as_ref().unwrap(),
        &triple.c.directory.join("model"),
        true,
    )
    .unwrap();
    close_test_book_sidecars(&triple.a);
    close_test_book_sidecars(&triple.b);
    let b_receipt = experiment_ledger::lifecycle::continuations(&triple.b.directory)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let b_sha = b_receipt["measurement_sha256"].as_str().unwrap().to_owned();
    let b_pnl = b_receipt["net_pnl_if_flat"].clone();
    let b_processed = b_receipt["processed_at_ms"].clone();
    check!(
        b_receipt["exit_status"] == "executed"
            && b_receipt["flat"] == true
            && b_receipt["accounting_complete"] == true
            && b_pnl.is_string(),
        "B continuation is not a complete executed episode: {b_receipt}"
    );
    let a_library =
        experiment_ledger::lifecycle::library_bounded_immutable(&triple.a.directory, "", 10_000)
            .unwrap();
    let a_item = a_library["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["candidate_id"] == triple.id.as_str())
        .unwrap()
        .clone();
    let original_body = a_item["observation_measurement"].clone();
    let original_sha = original_body["measurement_sha256"].clone();
    check!(
        original_body["exit_status"] == "exit_quote_missing"
            && original_body["net_pnl_if_flat"].is_null()
            && original_sha.is_string(),
        "A's original stored UNKNOWN changed before the read: {original_body}"
    );
    let status = observation_status(&triple.c, &triple.c_policy, cont_now);
    check!(
        status["lifecycle"]["matured"] == 1,
        "C did not count B's executed continuation exactly once: {status}"
    );
    if let Some(milestone) = e10_milestone(&status, &triple.id) {
        check!(
            milestone["measurement_status"] == "verified"
                && milestone["measurement_sha256"] == json!(b_sha)
                && milestone["exit_status"] == "executed"
                && milestone["continuation_state"] == "evaluated"
                && milestone["continuation_recorded_in_session"] == json!(triple.b_day),
            "C did not prefer B's verified continuation: {milestone}"
        );
        check!(
            milestone["original_recorded_state"] == "replayed"
                && milestone["effective_state"] == "evaluated",
            "C relabelled A's original recorded state: {milestone}"
        );
        check!(
            milestone["original_measurement_sha256"] == original_sha
                && milestone["original_exit_status"] == "exit_quote_missing",
            "C lost A's original recorded measurement: {milestone}"
        );
    } else {
        failures.push("C lost the continued milestone".into());
    }
    let chain = super::carry::predecessor_chain_verified_immutable(&triple.c).unwrap();
    let checked = check_library_observation(&triple.c, &triple.c_policy, &chain, &a_item, cont_now);
    check!(
        checked.status == "verified"
            && checked.matured
            && checked
                .measurement
                .as_ref()
                .is_some_and(|value| value["measurement_sha256"] == json!(b_sha)),
        "central reader did not prefer the verified continuation: status={}",
        checked.status
    );
    let carry = carry_state(&triple.c, &triple.c_policy);
    if let Some(reference) = carry["observation_refs"]
        .as_array()
        .and_then(|refs| {
            refs.iter()
                .find(|reference| reference["candidate_id"] == triple.id.as_str())
        })
        .map(|reference| reference["source_observation_ref"].clone())
    {
        match candidate_result_for_observation_ref(
            &triple.c,
            &triple.c_policy,
            &reference,
            cont_now,
        ) {
            Ok(native) => check!(
                native["parent_continuation_status"] == "verified"
                    && native["parent_continuation"]["measurement_sha256"] == json!(b_sha)
                    && native["parent_continuation"]["net_pnl_if_flat"] == b_pnl
                    && native["parent_continuation"]["processed_at_ms"] == b_processed,
                "C native result did not return B's continuation: {native}"
            ),
            Err(error) => failures.push(format!("C native result was refused: {error}")),
        }
    } else {
        failures.push("C carry_state lost the original observation ref".into());
    }
    check!(
        experiment_ledger::lifecycle::continuations(&triple.c.directory)
            .unwrap()
            .is_empty(),
        "C minted a duplicate continuation receipt"
    );
    check!(
        directory_manifest(&triple.a.directory) == triple.a_before,
        "C's continuation read changed the original source A"
    );
    check!(
        directory_manifest(&triple.b.directory) == triple.b_before,
        "C's continuation read changed B"
    );

    // (E10-3) CURRENT IDENTITY BINDING: the central reader must bind
    // candidate_id/task_id/revision/observation_sha256 to the verified task and
    // score. A changed or deleted identity field is UNAVAILABLE with no PnL.
    let Some(fixture) =
        e10_observation_fixture(root, "candidate-e10-identity", base_settings, base_policy)
    else {
        assert!(failures.is_empty(), "E10 identity failures: {failures:#?}");
        return;
    };
    let matured = mature_candidates(&fixture.c, &fixture.cfg, fixture.now).unwrap();
    let genuine = e2_item(&matured, &fixture.observation_id)
        .unwrap_or_else(|| panic!("E10 identity fixture did not mature: {matured}"))["observation"]
        .clone();
    check!(
        genuine["exit_status"] == "executed" && genuine["net_pnl_if_flat"].is_string(),
        "identity fixture is not an executed measurement: {genuine}"
    );
    let identity_mutations: Vec<(&str, E10ValueMutation)> = vec![
        (
            "changed candidate_id",
            Box::new(|body: &mut Value| body["candidate_id"] = json!("d".repeat(64))),
        ),
        (
            "changed task_id",
            Box::new(|body: &mut Value| body["task_id"] = json!("e".repeat(64))),
        ),
        (
            "changed revision",
            Box::new(|body: &mut Value| body["revision"] = json!(2)),
        ),
        (
            "changed observation_sha256",
            Box::new(|body: &mut Value| body["observation_sha256"] = json!("f".repeat(64))),
        ),
        (
            "missing candidate_id",
            Box::new(|body: &mut Value| {
                body.as_object_mut().unwrap().remove("candidate_id");
            }),
        ),
        (
            "missing task_id",
            Box::new(|body: &mut Value| {
                body.as_object_mut().unwrap().remove("task_id");
            }),
        ),
        (
            "missing revision",
            Box::new(|body: &mut Value| {
                body.as_object_mut().unwrap().remove("revision");
            }),
        ),
        (
            "missing observation_sha256",
            Box::new(|body: &mut Value| {
                body.as_object_mut().unwrap().remove("observation_sha256");
            }),
        ),
    ];
    for (label, mutate) in identity_mutations {
        let mut forged = genuine.clone();
        mutate(&mut forged);
        forged["measurement_sha256"] =
            json!(super::measure::measurement_content_hash(&forged).unwrap());
        e8_store_measurement(&fixture.c.directory, &fixture.observation_id, &forged);
        let response = candidate_result(
            &fixture.c,
            &fixture.cfg,
            &fixture.observation_task,
            1,
            fixture.now,
        );
        match response {
            Ok(response) => check!(
                response["measurement"]["status"] == "unavailable"
                    && response["measurement"]["pnl_claimed"] == false,
                "{label} published a financial measurement: {response}"
            ),
            Err(error) => failures.push(format!("{label} crashed the public read: {error}")),
        }
        let status = observation_status(&fixture.c, &fixture.cfg, fixture.now);
        check!(
            status["lifecycle"]["matured"] == 0,
            "{label} was counted as matured: {status}"
        );
        if let Some(milestone) = e10_milestone(&status, &fixture.observation_id) {
            check!(
                milestone["measurement_status"] == "unavailable"
                    && milestone["measurement_sha256"].is_null()
                    && milestone["exit_status"].is_null(),
                "{label} published raw financial fields: {milestone}"
            );
        }
    }
    e8_store_measurement(&fixture.c.directory, &fixture.observation_id, &genuine);

    // (E10-4) FROZEN TARGET METRIC PIN: an in-range quote_keys, a flipped
    // quote_budget_truncated or a changed decision_delay_ms must not verify
    // against the pinned frozen bundle metadata.
    let original_body = {
        let mut db = candidate_queue(&fixture.c, &fixture.cfg);
        let record = jobs::load_candidate(&mut db, &fixture.observation_task, 1).unwrap();
        serde_json::to_string(&record.spec).unwrap()
    };
    let metric_mutations: Vec<(&str, E10ValueMutation)> = vec![
        (
            "quote_keys in range",
            Box::new(|target: &mut Value| {
                target["quote_keys"] = json!(target["quote_keys"].as_u64().unwrap() + 1);
            }),
        ),
        (
            "quote_budget_truncated flip",
            Box::new(|target: &mut Value| {
                target["quote_budget_truncated"] =
                    json!(!target["quote_budget_truncated"].as_bool().unwrap());
            }),
        ),
        (
            "decision_delay_ms in range",
            Box::new(|target: &mut Value| {
                target["decision_delay_ms"] =
                    json!(target["decision_delay_ms"].as_i64().unwrap() + 500);
            }),
        ),
    ];
    for (label, mutate) in metric_mutations {
        let _ = e10_tamper_target(
            &fixture.c,
            &fixture.cfg,
            &fixture.observation_task,
            |target| mutate(target),
        );
        let response = candidate_result(
            &fixture.c,
            &fixture.cfg,
            &fixture.observation_task,
            1,
            fixture.now,
        );
        check!(
            response.is_err(),
            "{label} was accepted by the public read: {response:?}"
        );
        let status = observation_status(&fixture.c, &fixture.cfg, fixture.now);
        check!(
            status["lifecycle"]["matured"] == 0,
            "{label} was counted as matured: {status}"
        );
        e10_restore_task(
            &fixture.c,
            &fixture.cfg,
            &fixture.observation_task,
            &original_body,
        );
    }
    check!(
        candidate_result(
            &fixture.c,
            &fixture.cfg,
            &fixture.observation_task,
            1,
            fixture.now
        )
        .is_ok(),
        "restoring the genuine target did not restore the public read"
    );

    // (E10-5) DIRECT OBSERVATION STATUS: an Observation task's authoring parent
    // is NOT APPLICABLE, not an unavailable/partial authoring parent.
    match candidate_status(
        &fixture.c,
        &fixture.cfg,
        &fixture.observation_task,
        1,
        fixture.now,
    ) {
        Ok(status) => {
            check!(
                status["parent_ref_status"] == "not_applicable"
                    && status["parent_ref_reason"]
                        == "observation_parent_has_source_observation_ref",
                "direct observation status kept an authoring-parent error: {status}"
            );
            check!(
                status.get("context_status").is_none() || status["context_status"] == "complete",
                "direct observation status was partial: {status}"
            );
        }
        Err(error) => failures.push(format!("direct observation status failed: {error}")),
    }

    // (E10-7) LEGACY SOURCE CONTEXT: a changed/unavailable authoring parent A
    // must report an explicit partial context and never copy A's result/PnL.
    let authoring_task = fixture.source_task.clone();
    let original_a_result = {
        let db = candidate_queue(&fixture.c, &fixture.cfg);
        let record = {
            let mut db2 = candidate_queue(&fixture.c, &fixture.cfg);
            jobs::load_candidate(&mut db2, &authoring_task, 1).unwrap()
        };
        let hash = record.job.result_hash.clone().unwrap();
        jobs::read_candidate_artifact(&db, &hash, "result", 1_048_576).unwrap()
    };
    {
        let db = candidate_queue(&fixture.c, &fixture.cfg);
        db.execute(
            "UPDATE jobs SET result=? WHERE id=(SELECT job_id FROM candidate_tasks WHERE task_id=? AND revision=1)",
            rusqlite::params!["0".repeat(64), authoring_task],
        )
        .unwrap();
    }
    match candidate_result(
        &fixture.c,
        &fixture.cfg,
        &fixture.observation_task,
        1,
        fixture.now,
    ) {
        Ok(response) => {
            check!(
                response["source_result_status"] != "verified"
                    && response["source_result"].is_null()
                    && response["authoring_parent_ref"].is_null(),
                "legacy source error copied A's result: {response}"
            );
            check!(
                response["context_status"] == "partial",
                "legacy source error did not report partial context: {response}"
            );
        }
        Err(error) => failures.push(format!("legacy source error crashed the read: {error}")),
    }
    {
        let db = candidate_queue(&fixture.c, &fixture.cfg);
        db.execute(
            "UPDATE jobs SET result=? WHERE id=(SELECT job_id FROM candidate_tasks WHERE task_id=? AND revision=1)",
            rusqlite::params![
                String::from_utf8(original_a_result.clone()).unwrap(),
                authoring_task
            ],
        )
        .unwrap();
    }

    // (E10-9) EXPLICIT NULL CONTINUATION HASH: `unavailable` requires the
    // source_result_sha256 KEY to be PRESENT with an explicit null; a missing
    // key is a different schema.
    let mut explicit = b_receipt.clone();
    explicit["source_result_status"] = json!("unavailable");
    explicit["source_result_sha256"] = Value::Null;
    e7_rehash_continuation(&mut explicit);
    check!(
        experiment_ledger::lifecycle::validate_continuation_body(&explicit).is_ok(),
        "explicit-null continuation hash was refused"
    );
    let mut missing = explicit.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("source_result_sha256");
    e7_rehash_continuation(&mut missing);
    check!(
        experiment_ledger::lifecycle::validate_continuation_body(&missing).is_err(),
        "missing continuation hash key was accepted as explicit null"
    );

    // (E10-8) FOREIGN BOOK BINDING: a shared queue row from a different book
    // must never resolve through a caller-supplied ref or be enumerated.
    let Some(foreign) =
        e10_observation_fixture(root, "candidate-e10-foreign", base_settings, base_policy)
    else {
        assert!(failures.is_empty(), "E10 foreign failures: {failures:#?}");
        return;
    };
    let mut foreign_c = foreign.c.clone();
    foreign_c.account = "FOREIGN_BOOK_ACCOUNT".into();
    let foreign_cfg = foreign.cfg.clone();
    let mut foreign_source_args =
        candidate_submit_args("e10-foreign-source", CANDIDATE_SOURCE, None);
    foreign_source_args.hypothesis = "E10 foreign book authoring source".into();
    foreign_source_args.horizon_minutes = 1;
    let foreign_source = candidate_submit(
        &foreign_c,
        &foreign_cfg,
        "grok-4.7",
        3,
        "turn-e10-foreign",
        foreign_source_args,
        candidate_at(9, 23),
        Some(candidate_evidence()),
    )
    .unwrap();
    check!(
        foreign_source["status"] == "queued",
        "foreign source was not admitted: {foreign_source}"
    );
    let foreign_source_task = foreign_source["task"]["task_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    if !foreign_source_task.is_empty() {
        candidate_drive(&foreign_c, &foreign_cfg, &foreign_source_task, || {
            candidate_at(9, 23) + 10
        });
        {
            let mut db = candidate_queue(&foreign_c, &foreign_cfg);
            let record = jobs::load_candidate(&mut db, &foreign_source_task, 1).unwrap();
            reconcile_candidate(&foreign_c, &foreign_cfg, &mut db, &record).unwrap();
        }
        let mut args = candidate_submit_args("e10-foreign-observation", CANDIDATE_SOURCE, None);
        args.hypothesis = "E10 foreign book observation episode".into();
        args.observation = Some(Box::new(candidate_observation_args(
            &foreign_source_task,
            candidate_at(9, 21),
            candidate_at(9, 22),
        )));
        let foreign_observation = candidate_submit(
            &foreign_c,
            &foreign_cfg,
            "grok-4.7",
            3,
            "turn-e10-foreign",
            args,
            candidate_at(9, 23) + 1_000,
            Some(candidate_evidence()),
        )
        .unwrap();
        check!(
            foreign_observation["status"] == "queued",
            "foreign observation was not admitted: {foreign_observation}"
        );
        let foreign_task = foreign_observation["task"]["task_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if !foreign_task.is_empty() {
            candidate_drive(&foreign_c, &foreign_cfg, &foreign_task, || {
                candidate_at(9, 23) + 2_000
            });
            {
                let mut db = candidate_queue(&foreign_c, &foreign_cfg);
                let record = jobs::load_candidate(&mut db, &foreign_task, 1).unwrap();
                reconcile_candidate(&foreign_c, &foreign_cfg, &mut db, &record).unwrap();
            }
            let foreign_ref = {
                let mut db = candidate_queue(&foreign_c, &foreign_cfg);
                let record = jobs::load_candidate(&mut db, &foreign_task, 1).unwrap();
                let result = read_verified_candidate_result(&mut db, &foreign_task, 1)
                    .unwrap()
                    .unwrap();
                let score = result.observation.as_ref().unwrap();
                source_observation_ref_from_task(&record, score).unwrap()
            };
            let legitimate = candidate_result_for_observation_ref(
                &foreign.c,
                &foreign.cfg,
                &foreign_ref,
                candidate_at(9, 26),
            );
            check!(
                legitimate.is_err(),
                "foreign book ref resolved through the legitimate book: {legitimate:?}"
            );
            let carry = carry_state(&foreign.c, &foreign.cfg);
            let foreign_id = foreign_observation["task"]["lifecycle_candidate_id"]
                .as_str()
                .unwrap_or_default();
            check!(
                !carry["observation_refs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|reference| reference["candidate_id"] == foreign_id),
                "foreign book ref was enumerated: {carry}"
            );
        }
    }

    // (E10-6) PAGING TRUTH: exactly 16 verified observation refs are complete;
    // a 17th is an explicit truncation with an honest scan count; a source
    // error after 16 refs is PARTIAL, never masked as a truncation.
    let Some(paging) =
        e10_paging_fixture(root, "candidate-e10-paging", base_settings, base_policy, 16)
    else {
        assert!(failures.is_empty(), "E10 paging failures: {failures:#?}");
        return;
    };
    let paging_now = paging.now;
    let carry16 = carry_state(&paging.c, &paging.cfg);
    check!(
        carry16["observation_refs"].as_array().map(Vec::len) == Some(16)
            && carry16["observation_refs_truncated"] == false
            && carry16["observation_refs_status"] == "available"
            && carry16["observation_refs_scanned"] == 16,
        "exactly 16 verified refs were labelled truncated: {carry16}"
    );
    let related16 =
        candidate_result(&paging.c, &paging.cfg, &paging.source_task, 1, paging_now).unwrap();
    check!(
        related16["related_observations"].as_array().map(Vec::len) == Some(16)
            && related16["related_observations_truncated"] == false
            && related16["related_observations_source_scan_truncated"] == false,
        "exactly 16 related observations were labelled truncated: {related16}"
    );
    e10_add_observation(&paging, 16);
    let carry17 = carry_state(&paging.c, &paging.cfg);
    check!(
        carry17["observation_refs"].as_array().map(Vec::len) == Some(16)
            && carry17["observation_refs_truncated"] == true
            && carry17["observation_refs_scanned"] == 17
            && carry17["observation_refs_limit"] == 16,
        "17th verified ref was hidden without an honest lower bound: {carry17}"
    );
    let related17 =
        candidate_result(&paging.c, &paging.cfg, &paging.source_task, 1, paging_now).unwrap();
    check!(
        related17["related_observations"].as_array().map(Vec::len) == Some(16)
            && related17["related_observations_truncated"] == true
            && related17["related_observations_scanned"]
                .as_u64()
                .unwrap_or(0)
                >= 17,
        "17th related observation was hidden without an honest lower bound: {related17}"
    );
    // Corrupt the LAST (largest id) observation's result: the first 16 refs
    // are still valid, but the 17th is a source error. The public status must
    // say partial, not a clean truncation.
    {
        let library = experiment_ledger::lifecycle::library(
            &paging.c.directory,
            &paging.c.session.to_string(),
        )
        .unwrap();
        let last = library["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| item["feature_abi"] == sadr_cli::candidate_execution::OBSERVATION_ABI)
            .max_by_key(|item| item["candidate_id"].as_str().unwrap_or_default().to_owned())
            .unwrap();
        let last_task = last["observation_score"]["task_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut db = candidate_queue(&paging.c, &paging.cfg);
        let task = jobs::load_candidate(&mut db, &last_task, 1).unwrap();
        let hash = task.job.result_hash.clone().unwrap();
        db.execute(
            "UPDATE candidate_artifacts SET body=? WHERE hash=? AND kind='result'",
            rusqlite::params![b"{}".as_slice(), hash],
        )
        .unwrap();
    }
    let partial = carry_state(&paging.c, &paging.cfg);
    check!(
        partial["observation_refs"].as_array().map(Vec::len) == Some(16)
            && partial["observation_refs_partial"] == true
            && partial["observation_refs_status"] == "partial"
            && partial["observation_refs_source_error_reason"].is_string(),
        "source error after 16 refs was masked as a clean truncation: {partial}"
    );

    assert!(
        failures.is_empty(),
        "E10 corrective contract failures ({}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

struct E10PagingFixture {
    c: Config,
    cfg: PlannerConfig,
    source_task: String,
    now: i64,
}

/// One source task plus `count` verified observation episodes, built with the
/// existing synthetic book/queue/helper. Used for the 16/17 enumeration bound.
fn e10_paging_fixture(
    root: &Path,
    name: &str,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
    count: usize,
) -> Option<E10PagingFixture> {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let t0 = candidate_at(9, 23);
    let mut settings = candidate_observation_settings(&dir, base_settings);
    settings.admission_limits = Some(jobs::AdmissionLimits {
        max_trials_per_input: 10_000,
        max_session_trials: 10_000,
    });
    if !candidate_sandbox_ready(&settings) {
        return None;
    }
    let (c, cfg) = candidate_config(&dir, base_policy, settings);
    candidate_observation_book(&c);
    let mut source_args = candidate_submit_args("e10-paging-source", CANDIDATE_SOURCE, None);
    source_args.horizon_minutes = 1;
    let source = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-e10-paging",
        source_args,
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(source["status"], "queued", "{source}");
    let source_task = source["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &cfg, &source_task, move || t0 + 10);
    {
        let mut db = candidate_queue(&c, &cfg);
        let record = jobs::load_candidate(&mut db, &source_task, 1).unwrap();
        reconcile_candidate(&c, &cfg, &mut db, &record).unwrap();
    }
    let fixture = E10PagingFixture {
        c,
        cfg,
        source_task,
        now: candidate_at(9, 26),
    };
    for index in 0..count {
        e10_add_observation(&fixture, index);
    }
    Some(fixture)
}

/// Submit, drive and reconcile one more verified observation of the fixture's
/// source task. Returns the observation task id.
fn e10_add_observation(fixture: &E10PagingFixture, index: usize) -> String {
    let t0 = candidate_at(9, 23);
    let mut args =
        candidate_submit_args(&format!("e10-paging-obs-{index}"), CANDIDATE_SOURCE, None);
    args.hypothesis = format!("E10 paging observation episode {index}");
    args.observation = Some(Box::new(candidate_observation_args(
        &fixture.source_task,
        candidate_at(9, 21),
        candidate_at(9, 22),
    )));
    let submitted = candidate_submit(
        &fixture.c,
        &fixture.cfg,
        "grok-4.7",
        3,
        "turn-e10-paging",
        args,
        t0 + 1_000 + index as i64,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(submitted["status"], "queued", "{submitted}");
    let task = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&fixture.c, &fixture.cfg, &task, move || {
        t0 + 2_000 + index as i64
    });
    {
        let mut db = candidate_queue(&fixture.c, &fixture.cfg);
        let record = jobs::load_candidate(&mut db, &task, 1).unwrap();
        reconcile_candidate(&fixture.c, &fixture.cfg, &mut db, &record).unwrap();
    }
    task
}

fn db_dummy(c: &Config, cfg: &PlannerConfig) -> Connection {
    candidate_queue(c, cfg)
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
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
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
        sadr_cli::candidate_execution::artifact_sha256(&spec).unwrap()
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
        sadr_cli::candidate_execution::artifact_sha256(&execution["report"]).unwrap()
    );
    assert_eq!(
        execution["action_tape_sha256"],
        sadr_cli::candidate_execution::artifact_sha256(&execution["action_tape"]).unwrap()
    );
    assert_eq!(
        execution["params_sha256"],
        sadr_cli::candidate_execution::artifact_sha256(&execution["execution_spec"]["parameters"])
            .unwrap()
    );
    assert_eq!(
        execution["scoring"]["scored_signals_sha256"],
        sadr_cli::candidate_execution::artifact_sha256(&execution["scored_signals"]).unwrap()
    );
    assert_eq!(
        execution["cost_stress"][0]["costs_sha256"],
        sadr_cli::candidate_execution::artifact_sha256(&execution["cost_stress"][0]["costs"])
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

fn candidate_contract(root: &Path, base_settings: &ResearchSettings, base_policy: &PlannerConfig) {
    let candidate_dir = root.join("candidate-contract");
    fs::create_dir_all(&candidate_dir).unwrap();
    let t0 = candidate_at(9, 23);
    let settings = candidate_settings(&candidate_dir, base_settings);
    if !candidate_sandbox_ready(&settings) {
        assert!(
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
            "candidate sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!("skipping candidate contract: python3 or bwrap unavailable on host");
        return;
    }
    let (c, cfg) = candidate_config(&candidate_dir, base_policy, settings.clone());
    candidate_book(&c);
    let sandbox = settings.candidate_compute.as_ref().unwrap().sandbox.clone();
    let mut features = sadr_sandbox::Features::new();
    features.insert("NSE_EQ|INE000000001".into(), json!({}));
    let typed = sadr_sandbox::score_source_with_receipt(
        &sandbox,
        b"import json,sys; print(json.dumps({'NSE_EQ|INE000000001': 1.0}))",
        &features,
    );
    assert!(typed.result.is_ok());
    assert!(typed.receipt.started);
    assert!(typed.receipt.advisory_ok());
    let not_started = sadr_sandbox::score_source_with_receipt(
        &sadr_sandbox::ScorerSandboxConfig {
            memory_mb: 0,
            ..sandbox.clone()
        },
        b"print('{}')",
        &features,
    );
    assert!(!not_started.receipt.started);
    assert!(!not_started.receipt.advisory_ok());
    let evidence = candidate_evidence();
    let first = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-replay", CANDIDATE_SOURCE, None),
        t0,
        Some(evidence.clone()),
    )
    .unwrap();
    assert_eq!(first["status"], "queued");
    assert_eq!(first["execution"], "not_started");
    assert_eq!(first["task"]["status"], "pending");
    assert_eq!(first["task"]["revision"], 1);
    assert_eq!(first["task"]["promotion_authorized"], false);
    assert_eq!(first["task"]["activation_authorized"], false);
    assert_eq!(first["task"]["edge_qualified"], false);
    assert_eq!(first["task"]["parent_candidate_id"], Value::Null);
    assert_eq!(first["lifecycle"]["state"], "sandbox_abi");
    assert_eq!(first["lifecycle"]["activation_authorized"], false);
    let task_id = first["task"]["task_id"].as_str().unwrap().to_owned();
    let candidate_id = first["task"]["candidate_id"].as_str().unwrap().to_owned();
    let record = candidate_record(&c, &cfg, &task_id);
    assert_eq!(record.status, jobs::TaskState::Pending);
    assert_eq!(record.job.attempts, 0);
    assert!(record.job.result_hash.is_none());
    assert_eq!(
        candidate_status(&c, &cfg, &task_id, 1, t0 + 1).unwrap()["task"]["status"],
        "pending"
    );
    assert_eq!(
        candidate_result(&c, &cfg, &task_id, 1, t0 + 1).unwrap()["result_status"],
        "not_verified"
    );
    let duplicate = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-replay", CANDIDATE_SOURCE, None),
        t0,
        Some(evidence.clone()),
    )
    .unwrap();
    assert_eq!(duplicate["status"], "existing");
    assert_eq!(duplicate["task"]["task_id"], task_id);
    let changed = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-replay", CANDIDATE_SOURCE_ALT, None),
        t0,
        Some(evidence.clone()),
    )
    .unwrap();
    assert_eq!(changed["status"], "not_dispatched");
    assert_eq!(changed["reason"], "candidate_parent_pending");
    candidate_drive(&c, &cfg, &task_id, move || t0 + 1);
    let status = candidate_status(&c, &cfg, &task_id, 1, t0 + 2).unwrap();
    assert_eq!(status["task"]["status"], "succeeded");
    assert_eq!(status["lifecycle"]["state"], "forward_shadow");
    assert_eq!(status["lifecycle"]["activation_authorized"], false);
    let result = candidate_result(&c, &cfg, &task_id, 1, t0 + 2).unwrap();
    assert_eq!(result["result_status"], "verified");
    assert_eq!(result["result"]["result_kind"], "ranking_diagnostic");
    assert_eq!(
        result["result"]["evidence_scope"],
        "current_session_selection_window"
    );
    assert_eq!(result["result"]["partial_evidence"], true);
    assert_eq!(result["result"]["promotion_authorized"], false);
    assert_eq!(result["result"]["activation_authorized"], false);
    assert_eq!(result["result"]["edge_qualified"], false);
    assert_eq!(result["result"]["report"]["status"], "ok");
    assert_eq!(result["result"]["qualified"], true);
    assert_eq!(result["result"]["analysis_status"], "qualified");
    assert_eq!(result["result"]["unit_proof"]["active_state"], "inactive");
    assert_eq!(result["result"]["unit_proof"]["main_pid"], 0);
    assert_eq!(result["result"]["unit_proof"]["cgroup_absent"], true);
    assert_eq!(result["result"]["unit_proof"]["proof_origin"], "fixture");
    assert_eq!(result["result"]["advisory"]["leader_reaped"], true);
    assert_eq!(result["result"]["host_qualified"], false);
    // The terminal verified candidate is eligible research evidence with the
    // original result hash and lifecycle id. Appending a research turn for the
    // same evidence id excludes it, so one attempt is enforced.
    let mut research_cfg = cfg.clone();
    research_cfg.owner_tasks = Some(super::super::super::owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: super::super::super::owner_tasks::ReviewSettings::default(),
        research: Some(super::super::super::owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: Some(Default::default()),
        }),
    });
    research_cfg.workspace = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
    let mut orchestrator = research_cfg.orchestrator.clone().unwrap_or_default();
    orchestrator.enabled = true;
    orchestrator
        .triggers
        .entry(super::super::super::orchestrator_model::Trigger::Supervisor)
        .or_default();
    research_cfg.orchestrator = Some(orchestrator);
    let due = super::super::super::supervisor::research_due_evidence(&c, &research_cfg).unwrap();
    let verified_candidate = due
        .iter()
        .find(|evidence| {
            evidence.kind == super::super::super::owner_tasks::ResearchEvidenceKind::CandidateResult
                && evidence.task_id == task_id
        })
        .expect("verified candidate evidence");
    assert_eq!(
        verified_candidate.result_sha256,
        result["result"]["report_sha256"]
    );
    assert_eq!(
        verified_candidate.lifecycle_candidate_id.as_deref(),
        result["result"]["lifecycle_candidate_id"].as_str()
    );
    let identity = super::super::super::owner_tasks::ResearchIdentity {
        version: 1,
        session: c.session.to_string(),
        evidence_id: verified_candidate.evidence_id().unwrap(),
        policy_sha256: super::super::super::digest(
            &super::super::super::planner::policy_bytes(&research_cfg).unwrap(),
        ),
        prompt_version: super::super::super::digest(
            super::super::super::owner_tasks::RESEARCH_PROMPT.as_bytes(),
        ),
    };
    let research_task = super::super::super::owner_tasks::OwnerTask::Research {
        identity: identity.clone(),
        evidence: verified_candidate.clone(),
    };
    let research_turn = super::super::super::session::Turn {
        id: "turn-research-evidence".into(),
        session: c.session.to_string(),
        trigger: "research".into(),
        prompt_version: identity.prompt_version.clone(),
        notes_version: 0,
        fills_at_start: 0,
        started_at_ms: t0 + 3,
        request: json!({
            "owner_task":research_task,
            "turn_id":"turn-research-evidence",
            "research_id":identity.id().unwrap(),
            "model_id":"grok-4.7",
            "cutoff_ms":t0 + 90,
        }),
    };
    super::super::super::session::SessionLog::new(&c.directory, &c.session.to_string())
        .append(
            t0 + 3,
            super::super::super::session::Body::Turn {
                turn: research_turn,
            },
        )
        .unwrap();
    let after = super::super::super::supervisor::research_due_evidence(&c, &research_cfg).unwrap();
    assert!(!after.iter().any(|evidence| {
        evidence.kind == super::super::super::owner_tasks::ResearchEvidenceKind::CandidateResult
            && evidence.task_id == task_id
    }));
    assert!(
        result["result"]["report"]["valid_snapshots"]
            .as_u64()
            .unwrap()
            >= 1
    );
    let ticks = result["result"]["report"]["ticks"].as_array().unwrap();
    assert_eq!(
        ticks.last().unwrap()["candidate_top_n"][0],
        "NSE_EQ|INE000000001"
    );
    let encoded = result.to_string();
    for forbidden in [
        "fees_inr",
        "cash_inr",
        "realized_pnl_if_flat_inr",
        "net_pnl",
    ] {
        assert!(!encoded.contains(forbidden), "{forbidden}");
    }
    assert!(
        result["result"]["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gap| gap == "forward_returns_are_not_pnl")
    );
    // A later host upgrade changes the current interpreter bytes, but the
    // authentic historical result is verified from its recorded policy/proof.
    let mut upgraded = cfg.clone();
    let changed_python = candidate_dir.join("upgraded-python");
    fs::write(&changed_python, b"upgraded-python-bytes").unwrap();
    upgraded
        .research_tools
        .as_mut()
        .unwrap()
        .candidate_compute
        .as_mut()
        .unwrap()
        .sandbox
        .python_path = changed_python;
    let historical = candidate_result(&c, &upgraded, &task_id, 1, t0 + 2).unwrap();
    assert_eq!(historical["result_status"], "verified");
    assert_eq!(historical["result"]["host_qualified"], false);
    // A later result-cap change also reads the original recorded cap.
    let mut recapped = cfg.clone();
    recapped
        .research_tools
        .as_mut()
        .unwrap()
        .candidate_compute
        .as_mut()
        .unwrap()
        .max_result_bytes = jobs::CANDIDATE_RESULT_MIN_BYTES;
    let historical_cap = candidate_result(&c, &recapped, &task_id, 1, t0 + 2).unwrap();
    assert_eq!(historical_cap["result_status"], "verified");
    let original_result = candidate_record(&c, &cfg, &task_id);
    let original_db = candidate_queue(&c, &cfg);
    let original_bytes = jobs::read_candidate_artifact(
        &original_db,
        original_result.job.result_hash.as_deref().unwrap(),
        "result",
        1_048_576,
    )
    .unwrap();
    drop(original_db);
    assert!(original_bytes.len() > jobs::CANDIDATE_RESULT_MIN_BYTES as usize);
    let lower_cap = (original_bytes.len() as u32).saturating_sub(1);
    let mut lower = cfg.clone();
    lower
        .research_tools
        .as_mut()
        .unwrap()
        .candidate_compute
        .as_mut()
        .unwrap()
        .max_result_bytes = lower_cap;
    let lower_task = candidate_submit(
        &c,
        &lower,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-lower-result-cap", CANDIDATE_SOURCE, None),
        t0 + 90,
        Some(evidence.clone()),
    )
    .unwrap();
    assert_eq!(lower_task["status"], "queued", "{lower_task}");
    let lower_task_id = lower_task["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_drive(&c, &lower, &lower_task_id, move || t0 + 91);
    assert_eq!(
        candidate_status(&c, &lower, &lower_task_id, 1, t0 + 92).unwrap()["task"]["status"],
        "terminated"
    );
    assert_eq!(
        candidate_result(&c, &lower, &lower_task_id, 1, t0 + 92).unwrap()["result_status"],
        "not_verified"
    );
    let lower_record = candidate_record(&c, &lower, &lower_task_id);
    let lower_db = candidate_queue(&c, &lower);
    let lower_stopped = worker::read_receipt(
        &lower_db,
        &lower_record.spec.job_id,
        lower_record.job.lease.as_deref().unwrap(),
        "candidate_stopped",
        65_536,
    )
    .unwrap()
    .unwrap();
    let lower_stopped: Value = serde_json::from_slice(&lower_stopped).unwrap();
    assert_eq!(lower_stopped["cause"], "failure");
    assert_eq!(lower_stopped["outcome"], "result_too_large");
    assert!(lower_stopped["result_bytes"].as_u64().unwrap() > u64::from(lower_cap));
    assert_eq!(lower_stopped["result_sha256"].as_str().unwrap().len(), 64);
    assert!(lower_stopped["failure_code"].as_str().is_some());
    // A terminal CandidateStopped failure is original failure evidence. It is
    // not a result, fill, P&L or a reason to replay the unit.
    let mut lower_research = lower.clone();
    lower_research.owner_tasks = Some(super::super::super::owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: super::super::super::owner_tasks::ReviewSettings::default(),
        research: Some(super::super::super::owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: Some(Default::default()),
        }),
    });
    lower_research.workspace = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
    let mut lower_orchestrator = lower_research.orchestrator.clone().unwrap_or_default();
    lower_orchestrator.enabled = true;
    lower_orchestrator
        .triggers
        .entry(super::super::super::orchestrator_model::Trigger::Supervisor)
        .or_default();
    lower_research.orchestrator = Some(lower_orchestrator);
    let failure_due =
        super::super::super::supervisor::research_due_evidence(&c, &lower_research).unwrap();
    assert!(failure_due.iter().any(|evidence| {
        evidence.kind == super::super::super::owner_tasks::ResearchEvidenceKind::Failure
            && evidence.task_id == lower_task_id
    }));
    let library =
        super::super::super::experiment_ledger::lifecycle::library(&c.directory, "2026-10-16")
            .unwrap();
    assert_eq!(library["items"][0]["state"], "forward_shadow");
    assert_eq!(library["activation_authorized"], false);
    let entry_ms = ticks.last().unwrap()["observed_ms"].as_i64().unwrap();
    let due_ms = entry_ms + 5 * 60_000;
    let db = super::super::super::open_book(&c).unwrap();
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,?,?,'scanner_metadata')",
        rusqlite::params![due_ms, due_ms, candidate_archive(due_ms).to_string()],
    )
    .unwrap();
    drop(db);
    let book_hash = digest(&fs::read(c.directory.join("paper.sqlite")).unwrap());
    let universe_hash = digest(&fs::read(c.directory.join("wide-universe.json")).unwrap());
    let matured = mature_candidates(&c, &cfg, due_ms + 60_000).unwrap();
    assert_eq!(matured["status"], "matured");
    assert!(matches!(
        matured["items"][0]["state"].as_str(),
        Some("eligible" | "insufficient")
    ));
    assert_eq!(matured["items"][0]["activation_authorized"], false);
    assert_eq!(matured["items"][0]["edge_qualified"], false);
    let library =
        super::super::super::experiment_ledger::lifecycle::library(&c.directory, "2026-10-16")
            .unwrap();
    assert!(matches!(
        library["items"][0]["state"].as_str(),
        Some("eligible" | "insufficient")
    ));
    assert_eq!(library["activation_authorized"], false);
    let after = candidate_result(&c, &cfg, &task_id, 1, due_ms + 60_001).unwrap();
    assert_eq!(after["result_status"], "verified");
    assert!(matches!(
        after["lifecycle"]["state"].as_str(),
        Some("eligible" | "insufficient")
    ));
    assert_eq!(after["lifecycle"]["activation_authorized"], false);

    // A crash/failure after the terminal mark commit but before `conclude`
    // leaves forward_shadow with no pending marks. The mechanical projection
    // must retry the conclusion from the original ledger, including an
    // evaluated half-state, and repeat idempotently without model/replay.
    let half = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-half-conclude", CANDIDATE_SOURCE_ALT, None),
        t0 + 200,
        Some(evidence.clone()),
    )
    .unwrap();
    assert_eq!(half["status"], "queued");
    let half_task = half["task"]["task_id"].as_str().unwrap().to_owned();
    let half_id = half["task"]["lifecycle_candidate_id"]
        .as_str()
        .unwrap()
        .to_owned();
    candidate_drive(&c, &cfg, &half_task, move || t0 + 201);
    assert_eq!(
        candidate_status(&c, &cfg, &half_task, 1, t0 + 202).unwrap()["lifecycle"]["state"],
        "forward_shadow"
    );
    let lifecycle_db = c.directory.join("model").join("experiment-trials.sqlite");
    let lifecycle = rusqlite::Connection::open(&lifecycle_db).unwrap();
    let projection: String = lifecycle
        .query_row(
            "SELECT projection FROM experiment_lifecycle_state WHERE candidate_id=?",
            [&half_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut projection: Value = serde_json::from_str(&projection).unwrap();
    for mark in projection["marks"].as_array_mut().unwrap() {
        mark["status"] = json!("matured");
        mark["disposition"] = json!("matured");
    }
    lifecycle
        .execute(
            "UPDATE experiment_lifecycle_state SET projection=? WHERE candidate_id=?",
            rusqlite::params![projection.to_string(), half_id],
        )
        .unwrap();
    drop(lifecycle);
    let retried = mature_candidates(&c, &cfg, t0 + 202).unwrap();
    assert_eq!(retried["status"], "matured");
    assert!(matches!(
        super::super::super::experiment_ledger::lifecycle::state(&c.directory, &half_id)
            .unwrap()
            .as_deref(),
        Some("eligible" | "insufficient" | "rejected")
    ));
    let settled = super::super::super::experiment_ledger::lifecycle::state(&c.directory, &half_id)
        .unwrap()
        .unwrap();
    let repeat = mature_candidates(&c, &cfg, t0 + 203).unwrap();
    assert_eq!(repeat["status"], "idle");
    assert_eq!(
        super::super::super::experiment_ledger::lifecycle::state(&c.directory, &half_id)
            .unwrap()
            .unwrap(),
        settled
    );

    // A new source version is a distinct immutable task with an explicit parent.
    let version = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args(
            "candidate-replay",
            CANDIDATE_SOURCE_ALT,
            Some(task_id.as_str()),
        ),
        t0 + 10,
        Some(evidence.clone()),
    )
    .unwrap();
    assert_eq!(version["status"], "queued");
    let version_task = version["task"]["task_id"].as_str().unwrap().to_owned();
    assert_ne!(version_task, task_id);
    assert_eq!(version["task"]["parent_task_id"], task_id);
    assert_eq!(version["task"]["parent_candidate_id"], candidate_id);
    assert_eq!(
        version["task"]["lifecycle_candidate_id"],
        version["lifecycle"]["candidate_id"]
    );
    assert_eq!(
        candidate_cancel(&c, &cfg, &version_task, 1, t0 + 11).unwrap()["task"]["status"],
        "cancelled"
    );
    let duplicate_version = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-replay", CANDIDATE_SOURCE_ALT, None),
        t0 + 10,
        Some(evidence.clone()),
    )
    .unwrap();
    assert_eq!(duplicate_version["status"], "existing");
    assert_eq!(duplicate_version["task"]["task_id"], version_task);

    // Pending cancellation is immediate and idempotent; a cancelled task never runs.
    let pending = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-pending-cancel", CANDIDATE_SOURCE, None),
        t0 + 20,
        Some(evidence.clone()),
    )
    .unwrap();
    let pending_task = pending["task"]["task_id"].as_str().unwrap().to_owned();
    let cancelled = candidate_cancel(&c, &cfg, &pending_task, 1, t0 + 21).unwrap();
    assert_eq!(cancelled["task"]["status"], "cancelled");
    assert_eq!(cancelled["terminal"], true);
    assert_eq!(
        candidate_cancel(&c, &cfg, &pending_task, 1, t0 + 21).unwrap()["task"]["status"],
        "cancelled"
    );
    let pending_settings = cfg.research_tools.clone().unwrap();
    let pending_authority = c.directory.join("model");
    let mut pending_unit = FakeCandidateUnit {
        clock: Arc::new(move || t0 + 22),
    };
    assert_eq!(
        drive_candidate_with_unit(
            pending_settings,
            pending_authority,
            pending_task.clone(),
            1,
            Arc::new(AtomicBool::new(false)),
            || t0 + 22,
            &mut pending_unit,
        ),
        Err("candidate_recovery_required")
    );
    assert_eq!(
        candidate_status(&c, &cfg, &pending_task, 1, t0 + 22).unwrap()["task"]["status"],
        "cancelled"
    );

    // Running cancellation stays CancelRequested until the bounded snapshot
    // returns and the sandbox cleanup receipt is durable.
    let running = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-running-cancel", CANDIDATE_SLEEP_SOURCE, None),
        t0 + 30,
        Some(evidence.clone()),
    )
    .unwrap();
    let running_task = running["task"]["task_id"].as_str().unwrap().to_owned();
    let cancel = Arc::new(AtomicBool::new(false));
    let driver_cancel = Arc::clone(&cancel);
    let driver_settings = cfg.research_tools.clone().unwrap();
    let driver_authority = c.directory.join("model");
    let driver_task = running_task.clone();
    let drive_now = Arc::new(AtomicI64::new(t0 + 31));
    let drive_clock = Arc::clone(&drive_now);
    let handle = thread::spawn(move || {
        let clock: Arc<dyn Fn() -> i64 + Send + Sync> =
            Arc::new(move || drive_clock.load(Ordering::Relaxed));
        let mut unit = FakeCandidateUnit {
            clock: Arc::clone(&clock),
        };
        drive_candidate_with_unit(
            driver_settings,
            driver_authority,
            driver_task,
            1,
            driver_cancel,
            || clock(),
            &mut unit,
        )
    });
    let mut observed_running = false;
    for _ in 0..100 {
        let status = candidate_status(&c, &cfg, &running_task, 1, t0 + 31).unwrap();
        if status["task"]["status"] == "running" {
            observed_running = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(observed_running, "candidate did not enter running");
    let requested = candidate_cancel(&c, &cfg, &running_task, 1, t0 + 32).unwrap();
    assert_eq!(requested["task"]["status"], "cancel_requested");
    assert_eq!(requested["terminal"], false);
    cancel.store(true, Ordering::Relaxed);
    drive_now.store(t0 + 33, Ordering::Relaxed);
    handle.join().unwrap().unwrap();
    let cancelled = candidate_status(&c, &cfg, &running_task, 1, t0 + 33).unwrap();
    assert_eq!(cancelled["task"]["status"], "cancelled");
    let record = candidate_record(&c, &cfg, &running_task);
    let lease = record.job.lease.clone().unwrap();
    let db = candidate_queue(&c, &cfg);
    let stopped = worker::read_receipt(
        &db,
        &record.spec.job_id,
        &lease,
        "candidate_stopped",
        65_536,
    )
    .unwrap()
    .unwrap();
    let stopped: Value = serde_json::from_slice(&stopped).unwrap();
    assert_eq!(stopped["kind"], "candidate_unit_stopped");
    assert_eq!(stopped["outcome"], "cancelled");
    assert_eq!(stopped["proof"]["active_state"], "inactive");
    assert_eq!(stopped["proof"]["proof_origin"], "fixture");

    // Expiry refuses before any claim.
    let expiring = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-expired", CANDIDATE_SOURCE, None),
        t0 + 1_000_000,
        Some(evidence.clone()),
    )
    .unwrap();
    let expiring_task = expiring["task"]["task_id"].as_str().unwrap().to_owned();
    let expired_at = t0 + 1_000_000 + cfg.research_tools.as_ref().unwrap().task_ttl_ms as i64 + 1;
    candidate_drive(&c, &cfg, &expiring_task, move || expired_at);
    assert_eq!(
        candidate_status(&c, &cfg, &expiring_task, 1, expired_at).unwrap()["task"]["status"],
        "refused"
    );

    // A per-snapshot budget never exceeds the remaining global wall.
    let mut short = settings.clone();
    short.queue = c.directory.join("candidate-short.sqlite");
    short.candidate_compute.as_mut().unwrap().wall_millis = 1_000;
    short.candidate_compute.as_mut().unwrap().sandbox.wall_ms = 1_000;
    short.candidate_compute.as_mut().unwrap().limits.cpu_millis = 1_000;
    let short_cfg = PlannerConfig {
        research_tools: Some(short),
        ..cfg.clone()
    };
    let wall = candidate_submit(
        &c,
        &short_cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-global-wall", CANDIDATE_SLEEP_SOURCE, None),
        t0 + 40,
        Some(evidence.clone()),
    )
    .unwrap();
    let wall_task = wall["task"]["task_id"].as_str().unwrap().to_owned();
    let ticks = Arc::new(AtomicI64::new(0));
    let clock = Arc::clone(&ticks);
    candidate_drive(&c, &short_cfg, &wall_task, move || {
        if clock.fetch_add(1, Ordering::Relaxed) < 2 {
            t0 + 41
        } else {
            t0 + 100_000
        }
    });
    assert_eq!(
        candidate_status(&c, &short_cfg, &wall_task, 1, t0 + 100_000).unwrap()["task"]["status"],
        "terminated"
    );
    assert_eq!(
        candidate_result(&c, &short_cfg, &wall_task, 1, t0 + 100_000).unwrap()["result_status"],
        "not_verified"
    );

    // A tampered pinned source is refused before any claim.
    let tampered = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-source-tamper", CANDIDATE_SOURCE, None),
        t0 + 50,
        Some(evidence.clone()),
    )
    .unwrap();
    let tampered_task = tampered["task"]["task_id"].as_str().unwrap().to_owned();
    let tampered_record = candidate_record(&c, &cfg, &tampered_task);
    let db = candidate_queue(&c, &cfg);
    db.execute(
        "UPDATE candidate_artifacts SET body=? WHERE hash=?",
        rusqlite::params![b"tampered".as_slice(), tampered_record.spec.source_sha256],
    )
    .unwrap();
    candidate_drive(&c, &cfg, &tampered_task, move || t0 + 51);
    assert_eq!(
        candidate_status(&c, &cfg, &tampered_task, 1, t0 + 51).unwrap()["task"]["status"],
        "refused"
    );
    db.execute(
        "UPDATE candidate_artifacts SET body=? WHERE hash=?",
        rusqlite::params![
            CANDIDATE_SOURCE.as_bytes(),
            tampered_record.spec.source_sha256
        ],
    )
    .unwrap();
    drop(db);

    // The external registered worker refuses a candidate job before claiming it.
    let external = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-external-worker", CANDIDATE_SOURCE_ALT, None),
        t0 + 60,
        Some(evidence.clone()),
    )
    .unwrap();
    let external_task = external["task"]["task_id"].as_str().unwrap().to_owned();
    let external_record = candidate_record(&c, &cfg, &external_task);
    let mut db = candidate_queue(&c, &cfg);
    let registered = RegisteredRun {
        spec: external_record.job.spec.clone(),
        split: worker::DevelopmentSplit::Development,
        limits: ResourceLimits {
            cpu_millis: 1_000,
            tasks: 1,
            workspace_bytes: 1_048_576,
            file_bytes: 65_536,
        },
        code: b"print('{}')".to_vec(),
        runtime: b"fixture".to_vec(),
        input: b"{}".to_vec(),
    };
    let mut runner = NoRunner;
    assert!(matches!(
        worker::start_one(&mut db, &registered, &"f".repeat(64), &mut runner, || {
            t0 + 61
        }),
        Err(worker::Error::Registration)
    ));
    let after = jobs::load(&mut db, &external_record.spec.job_id).unwrap();
    assert_eq!(after.state, jobs::State::Pending);
    assert_eq!(after.attempts, 0);
    drop(db);
    assert_eq!(
        candidate_cancel(&c, &cfg, &external_task, 1, t0 + 62).unwrap()["task"]["status"],
        "cancelled"
    );

    // Invalid inputs are not dispatched and create no task row.
    for (args, reason) in [
        (
            CandidateSubmitArgs {
                source: "x".repeat(262_145),
                ..candidate_submit_args("candidate-oversized", CANDIDATE_SOURCE, None)
            },
            "candidate_source_invalid",
        ),
        (
            CandidateSubmitArgs {
                source: "token = 'fixture'\nprint('{}')".into(),
                ..candidate_submit_args("candidate-secret", CANDIDATE_SOURCE, None)
            },
            "candidate_source_secret_refused",
        ),
        (
            CandidateSubmitArgs {
                params: json!([]),
                ..candidate_submit_args("candidate-params", CANDIDATE_SOURCE, None)
            },
            "candidate_params_invalid",
        ),
        (
            CandidateSubmitArgs {
                dataset_id: "missing".into(),
                ..candidate_submit_args("candidate-missing-dataset", CANDIDATE_SOURCE, None)
            },
            "candidate_dataset_unknown",
        ),
        (
            CandidateSubmitArgs {
                training_to_ms: candidate_at(9, 21),
                ..candidate_submit_args("candidate-windows", CANDIDATE_SOURCE, None)
            },
            "candidate_windows_invalid",
        ),
    ] {
        let refused = candidate_submit(
            &c,
            &cfg,
            "grok-4.7",
            3,
            "turn-candidate",
            args,
            t0 + 70,
            Some(evidence.clone()),
        )
        .unwrap();
        assert_eq!(refused["status"], "not_dispatched");
        assert_eq!(refused["reason"], reason);
    }
    let mut unsupported = settings.clone();
    unsupported.candidate_compute.as_mut().unwrap().abi = "scorer_replay_v2".into();
    assert_eq!(
        unsupported.validate().unwrap_err().to_string(),
        "candidate_compute_invalid"
    );
    // A quota below 0.01% has no exact systemd representation. Rounding it up
    // would exceed the declared CPU budget, so the configuration is refused.
    let mut tiny_quota = settings.clone();
    tiny_quota
        .candidate_compute
        .as_mut()
        .unwrap()
        .unit_cpu_millis = 1;
    tiny_quota
        .candidate_compute
        .as_mut()
        .unwrap()
        .unit_wall_millis = 600_000;
    assert_eq!(
        tiny_quota.validate().unwrap_err().to_string(),
        "candidate_compute_invalid"
    );

    // A tampered result binding is refused even with a matching stored hash.
    let record = candidate_record(&c, &cfg, &task_id);
    let mut db = candidate_queue(&c, &cfg);
    let result_hash = record.job.result_hash.clone().unwrap();
    let bytes = jobs::read_candidate_artifact(&db, &result_hash, "result", 1_048_576).unwrap();
    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
    value["promotion_authorized"] = json!(true);
    let tampered_result = serde_json::to_vec(&value).unwrap();
    let tampered_hash =
        jobs::put_candidate_artifact(&mut db, "result", &tampered_result, t0 + 80).unwrap();
    db.execute(
        "UPDATE jobs SET result=? WHERE id=?",
        rusqlite::params![tampered_hash, record.spec.job_id],
    )
    .unwrap();
    assert!(read_verified_candidate_result(&mut db, &task_id, 1).is_err());
    db.execute(
        "UPDATE jobs SET result=? WHERE id=?",
        rusqlite::params![result_hash, record.spec.job_id],
    )
    .unwrap();
    drop(db);

    // Original pinned bytes and the paper book are unchanged by read-only replay.
    let record = candidate_record(&c, &cfg, &task_id);
    let db = candidate_queue(&c, &cfg);
    assert_eq!(
        jobs::read_candidate_artifact(&db, &record.spec.source_sha256, "source", 262_144).unwrap(),
        CANDIDATE_SOURCE.as_bytes()
    );
    assert_eq!(
        jobs::read_candidate_artifact(&db, &record.spec.params_sha256, "params", 65_536).unwrap(),
        b"{}"
    );
    let dataset =
        jobs::read_candidate_artifact(&db, &record.spec.dataset_hash, "dataset", 4 * 1024 * 1024)
            .unwrap();
    let snapshots: Vec<sadr_core::strategy::FeatureSnapshot> =
        serde_json::from_slice(&dataset).unwrap();
    assert!(!snapshots.is_empty());
    drop(db);
    assert_eq!(
        digest(&fs::read(c.directory.join("paper.sqlite")).unwrap()),
        book_hash
    );
    assert_eq!(
        digest(&fs::read(c.directory.join("wide-universe.json")).unwrap()),
        universe_hash
    );

    // Compute-produced deadline vs physical stop grace: a stop proof may be
    // recorded after the compute deadline, but the result itself must have been
    // produced before it. The durable field is `stopped_at_ms`; there is no
    // `at_ms` field, and no database event is backdated.
    let record = candidate_record(&c, &cfg, &task_id);
    let lease = record.job.lease.clone().unwrap();
    let mut db = candidate_queue(&c, &cfg);
    let launch_bytes =
        worker::read_receipt(&db, &record.spec.job_id, &lease, "candidate_launch", 65_536)
            .unwrap()
            .unwrap();
    let launch: worker::CandidateLaunch = serde_json::from_slice(&launch_bytes).unwrap();
    let stopped_bytes = worker::read_receipt(
        &db,
        &record.spec.job_id,
        &lease,
        "candidate_stopped",
        65_536,
    )
    .unwrap()
    .unwrap();
    let mut stopped: CandidateStopped = serde_json::from_slice(&stopped_bytes).unwrap();
    let original_result_hash = stopped.result_sha256.clone().unwrap();
    let original_result_at = stopped.result_at_ms.unwrap();
    let physical_stop = launch.task_deadline_ms + 5_000;
    stopped.stopped_at_ms = physical_stop;
    db.execute(
        "UPDATE registered_worker_receipts SET body=? WHERE job=? AND lease=? AND kind='candidate_stopped'",
        rusqlite::params![
            serde_json::to_vec(&stopped).unwrap(),
            record.spec.job_id,
            lease
        ],
    )
    .unwrap();
    db.execute(
        "UPDATE job_events SET at_ms=? WHERE job=? AND event IN ('completed','completed_reconciled')",
        rusqlite::params![physical_stop, record.spec.job_id],
    )
    .unwrap();
    assert!(
        read_verified_candidate_result(&mut db, &task_id, 1).is_ok(),
        "a physical stop after the compute deadline must not invalidate a timely result"
    );

    let result_bytes =
        jobs::read_candidate_artifact(&db, &original_result_hash, "result", 1_048_576).unwrap();
    let mut envelope: CandidateResultV1 = serde_json::from_slice(&result_bytes).unwrap();
    let late_result_at = launch.task_deadline_ms + 1;
    envelope.result_produced_at_ms = late_result_at;
    let late_bytes = serde_json::to_vec(&envelope).unwrap();
    let late_hash =
        jobs::put_candidate_artifact(&mut db, "result", &late_bytes, physical_stop).unwrap();
    stopped.result_sha256 = Some(late_hash.clone());
    stopped.result_at_ms = Some(late_result_at);
    db.execute(
        "UPDATE registered_worker_receipts SET body=? WHERE job=? AND lease=? AND kind='candidate_stopped'",
        rusqlite::params![
            serde_json::to_vec(&stopped).unwrap(),
            record.spec.job_id,
            lease
        ],
    )
    .unwrap();
    db.execute(
        "UPDATE jobs SET result=? WHERE id=?",
        rusqlite::params![late_hash, record.spec.job_id],
    )
    .unwrap();
    assert_eq!(stopped.result_at_ms, Some(late_result_at));
    assert!(stopped.result_at_ms.unwrap() > original_result_at);
    assert!(
        read_verified_candidate_result(&mut db, &task_id, 1).is_err(),
        "a result produced after the compute deadline must not verify"
    );

    let candidate = settings.candidate_compute.as_ref().unwrap();
    // A genuine helper result produced after the compute deadline while the
    // controller's pre-poll clock sample is still inside it is authentic late
    // output, not an invalid payload. It is classified as
    // deadline_expired/result_after_deadline with a bounded code, and the
    // artifact is preserved.
    let late_submit = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-late-result", CANDIDATE_SOURCE, None),
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(late_submit["status"], "queued");
    let late_task_id = late_submit["task"]["task_id"].as_str().unwrap().to_owned();
    let late_head = candidate_record(&c, &cfg, &late_task_id);
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
        &c,
        &cfg,
        &late_task_id,
        candidate_scripted_clock(vec![claim_at, compute_deadline - 1, ended_at]),
        &mut late_unit,
    )
    .unwrap();
    let late_record = candidate_record(&c, &cfg, &late_task_id);
    assert_eq!(late_record.status, jobs::TaskState::Terminated);
    assert_eq!(late_record.job.attempts, 1);
    assert!(late_record.cancel_receipt.is_none());
    let late_db = candidate_queue(&c, &cfg);
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
    assert_eq!(late_stopped.result_at_ms, Some(late_at));
    assert!(late_stopped.stopped_at_ms >= late_at);
    let late_hash = late_stopped.result_sha256.clone().unwrap();
    let late_bytes = jobs::read_candidate_artifact(
        &late_db,
        &late_hash,
        "result",
        candidate.max_result_bytes as usize,
    )
    .unwrap();
    assert!(!late_bytes.is_empty());
    assert_eq!(digest(&late_bytes), late_hash);

    // The same late timing with a tampered identity binding is not trusted as
    // authentic late output; it remains a bounded invalid-result failure.
    let invalid_submit = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate",
        candidate_submit_args("candidate-late-invalid", CANDIDATE_SOURCE, None),
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(invalid_submit["status"], "queued");
    let invalid_task_id = invalid_submit["task"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let invalid_head = candidate_record(&c, &cfg, &invalid_task_id);
    let invalid_deadline =
        (claim_at + i64::from(candidate.wall_millis)).min(invalid_head.spec.expires_at_ms);
    let invalid_late = invalid_deadline + 1;
    let invalid_ended = invalid_late + 10;
    let mut invalid_unit = LateCandidateUnit {
        helper_at: invalid_late,
        proof_at: invalid_ended,
        tamper_binding: true,
    };
    candidate_drive_result(
        &c,
        &cfg,
        &invalid_task_id,
        candidate_scripted_clock(vec![claim_at, invalid_deadline - 1, invalid_ended]),
        &mut invalid_unit,
    )
    .unwrap();
    let invalid_record = candidate_record(&c, &cfg, &invalid_task_id);
    assert_eq!(invalid_record.status, jobs::TaskState::Terminated);
    let invalid_db = candidate_queue(&c, &cfg);
    let invalid_lease = invalid_record.job.lease.clone().unwrap();
    let invalid_stopped_bytes = worker::read_receipt(
        &invalid_db,
        &invalid_record.spec.job_id,
        &invalid_lease,
        "candidate_stopped",
        65_536,
    )
    .unwrap()
    .unwrap();
    let invalid_stopped: CandidateStopped = serde_json::from_slice(&invalid_stopped_bytes).unwrap();
    assert_eq!(invalid_stopped.outcome, CandidateStopOutcome::InvalidResult);
    assert_eq!(
        invalid_stopped.failure_code.as_deref(),
        Some("candidate_result_invalid")
    );
    assert_eq!(invalid_stopped.result_at_ms, Some(invalid_late));
}

fn candidate_admission_union(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let union_dir = root.join("candidate-union");
    fs::create_dir_all(&union_dir).unwrap();
    let t0 = candidate_at(9, 23);
    let mut settings = candidate_settings(&union_dir, base_settings);
    settings.admission_limits = Some(jobs::AdmissionLimits {
        max_trials_per_input: 1,
        max_session_trials: 2,
    });
    let (c, cfg) = candidate_config(&union_dir, base_policy, settings);
    candidate_book(&c);
    let registered = owner_task_enqueue_with_evidence(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-union",
        "registered-union",
        "Measure registered inputs",
        "shared_core_backtest",
        "synthetic_no_quotes_fixture",
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(registered["status"], "pending");
    let first = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-union",
        candidate_submit_args("candidate-union-one", CANDIDATE_SOURCE, None),
        t0 + 1,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(first["status"], "queued");
    let second = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-union",
        candidate_submit_args("candidate-union-two", CANDIDATE_SOURCE_ALT, None),
        t0 + 2,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(second["status"], "not_dispatched");
    assert_eq!(
        second["reason"],
        "research_session_compute_budget_exhausted"
    );

    // Candidate first, registered second: the same session total is shared.
    let candidate_first_dir = root.join("candidate-union-candidate-first");
    fs::create_dir_all(&candidate_first_dir).unwrap();
    let mut candidate_first = candidate_settings(&candidate_first_dir, base_settings);
    candidate_first.admission_limits = Some(jobs::AdmissionLimits {
        max_trials_per_input: 1,
        max_session_trials: 1,
    });
    let (c, cfg) = candidate_config(&candidate_first_dir, base_policy, candidate_first);
    candidate_book(&c);
    assert_eq!(
        candidate_submit(
            &c,
            &cfg,
            "grok-4.7",
            3,
            "turn-union",
            candidate_submit_args("candidate-first", CANDIDATE_SOURCE, None),
            t0,
            Some(candidate_evidence()),
        )
        .unwrap()["status"],
        "queued"
    );
    let registered_after = owner_task_enqueue_with_evidence(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-union",
        "registered-after-candidate",
        "Measure registered inputs",
        "shared_core_backtest",
        "synthetic_no_quotes_fixture",
        t0 + 1,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(registered_after["status"], "not_dispatched");
    assert_eq!(
        registered_after["reason"],
        "research_session_compute_budget_exhausted"
    );

    // Rewriting prose or the task key does not reset the frozen-input allowance.
    let same_dir = root.join("candidate-union-same-input");
    fs::create_dir_all(&same_dir).unwrap();
    let mut same = candidate_settings(&same_dir, base_settings);
    same.admission_limits = Some(jobs::AdmissionLimits {
        max_trials_per_input: 1,
        max_session_trials: 10,
    });
    let (c, cfg) = candidate_config(&same_dir, base_policy, same);
    candidate_book(&c);
    assert_eq!(
        candidate_submit(
            &c,
            &cfg,
            "grok-4.7",
            3,
            "turn-union",
            candidate_submit_args("same-one", CANDIDATE_SOURCE, None),
            t0,
            Some(candidate_evidence()),
        )
        .unwrap()["status"],
        "queued"
    );
    let mut rewritten = candidate_submit_args("same-two", CANDIDATE_SOURCE, None);
    rewritten.hypothesis = "A different description of the same frozen computation".into();
    let deferred = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-union",
        rewritten,
        t0 + 1,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(deferred["status"], "not_dispatched");
    assert_eq!(
        deferred["reason"],
        "research_unchanged_input_budget_exhausted"
    );
    let mut same_key = candidate_submit_args("same-one", CANDIDATE_SOURCE, None);
    same_key.hypothesis = "Another description of the same frozen computation".into();
    assert_eq!(
        candidate_submit(
            &c,
            &cfg,
            "grok-4.7",
            3,
            "turn-union",
            same_key,
            t0 + 2,
            Some(candidate_evidence()),
        )
        .unwrap()["status"],
        "existing"
    );

    // Cancellation is terminal but never refunds the input allowance.
    let cancel_dir = root.join("candidate-union-cancel");
    fs::create_dir_all(&cancel_dir).unwrap();
    let mut cancel = candidate_settings(&cancel_dir, base_settings);
    cancel.admission_limits = Some(jobs::AdmissionLimits {
        max_trials_per_input: 1,
        max_session_trials: 10,
    });
    let (c, cfg) = candidate_config(&cancel_dir, base_policy, cancel);
    candidate_book(&c);
    let cancelled = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-union",
        candidate_submit_args("cancel-one", CANDIDATE_SOURCE, None),
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    let cancelled_task = cancelled["task"]["task_id"].as_str().unwrap().to_owned();
    candidate_cancel(&c, &cfg, &cancelled_task, 1, t0 + 1).unwrap();
    let after_cancel = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-union",
        candidate_submit_args("cancel-two", CANDIDATE_SOURCE, None),
        t0 + 2,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(after_cancel["status"], "not_dispatched");
    assert_eq!(
        after_cancel["reason"],
        "research_unchanged_input_budget_exhausted"
    );

    // A deferred submission with new bytes leaves no orphan artifact rows.
    let orphan_dir = root.join("candidate-union-orphan");
    fs::create_dir_all(&orphan_dir).unwrap();
    let mut orphan = candidate_settings(&orphan_dir, base_settings);
    orphan.admission_limits = Some(jobs::AdmissionLimits {
        max_trials_per_input: 1,
        max_session_trials: 1,
    });
    let (c, cfg) = candidate_config(&orphan_dir, base_policy, orphan);
    candidate_book(&c);
    assert_eq!(
        candidate_submit(
            &c,
            &cfg,
            "grok-4.7",
            3,
            "turn-union",
            candidate_submit_args("orphan-one", CANDIDATE_SOURCE, None),
            t0,
            Some(candidate_evidence()),
        )
        .unwrap()["status"],
        "queued"
    );
    let db = candidate_queue(&c, &cfg);
    let before: i64 = db
        .query_row("SELECT count(*) FROM candidate_artifacts", [], |row| {
            row.get(0)
        })
        .unwrap();
    drop(db);
    let deferred_orphan = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-union",
        candidate_submit_args("orphan-two", CANDIDATE_SOURCE_ALT, None),
        t0 + 1,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(deferred_orphan["status"], "not_dispatched");
    let db = candidate_queue(&c, &cfg);
    let after: i64 = db
        .query_row("SELECT count(*) FROM candidate_artifacts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(before, after);
}

fn candidate_provenance_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-provenance");
    fs::create_dir_all(&dir).unwrap();
    let python = dir.join("fixture-python");
    let bwrap = dir.join("fixture-bwrap");
    fs::write(&python, b"fixture-python-v1").unwrap();
    fs::write(&bwrap, b"fixture-bwrap-v1").unwrap();
    let mut settings = candidate_settings(&dir, base_settings);
    settings
        .candidate_compute
        .as_mut()
        .unwrap()
        .sandbox
        .python_path = python.clone();
    settings
        .candidate_compute
        .as_mut()
        .unwrap()
        .sandbox
        .bwrap_path = bwrap;
    let (c, cfg) = candidate_config(&dir, base_policy, settings);
    candidate_book(&c);
    let t0 = candidate_at(9, 23);
    let submitted = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-provenance",
        candidate_submit_args("provenance", CANDIDATE_SOURCE, None),
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(submitted["status"], "queued");
    let task_id = submitted["task"]["task_id"].as_str().unwrap().to_owned();
    // Replace the configured interpreter bytes at the same path. The queued
    // task must refuse before any claim or spawn, not silently run new bytes.
    fs::write(&python, b"fixture-python-v2").unwrap();
    candidate_drive(&c, &cfg, &task_id, move || t0 + 1);
    let record = candidate_record(&c, &cfg, &task_id);
    assert_eq!(record.status, jobs::TaskState::Refused);
    assert_eq!(record.job.attempts, 0);
    assert_eq!(
        record
            .refusal_receipt
            .as_ref()
            .map(|receipt| receipt.reason),
        Some(jobs::RefusalReason::RuntimeExecutableChanged)
    );
    let db = candidate_queue(&c, &cfg);
    assert_eq!(candidate_launch_count(&db, &record.spec.job_id), 0);
}

fn candidate_candidate_only_contract(
    root: &Path,
    base_settings: &ResearchSettings,
    base_policy: &PlannerConfig,
) {
    let dir = root.join("candidate-only");
    fs::create_dir_all(&dir).unwrap();
    let mut settings = candidate_settings(&dir, base_settings);
    settings.registrations.clear();
    assert!(settings.validate().is_ok());
    let mut missing = settings.clone();
    missing
        .candidate_compute
        .as_mut()
        .unwrap()
        .sandbox
        .python_path = dir.join("missing-python");
    let readiness = status(&missing);
    assert_eq!(readiness["status"], "blocked");
    assert_eq!(readiness["candidate_compute"]["status"], "blocked");
    assert_eq!(
        readiness["candidate_compute"]["reason"],
        "candidate_sandbox_unavailable"
    );
    let (c, cfg) = candidate_config(&dir, base_policy, settings);
    candidate_book(&c);
    let t0 = candidate_at(9, 23);
    let submitted = candidate_submit(
        &c,
        &cfg,
        "grok-4.7",
        3,
        "turn-candidate-only",
        candidate_submit_args("candidate-only", CANDIDATE_SOURCE, None),
        t0,
        Some(candidate_evidence()),
    )
    .unwrap();
    assert_eq!(submitted["status"], "queued");
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
    let snapshots: Vec<sadr_core::strategy::FeatureSnapshot> =
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
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
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
    bad.unit = "sadr-candidate-".to_owned() + &"f".repeat(64);
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
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
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

fn research_launch(registered: bool) -> ResearchLaunch {
    ResearchLaunch {
        registered,
        candidate: true,
        reason: "fixture",
    }
}

#[test]
fn registered_file_failures_name_the_exact_artifact() {
    let root = tempfile::tempdir().unwrap();
    let default_settings = settings(root.path(), &root.path().join("runtime-default"));
    let encoded = serde_json::to_value(&default_settings).unwrap();
    assert!(encoded.get("termination_grace_ms").is_none());
    let mut invalid_grace = default_settings.clone();
    invalid_grace.termination_grace_ms = 300_001;
    assert_eq!(
        invalid_grace.validate().unwrap_err().to_string(),
        "research_settings_invalid"
    );

    // Normal work can exceed the cleanup grace; the grace starts only
    // after a poll reports that whole-unit termination is pending.
    let mut cleanup_deadline = None;
    assert!(!cleanup_grace_expired(
        false,
        60_000,
        DEFAULT_TERMINATION_GRACE_MS,
        &mut cleanup_deadline
    ));
    assert!(cleanup_deadline.is_none());
    assert!(!cleanup_grace_expired(
        true,
        60_000,
        DEFAULT_TERMINATION_GRACE_MS,
        &mut cleanup_deadline
    ));
    assert!(!cleanup_grace_expired(
        true,
        89_999,
        DEFAULT_TERMINATION_GRACE_MS,
        &mut cleanup_deadline
    ));
    assert!(cleanup_grace_expired(
        true,
        90_000,
        DEFAULT_TERMINATION_GRACE_MS,
        &mut cleanup_deadline
    ));
    let (sender, receiver) = std::sync::mpsc::channel();
    drop(sender);
    let mut runtime = OwnerResearchRuntime {
        active: Some((
            ResearchTaskKind::Registered,
            "a".repeat(64),
            2,
            Arc::new(AtomicBool::new(false)),
            receiver,
        )),
        ..OwnerResearchRuntime::default()
    };
    runtime.observe_channel();
    assert_eq!(
        runtime.blocked.as_ref().map(|block| block.3),
        Some("research_worker_channel_lost")
    );
    runtime.clear_blocked_if_terminal(
        ResearchTaskKind::Registered,
        &"a".repeat(64),
        2,
        jobs::TaskState::Running,
    );
    runtime.clear_blocked_if_terminal(
        ResearchTaskKind::Registered,
        &"a".repeat(64),
        2,
        jobs::TaskState::Unknown,
    );
    assert!(runtime.blocked.is_some());
    runtime.clear_blocked_if_terminal(
        ResearchTaskKind::Registered,
        &"a".repeat(64),
        2,
        jobs::TaskState::Terminated,
    );
    assert!(runtime.blocked.is_none());

    let missing = root.path().join("missing");
    for (artifact, code) in [
        (Artifact::Code, "research_code_unreadable"),
        (Artifact::Runtime, "research_runtime_unreadable"),
        (Artifact::Input, "research_input_unreadable"),
    ] {
        assert_eq!(
            verified(&missing, &digest(b""), artifact)
                .unwrap_err()
                .to_string(),
            code
        );
    }

    // A missing runtime binary is the deployed #114 failure, so execute() must
    // report the runtime code rather than blaming the dataset.
    let stale = settings(root.path(), &root.path().join("stale-sadr-analysis"));
    assert_eq!(
        execute(
            &stale,
            "shared_core_backtest",
            "synthetic_no_quotes_fixture",
            "review",
            "call",
            root.path(),
            || false
        )
        .unwrap_err()
        .to_string(),
        "research_runtime_unreadable"
    );

    let runtime = root.path().join("sadr-analysis");
    std::fs::write(&runtime, b"sadr-analysis-fixture").unwrap();
    let current = settings(root.path(), &runtime);
    assert_eq!(validate_files(&current), Ok(()));
    assert_eq!(status(&current)["status"], "ready");
    assert_eq!(
        verified(
            &runtime,
            &current.registrations[0].runtime_sha256,
            Artifact::Runtime
        )
        .unwrap(),
        b"sadr-analysis-fixture"
    );

    // Exercise the real queue identity/pin path and owner-scoped ledger.
    // Enqueue is idempotent, status/result stay nonblocking before any
    // worker receipt exists, and cancellation does not reuse that revision.
    // A later request of the same key is a new revision, not a revival.
    let owner_dir = tempfile::tempdir().unwrap();
    let (owner_config, owner_policy) = owner_fixture(owner_dir.path(), &current);
    let enqueue = |key: &str, at: i64| {
        owner_task_enqueue(
            &owner_config,
            &owner_policy,
            "grok-4.7",
            3,
            "turn-owner-fixture",
            key,
            "Compare the registered analysis output",
            "shared_core_backtest",
            "synthetic_no_quotes_fixture",
            at,
        )
    };
    let first = enqueue("daily-replay", 100_000).unwrap();
    assert_eq!(enqueue("daily-replay", 100_000).unwrap(), first);
    let task_id = first["task_id"].as_str().unwrap();
    let revision = first["revision"].as_u64().unwrap() as u32;
    assert_eq!(
        owner_task_status(&owner_config, &owner_policy, task_id, revision, 100_001).unwrap()["status"],
        "pending"
    );
    assert_eq!(
        owner_task_result(&owner_config, &owner_policy, task_id, revision, 100_001).unwrap()["result_status"],
        "not_verified"
    );
    assert_eq!(
        owner_task_cancel(&owner_config, &owner_policy, task_id, revision, 100_002).unwrap()["status"],
        "cancelled"
    );
    let next = enqueue("daily-replay", 100_003).unwrap();
    assert_eq!(next["task_id"], task_id);
    assert_eq!(next["revision"], revision + 1);
    assert_eq!(next["status"], "pending");
    assert_ne!(next["job_id"], first["job_id"]);
    let ledger = super::super::super::experiment_ledger::research::snapshot(&owner_config).unwrap();
    assert_eq!(ledger.0.len(), 2);
    assert_eq!(ledger.1.len(), 1);
    let child = ledger
        .0
        .iter()
        .find(|candidate| candidate["candidate_id"] == next["experiment_candidate_id"])
        .unwrap();
    assert_eq!(
        child["parent_candidate_id"],
        first["experiment_candidate_id"]
    );
    assert!(child["strategy_abi"].is_null());
    assert!(child["holding_horizon"].is_null());
    assert_eq!(ledger.1[0]["event"], "analysis_cancelled");
    assert_eq!(ledger.1[0]["edge_qualified"], false);
    let library =
        super::super::super::experiment_ledger::research::library(&owner_config, &owner_policy)
            .unwrap();
    assert_eq!(library["items"].as_array().unwrap().len(), 2);
    assert_eq!(library["activation_authorized"], false);
    assert!(
        library["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["strategy_abi"].is_null())
    );
    assert!(library.to_string().contains("analysis_cancelled"));
    let mut bounded_policy = owner_policy.clone();
    bounded_policy
        .research_tools
        .as_mut()
        .unwrap()
        .library_max_candidates = Some(1);
    let bounded =
        super::super::super::experiment_ledger::research::library(&owner_config, &bounded_policy)
            .unwrap();
    assert_eq!(bounded["items"].as_array().unwrap().len(), 1);
    assert_eq!(bounded["truncated"], true);
    assert_eq!(
        super::super::super::experiment_ledger::research::snapshot(&owner_config).unwrap(),
        ledger
    );

    // Content limits cover terminal revisions and different task keys/goals.
    // The rejection is durable; restarting/repeating does not spend a trial.
    let bounded_dir = tempfile::tempdir().unwrap();
    let (bounded_config, mut limited_policy) = owner_fixture(bounded_dir.path(), &current);
    limited_policy
        .research_tools
        .as_mut()
        .unwrap()
        .admission_limits = Some(jobs::AdmissionLimits {
        max_trials_per_input: 1,
        max_session_trials: 2,
    });
    let evidence = jobs::OwnerEvidence {
        digest_id: "digest-fixture".into(),
        owner_work_id: digest(b"work"),
        facts_sha256: digest(b"original-facts"),
        profile_sha256: None,
        prompt_hashes_sha256: digest(b"original-prompts"),
        owner_claim_sha256: digest(b"original-claim"),
    };
    let bounded_enqueue = |key: &str, policy: &PlannerConfig| {
        owner_task_enqueue_with_evidence(
            &bounded_config,
            policy,
            "grok-4.7",
            3,
            "turn-owner-fixture",
            key,
            "Measure registered inputs",
            "shared_core_backtest",
            "synthetic_no_quotes_fixture",
            100_000,
            Some(evidence.clone()),
        )
    };
    let admitted = bounded_enqueue("bounded-one", &limited_policy).unwrap();
    assert_eq!(admitted["status"], "pending");
    assert_eq!(
        bounded_enqueue("bounded-one", &limited_policy).unwrap(),
        admitted
    );
    let persisted = scoped_task(
        &bounded_config,
        &limited_policy,
        admitted["task_id"].as_str().unwrap(),
        1,
    )
    .unwrap()
    .1;
    assert_eq!(persisted.spec.owner_evidence, Some(evidence.clone()));
    owner_task_cancel(
        &bounded_config,
        &limited_policy,
        admitted["task_id"].as_str().unwrap(),
        1,
        100_000,
    )
    .unwrap();
    for key in ["bounded-one", "different-key"] {
        let deferred = bounded_enqueue(key, &limited_policy).unwrap();
        assert_eq!(deferred["status"], "not_dispatched");
        assert_eq!(
            deferred["reason"],
            "research_unchanged_input_budget_exhausted"
        );
    }
    let mut session_limited = limited_policy.clone();
    session_limited
        .research_tools
        .as_mut()
        .unwrap()
        .admission_limits = Some(jobs::AdmissionLimits {
        max_trials_per_input: 1,
        max_session_trials: 1,
    });
    assert_eq!(
        bounded_enqueue("session-limit", &session_limited).unwrap()["reason"],
        "research_session_compute_budget_exhausted"
    );
    let (mut bounded_queue, _) = scoped_task(
        &bounded_config,
        &limited_policy,
        admitted["task_id"].as_str().unwrap(),
        1,
    )
    .unwrap();
    let deferrals: i64 = bounded_queue
        .query_row("SELECT count(*) FROM scoped_admission_deferrals", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(deferrals, 3);
    assert_eq!(
        bounded_enqueue("different-key", &limited_policy).unwrap()["status"],
        "not_dispatched"
    );
    assert_eq!(
        bounded_queue
            .query_row("SELECT count(*) FROM scoped_admission_deferrals", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        deferrals
    );
    assert_eq!(
        jobs::list_scoped(
            &mut bounded_queue,
            &bounded_config.session.to_string(),
            None,
            100
        )
        .unwrap()
        .len(),
        1
    );

    let (mut queue, mut changed) =
        scoped_task(&owner_config, &owner_policy, task_id, revision + 1).unwrap();
    changed.spec.goal = "Changed immutable hypothesis".into();
    assert_eq!(
        super::super::super::experiment_ledger::research::reconcile(
            &owner_config,
            &mut queue,
            &changed
        )
        .unwrap_err()
        .to_string(),
        "research_candidate_identity_changed"
    );
    changed.spec.goal = "Compare the registered analysis output".into();
    changed.status = jobs::TaskState::Succeeded;
    changed.terminal_at_ms = Some(100_004);
    assert_eq!(
        super::super::super::experiment_ledger::research::reconcile(
            &owner_config,
            &mut queue,
            &changed
        )
        .unwrap_err()
        .to_string(),
        "research_candidate_result_unverified"
    );
    assert_eq!(
        super::super::super::experiment_ledger::research::snapshot(&owner_config).unwrap(),
        ledger
    );
    assert_eq!(
        owner_task_status(&owner_config, &owner_policy, task_id, revision, 100_003).unwrap()["status"],
        "cancelled"
    );

    // A later session sharing the original queue must not refuse or relaunch
    // the predecessor revision. Resolution and adoption are read-only.
    let mut shared_successor = owner_config.clone();
    shared_successor.session = "2026-10-19".parse().unwrap();
    let mut shared_book = Connection::open_in_memory().unwrap();
    let mut shared_runtime = OwnerResearchRuntime::default();
    let retained = tick_owner_tasks(
        &mut shared_book,
        &shared_successor,
        &owner_policy,
        research_launch(true),
        &mut shared_runtime,
        100_004,
    )
    .unwrap();
    assert_eq!(retained["status"], "predecessor_retained");
    assert_eq!(retained["action"], "none");
    assert_eq!(retained["task_id"], next["task_id"]);
    assert!(shared_runtime.active.is_none());
    assert_eq!(
        owner_task_status(
            &owner_config,
            &owner_policy,
            next["task_id"].as_str().unwrap(),
            next["revision"].as_u64().unwrap() as u32,
            100_004,
        )
        .unwrap()["status"],
        "pending"
    );
    let mut running_queue = open_queue_for_owner(
        owner_policy.research_tools.as_ref().unwrap(),
        &owner_config.directory.join("model"),
        false,
    )
    .unwrap();
    jobs::claim(
        &mut running_queue,
        next["job_id"].as_str().unwrap(),
        &"e".repeat(64),
        100_005,
    )
    .unwrap();
    drop(running_queue);
    let running = tick_owner_tasks(
        &mut shared_book,
        &shared_successor,
        &owner_policy,
        research_launch(true),
        &mut shared_runtime,
        100_006,
    )
    .unwrap();
    assert_eq!(running["status"], "predecessor_retained");
    assert_eq!(running["task_status"], "running");
    assert!(shared_runtime.active.is_none());
    assert_eq!(
        owner_task_status(
            &owner_config,
            &owner_policy,
            next["task_id"].as_str().unwrap(),
            next["revision"].as_u64().unwrap() as u32,
            100_006,
        )
        .unwrap()["status"],
        "running"
    );

    let family = tempfile::tempdir().unwrap();
    let source_dir = family.path().join("2026-10-16");
    std::fs::create_dir_all(&source_dir).unwrap();
    let (mut source_config, source_policy) = owner_fixture(&source_dir, &current);
    source_config.sessions_root = Some(family.path().to_owned());
    let carried_task = owner_task_enqueue(
        &source_config,
        &source_policy,
        "grok-4.7",
        3,
        "turn-source",
        "carry-me",
        "Compare the registered analysis output",
        "shared_core_backtest",
        "synthetic_no_quotes_fixture",
        110_000,
    )
    .unwrap();
    let carried = carry_state(&source_config, &source_policy);
    let seed = json!({"source_session":"2026-10-16","research_state":carried});
    let successor_dir = family.path().join("2026-10-19");
    std::fs::create_dir_all(&successor_dir).unwrap();
    let mut successor = source_config.clone();
    successor.directory = successor_dir.clone();
    successor.session = "2026-10-19".parse().unwrap();
    successor.sessions_root = Some(family.path().to_owned());
    write_test_carry(&source_config, &successor);
    // The successor has its OWN queue. The dated planner chain records the
    // predecessor's original queue locator; a carry read must load that, not
    // the successor policy's queue.
    let mut successor_policy = source_policy.clone();
    successor_policy.research_tools.as_mut().unwrap().queue = successor_dir.join("research.sqlite");
    successor_policy.research_tools.as_mut().unwrap().stage_root =
        successor_dir.join("research-stage");
    successor_policy.effective_from = Some("2026-10-19".parse().unwrap());
    let source_planner = family.path().join("planner-source.json");
    let successor_planner = family.path().join("planner.json");
    successor_policy.previous_config = Some(source_planner.clone());
    fs::write(&source_planner, serde_json::to_vec(&source_policy).unwrap()).unwrap();
    fs::write(
        &successor_planner,
        serde_json::to_vec(&successor_policy).unwrap(),
    )
    .unwrap();
    successor.planner_config = Some(successor_planner);
    let resolved = resolve_carried_research(&successor, &successor_policy, &seed);
    assert_eq!(resolved["status"], "resolved");
    assert_eq!(resolved["launched"], false);
    assert_eq!(resolved["action"], "none");
    assert_eq!(resolved["tasks"][0]["resolution"], "matched");
    assert_eq!(resolved["tasks"][0]["status"], "pending");
    assert_eq!(resolved["tasks"][0]["task_id"], carried_task["task_id"]);
    assert!(
        !successor_dir
            .join("model")
            .join("research-adoption.json")
            .exists()
    );
    let adopted = adopt_carried_research(&successor, &successor_policy, &seed);
    assert_eq!(adopted["adoption"], "recorded");
    assert_eq!(adopted["launched"], false);
    assert_eq!(
        adopt_carried_research(&successor, &successor_policy, &seed)["adoption"],
        "recorded"
    );
    let mut changed = seed.clone();
    changed["research_state"]["tasks"][0]["task_id"] = json!("f".repeat(64));
    assert_eq!(
        adopt_carried_research(&successor, &successor_policy, &changed)["status"],
        "adoption_conflict"
    );
    assert_eq!(
        owner_task_status(
            &source_config,
            &source_policy,
            carried_task["task_id"].as_str().unwrap(),
            carried_task["revision"].as_u64().unwrap() as u32,
            110_001,
        )
        .unwrap()["status"],
        "pending"
    );
    let mut successor_book = Connection::open_in_memory().unwrap();
    let mut successor_runtime = OwnerResearchRuntime::default();
    assert_eq!(
        tick_owner_tasks(
            &mut successor_book,
            &successor,
            &source_policy,
            research_launch(true),
            &mut successor_runtime,
            110_002,
        )
        .unwrap()["status"],
        "unavailable"
    );
    assert!(successor_runtime.active.is_none());

    // Expiry is enforced before a deferred launch, and a lost worker
    // channel leaves the occupied task explicitly blocked on later ticks.
    let expiry_dir = tempfile::tempdir().unwrap();
    let (expiry_config, expiry_policy) = owner_fixture(expiry_dir.path(), &current);
    let expiring = owner_task_enqueue(
        &expiry_config,
        &expiry_policy,
        "grok-4.7",
        3,
        "turn-expiry-fixture",
        "expire-me",
        "Compare the registered analysis output",
        "shared_core_backtest",
        "synthetic_no_quotes_fixture",
        200_000,
    )
    .unwrap();
    let mut controller_db = Connection::open_in_memory().unwrap();
    let mut controller = OwnerResearchRuntime::default();
    let expired_at = 200_000 + current.task_ttl_ms as i64 + 1;
    assert_eq!(
        tick_owner_tasks(
            &mut controller_db,
            &expiry_config,
            &expiry_policy,
            research_launch(false),
            &mut controller,
            expired_at,
        )
        .unwrap()["status"],
        "refused_expired"
    );
    assert_eq!(
        owner_task_status(
            &expiry_config,
            &expiry_policy,
            expiring["task_id"].as_str().unwrap(),
            expiring["revision"].as_u64().unwrap() as u32,
            expired_at,
        )
        .unwrap()["status"],
        "refused"
    );

    let blocked_dir = tempfile::tempdir().unwrap();
    let (blocked_config, blocked_policy) = owner_fixture(blocked_dir.path(), &current);
    let blocked_task = owner_task_enqueue(
        &blocked_config,
        &blocked_policy,
        "grok-4.7",
        3,
        "turn-channel-fixture",
        "channel-loss",
        "Compare the registered analysis output",
        "shared_core_backtest",
        "synthetic_no_quotes_fixture",
        300_000,
    )
    .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    drop(sender);
    let mut blocked_runtime = OwnerResearchRuntime {
        active: Some((
            ResearchTaskKind::Registered,
            blocked_task["task_id"].as_str().unwrap().to_owned(),
            blocked_task["revision"].as_u64().unwrap() as u32,
            Arc::new(AtomicBool::new(false)),
            receiver,
        )),
        ..OwnerResearchRuntime::default()
    };
    let mut blocked_db = Connection::open_in_memory().unwrap();
    for _ in 0..2 {
        assert_eq!(
            tick_owner_tasks(
                &mut blocked_db,
                &blocked_config,
                &blocked_policy,
                research_launch(true),
                &mut blocked_runtime,
                300_001,
            )
            .unwrap()["status"],
            "reconciliation_required"
        );
    }
    assert_eq!(
        owner_task_status(
            &blocked_config,
            &blocked_policy,
            blocked_task["task_id"].as_str().unwrap(),
            blocked_task["revision"].as_u64().unwrap() as u32,
            300_001,
        )
        .unwrap()["status"],
        "pending"
    );

    // A blocked research task and a queued question do not stop owner
    // decisions, and the blocked task is not restarted.
    let progress_dir = tempfile::tempdir().unwrap();
    let (progress_config, mut progress_policy) =
        super::super::super::decision_owner::owner_fixture_config(progress_dir.path());
    let mut progress_research = current.clone();
    progress_research.queue = progress_dir.path().join("research.sqlite");
    progress_research.stage_root = progress_dir.path().join("research-stage");
    progress_policy.research_tools = Some(progress_research);
    let progress_task = owner_task_enqueue(
        &progress_config,
        &progress_policy,
        "grok-4.7",
        3,
        "turn-progress",
        "blocked-task",
        "Compare the registered analysis output",
        "shared_core_backtest",
        "synthetic_no_quotes_fixture",
        400_000,
    )
    .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    drop(sender);
    let mut progress_runtime = OwnerResearchRuntime {
        active: Some((
            ResearchTaskKind::Registered,
            progress_task["task_id"].as_str().unwrap().to_owned(),
            progress_task["revision"].as_u64().unwrap() as u32,
            Arc::new(AtomicBool::new(false)),
            receiver,
        )),
        ..OwnerResearchRuntime::default()
    };
    let mut progress_book = super::super::super::open_book(&progress_config).unwrap();
    super::super::super::wide::ensure_actions(&progress_book).unwrap();
    let mut progress_portfolio = super::super::super::tests::test_portfolio();
    progress_portfolio.model_led = true;
    let progress_at = chrono::DateTime::parse_from_rfc3339("2026-10-16T09:16:00+05:30")
        .unwrap()
        .timestamp_millis();
    super::super::super::redecision::initialize(
        &mut progress_book,
        &progress_config,
        &progress_portfolio,
        Some(&progress_policy),
        progress_at,
    )
    .unwrap();
    assert_eq!(
        tick_owner_tasks(
            &mut progress_book,
            &progress_config,
            &progress_policy,
            research_launch(true),
            &mut progress_runtime,
            progress_at + 1,
        )
        .unwrap()["status"],
        "reconciliation_required"
    );
    assert!(progress_runtime.active.is_none());
    let question = super::super::super::agent_cli::queue_question(
        &progress_config,
        "Why was the entry skipped?",
        "ask-progress",
        progress_at + 2,
    )
    .unwrap();
    assert_eq!(question["status"], "queued");
    let first_claim = super::super::super::redecision::reserve(
        &mut progress_book,
        &progress_config,
        &progress_policy,
        &progress_portfolio,
        progress_at + 3,
        true,
    )
    .unwrap()
    .unwrap();
    let instrument = super::super::super::decision_owner::exercise_instrument();
    let input = json!({
        "allowlist":[instrument],
        "cash":progress_portfolio.cash,
        "fees":progress_portfolio.costs,
        "positions":progress_portfolio.positions,
    });
    let plan = json!({
        "decision_id":format!("{}-{}", progress_config.session, first_claim.id),
        "attempt_id":first_claim.id,
        "session_binding":super::super::super::book_binding(&progress_config).unwrap(),
        "input_hash":digest(&serde_json::to_vec(&input).unwrap()),
        "input":input,
        "portfolio_revision":first_claim.portfolio_hash,
        "policy_sha256":digest(&super::super::super::planner::policy_bytes(&progress_policy).unwrap()),
        "profile_sha256":progress_policy.profile_sha256().map(str::to_owned),
        "digest_id":"d".repeat(64),
        "owner_digest":{"version":1,"digest":{"complete":true},"tool_results":[],"eligible":true,"facts_sha256":"0".repeat(64)},
        "luna_handoff_requested":false,
        "fake":false,
        "dry_run":false,
        "verdicts":[],
        "valid_until_ms":first_claim.deadline_ms
    });
    super::super::super::redecision::settle(
        &mut progress_book,
        &progress_config,
        &progress_policy,
        &progress_portfolio,
        &first_claim,
        Ok(plan),
        progress_at + 3,
        true,
    )
    .unwrap();
    progress_book
        .execute(
            "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
            rusqlite::params![
                "owner-progress-next",
                progress_at + 4,
                json!({"id":"owner-progress-next","kind":"price_move","instrument_key":instrument.key,"observed_ms":progress_at+4,"evidence":{}}).to_string()
            ],
        )
        .unwrap();
    let second_claim = super::super::super::redecision::reserve(
        &mut progress_book,
        &progress_config,
        &progress_policy,
        &progress_portfolio,
        progress_at + 4,
        true,
    )
    .unwrap()
    .unwrap();
    assert_ne!(second_claim.id, first_claim.id);
    assert_eq!(
        owner_task_status(
            &progress_config,
            &progress_policy,
            progress_task["task_id"].as_str().unwrap(),
            progress_task["revision"].as_u64().unwrap() as u32,
            progress_at + 5,
        )
        .unwrap()["status"],
        "pending"
    );
    assert_eq!(
        tick_owner_tasks(
            &mut progress_book,
            &progress_config,
            &progress_policy,
            research_launch(false),
            &mut progress_runtime,
            progress_at + 5,
        )
        .unwrap()["status"],
        "reconciliation_required"
    );
    assert!(progress_runtime.active.is_none());
    assert_eq!(question["id"], "ask-progress");

    std::fs::write(&runtime, b"replaced").unwrap();
    assert_eq!(
        validate_files(&current).unwrap_err(),
        "research_registered_bytes_changed"
    );
    let blocked = status(&current);
    assert_eq!(blocked["status"], "blocked");
    assert_eq!(
        blocked["registrations"][0]["files"][1]["reason"],
        "research_registered_bytes_changed"
    );
    assert!(
        !blocked
            .to_string()
            .contains(&root.path().to_string_lossy().to_string())
    );
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
        let runtime = root.path().join("sadr-analysis");
        fs::write(&runtime, b"sadr-analysis-fixture").unwrap();
        let current = settings(root.path(), &runtime);
        if sandboxed && !candidate_sandbox_ready(&candidate_settings(root.path(), &current)) {
            assert!(
                std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
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
fn candidate_admission_provenance_and_core_contracts() {
    run_contracts(
        &[
            ("admission_union", candidate_admission_union),
            ("provenance", candidate_provenance_contract),
            ("candidate_only", candidate_candidate_only_contract),
        ],
        false,
    );
    run_contracts(&[("candidate_core", candidate_contract)], true);
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
            ("observation", case_observation_contract),
            ("carry", case_carry_contract),
            ("e2", case_e2_contract),
            ("e2_bounds", case_e2_bounds_contract),
            ("e3", case_e3_contract),
            ("e3_bounds", case_e3_bounds_contract),
            ("e3_paging_lineage", case_e3_paging_lineage_contract),
        ],
        true,
    );
}

#[test]
fn continuation_and_corrective_contracts() {
    run_contracts(
        &[
            ("e4_continuation", case_e4_continuation_contract),
            ("e5_corrective", case_e5_corrective_contract),
            ("e6_corrective", case_e6_corrective_contract),
            ("e7_corrective", case_e7_corrective_contract),
            ("e8_corrective", case_e8_corrective_contract),
            ("e9_corrective", case_e9_corrective_contract),
            ("e10_corrective", case_e10_corrective_contract),
        ],
        true,
    );
}
