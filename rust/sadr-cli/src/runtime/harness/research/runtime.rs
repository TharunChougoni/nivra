use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResearchTaskKind {
    Registered,
    Candidate,
}

/// Typed launch gate. Both kinds are gated by the book's own state. Registered
/// work additionally waits for a pending owner decision, so research never
/// delays or displaces a queued trading decision. Neither waits for an idle
/// model worker: a busy scorer, aside, review or supervisor turn does not
/// starve research (#236).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResearchLaunch {
    pub registered: bool,
    pub candidate: bool,
    pub reason: &'static str,
}

/// The book state the launch gate reads. Model workers are deliberately not
/// an input.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LaunchBook {
    pub halted: bool,
    pub stopped: bool,
    pub disposed: bool,
    pub owner_decision_pending: bool,
    pub context_unavailable: bool,
}

impl ResearchLaunch {
    pub(crate) fn for_book(book: LaunchBook) -> Self {
        let runnable = !book.halted && !book.stopped && !book.disposed;
        let usable = runnable && !book.context_unavailable;
        Self {
            registered: usable && !book.owner_decision_pending,
            candidate: usable,
            reason: if book.context_unavailable {
                "controller_session_end_unavailable"
            } else if book.halted {
                "book_halted"
            } else if book.stopped {
                "book_stopped"
            } else if book.disposed {
                "book_disposed"
            } else if book.owner_decision_pending {
                "owner_decision_pending"
            } else {
                "ready"
            },
        }
    }
}

/// One process-local waiter for the sole durable queue slot. Dropping it does
/// not cancel a job; its database row and worker receipts remain authoritative.
pub(crate) type ActiveResearch = (
    ResearchTaskKind,
    String,
    u32,
    Arc<AtomicBool>,
    std::sync::mpsc::Receiver<std::result::Result<(), &'static str>>,
);

#[derive(Default)]
pub(crate) struct OwnerResearchRuntime {
    pub(crate) active: Option<ActiveResearch>,
    pub(crate) terminal_cursor: Option<jobs::TaskCursor>,
    pub(crate) candidate_terminal_cursor: Option<jobs::TaskCursor>,
    pub(crate) blocked: Option<(ResearchTaskKind, String, u32, &'static str)>,
    pub(crate) last_mature_ms: Option<i64>,
    pub(crate) lifecycle: Option<Value>,
}

impl OwnerResearchRuntime {
    pub(crate) fn observe_channel(&mut self) {
        let Some((kind, task_id, revision, _, receiver)) = self.active.as_ref() else {
            return;
        };
        let kind = *kind;
        let task_id = task_id.clone();
        let revision = *revision;
        match receiver.try_recv() {
            Ok(Ok(())) => self.active = None,
            Ok(Err(reason)) => {
                self.active = None;
                self.blocked = Some((kind, task_id, revision, reason));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.active = None;
                self.blocked = Some((kind, task_id, revision, "research_worker_channel_lost"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    pub(crate) fn clear_blocked_if_terminal(
        &mut self,
        kind: ResearchTaskKind,
        task_id: &str,
        revision: u32,
        state: jobs::TaskState,
    ) {
        if task_state_terminal(state)
            && self.blocked.as_ref().is_some_and(
                |(blocked_kind, blocked_id, blocked_revision, _)| {
                    *blocked_kind == kind && blocked_id == task_id && *blocked_revision == revision
                },
            )
        {
            self.blocked = None;
        }
    }

    fn active_candidate_cancel(&self) -> Option<(String, u32, Arc<AtomicBool>)> {
        match &self.active {
            Some((ResearchTaskKind::Candidate, task_id, revision, cancel, _)) => {
                Some((task_id.clone(), *revision, Arc::clone(cancel)))
            }
            _ => None,
        }
    }
}

pub(crate) fn registered_run(
    settings: &ResearchSettings,
    task: &jobs::TaskRecord,
) -> Result<RegisteredRun> {
    if task.job.spec.capability != jobs::SpecCapability::RegisteredAnalysis {
        return Err("research_registration_changed".into());
    }
    let alias = &task.spec.registered_analysis;
    let registration = settings
        .registrations
        .iter()
        .find(|r| r.analysis_id == alias.analysis_id && r.dataset_id == alias.dataset_id)
        .ok_or("research_registration_changed")?;
    if registration.code_sha256 != alias.code_sha256
        || registration.runtime_sha256 != alias.runtime_sha256
        || registration.input_sha256 != alias.input_sha256
        || registration.memory_bytes != task.spec.resources.memory_bytes
        || registration.wall_millis != task.spec.resources.wall_millis
        || registration.limits.cpu_millis != task.spec.resources.cpu_millis
        || registration.limits.tasks != task.spec.resources.tasks
        || registration.limits.workspace_bytes != task.spec.resources.workspace_bytes
        || registration.limits.file_bytes != task.spec.resources.file_bytes
    {
        return Err("research_registration_changed".into());
    }
    Ok(RegisteredRun {
        spec: task.job.spec.clone(),
        split: worker::DevelopmentSplit::Development,
        limits: registration.limits.clone(),
        code: verified(
            &registration.code_file,
            &registration.code_sha256,
            Artifact::Code,
        )?,
        runtime: verified(
            &registration.runtime_file,
            &registration.runtime_sha256,
            Artifact::Runtime,
        )?,
        input: verified(
            &registration.input_file,
            &registration.input_sha256,
            Artifact::Input,
        )?,
    })
}

pub(crate) fn drive_task(
    settings: ResearchSettings,
    authority: PathBuf,
    task_id: String,
    revision: u32,
) -> std::result::Result<(), &'static str> {
    let result = (|| -> Result<()> {
        let mut db = open_queue_for_owner(&settings, &authority, false)?;
        let task = jobs::load_scoped(&mut db, &task_id, revision)
            .map_err(|_| "research_task_unavailable")?;
        if task.status == jobs::TaskState::Pending && task.job.attempts == 0 {
            let now = millis();
            if now >= task.spec.expires_at_ms {
                jobs::refuse_unstarted(
                    &mut db,
                    &task_id,
                    revision,
                    jobs::RefusalReason::DeadlineExpiredBeforeStart,
                    now,
                )
                .map_err(|_| "research_task_refusal_failed")?;
                return Ok(());
            }
        }
        let registered = match registered_run(&settings, &task) {
            Ok(registered) => registered,
            Err(_) if task.status == jobs::TaskState::Pending && task.job.attempts == 0 => {
                jobs::refuse_unstarted(
                    &mut db,
                    &task_id,
                    revision,
                    jobs::RefusalReason::RegistrationChanged,
                    millis(),
                )
                .map_err(|_| "research_task_refusal_failed")?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if !settings.stage_root.exists() {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&settings.stage_root)?;
        }
        let mut runner = bubblewrap::Runner::new(settings.stage_root.clone());
        let mut active = match task.status {
            jobs::TaskState::Pending if task.job.attempts == 0 => {
                let lease =
                    digest(format!("owner-research-lease:{}:{}", task_id, revision).as_bytes());
                worker::start_one(&mut db, &registered, &lease, &mut runner, millis)
                    .map_err(|_| "research_worker_start_failed")?
            }
            jobs::TaskState::Running
            | jobs::TaskState::Unknown
            | jobs::TaskState::CancelRequested => {
                let lease = task
                    .job
                    .lease
                    .as_deref()
                    .ok_or("research_recovery_receipt_missing")?;
                worker::recover(&mut db, &registered, lease, millis)
                    .map_err(|_| "research_recovery_unavailable")?
            }
            _ => return Ok(()),
        };
        let mut terminate_deadline = None;
        loop {
            let now = millis();
            match active
                .poll(&mut db, &mut runner, millis, now >= task.spec.expires_at_ms)
                .map_err(|_| "research_worker_poll_failed")?
            {
                Outcome::Running => thread::sleep(Duration::from_millis(50)),
                Outcome::AwaitingTermination { .. } => {
                    if cleanup_grace_expired(
                        true,
                        now,
                        settings.termination_grace_ms,
                        &mut terminate_deadline,
                    ) {
                        return Err("research_termination_unconfirmed".into());
                    }
                    thread::sleep(Duration::from_millis(50))
                }
                Outcome::Succeeded(_) | Outcome::Terminated => return Ok(()),
            }
        }
    })();
    result.map_err(|_| "research_worker_failed")
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub(crate) enum ActiveChoice {
    Registered(jobs::TaskRecord),
    Candidate(jobs::CandidateTaskRecord),
}

fn state_priority(state: jobs::TaskState) -> u8 {
    match state {
        jobs::TaskState::Running => 0,
        jobs::TaskState::Unknown => 1,
        jobs::TaskState::CancelRequested => 2,
        _ => 3,
    }
}

fn older(registered: &jobs::TaskRecord, candidate: &jobs::CandidateTaskRecord) -> bool {
    candidate.spec.created_at_ms < registered.spec.created_at_ms
        || (candidate.spec.created_at_ms == registered.spec.created_at_ms
            && (candidate.spec.task_id.as_str(), candidate.spec.revision)
                < (registered.spec.task_id.as_str(), registered.spec.revision))
}

fn pending_eligible_registered(
    task: &jobs::TaskRecord,
    launch: ResearchLaunch,
    binding: &str,
    session: &str,
    now: i64,
) -> bool {
    now >= task.spec.expires_at_ms
        || (launch.registered && task.spec.book_binding == binding && task.spec.session == session)
}

fn pending_eligible_candidate(
    task: &jobs::CandidateTaskRecord,
    launch: ResearchLaunch,
    binding: &str,
    session: &str,
    now: i64,
) -> bool {
    now >= task.spec.expires_at_ms
        || (launch.candidate && task.spec.book_binding == binding && task.spec.session == session)
}

/// Pick the oldest eligible Pending task when no live attempt occupies the
/// original queue slot. A live Running/Unknown/CancelRequested attempt is never
/// skipped, and no attempt is killed to make room. The caller supplies a paged
/// scan of Pending candidates so an ineligible predecessor on an old book
/// binding cannot starve a newer launchable row.
pub(crate) fn choose_active(
    registered: Option<jobs::TaskRecord>,
    candidates: Vec<jobs::CandidateTaskRecord>,
    launch: ResearchLaunch,
    binding: &str,
    session: &str,
    now: i64,
) -> Option<ActiveChoice> {
    let live_candidate = candidates
        .iter()
        .find(|candidate| candidate.status != jobs::TaskState::Pending)
        .cloned();
    let registered_live = registered
        .as_ref()
        .filter(|registered| registered.status != jobs::TaskState::Pending);
    if let Some(candidate) = live_candidate {
        if let Some(registered) = registered_live {
            let registered_priority = state_priority(registered.status);
            let candidate_priority = state_priority(candidate.status);
            if candidate_priority < registered_priority
                || (candidate_priority == registered_priority && older(registered, &candidate))
            {
                return Some(ActiveChoice::Candidate(candidate));
            }
            return Some(ActiveChoice::Registered(registered.clone()));
        }
        return Some(ActiveChoice::Candidate(candidate));
    }
    if let Some(registered) = registered_live {
        return Some(ActiveChoice::Registered(registered.clone()));
    }
    let mut best: Option<((i64, String, u32), ActiveChoice)> = None;
    if let Some(registered) = registered.as_ref()
        && pending_eligible_registered(registered, launch, binding, session, now)
    {
        best = Some((
            (
                registered.spec.created_at_ms,
                registered.spec.task_id.clone(),
                registered.spec.revision,
            ),
            ActiveChoice::Registered(registered.clone()),
        ));
    }
    for candidate in &candidates {
        if !pending_eligible_candidate(candidate, launch, binding, session, now) {
            continue;
        }
        let key = (
            candidate.spec.created_at_ms,
            candidate.spec.task_id.clone(),
            candidate.spec.revision,
        );
        if best.as_ref().is_none_or(|(current, _)| key < *current) {
            best = Some((key, ActiveChoice::Candidate(candidate.clone())));
        }
    }
    best.map(|(_, choice)| choice).or_else(|| {
        // No launchable row: retain the oldest ineligible predecessor without
        // launching or mutating it, so the caller can report it truthfully.
        candidates
            .into_iter()
            .next()
            .map(ActiveChoice::Candidate)
            .or_else(|| registered.map(ActiveChoice::Registered))
    })
}

fn pending_candidate_scan(queue: &mut Connection) -> Result<Vec<jobs::CandidateTaskRecord>> {
    let mut rows = Vec::new();
    let mut cursor: Option<jobs::TaskCursor> = None;
    for _ in 0..10 {
        let page = jobs::pending_candidates_page(queue, cursor.as_ref(), 100)
            .map_err(|_| "candidate_task_state_unavailable")?;
        let count = page.len();
        rows.extend(page);
        if count < 100 || rows.len() >= 1000 {
            break;
        }
        cursor = rows.last().map(|row| jobs::TaskCursor {
            created_at_ms: row.spec.created_at_ms,
            task_id: row.spec.task_id.clone(),
            revision: row.spec.revision,
        });
    }
    if let Some(live) =
        jobs::active_candidate(queue).map_err(|_| "candidate_task_state_unavailable")?
        && live.status != jobs::TaskState::Pending
    {
        rows.insert(0, live);
    }
    Ok(rows)
}

/// The in-process flag carries operator-cancel intent only. Task TTL and unit
/// wall expiry are derived by the driver from the immutable launch deadline, so
/// an expiry can never be mislabelled as a human cancellation.
pub(crate) fn candidate_operator_cancel(record: &jobs::CandidateTaskRecord) -> bool {
    record.status == jobs::TaskState::CancelRequested && record.cancel_receipt.is_some()
}

/// Low-priority controller tick: terminal events are deduped by the owner
/// ledger. Registered launch keeps the old owner/model-worker gate; candidate
/// launch uses the typed book gate and never recovers an ambiguous attempt.
pub(crate) fn tick_owner_tasks(
    db: &mut Connection,
    c: &Config,
    cfg: &PlannerConfig,
    launch: ResearchLaunch,
    runtime: &mut OwnerResearchRuntime,
    now: i64,
) -> Result<Value> {
    let stage_root = cfg
        .research_tools
        .as_ref()
        .map(|settings| settings.stage_root.clone())
        .unwrap_or_default();
    tick_owner_tasks_with_runner(db, c, cfg, launch, runtime, now, move || {
        Box::new(bubblewrap::CandidateRunner::new(stage_root.clone()))
    })
}

/// Testable variant of [`tick_owner_tasks`] with an explicit owned-unit runner
/// factory. Production uses [`bubblewrap::CandidateRunner`]; tests use a fake
/// that never invokes systemd.
pub(crate) fn tick_owner_tasks_with_runner(
    db: &mut Connection,
    c: &Config,
    cfg: &PlannerConfig,
    launch: ResearchLaunch,
    runtime: &mut OwnerResearchRuntime,
    now: i64,
    mut runner_for: impl FnMut() -> Box<dyn worker::CandidateUnitRunner + Send> + Send + 'static,
) -> Result<Value> {
    let Some(settings) = cfg.research_tools.as_ref() else {
        return Ok(json!({"status":"disabled"}));
    };
    let binding = book_binding(c)?;
    runtime.observe_channel();
    let Ok(mut queue) = open_queue_for_owner(settings, &c.directory.join("model"), false) else {
        return Ok(json!({"status":"unavailable"}));
    };
    if let Some((task_id, revision, cancel)) = runtime.active_candidate_cancel()
        && let Ok(active) = jobs::load_candidate(&mut queue, &task_id, revision)
        && candidate_operator_cancel(&active)
    {
        cancel.store(true, Ordering::Relaxed);
    }
    // Repeating this scan is safe: terminal event identity is stable per task
    // revision, so a crash between completion and ledger insertion is repaired.
    let terminal = jobs::list_scoped_page(
        &mut queue,
        &c.session.to_string(),
        None,
        runtime.terminal_cursor.as_ref(),
        100,
    )
    .map_err(|_| "research_task_state_unavailable")?;
    runtime.terminal_cursor = terminal
        .last()
        .filter(|_| terminal.len() == 100)
        .map(|row| jobs::TaskCursor {
            created_at_ms: row.spec.created_at_ms,
            task_id: row.spec.task_id.clone(),
            revision: row.spec.revision,
        });
    super::super::super::experiment_ledger::research::reconcile_page(
        c,
        &mut queue,
        terminal
            .iter()
            .filter(|row| row.spec.book_binding == binding),
    )?;
    for row in terminal.iter().filter(|row| {
        row.spec.book_binding == binding
            && matches!(
                row.status,
                jobs::TaskState::Succeeded
                    | jobs::TaskState::Terminated
                    | jobs::TaskState::Cancelled
                    | jobs::TaskState::Refused
            )
    }) {
        let status = task_state_name(row.status);
        let terminal_at_ms = row.terminal_at_ms.ok_or("research_terminal_time_missing")?;
        super::super::super::redecision::enqueue_owner_research_event(
            db,
            c,
            cfg,
            &row.spec.task_id,
            row.spec.revision,
            row.job.result_hash.as_deref(),
            status,
            terminal_at_ms,
        )?;
    }
    for row in &terminal {
        runtime.clear_blocked_if_terminal(
            ResearchTaskKind::Registered,
            &row.spec.task_id,
            row.spec.revision,
            row.status,
        );
    }
    let candidate_terminal = jobs::list_candidates_page(
        &mut queue,
        &c.session.to_string(),
        None,
        runtime.candidate_terminal_cursor.as_ref(),
        100,
    )
    .map_err(|_| "candidate_task_state_unavailable")?;
    runtime.candidate_terminal_cursor = candidate_terminal
        .last()
        .filter(|_| candidate_terminal.len() == 100)
        .map(|row| jobs::TaskCursor {
            created_at_ms: row.spec.created_at_ms,
            task_id: row.spec.task_id.clone(),
            revision: row.spec.revision,
        });
    for row in candidate_terminal
        .iter()
        .filter(|row| row.spec.book_binding == binding && row.status == jobs::TaskState::Succeeded)
    {
        if reconcile_candidate(c, cfg, &mut queue, row).is_err() {
            runtime.lifecycle = Some(
                json!({"status":"error","reason":"candidate_lifecycle_unavailable","task_id":row.spec.task_id,"revision":row.spec.revision}),
            );
        }
    }
    // A later TTL/deadline termination can race an already-produced timely
    // result. Retry that narrow terminal case from the original stop proof
    // before emitting a termination event; the full payload check stays in
    // `reconcile_candidate_attempt`.
    let late_ready = candidate_terminal
        .iter()
        .filter(|row| {
            row.spec.book_binding == binding
                && row.status == jobs::TaskState::Terminated
                && candidate_late_result_ready(&queue, row)
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut late_completed = Vec::new();
    for row in &late_ready {
        let mut runner = runner_for();
        if let Ok(value) =
            reconcile_candidate_attempt(settings, &mut queue, row, &mut *runner, millis)
            && value["status"] == "reconciled"
        {
            late_completed.push(row.spec.task_id.clone());
        }
    }
    for row in candidate_terminal.iter().filter(|row| {
        row.spec.book_binding == binding
            && !late_completed.iter().any(|id| id == &row.spec.task_id)
            && matches!(
                row.status,
                jobs::TaskState::Succeeded
                    | jobs::TaskState::Terminated
                    | jobs::TaskState::Cancelled
                    | jobs::TaskState::Refused
            )
    }) {
        let status = task_state_name(row.status);
        let terminal_at_ms = row
            .terminal_at_ms
            .ok_or("candidate_terminal_time_missing")?;
        super::super::super::redecision::enqueue_owner_research_event_kind(
            db,
            c,
            cfg,
            &row.spec.task_id,
            row.spec.revision,
            row.job.result_hash.as_deref(),
            status,
            terminal_at_ms,
            "candidate",
        )?;
        runtime.clear_blocked_if_terminal(
            ResearchTaskKind::Candidate,
            &row.spec.task_id,
            row.spec.revision,
            row.status,
        );
    }
    if runtime.active.is_none()
        && runtime
            .last_mature_ms
            .is_none_or(|last| now.saturating_sub(last) >= 60_000)
    {
        runtime.last_mature_ms = Some(now);
        match mature_candidates(c, cfg, now) {
            Ok(value) => {
                runtime.lifecycle = Some(json!({"status":"ok","maturation":value["status"]}));
            }
            Err(_) => {
                runtime.lifecycle = Some(json!({
                    "status":"error",
                    "reason":"candidate_maturation_unavailable"
                }));
            }
        }
    }
    if runtime.active.is_some() {
        return Ok(json!({"status":"running","lifecycle":runtime.lifecycle}));
    }
    let registered =
        jobs::active_scoped(&mut queue).map_err(|_| "research_task_state_unavailable")?;
    let candidates = pending_candidate_scan(&mut queue)?;
    let Some(choice) = choose_active(
        registered,
        candidates,
        launch,
        &binding,
        &c.session.to_string(),
        now,
    ) else {
        return Ok(json!({"status":"idle","lifecycle":runtime.lifecycle}));
    };
    match choice {
        ActiveChoice::Registered(task) => {
            if let Some((kind, blocked_id, blocked_revision, reason)) = &runtime.blocked
                && *kind == ResearchTaskKind::Registered
                && blocked_id == &task.spec.task_id
                && *blocked_revision == task.spec.revision
            {
                return Ok(
                    json!({"status":"reconciliation_required","task_id":blocked_id,"revision":blocked_revision,"reason":reason}),
                );
            }
            if task.status == jobs::TaskState::Pending && now >= task.spec.expires_at_ms {
                jobs::refuse_unstarted(
                    &mut queue,
                    &task.spec.task_id,
                    task.spec.revision,
                    jobs::RefusalReason::DeadlineExpiredBeforeStart,
                    now,
                )
                .map_err(|_| "research_task_refusal_failed")?;
                return Ok(
                    json!({"status":"refused_expired","task_id":task.spec.task_id,"revision":task.spec.revision}),
                );
            }
            // A predecessor task keeps its original queue row, lease and authority.
            // The successor may read it; it must not refuse, recover or launch it.
            if task.spec.book_binding != binding || task.spec.session != c.session.to_string() {
                return Ok(json!({
                    "status":"predecessor_retained",
                    "task_id":task.spec.task_id,
                    "revision":task.spec.revision,
                    "task_status":task_state_name(task.status),
                    "action":"none"
                }));
            }
            if task.status == jobs::TaskState::Pending && !launch.registered {
                return Ok(json!({"status":"deferred_owner_work","reason":launch.reason}));
            }
            let task_id = task.spec.task_id.clone();
            let revision = task.spec.revision;
            let (sender, receiver) = std::sync::mpsc::channel();
            let settings = settings.clone();
            let authority = c.directory.join("model");
            thread::Builder::new()
                .name("sadr-research-owner".into())
                .spawn(move || {
                    let result = drive_task(settings, authority, task_id, revision);
                    let _ = sender.send(result);
                })
                .map_err(|_| "research_worker_unavailable")?;
            runtime.active = Some((
                ResearchTaskKind::Registered,
                task.spec.task_id,
                task.spec.revision,
                Arc::new(AtomicBool::new(false)),
                receiver,
            ));
            Ok(
                json!({"status":"started","kind":"registered","task_id":runtime.active.as_ref().map(|v|v.1.as_str()),"revision":runtime.active.as_ref().map(|v|v.2),"lifecycle":runtime.lifecycle}),
            )
        }
        ActiveChoice::Candidate(task) => {
            if task.status != jobs::TaskState::Pending {
                if runtime.active.is_none() {
                    let mut runner = runner_for();
                    match reconcile_candidate_attempt(
                        settings,
                        &mut queue,
                        &task,
                        &mut *runner,
                        millis,
                    ) {
                        Ok(value) => return Ok(value),
                        Err(_) => {
                            return Ok(json!({
                                "status":"candidate_recovery_required",
                                "task_id":task.spec.task_id,
                                "revision":task.spec.revision,
                                "task_status":task_state_name(task.status),
                                "action":"none",
                                "reason":"ambiguous_candidate_attempt_retains_lease_no_replay",
                                "lifecycle":runtime.lifecycle
                            }));
                        }
                    }
                }
                return Ok(json!({
                    "status":"candidate_recovery_required",
                    "task_id":task.spec.task_id,
                    "revision":task.spec.revision,
                    "task_status":task_state_name(task.status),
                    "action":"none",
                    "reason":"ambiguous_candidate_attempt_retains_lease_no_replay",
                    "lifecycle":runtime.lifecycle
                }));
            }
            if let Some((kind, blocked_id, blocked_revision, reason)) = &runtime.blocked
                && *kind == ResearchTaskKind::Candidate
                && blocked_id == &task.spec.task_id
                && *blocked_revision == task.spec.revision
            {
                return Ok(
                    json!({"status":"reconciliation_required","task_id":blocked_id,"revision":blocked_revision,"reason":reason}),
                );
            }
            if now >= task.spec.expires_at_ms {
                jobs::refuse_unstarted_candidate(
                    &mut queue,
                    &task.spec.task_id,
                    task.spec.revision,
                    jobs::RefusalReason::DeadlineExpiredBeforeStart,
                    now,
                )
                .map_err(|_| "candidate_task_refusal_failed")?;
                return Ok(
                    json!({"status":"refused_expired","task_id":task.spec.task_id,"revision":task.spec.revision}),
                );
            }
            if task.spec.book_binding != binding || task.spec.session != c.session.to_string() {
                return Ok(json!({
                    "status":"predecessor_retained",
                    "task_id":task.spec.task_id,
                    "revision":task.spec.revision,
                    "task_status":task_state_name(task.status),
                    "action":"none"
                }));
            }
            if !launch.candidate {
                return Ok(
                    json!({"status":"deferred_candidate_book_unavailable","reason":launch.reason}),
                );
            }
            let task_id = task.spec.task_id.clone();
            let revision = task.spec.revision;
            let (sender, receiver) = std::sync::mpsc::channel();
            let settings = settings.clone();
            let authority = c.directory.join("model");
            let cancel = Arc::new(AtomicBool::new(false));
            let driver_cancel = Arc::clone(&cancel);
            let mut runner = runner_for();
            thread::Builder::new()
                .name("sadr-research-candidate".into())
                .spawn(move || {
                    let result = drive_candidate_with_unit(
                        settings,
                        authority,
                        task_id,
                        revision,
                        driver_cancel,
                        millis,
                        &mut *runner,
                    );
                    let _ = sender.send(result);
                })
                .map_err(|_| "candidate_worker_unavailable")?;
            runtime.active = Some((
                ResearchTaskKind::Candidate,
                task.spec.task_id,
                task.spec.revision,
                cancel,
                receiver,
            ));
            Ok(
                json!({"status":"started","kind":"candidate","task_id":runtime.active.as_ref().map(|v|v.1.as_str()),"revision":runtime.active.as_ref().map(|v|v.2),"lifecycle":runtime.lifecycle}),
            )
        }
    }
}
pub(crate) fn execute(
    settings: &ResearchSettings,
    analysis: &str,
    dataset: &str,
    review: &str,
    call: &str,
    authority: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<Value> {
    settings.validate()?;
    let r = settings
        .registrations
        .iter()
        .find(|r| r.analysis_id == analysis && r.dataset_id == dataset)
        .ok_or("research_unregistered_alias")?;
    let code = verified(&r.code_file, &r.code_sha256, Artifact::Code)?;
    let runtime = verified(&r.runtime_file, &r.runtime_sha256, Artifact::Runtime)?;
    let input = verified(&r.input_file, &r.input_sha256, Artifact::Input)?;
    let mut db = open_queue_for_owner(settings, authority, true)?;
    let active: i64 = db.query_row(
        "SELECT COUNT(*) FROM jobs WHERE state IN (?1,?2,?3)",
        ["Running", "Unknown", "CancelRequested"],
        |r| r.get(0),
    )?;
    if active != 0 {
        return Err("research_original_attempt_requires_reconciliation".into());
    }
    register_reviewed_locked(settings, authority, &mut db, r)?;
    let shared = Arc::new(Mutex::new(db));
    let tool = AnalysisTool::bind(
        review.to_owned(),
        Arc::clone(&shared),
        vec![RegisteredAnalysis {
            id: analysis.to_owned(),
            script_hash: r.code_sha256.clone(),
            runtime_hash: r.runtime_sha256.clone(),
            memory_bytes: r.memory_bytes,
            output_bytes: 65536,
            wall_millis: r.wall_millis,
        }],
        vec![Dataset {
            id: dataset.to_owned(),
            input_hash: r.input_sha256.clone(),
            split: sadr_research::DevelopmentSplit::Development,
        }],
        Arc::new(millis),
    )?;
    let queued = tool.enqueue(AnalysisArgs {
        experiment_id: call.to_owned(),
        analysis_id: analysis.to_owned(),
        dataset_id: dataset.to_owned(),
    })?;
    if queued.state != jobs::State::Pending {
        return Err("research_job_consumed_no_replay".into());
    }
    if !settings.stage_root.exists() {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&settings.stage_root)?;
    }
    let mut db = shared.lock().map_err(|_| "research_queue_busy")?;
    let job = jobs::load(&mut db, &queued.job_id).map_err(|_| "research_job_missing")?;
    // The durable job budget never authorizes a second launch here.
    if job.attempts != 0 {
        return Err("research_attempt_consumed_no_replay".into());
    }
    let registered = RegisteredRun {
        spec: job.spec,
        split: worker::DevelopmentSplit::Development,
        limits: r.limits.clone(),
        code,
        runtime,
        input,
    };
    let mut runner = bubblewrap::Runner::new(settings.stage_root.clone());
    let lease = digest(format!("{review}:{call}").as_bytes());
    let mut active = worker::start_one(&mut db, &registered, &lease, &mut runner, millis)?;
    let deadline = millis() + i64::from(r.wall_millis) + 30000;
    loop {
        if millis() > deadline {
            return Err("research_ambiguous_preserve_attempt".into());
        }
        match active.poll(&mut db, &mut runner, millis, cancelled())? {
            Outcome::Running | Outcome::AwaitingTermination { .. } => {
                thread::sleep(Duration::from_millis(50))
            }
            Outcome::Succeeded(result) => {
                return Ok(
                    json!({"evidence_id":queued.job_id,"status":"measured","result":result,"scientific_acceptance":"NOT_ACCEPTED","promotion_authorized":false}),
                );
            }
            Outcome::Terminated => return Err("research_failed_preserved_no_retry".into()),
        }
    }
}
