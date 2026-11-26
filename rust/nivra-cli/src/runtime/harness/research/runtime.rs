use super::*;

/// Typed launch gate, read from the book's own state. Research waits for a
/// pending owner decision, so it never delays or displaces a queued trading
/// decision. It does not wait for an idle model worker (#236).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResearchLaunch {
    pub registered: bool,
    pub reason: &'static str,
}

/// The book state the launch gate reads. Model workers are deliberately not
/// an input.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LaunchBook {
    pub halted: bool,
    pub stopped: bool,
    pub decision_pending: bool,
    pub context_unavailable: bool,
}

impl ResearchLaunch {
    pub(crate) fn for_book(book: LaunchBook) -> Self {
        Self {
            registered: !book.halted
                && !book.stopped
                && !book.context_unavailable
                && !book.decision_pending,
            reason: if book.context_unavailable {
                "controller_session_end_unavailable"
            } else if book.halted {
                "book_halted"
            } else if book.stopped {
                "book_stopped"
            } else if book.decision_pending {
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
    String,
    u32,
    std::sync::mpsc::Receiver<std::result::Result<(), &'static str>>,
);

#[derive(Default)]
pub(crate) struct ResearchRuntime {
    pub(crate) active: Option<ActiveResearch>,
    pub(crate) terminal_cursor: Option<jobs::TaskCursor>,
    pub(crate) blocked: Option<(String, u32, &'static str)>,
}

impl ResearchRuntime {
    pub(crate) fn observe_channel(&mut self) {
        let Some((task_id, revision, receiver)) = self.active.as_ref() else {
            return;
        };
        let task_id = task_id.clone();
        let revision = *revision;
        match receiver.try_recv() {
            Ok(Ok(())) => self.active = None,
            Ok(Err(reason)) => {
                self.active = None;
                self.blocked = Some((task_id, revision, reason));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.active = None;
                self.blocked = Some((task_id, revision, "research_worker_channel_lost"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    pub(crate) fn clear_blocked_if_terminal(
        &mut self,
        task_id: &str,
        revision: u32,
        state: jobs::TaskState,
    ) {
        if task_state_terminal(state)
            && self
                .blocked
                .as_ref()
                .is_some_and(|(blocked_id, blocked_revision, _)| {
                    blocked_id == task_id && *blocked_revision == revision
                })
        {
            self.blocked = None;
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
        let mut db = open_queue(&settings, &authority, false)?;
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
        let registered = match if task.spec.capability == jobs::Capability::ArchiveBars {
            bar_worker_run(&authority, &task)
        } else {
            registered_run(&settings, &task)
        } {
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

/// Low-priority controller tick: terminal events are deduped by the owner
/// ledger, and at most one task runs at a time.
pub(crate) fn tick_tasks(
    db: &mut Connection,
    c: &Config,
    cfg: &PlannerConfig,
    launch: ResearchLaunch,
    runtime: &mut ResearchRuntime,
    now: i64,
) -> Result<Value> {
    let Some(settings) = cfg.research_tools.as_ref() else {
        return Ok(json!({"status":"disabled"}));
    };
    let binding = book_binding(c)?;
    runtime.observe_channel();
    let Ok(mut queue) = open_queue(settings, &c.directory.join("model"), false) else {
        return Ok(json!({"status":"unavailable"}));
    };
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
        super::super::super::redecision::enqueue_research_event(
            db,
            c,
            &row.spec.task_id,
            row.spec.revision,
            row.job.result_hash.as_deref(),
            status,
            terminal_at_ms,
        )?;
    }
    for row in &terminal {
        runtime.clear_blocked_if_terminal(&row.spec.task_id, row.spec.revision, row.status);
    }
    if runtime.active.is_some() {
        return Ok(json!({"status":"running"}));
    }
    let Some(task) =
        jobs::active_scoped(&mut queue).map_err(|_| "research_task_state_unavailable")?
    else {
        return Ok(json!({"status":"idle"}));
    };
    if let Some((blocked_id, blocked_revision, reason)) = &runtime.blocked
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
        .name("nivra-research-owner".into())
        .spawn(move || {
            let result = drive_task(settings, authority, task_id, revision);
            let _ = sender.send(result);
        })
        .map_err(|_| "research_worker_unavailable")?;
    let started =
        json!({"status":"started","task_id":task.spec.task_id,"revision":task.spec.revision});
    runtime.active = Some((task.spec.task_id, task.spec.revision, receiver));
    Ok(started)
}
