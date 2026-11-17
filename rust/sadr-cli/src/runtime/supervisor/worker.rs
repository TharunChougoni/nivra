use super::*;

#[derive(Debug)]
pub(crate) struct Worker {
    pub cancel: Arc<AtomicBool>,
    pub(super) receiver: mpsc::Receiver<std::result::Result<String, String>>,
    pub(super) reason: &'static str,
    pub(super) hashes: AppliedHashes,
    pub(super) run_first: bool,
    pub(super) hold_until_ms: i64,
    /// Persistent-session identity for this turn (#141).
    pub(super) session: String,
    pub(super) turn_id: String,
    pub(super) notes_version: u64,
    #[allow(dead_code)] // Recorded on the turn and surfaced by the session API.
    pub(super) prompt_version: String,
    pub(super) kind: RunKind,
    pub(super) decision: Option<super::super::decision_owner::Claim>,
    pub(super) lifecycle: Option<Lifecycle>,
    /// Render bounds this turn was built with; a failed turn classifies its own
    /// near-cap evidence against them (#224).
    pub(super) bounds: Compaction,
    /// JSON-escaped rendered prompt bytes, as embedded in the request body.
    pub(super) escaped_prompt_bytes: u64,
    /// Backend provenance after the call: request/token/incomplete evidence.
    pub(super) evidence: Arc<Mutex<Option<Value>>>,
    /// Monotonic start of this worker; review artifacts record honest elapsed
    /// time from here, never a fabricated wall timestamp.
    pub(super) started: std::time::Instant,
    /// The model-selection profile/revision this turn was admitted under. A
    /// failure backoff must not leak to another selection.
    pub(super) selection_binding: Option<String>,
}

impl Worker {
    /// Bounded backend provenance for this turn. An absent call leaves `null`.
    pub(super) fn evidence(&self) -> Value {
        self.evidence
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
            .unwrap_or(Value::Null)
    }
}

/// Luna may be held only for a run-first supervisor pass and only until the
/// configured hold expires. A failed or slow supervisor fails open.
pub(crate) fn holding_luna(worker: Option<&Worker>, now: i64) -> bool {
    worker.is_some_and(|worker| should_hold(worker.run_first, worker.hold_until_ms, now))
}

pub(super) fn should_hold(run_first: bool, hold_until_ms: i64, now: i64) -> bool {
    run_first && now < hold_until_ms
}

#[cfg(test)]
pub(super) fn test_worker(session: &str, turn_id: &str, notes_version: u64) -> Worker {
    let (_sender, receiver) = mpsc::sync_channel(1);
    Worker {
        cancel: Arc::new(AtomicBool::new(false)),
        receiver,
        reason: "idle",
        hashes: AppliedHashes::default(),
        run_first: false,
        hold_until_ms: 0,
        session: session.to_owned(),
        turn_id: turn_id.to_owned(),
        notes_version,
        prompt_version: "prompt-test".into(),
        kind: RunKind::Supervisor,
        decision: None,
        lifecycle: None,
        bounds: Compaction::default(),
        escaped_prompt_bytes: 0,
        evidence: Arc::new(Mutex::new(None)),
        started: std::time::Instant::now(),
        selection_binding: None,
    }
}

#[cfg(test)]
pub(super) fn test_worker_with(
    receiver: mpsc::Receiver<std::result::Result<String, String>>,
    session: &str,
    turn_id: &str,
    kind: RunKind,
) -> Worker {
    Worker {
        cancel: Arc::new(AtomicBool::new(false)),
        receiver,
        reason: "idle",
        hashes: AppliedHashes::default(),
        run_first: false,
        hold_until_ms: 0,
        session: session.to_owned(),
        turn_id: turn_id.to_owned(),
        notes_version: 0,
        prompt_version: "prompt-test".into(),
        kind,
        decision: None,
        lifecycle: None,
        bounds: Compaction::default(),
        escaped_prompt_bytes: 0,
        evidence: Arc::new(Mutex::new(None)),
        started: std::time::Instant::now(),
        selection_binding: None,
    }
}

#[cfg(test)]
pub(super) fn begin_turn(root: &std::path::Path, session: &str, turn_id: &str, now: i64) -> Worker {
    let notes_version = session::Notes::new(root, session)
        .pin_waiting(turn_id, now)
        .unwrap();
    session::SessionLog::new(root, session)
        .append(
            now,
            session::Body::Turn {
                turn: session::Turn {
                    id: turn_id.into(),
                    session: session.into(),
                    trigger: "idle".into(),
                    prompt_version: "prompt-test".into(),
                    notes_version,
                    fills_at_start: 0,
                    started_at_ms: now,
                    request: json!({"trigger":"idle"}),
                },
            },
        )
        .unwrap();
    test_worker(session, turn_id, notes_version)
}

#[cfg(test)]
pub(super) fn begin_test_lifecycle(
    root: &std::path::Path,
    session: &str,
    id: &str,
    now: i64,
    task: LifecycleTask,
) -> Worker {
    let lifecycle = Lifecycle {
        version: 1,
        hashes: AppliedHashes::default(),
        task,
    };
    let kind = lifecycle.kind();
    let reason = if kind == RunKind::Handoff {
        "handoff"
    } else {
        "compaction"
    };
    let notes_version = session::Notes::new(root, session)
        .pin_waiting(id, now)
        .unwrap();
    session::SessionLog::new(root, session)
        .append(
            now,
            session::Body::Turn {
                turn: session::Turn {
                    id: id.into(),
                    session: session.into(),
                    trigger: reason.into(),
                    prompt_version: "prompt-test".into(),
                    notes_version,
                    fills_at_start: 0,
                    started_at_ms: now,
                    request: json!({"host_lifecycle": lifecycle}),
                },
            },
        )
        .unwrap();
    let mut worker = test_worker(session, id, notes_version);
    worker.kind = kind;
    worker.reason = reason;
    worker.lifecycle = Some(lifecycle);
    worker
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
