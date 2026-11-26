use super::*;
use std::{cell::Cell, rc::Rc};
struct Fake {
    clock: Rc<Cell<i64>>,
    launch: Option<Launch>,
    starts: usize,
    mode: u8,
    terminate_fail: bool,
    advance: Option<i64>,
}
impl Fake {
    fn new(clock: Rc<Cell<i64>>, mode: u8) -> Self {
        Self {
            clock,
            launch: None,
            starts: 0,
            mode,
            terminate_fail: false,
            advance: None,
        }
    }
    fn stopped(&self) -> StoppedReceipt {
        StoppedReceipt {
            identity: self.launch.as_ref().unwrap().identity.clone(),
            evidence_sha256: "d".repeat(64),
        }
    }
}
impl SandboxRunner for Fake {
    fn start(&mut self, l: &Launch, _: &RegisteredRun) -> std::result::Result<(), RunnerError> {
        self.starts += 1;
        self.launch = Some(l.clone());
        if self.mode == 1 {
            Err(RunnerError::Ambiguous)
        } else {
            Ok(())
        }
    }
    fn poll(&mut self, _: &ExecutionIdentity) -> std::result::Result<SandboxPoll, RunnerError> {
        if let Some(at) = self.advance {
            self.clock.set(at);
        }
        if self.mode == 2 {
            return Err(RunnerError::Unavailable);
        }
        if self.mode == 3 {
            return Ok(SandboxPoll::Running);
        }
        let l = self.launch.as_ref().unwrap();
        let mut result = AnalysisResult {
            identity: l.identity.clone(),
            script_hash: l.spec.script_hash.clone(),
            runtime_hash: l.spec.runtime_hash.clone(),
            input_hash: l.spec.input_hash.clone(),
            metrics: vec![Metric {
                name: "count".into(),
                value: "1".into(),
            }],
            observations: vec!["Synthetic computation only".into()],
        };
        if self.mode == 4 {
            result.identity.lease = "e".repeat(64);
        }
        if self.mode == 5 {
            result.metrics[0].value = "NaN".into();
        }
        let mut raw = canonical(&result).unwrap();
        if self.mode == 6 {
            raw = br#"{"identity":{},"identity":{}}"#.to_vec();
        }
        if self.mode == 7 {
            raw = vec![b'x'; 4097];
        }
        if self.mode == 8 {
            result.metrics.push(result.metrics[0].clone());
            raw = canonical(&result).unwrap();
        }
        if self.mode == 9 {
            raw = String::from_utf8(raw)
                .unwrap()
                .replacen("{", "{\"output_path\":\"/tmp/escape\",", 1)
                .into_bytes();
        }
        let mut stopped = self.stopped();
        if self.mode == 10 {
            stopped.identity.job_id = "f".repeat(64);
        }
        Ok(SandboxPoll::Exited {
            stopped,
            exit_code: if self.mode == 11 { 1 } else { 0 },
            output: raw,
        })
    }
    fn terminate(
        &mut self,
        _: &ExecutionIdentity,
    ) -> std::result::Result<StoppedReceipt, RunnerError> {
        if self.terminate_fail {
            Err(RunnerError::Ambiguous)
        } else {
            Ok(self.stopped())
        }
    }
}
fn setup_with_expiry(expires_at_ms: i64) -> (Connection, RegisteredRun, Rc<Cell<i64>>) {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch("PRAGMA synchronous=FULL").unwrap();
    jobs::initialize(&mut db).unwrap();
    let (code, runtime, input) = (
        b"registered-analysis-v1".to_vec(),
        b"reviewed-static-runtime".to_vec(),
        b"development-data".to_vec(),
    );
    let task_id = jobs::scoped_task_id(&"1".repeat(64), "2026-10-31", "shadow-check");
    let job_id = jobs::scoped_job_id(&task_id, 1);
    let spec = Spec {
        id: job_id.clone(),
        review_id: "a".repeat(64),
        script_hash: hash(&code),
        runtime_hash: hash(&runtime),
        input_hash: hash(&input),
        memory_bytes: 64 * 1024 * 1024,
        output_bytes: 4096,
        wall_millis: 1000,
        retry_safe_compute: false,
        max_attempts: 1,
        capability: jobs::SpecCapability::RegisteredAnalysis,
    };
    jobs::register_reviewed(&mut db, &spec.script_hash, &spec.runtime_hash).unwrap();
    jobs::enqueue_scoped(
        &mut db,
        &spec,
        &jobs::ScopedTaskSpec {
            owner_evidence: None,
            version: 1,
            task_id,
            revision: 1,
            task_key: "shadow-check".into(),
            job_id: spec.id.clone(),
            parent_turn_id: "turn-2026-10-31-1".into(),
            session: "2026-10-31".into(),
            book_binding: "1".repeat(64),
            policy_sha256: "2".repeat(64),
            model_id: "gpt-6.1-sol".into(),
            model_revision: 3,
            goal: "Evaluate the registered analysis on the development split".into(),
            registered_analysis: jobs::RegisteredAnalysis {
                analysis_id: "registered-analysis".into(),
                dataset_id: "development-v1".into(),
                code_sha256: spec.script_hash.clone(),
                runtime_sha256: spec.runtime_hash.clone(),
                input_sha256: spec.input_hash.clone(),
            },
            capability: jobs::Capability::RegisteredAnalysis,
            resources: jobs::ResourceCaps {
                cpu_millis: 500,
                tasks: 2,
                workspace_bytes: 1_048_576,
                file_bytes: 4096,
                memory_bytes: spec.memory_bytes,
                output_bytes: spec.output_bytes,
                wall_millis: spec.wall_millis,
            },
            created_at_ms: 100,
            expires_at_ms,
        },
        100,
    )
    .unwrap();
    (
        db,
        RegisteredRun {
            spec,
            split: DevelopmentSplit::Development,
            limits: ResourceLimits {
                cpu_millis: 500,
                tasks: 2,
                workspace_bytes: 1048576,
                file_bytes: 4096,
            },
            code,
            runtime,
            input,
        },
        Rc::new(Cell::new(100)),
    )
}
fn setup() -> (Connection, RegisteredRun, Rc<Cell<i64>>) {
    setup_with_expiry(60_100)
}
#[test]
fn registered_worker_retains_uncertain_slots_and_imports_only_bound_results() {
    let lease = "c".repeat(64);
    let task_id = jobs::scoped_task_id(&"1".repeat(64), "2026-10-31", "shadow-check");
    let (mut scoped_db, scoped_registration, _) = setup();
    let initial = jobs::load_scoped(&mut scoped_db, &task_id, 1).unwrap();
    assert_eq!(initial.status, jobs::TaskState::Pending);
    assert_eq!(
        jobs::enqueue_scoped(
            &mut scoped_db,
            &scoped_registration.spec,
            &initial.spec,
            101,
        )
        .unwrap(),
        initial
    );
    assert!(
        read_verified_result(&mut scoped_db, &task_id, 1)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        jobs::enqueue_scoped(
            &mut scoped_db,
            &scoped_registration.spec,
            &initial.spec,
            100
        )
        .unwrap(),
        initial
    );
    let mut changed_task = initial.spec.clone();
    changed_task.goal.push_str(" with changed bytes");
    assert_eq!(
        jobs::enqueue_scoped(
            &mut scoped_db,
            &scoped_registration.spec,
            &changed_task,
            100
        )
        .unwrap_err(),
        jobs::Error::Collision
    );
    assert_eq!(
        jobs::active_scoped(&mut scoped_db)
            .unwrap()
            .unwrap()
            .spec
            .task_id,
        initial.spec.task_id
    );
    assert_eq!(
        jobs::list_scoped(&mut scoped_db, "2026-10-31", None, 100)
            .unwrap()
            .len(),
        1
    );
    let cancelled = jobs::cancel_scoped(&mut scoped_db, &initial.spec.task_id, 1, 1, 101).unwrap();
    assert_eq!(cancelled.status, jobs::TaskState::Cancelled);
    assert!(cancelled.cancel_receipt.is_some());
    assert_eq!(
        jobs::cancel_scoped(&mut scoped_db, &initial.spec.task_id, 1, 1, 102).unwrap(),
        cancelled
    );
    let mut cancelled_revision = initial.spec.clone();
    cancelled_revision.revision = 2;
    cancelled_revision.job_id = jobs::scoped_job_id(&cancelled_revision.task_id, 2);
    cancelled_revision.created_at_ms = 102;
    cancelled_revision.expires_at_ms = 60_102;
    let mut cancelled_job = scoped_registration.spec.clone();
    cancelled_job.id = cancelled_revision.job_id.clone();
    assert_eq!(
        jobs::enqueue_scoped(&mut scoped_db, &cancelled_job, &cancelled_revision, 102)
            .unwrap()
            .status,
        jobs::TaskState::Pending
    );
    let successor = jobs::active_scoped(&mut scoped_db).unwrap().unwrap();
    assert_eq!(successor.spec.revision, 2);
    assert_eq!(successor.status, jobs::TaskState::Pending);
    assert_eq!(
        jobs::load_scoped(&mut scoped_db, &initial.spec.task_id, 1)
            .unwrap()
            .status,
        jobs::TaskState::Cancelled
    );
    let (mut refusal_db, refusal_registration, _) = setup();
    let pending = jobs::load_scoped(&mut refusal_db, &task_id, 1).unwrap();
    let refused = jobs::refuse_unstarted(
        &mut refusal_db,
        &pending.spec.task_id,
        1,
        jobs::RefusalReason::RegistrationChanged,
        101,
    )
    .unwrap();
    assert_eq!(refused.status, jobs::TaskState::Refused);
    assert!(refused.refusal_receipt.is_some());
    assert!(jobs::active_scoped(&mut refusal_db).unwrap().is_none());
    let mut next_spec = refusal_registration.spec.clone();
    let mut next_task = pending.spec.clone();
    next_task.revision = 2;
    next_task.job_id = jobs::scoped_job_id(&next_task.task_id, 2);
    next_spec.id = next_task.job_id.clone();
    next_task.parent_turn_id = "turn-2026-10-31-2".into();
    next_task.goal.push_str(" revised");
    next_task.created_at_ms = 102;
    next_task.expires_at_ms = 60_102;
    assert_eq!(
        jobs::enqueue_scoped(&mut refusal_db, &next_spec, &next_task, 102)
            .unwrap()
            .status,
        jobs::TaskState::Pending
    );
    assert_eq!(
        jobs::latest_scoped(&mut refusal_db, &next_task.task_id)
            .unwrap()
            .unwrap()
            .spec
            .revision,
        2
    );
    let (mut mismatched_db, mut mismatched, mismatched_clock) = setup();
    let mut mismatched_runner = Fake::new(mismatched_clock.clone(), 0);
    mismatched.input.push(b'x');
    assert!(
        start_one(
            &mut mismatched_db,
            &mismatched,
            &lease,
            &mut mismatched_runner,
            || mismatched_clock.get()
        )
        .is_err()
    );
    assert_eq!(mismatched_runner.starts, 0);
    assert_eq!(
        jobs::load_scoped(&mut mismatched_db, &task_id, 1)
            .unwrap()
            .status,
        jobs::TaskState::Refused
    );
    let (mut db, mut registered, clock) = setup();
    let mut runner = Fake::new(clock.clone(), 0);
    registered.spec.memory_bytes += 1;
    assert!(start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).is_err());
    assert_eq!(runner.starts, 0);
    assert_eq!(
        jobs::load_scoped(&mut db, &task_id, 1).unwrap().status,
        jobs::TaskState::Pending
    );
    registered.spec.memory_bytes -= 1;
    let mut worker = start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
    assert_eq!(
        jobs::load(&mut db, &registered.spec.id).unwrap().state,
        State::Running
    );
    assert!(start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).is_err());
    assert_eq!(runner.starts, 1);
    // A state/capacity refusal must leave no launch receipt for the
    // rejected lease. This is the deterministic retry-poison regression.
    let refused_lease = "b".repeat(64);
    assert!(
        start_one(&mut db, &registered, &refused_lease, &mut runner, || clock
            .get())
        .is_err()
    );
    let refused_identity = ExecutionIdentity {
        job_id: registered.spec.id.clone(),
        lease: refused_lease,
        registration_sha256: String::new(),
    };
    assert!(saved(&db, &refused_identity, "launch").unwrap().is_none());
    let identity = worker.identity().clone();
    assert!(matches!(
        worker
            .poll(&mut db, &mut runner, || clock.get(), false)
            .unwrap(),
        Outcome::Succeeded(_)
    ));
    let job = jobs::load(&mut db, &registered.spec.id).unwrap();
    assert_eq!(job.state, State::Succeeded);
    let bytes = saved(&db, &identity, "result").unwrap().unwrap();
    assert_eq!(job.result_hash, Some(hash(&bytes)));
    let verified = read_verified_result(&mut db, &task_id, 1).unwrap().unwrap();
    assert_eq!(verified.job_id, registered.spec.id);
    assert_eq!(verified.result_hash, job.result_hash.unwrap());
    assert_eq!(verified.result.metrics[0].name, "count");
    assert!(recover(&mut db, &registered, &lease, || clock.get()).is_err());
    assert_eq!(runner.starts, 1);
    let (mut expiry_db, expiry_registration, expiry_clock) = setup_with_expiry(106);
    let mut expiry_runner = Fake::new(expiry_clock.clone(), 0);
    let start_clock = expiry_clock.clone();
    let mut expiry_worker = start_one(
        &mut expiry_db,
        &expiry_registration,
        &lease,
        &mut expiry_runner,
        move || {
            let now = start_clock.get() + 1;
            start_clock.set(now);
            now
        },
    )
    .unwrap();
    let poll_clock = expiry_clock.clone();
    assert!(matches!(
        expiry_worker
            .poll(
                &mut expiry_db,
                &mut expiry_runner,
                move || {
                    let now = poll_clock.get() + 1;
                    poll_clock.set(now);
                    now
                },
                false
            )
            .unwrap(),
        Outcome::AwaitingTermination { .. }
    ));
    assert_ne!(
        jobs::load(&mut expiry_db, &expiry_registration.spec.id)
            .unwrap()
            .state,
        State::Succeeded
    );
    assert!(
        read_verified_result(&mut expiry_db, &task_id, 1)
            .unwrap()
            .is_none()
    );
    assert_eq!(expiry_runner.starts, 1);
    let (mut fault_db, fault_registration, fault_clock) = setup();
    let mut fault_runner = Fake::new(fault_clock.clone(), 0);
    let mut fault_worker = start_one(
        &mut fault_db,
        &fault_registration,
        &lease,
        &mut fault_runner,
        || fault_clock.get(),
    )
    .unwrap();
    fault_db.execute_batch("CREATE TRIGGER reject_result BEFORE INSERT ON registered_worker_receipts WHEN NEW.kind='result' BEGIN SELECT RAISE(FAIL,'injected storage failure'); END").unwrap();
    assert!(
        fault_worker
            .poll(
                &mut fault_db,
                &mut fault_runner,
                || fault_clock.get(),
                false
            )
            .is_err()
    );
    assert_eq!(
        jobs::load(&mut fault_db, &fault_registration.spec.id)
            .unwrap()
            .state,
        State::Running
    );
    assert!(
        saved(&fault_db, fault_worker.identity(), "result")
            .unwrap()
            .is_none()
    );
    let (mut import_db, import_registration, _) = setup();
    let import_clock = Rc::new(Cell::new(100));
    let mut import_runner = Fake::new(import_clock.clone(), 0);
    let mut import_worker = start_one(
        &mut import_db,
        &import_registration,
        &lease,
        &mut import_runner,
        || import_clock.get(),
    )
    .unwrap();
    let calls = Cell::new(0);
    assert!(matches!(
        import_worker
            .poll(
                &mut import_db,
                &mut import_runner,
                || {
                    let n = calls.get();
                    calls.set(n + 1);
                    if n < 2 { 100 } else { 1100 }
                },
                false
            )
            .unwrap(),
        Outcome::AwaitingTermination { .. }
    ));
    assert_eq!(
        jobs::load(&mut import_db, &import_registration.spec.id)
            .unwrap()
            .state,
        State::CancelRequested
    );
    assert!(
        saved(&import_db, import_worker.identity(), "result")
            .unwrap()
            .is_some()
    );
    // A fresh sample inside completion admission that is past the effective
    // deadline keeps the imported result and requests termination. It does
    // not mark the job succeeded or release the slot.
    let (mut locked_db, locked_registration, locked_clock) = setup();
    let mut locked_runner = Fake::new(locked_clock, 0);
    let mut locked_worker = start_one(
        &mut locked_db,
        &locked_registration,
        &lease,
        &mut locked_runner,
        || 100,
    )
    .unwrap();
    let samples = Cell::new(0);
    assert!(matches!(
        locked_worker
            .poll(
                &mut locked_db,
                &mut locked_runner,
                || {
                    let n = samples.get();
                    samples.set(n + 1);
                    if n < 3 { 100 } else { 1100 }
                },
                false
            )
            .unwrap(),
        Outcome::AwaitingTermination { reason } if reason == "deadline_or_state_changed_during_import"
    ));
    assert_eq!(
        jobs::load(&mut locked_db, &locked_registration.spec.id)
            .unwrap()
            .state,
        State::CancelRequested
    );
    assert_eq!(
        locked_worker
            .poll(&mut locked_db, &mut locked_runner, || 100, false)
            .unwrap_err(),
        Error::Clock
    );
    assert!(
        saved(&locked_db, locked_worker.identity(), "result")
            .unwrap()
            .is_some()
    );
    let mut recovered = recover(&mut locked_db, &locked_registration, &lease, || 1100).unwrap();
    locked_runner.terminate_fail = true;
    assert_eq!(
        recovered
            .poll(&mut locked_db, &mut locked_runner, || 1100, false)
            .unwrap(),
        Outcome::Terminated
    );
    assert_eq!(locked_runner.starts, 1);

    // Cancellation SQL failure retains the in-memory driver and occupied
    // slot. The next poll reissues intent, never launch, before termination.
    let (mut cancel_db, cancel_registration, cancel_clock) = setup();
    let mut cancel_runner = Fake::new(cancel_clock, 3);
    let mut cancel_worker = start_one(
        &mut cancel_db,
        &cancel_registration,
        &lease,
        &mut cancel_runner,
        || 100,
    )
    .unwrap();
    cancel_db.execute_batch("CREATE TRIGGER reject_cancel BEFORE UPDATE ON jobs WHEN NEW.state='CancelRequested' BEGIN SELECT RAISE(FAIL,'injected cancel failure'); END").unwrap();
    assert!(
        cancel_worker
            .poll(&mut cancel_db, &mut cancel_runner, || 100, true)
            .is_err()
    );
    assert_eq!(
        jobs::load(&mut cancel_db, &cancel_registration.spec.id)
            .unwrap()
            .state,
        State::Running
    );
    cancel_db
        .execute_batch("DROP TRIGGER reject_cancel")
        .unwrap();
    assert_eq!(
        cancel_worker
            .poll(&mut cancel_db, &mut cancel_runner, || 100, false)
            .unwrap(),
        Outcome::Terminated
    );
    assert_eq!(cancel_runner.starts, 1);

    for mode in 1..=11 {
        let (mut db, registered, clock) = setup();
        let mut runner = Fake::new(clock.clone(), mode);
        runner.terminate_fail = true;
        let mut worker =
            start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
        let outcome = worker.poll(&mut db, &mut runner, || clock.get(), mode == 3);
        assert!(!matches!(outcome, Ok(Outcome::Succeeded(_))));
        let job = jobs::load(&mut db, &registered.spec.id).unwrap();
        assert!(matches!(
            job.state,
            State::CancelRequested | State::Cancelled
        ));
        assert_eq!(job.result_hash, None);
        assert_eq!(runner.starts, 1);
        if job.state == State::CancelRequested {
            assert!(matches!(
                worker
                    .poll(&mut db, &mut runner, || clock.get(), false)
                    .unwrap(),
                Outcome::AwaitingTermination { .. }
            ));
            runner.terminate_fail = false;
            assert_eq!(
                worker
                    .poll(&mut db, &mut runner, || clock.get(), false)
                    .unwrap(),
                Outcome::Terminated
            );
        }
        assert_eq!(runner.starts, 1);
    }
    let (mut db, registered, clock) = setup();
    let mut runner = Fake::new(clock.clone(), 0);
    let mut worker = start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
    runner.advance = Some(1100);
    assert!(matches!(
        worker
            .poll(&mut db, &mut runner, || clock.get(), false)
            .unwrap(),
        Outcome::AwaitingTermination { .. }
    ));
    assert_eq!(
        jobs::load(&mut db, &registered.spec.id).unwrap().state,
        State::CancelRequested
    );
    assert!(saved(&db, worker.identity(), "result").unwrap().is_none());
    assert_eq!(
        worker
            .poll(&mut db, &mut runner, || clock.get(), false)
            .unwrap(),
        Outcome::Terminated
    );
    let (mut db, registered, clock) = setup();
    let mut runner = Fake::new(clock.clone(), 3);
    let worker = start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
    drop(worker);
    let mut restored = recover(&mut db, &registered, &lease, || clock.get()).unwrap();
    assert_eq!(runner.starts, 1);
    runner.terminate_fail = true;
    assert!(matches!(
        restored
            .poll(&mut db, &mut runner, || clock.get(), false)
            .unwrap(),
        Outcome::AwaitingTermination { .. }
    ));
    runner.terminate_fail = false;
    assert_eq!(
        restored
            .poll(&mut db, &mut runner, || clock.get(), false)
            .unwrap(),
        Outcome::Terminated
    );
    let (mut db, registered, clock) = setup();
    let mut runner = Fake::new(clock.clone(), 3);
    let mut worker = start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
    clock.set(99);
    assert_eq!(
        worker
            .poll(&mut db, &mut runner, || clock.get(), false)
            .unwrap_err(),
        Error::Clock
    );
    assert_eq!(
        jobs::load(&mut db, &registered.spec.id).unwrap().state,
        State::Running
    );
    let (mut crash_db, crash_registration, crash_clock) = setup();
    let mut crash_runner = Fake::new(crash_clock.clone(), 1);
    let mut crash_worker = start_one(
        &mut crash_db,
        &crash_registration,
        &lease,
        &mut crash_runner,
        || crash_clock.get(),
    )
    .unwrap();
    crash_db.execute_batch("CREATE TRIGGER fail_termination BEFORE UPDATE ON jobs WHEN NEW.state='Cancelled' BEGIN SELECT RAISE(FAIL,'injected crash boundary'); END").unwrap();
    assert!(
        crash_worker
            .poll(
                &mut crash_db,
                &mut crash_runner,
                || crash_clock.get(),
                false
            )
            .is_err()
    );
    assert!(
        saved(&crash_db, crash_worker.identity(), "termination")
            .unwrap()
            .is_some()
    );
    drop(crash_worker);
    crash_db
        .execute_batch("DROP TRIGGER fail_termination")
        .unwrap();
    let mut recovered = recover(&mut crash_db, &crash_registration, &lease, || {
        crash_clock.get()
    })
    .unwrap();
    crash_runner.terminate_fail = true; // Original durable proof must suffice.
    assert_eq!(
        recovered
            .poll(
                &mut crash_db,
                &mut crash_runner,
                || crash_clock.get(),
                false
            )
            .unwrap(),
        Outcome::Terminated
    );
    assert_eq!(crash_runner.starts, 1);
    for mode in [1, 2] {
        let (mut db, registered, clock) = setup();
        let mut runner = Fake::new(clock.clone(), mode);
        runner.terminate_fail = true;
        let mut worker =
            start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
        let _ = worker.poll(&mut db, &mut runner, || clock.get(), false);
        let original = saved(&db, worker.identity(), "refusal").unwrap().unwrap();
        drop(worker);
        let mut worker = recover(&mut db, &registered, &lease, || clock.get()).unwrap();
        assert_eq!(
            saved(&db, worker.identity(), "refusal").unwrap().unwrap(),
            original
        );
        runner.terminate_fail = false;
        assert_eq!(
            worker
                .poll(&mut db, &mut runner, || clock.get(), false)
                .unwrap(),
            Outcome::Terminated
        );
        assert_eq!(runner.starts, 1);
    }
    let (mut late_db, late_registered, late_clock) = setup();
    let mut late_runner = Fake::new(late_clock, 0);
    let samples = Cell::new(0);
    let late = start_one(
        &mut late_db,
        &late_registered,
        &lease,
        &mut late_runner,
        || {
            let n = samples.get();
            samples.set(n + 1);
            if n == 0 { 100 } else { 1100 }
        },
    )
    .unwrap();
    assert_eq!(late_runner.starts, 0);
    assert_eq!(
        jobs::load(&mut late_db, &late_registered.spec.id)
            .unwrap()
            .state,
        State::CancelRequested
    );
    assert!(
        saved(&late_db, late.identity(), "launch")
            .unwrap()
            .is_some()
    );
    let (_, plan_registered, _) = setup();
    let plan_launch = Launch {
        identity: ExecutionIdentity {
            job_id: plan_registered.spec.id.clone(),
            lease: lease.clone(),
            registration_sha256: "d".repeat(64),
        },
        spec: plan_registered.spec.clone(),
        split: plan_registered.split.clone(),
        limits: plan_registered.limits.clone(),
        started_ms: 100,
        deadline_ms: 1100,
    };
    let plan = crate::bubblewrap::command_plan(
        &plan_launch,
        std::path::Path::new("/var/tmp/nivra-research-plan"),
    )
    .unwrap();
    assert!(plan.argv.iter().any(|arg| arg == "--unshare-net"));
    assert!(plan.argv.iter().any(|arg| arg == "CPUQuota=50.00%"));
    assert!(plan.argv.iter().any(|arg| arg == "--unshare-user"));
    assert!(plan.argv.iter().any(|arg| arg == "--disable-userns"));
    assert!(
        !plan
            .argv
            .iter()
            .any(|arg| arg == "--share-net" || arg.contains("/home"))
    );
    let tail: Vec<_> = plan.argv.iter().rev().take(3).collect();
    assert_eq!(
        tail.into_iter()
            .rev()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["/runtime", "/inputs/code", "/inputs/input"]
    );
    assert!(
        crate::bubblewrap::command_plan(&plan_launch, std::path::Path::new("/home/review"))
            .is_err()
    );
    assert!(
        crate::bubblewrap::command_plan(&plan_launch, std::path::Path::new("/root/review"))
            .is_err()
    );
    assert!(
        crate::bubblewrap::command_plan(&plan_launch, std::path::Path::new("relative")).is_err()
    );
    let mut queued = Connection::open_in_memory().unwrap();
    queued.execute_batch("PRAGMA synchronous=FULL").unwrap();
    jobs::initialize(&mut queued).unwrap();
    let script = hash(b"registered-analysis-v1");
    let runtime_hash = hash(b"reviewed-static-runtime");
    let input_hash = hash(b"development-data");
    jobs::register_reviewed(&mut queued, &script, &runtime_hash).unwrap();
    let tool = crate::AnalysisTool::bind(
        "a".repeat(64),
        std::sync::Arc::new(std::sync::Mutex::new(queued)),
        vec![crate::RegisteredAnalysis {
            id: "cohort".into(),
            script_hash: script.clone(),
            runtime_hash: runtime_hash.clone(),
            memory_bytes: 64 * 1024 * 1024,
            output_bytes: 4096,
            wall_millis: 1000,
        }],
        vec![crate::Dataset {
            id: "day".into(),
            input_hash: input_hash.clone(),
            split: crate::DevelopmentSplit::Development,
        }],
        std::sync::Arc::new(|| 100),
    )
    .unwrap();
    let enqueued = tool
        .enqueue(crate::AnalysisArgs {
            experiment_id: "exp-1".into(),
            analysis_id: "cohort".into(),
            dataset_id: "day".into(),
        })
        .unwrap();
    assert!(!enqueued.execution_started);
    assert!(!enqueued.promotion_authorized);
    assert_eq!(enqueued.state, State::Pending);
    assert!(
        tool.enqueue(crate::AnalysisArgs {
            experiment_id: "exp-2".into(),
            analysis_id: "other".into(),
            dataset_id: "day".into(),
        })
        .is_err()
    );
    let measured = AnalysisResult {
        identity: ExecutionIdentity {
            job_id: "b".repeat(64),
            lease: lease.clone(),
            registration_sha256: "d".repeat(64),
        },
        script_hash: script.clone(),
        runtime_hash: runtime_hash.clone(),
        input_hash: input_hash.clone(),
        metrics: [
            "net_visible",
            "home_visible",
            "host_file_visible",
            "input_writable",
        ]
        .into_iter()
        .map(|name| Metric {
            name: name.into(),
            value: "0".into(),
        })
        .collect(),
        observations: vec!["containment_probe".into()],
    };
    let observed = HostObservation {
        network_unshared: true,
        home_unmounted: true,
        inputs_read_only: true,
        cgroup_absent: true,
    };
    let accepted =
        accept_measured_experiment(&measured, &script, &runtime_hash, &input_hash, &observed);
    assert_eq!(accepted.sandbox_experiment, "MEASURED");
    assert_eq!(accepted.scientific_acceptance, "NOT_ACCEPTED");
    assert_eq!(accepted.certification, "NOT_CERTIFIED");
    assert_eq!(accepted.promotion, "NOT_AUTHORIZED");
    assert!(!accepted.live_enabled);
    assert!(accepted.missing.iter().any(|gap| gap == "holdout"));
    let mut open_network = observed.clone();
    open_network.network_unshared = false;
    assert_eq!(
        accept_measured_experiment(
            &measured,
            &script,
            &runtime_hash,
            &input_hash,
            &open_network
        )
        .sandbox_experiment,
        "REJECTED"
    );
    let mut visible = measured.clone();
    visible.metrics[0].value = "1".into();
    assert_eq!(
        accept_measured_experiment(&visible, &script, &runtime_hash, &input_hash, &observed)
            .sandbox_experiment,
        "REJECTED"
    );
    // Recovery with changed registered split cannot adopt the original launch.
    let mut registered = registered;
    registered.split = DevelopmentSplit::Training;
    assert!(recover(&mut db, &registered, &lease, || 100).is_err());
}
