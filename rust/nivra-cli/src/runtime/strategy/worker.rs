//! Persistent Python strategy worker (`python3 -I -S -B` with one I/O thread,
//! bounded NDJSON exchange via `nivra_sandbox::stream::Stream`, continuous
//! stderr drain, monotonic per-call deadline, backoff, and quarantine).
use crate::runtime::millis;
use nivra_core::replay::digest;
use nivra_sandbox::{
    Failure,
    hardening::{
        HardeningPlan, TempDir, install_pre_exec, install_setpgid_only, kill_process_group,
        private_workdir,
    },
    stream::Stream,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::VecDeque,
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI32, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(crate) const RUNNER_PY: &str = include_str!("runner.py");
const STDERR_CAP: usize = 16 * 1024;
const PROTOCOL_VERSION: u32 = 1;

pub(crate) fn runner_sha256() -> String {
    digest(RUNNER_PY.as_bytes())
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct WorkerKey {
    pub source_sha256: String,
    pub params_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct WorkerConfig {
    pub python_path: PathBuf,
    pub call_deadline_ms: u64,
    pub startup_timeout_ms: u64,
    pub memory_mb: u64,
    pub max_open_files: u64,
    pub max_output_bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_sha256: Option<String>,
    pub hardening: bool,
    pub backoff_base_ms: i64,
    pub backoff_max_ms: i64,
    pub quarantine_failures: u32,
    pub quarantine_window_ms: i64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            python_path: PathBuf::from("/usr/bin/python3"),
            call_deadline_ms: 250,
            startup_timeout_ms: 2_000,
            memory_mb: 128,
            max_open_files: 32,
            max_output_bytes: 65_536,
            env_dir: None,
            env_sha256: None,
            hardening: true,
            backoff_base_ms: 1_000,
            backoff_max_ms: 60_000,
            quarantine_failures: 5,
            quarantine_window_ms: 60_000,
        }
    }
}

impl WorkerConfig {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if !self.python_path.is_absolute()
            || !(10..=30_000).contains(&self.call_deadline_ms)
            || !(100..=30_000).contains(&self.startup_timeout_ms)
            || !(32..=2048).contains(&self.memory_mb)
            || !(16..=128).contains(&self.max_open_files)
            || !(1..=262_144).contains(&self.max_output_bytes)
            || (self.env_sha256.is_some() && self.env_dir.is_none())
            || self
                .env_dir
                .as_deref()
                .is_some_and(|dir| !nivra_sandbox::env::path_ok(dir))
            || self
                .env_sha256
                .as_deref()
                .is_some_and(|hash| !nivra_sandbox::env::valid_hash(hash))
        {
            return Err("strategy_worker_config_invalid");
        }
        Ok(())
    }

    pub(crate) fn env_sha256(&self) -> Result<Option<String>, &'static str> {
        self.validate()?;
        match &self.env_dir {
            None => Ok(None),
            Some(dir) => {
                nivra_sandbox::env::verified_site_packages(dir, self.env_sha256.as_deref())
                    .map(|(_, hash)| Some(hash))
                    .map_err(|_| "strategy_env_invalid")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkerState {
    Starting,
    Ready,
    Busy {
        seq: u64,
        deadline: Instant,
    },
    Failed {
        reason: &'static str,
        retry_at_ms: i64,
        consecutive_failures: u32,
    },
    Quarantined {
        reason: &'static str,
    },
}

#[derive(Default)]
struct StderrRing {
    bytes: VecDeque<u8>,
}

impl StderrRing {
    fn push(&mut self, chunk: &[u8]) {
        for &b in chunk {
            if self.bytes.len() == STDERR_CAP {
                self.bytes.pop_front();
            }
            self.bytes.push_back(b);
        }
    }
    fn text(&self) -> String {
        let slice: Vec<u8> = self.bytes.iter().copied().collect();
        String::from_utf8_lossy(&slice).into_owned()
    }
}

struct WorkerRequest {
    seq: u64,
    payload: Vec<u8>,
    timeout: Duration,
}

enum WorkerEvent {
    Ready {
        generation: u64,
    },
    Reply {
        generation: u64,
        reply: WorkerReply,
        elapsed_us: u64,
    },
    Failed {
        generation: u64,
        reason: &'static str,
    },
}

pub(crate) struct WorkerReply {
    pub seq: u64,
    pub received_at_ms: Option<i64>,
    pub outcome: Result<Vec<u8>, &'static str>,
}

#[derive(Deserialize)]
struct ReadyMsg {
    v: u32,
    #[serde(rename = "type")]
    kind: String,
    abi: Option<String>,
    generation: Option<u64>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ReplyMsg {
    v: u32,
    #[serde(rename = "type")]
    kind: String,
    generation: u64,
    seq: Option<u64>,
    output: Option<Value>,
    error: Option<String>,
    elapsed_us: Option<u64>,
}

struct RunningProcess {
    _dir: TempDir,
    req_tx: Option<SyncSender<WorkerRequest>>,
    event_rx: Receiver<WorkerEvent>,
    pgid: Arc<AtomicI32>,
    reap_lock: Arc<Mutex<()>>,
    io_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
}

impl RunningProcess {
    fn terminate(&mut self) {
        self.req_tx.take();
        {
            let _guard = self.reap_lock.lock();
            let pid = self.pgid.load(Ordering::Acquire);
            kill_process_group(pid);
        }
        if let Some(handle) = self.io_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RunningProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn map_failure(failure: Failure) -> &'static str {
    match failure {
        Failure::Config => "strategy_worker_config",
        Failure::Input => "strategy_input_limit",
        Failure::Setup => "strategy_worker_setup",
        Failure::Unavailable => "strategy_worker_unavailable",
        Failure::Exit => "strategy_exit",
        Failure::WallTimeout => "strategy_wall_timeout",
        Failure::OutputLimit => "strategy_output_limit",
        Failure::OutputInvalid => "strategy_output_invalid",
    }
}

fn map_error_code(code: &str) -> &'static str {
    match code {
        "strategy_output_non_finite" => "strategy_output_non_finite",
        "strategy_missing_on_observation" => "strategy_missing_on_observation",
        "strategy_protocol_invalid" => "strategy_output_invalid",
        _ => "strategy_exit",
    }
}

fn spawn_process(
    source: &[u8],
    params: &Value,
    config: &WorkerConfig,
    generation: u64,
    stderr: Arc<Mutex<StderrRing>>,
) -> Result<RunningProcess, &'static str> {
    config.validate()?;
    if source.is_empty() || source.len() > 262_144 {
        return Err("strategy_input_limit");
    }
    if !config.python_path.is_file() {
        return Err("strategy_worker_unavailable");
    }
    let site_packages = match &config.env_dir {
        Some(dir) => Some(
            nivra_sandbox::env::verified_site_packages(dir, config.env_sha256.as_deref())
                .map_err(|_| "strategy_env_invalid")?
                .0,
        ),
        None => None,
    };
    let dir = private_workdir("nivra-strategy-").map_err(|_| "strategy_worker_setup")?;
    let runner_path = dir.path().join("runner.py");
    let strategy_path = dir.path().join("strategy.py");
    let params_path = dir.path().join("params.json");
    let params_bytes = serde_json::to_vec(params).map_err(|_| "strategy_input_limit")?;
    fs::write(&runner_path, RUNNER_PY).map_err(|_| "strategy_worker_setup")?;
    fs::write(&strategy_path, source).map_err(|_| "strategy_worker_setup")?;
    fs::write(&params_path, params_bytes).map_err(|_| "strategy_worker_setup")?;
    for file in [&runner_path, &strategy_path, &params_path] {
        let _ = fs::set_permissions(file, fs::Permissions::from_mode(0o400));
    }

    let mut cmd = Command::new(&config.python_path);
    cmd.env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(dir.path())
        .args(["-I", "-S", "-B"])
        .arg(&runner_path)
        .arg(&strategy_path)
        .arg(&params_path)
        .arg(generation.to_string());
    if let Some(site) = &site_packages {
        cmd.arg(site);
        for (key, value) in [
            ("OPENBLAS_NUM_THREADS", "1"),
            ("OMP_NUM_THREADS", "1"),
            ("MKL_NUM_THREADS", "1"),
            ("NUMEXPR_NUM_THREADS", "1"),
            ("VECLIB_MAXIMUM_THREADS", "1"),
            ("BLIS_NUM_THREADS", "1"),
        ] {
            cmd.env(key, value);
        }
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if config.hardening {
        let plan = HardeningPlan::prepare(
            &config.python_path,
            dir.path(),
            site_packages.as_deref(),
            config.memory_mb,
            config.max_open_files,
        )?;
        install_pre_exec(&mut cmd, plan);
    } else {
        install_setpgid_only(&mut cmd);
    }

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "strategy_worker_unavailable"
        } else {
            "strategy_worker_setup"
        }
    })?;
    let child_pid = child.id() as i32;
    let pgid = Arc::new(AtomicI32::new(child_pid));
    let reap_lock = Arc::new(Mutex::new(()));

    let child_stderr = child.stderr.take().ok_or("strategy_worker_setup")?;
    let stderr_ring = stderr;
    let stderr_thread = thread::Builder::new()
        .name(format!("strategy-stderr-{generation}"))
        .spawn(move || {
            let mut reader = child_stderr;
            let mut buf = [0u8; 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut guard) = stderr_ring.lock() {
                            guard.push(&buf[..n]);
                        }
                    }
                }
            }
        })
        .map_err(|_| "strategy_worker_setup")?;

    // Protocol envelope adds metadata around `max_output_bytes`.
    let frame_cap = config.max_output_bytes.saturating_add(4096);
    let startup_timeout = Duration::from_millis(config.startup_timeout_ms);
    let mut stream = Stream::from_pipes(
        child,
        Duration::from_millis(config.call_deadline_ms),
        frame_cap,
    )
    .map_err(map_failure)?;

    let (req_tx, req_rx) = mpsc::sync_channel::<WorkerRequest>(1);
    let (event_tx, event_rx) = mpsc::channel::<WorkerEvent>();
    let io_pgid = pgid.clone();
    let io_reap_lock = reap_lock.clone();
    let max_output = config.max_output_bytes;
    let io_thread = thread::Builder::new()
        .name(format!("strategy-io-{generation}"))
        .spawn(move || {
            let mut line = Vec::new();
            let ready = (|| -> Result<(), &'static str> {
                stream
                    .read_line_within(&mut line, startup_timeout)
                    .map_err(map_failure)?;
                let msg: ReadyMsg =
                    serde_json::from_slice(&line).map_err(|_| "strategy_output_invalid")?;
                if msg.v != PROTOCOL_VERSION {
                    return Err("strategy_output_invalid");
                }
                if msg.kind == "error" {
                    return Err(msg.error.as_deref().map_or("strategy_exit", map_error_code));
                }
                if msg.kind != "ready"
                    || msg.abi.as_deref() != Some(super::ABI)
                    || msg.generation != Some(generation)
                {
                    return Err("strategy_output_invalid");
                }
                Ok(())
            })();
            if let Err(reason) = ready {
                {
                    let _guard = io_reap_lock.lock();
                    io_pgid.store(0, Ordering::Release);
                    drop(stream);
                }
                let _ = event_tx.send(WorkerEvent::Failed { generation, reason });
                return;
            }
            let _ = event_tx.send(WorkerEvent::Ready { generation });

            while let Ok(req) = req_rx.recv() {
                line.clear();
                let exchange = stream.exchange_within(&req.payload, &mut line, req.timeout);
                let received_at_ms = exchange.is_ok().then(millis);
                let (outcome, elapsed_us) = match exchange {
                    Ok(()) => match parse_reply(&line, generation, req.seq, max_output) {
                        Ok((bytes, elapsed_us)) => (Ok(bytes), elapsed_us),
                        Err(reason) => (Err(reason), 0),
                    },
                    Err(failure) => (Err(map_failure(failure)), 0),
                };
                let failed = outcome.is_err();
                let _ = event_tx.send(WorkerEvent::Reply {
                    generation,
                    reply: WorkerReply {
                        seq: req.seq,
                        received_at_ms,
                        outcome,
                    },
                    elapsed_us,
                });
                if failed {
                    break;
                }
            }
            let _guard = io_reap_lock.lock();
            io_pgid.store(0, Ordering::Release);
            drop(stream);
        })
        .map_err(|_| "strategy_worker_setup")?;

    Ok(RunningProcess {
        _dir: dir,
        req_tx: Some(req_tx),
        event_rx,
        pgid,
        reap_lock,
        io_thread: Some(io_thread),
        stderr_thread: Some(stderr_thread),
    })
}

fn parse_reply(
    line: &[u8],
    expected_gen: u64,
    expected_seq: u64,
    max_output: usize,
) -> Result<(Vec<u8>, u64), &'static str> {
    let msg: ReplyMsg = serde_json::from_slice(line).map_err(|_| "strategy_output_invalid")?;
    if msg.v != PROTOCOL_VERSION || msg.generation != expected_gen || msg.seq != Some(expected_seq)
    {
        return Err("strategy_output_invalid");
    }
    if msg.kind == "error" {
        return Err(msg.error.as_deref().map_or("strategy_exit", map_error_code));
    }
    if msg.kind != "result" {
        return Err("strategy_output_invalid");
    }
    let output = msg.output.ok_or("strategy_output_invalid")?;
    let bytes = serde_json::to_vec(&output).map_err(|_| "strategy_output_invalid")?;
    if bytes.len() > max_output {
        return Err("strategy_output_limit");
    }
    Ok((bytes, msg.elapsed_us.unwrap_or(0)))
}

/// One persistent strategy worker owning a `python3 -I -S -B` process.
pub(crate) struct Worker {
    source: Vec<u8>,
    params: Value,
    config: WorkerConfig,
    generation: u64,
    next_seq: u64,
    state: WorkerState,
    pending_reply: Option<WorkerReply>,
    failure_times: VecDeque<i64>,
    stderr: Arc<Mutex<StderrRing>>,
    process: Option<RunningProcess>,
    last_elapsed_us: Option<u64>,
}

impl Worker {
    pub(crate) fn start(
        _key: WorkerKey,
        source: Vec<u8>,
        params: Value,
        config: WorkerConfig,
        now_ms: i64,
    ) -> Result<Self, &'static str> {
        let stderr = Arc::new(Mutex::new(StderrRing::default()));
        let mut worker = Self {
            source,
            params,
            config,
            generation: 0,
            next_seq: 0,
            state: WorkerState::Starting,
            pending_reply: None,
            failure_times: VecDeque::new(),
            stderr,
            process: None,
            last_elapsed_us: None,
        };
        worker.spawn_next(now_ms);
        Ok(worker)
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> &WorkerState {
        &self.state
    }

    pub(crate) fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .map(|guard| guard.text())
            .unwrap_or_default()
    }

    fn spawn_next(&mut self, now_ms: i64) {
        self.generation += 1;
        if let Ok(mut guard) = self.stderr.lock() {
            guard.bytes.clear();
        }
        match spawn_process(
            &self.source,
            &self.params,
            &self.config,
            self.generation,
            self.stderr.clone(),
        ) {
            Ok(process) => {
                self.process = Some(process);
                self.state = WorkerState::Starting;
            }
            Err(reason) => {
                self.record_failure(reason, now_ms);
            }
        }
    }

    fn record_failure(&mut self, reason: &'static str, now_ms: i64) {
        if let Some(mut process) = self.process.take() {
            process.terminate();
        }
        let consecutive = match self.state {
            WorkerState::Failed {
                consecutive_failures,
                ..
            } => consecutive_failures.saturating_add(1),
            WorkerState::Quarantined { .. } => self.config.quarantine_failures,
            _ => 1,
        };
        self.failure_times.push_back(now_ms);
        let cutoff = now_ms.saturating_sub(self.config.quarantine_window_ms);
        while self.failure_times.front().is_some_and(|&at| at < cutoff) {
            self.failure_times.pop_front();
        }
        if self.config.quarantine_failures > 0
            && self.failure_times.len() >= self.config.quarantine_failures as usize
        {
            self.state = WorkerState::Quarantined { reason };
            return;
        }
        let backoff = if self.config.backoff_base_ms <= 0 {
            0
        } else {
            self.config
                .backoff_base_ms
                .saturating_mul(1_i64 << consecutive.saturating_sub(1).min(6))
                .min(self.config.backoff_max_ms)
        };
        self.state = WorkerState::Failed {
            reason,
            retry_at_ms: now_ms.saturating_add(backoff),
            consecutive_failures: consecutive,
        };
    }

    /// Non-blocking internal drain of worker events into `state` / `pending_reply`.
    fn drain_events(&mut self, now_ms: i64) {
        let events: Vec<WorkerEvent> = if let Some(process) = &self.process {
            let mut items = Vec::new();
            loop {
                match process.event_rx.try_recv() {
                    Ok(ev) => items.push(ev),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        items.push(WorkerEvent::Failed {
                            generation: self.generation,
                            reason: "strategy_exit",
                        });
                        break;
                    }
                }
            }
            items
        } else {
            Vec::new()
        };

        for ev in events {
            match ev {
                WorkerEvent::Ready { generation } if generation == self.generation => {
                    if matches!(self.state, WorkerState::Starting) {
                        self.state = WorkerState::Ready;
                    }
                }
                WorkerEvent::Reply {
                    generation,
                    mut reply,
                    elapsed_us,
                } if generation == self.generation => {
                    if let WorkerState::Busy { seq, .. } = self.state {
                        if reply.seq != seq {
                            reply.seq = seq;
                            reply.outcome = Err("strategy_output_invalid");
                        }
                        match reply.outcome {
                            Ok(_) => {
                                self.last_elapsed_us = Some(elapsed_us);
                                self.state = WorkerState::Ready;
                            }
                            Err(reason) => self.record_failure(reason, now_ms),
                        }
                        self.pending_reply = Some(reply);
                    }
                }
                WorkerEvent::Failed { generation, reason } if generation == self.generation => {
                    if let WorkerState::Busy { seq, .. } = self.state {
                        self.record_failure(reason, now_ms);
                        self.pending_reply = Some(WorkerReply {
                            seq,
                            received_at_ms: None,
                            outcome: Err(reason),
                        });
                    } else if !matches!(self.state, WorkerState::Quarantined { .. }) {
                        self.record_failure(reason, now_ms);
                    }
                }
                _ => {}
            }
        }
        // Guard against a wedged I/O thread past deadline + small grace.
        if let WorkerState::Busy { seq, deadline } = self.state
            && Instant::now() >= deadline + Duration::from_millis(50)
        {
            self.record_failure("strategy_wall_timeout", now_ms);
            self.pending_reply = Some(WorkerReply {
                seq,
                received_at_ms: None,
                outcome: Err("strategy_wall_timeout"),
            });
        }
    }

    #[cfg(test)]
    pub(crate) fn is_call_finished(&mut self, now_ms: i64) -> bool {
        self.drain_events(now_ms);
        self.pending_reply.is_some() || !matches!(self.state, WorkerState::Busy { .. })
    }

    /// Non-blocking poll for ready or reply events. Returns `Some(...)` if an
    /// active in-flight call finished on this poll.
    pub(crate) fn poll(&mut self, now_ms: i64) -> Option<WorkerReply> {
        self.drain_events(now_ms);
        self.pending_reply.take()
    }

    /// Ensure the worker is ready (restarting if backoff elapsed). Returns
    /// `Ok(true)` when ready to accept a call, `Ok(false)` while starting/busy,
    /// or `Err(reason)` if quarantined or waiting on backoff.
    pub(crate) fn ensure_ready(&mut self, now_ms: i64) -> Result<bool, &'static str> {
        let _ = self.poll(now_ms);
        match self.state {
            WorkerState::Ready => Ok(true),
            WorkerState::Starting | WorkerState::Busy { .. } => Ok(false),
            WorkerState::Quarantined { reason } => Err(reason),
            WorkerState::Failed {
                reason,
                retry_at_ms,
                ..
            } => {
                if now_ms < retry_at_ms {
                    Err(reason)
                } else {
                    self.spawn_next(now_ms);
                    Ok(false)
                }
            }
        }
    }

    /// Submit one observation non-blockingly. `observation_json` is the
    /// serialized JSON object for the `observation` field.
    pub(crate) fn submit(
        &mut self,
        observation_json: &[u8],
        now_ms: i64,
    ) -> Result<u64, &'static str> {
        let _ = self.poll(now_ms);
        if !matches!(self.state, WorkerState::Ready) {
            return Err(match self.state {
                WorkerState::Failed { reason, .. } | WorkerState::Quarantined { reason } => reason,
                _ => "strategy_worker_busy",
            });
        }
        let seq = self.next_seq + 1;
        self.next_seq = seq;
        let payload = build_request_line(self.generation, seq, observation_json);
        let timeout = Duration::from_millis(self.config.call_deadline_ms);
        let deadline = Instant::now() + timeout;
        let Some(process) = &self.process else {
            self.record_failure("strategy_worker_unavailable", now_ms);
            return Err("strategy_worker_unavailable");
        };
        let Some(tx) = &process.req_tx else {
            self.record_failure("strategy_exit", now_ms);
            return Err("strategy_exit");
        };
        match tx.try_send(WorkerRequest {
            seq,
            payload,
            timeout,
        }) {
            Ok(()) => {
                self.state = WorkerState::Busy { seq, deadline };
                Ok(seq)
            }
            Err(_) => {
                self.record_failure("strategy_worker_busy", now_ms);
                Err("strategy_worker_busy")
            }
        }
    }

    /// Wait synchronously for `Starting` -> `Ready` (used by replay/bench/tests).
    pub(crate) fn wait_ready(&mut self, now_ms: i64) -> Result<(), &'static str> {
        let _ = self.poll(now_ms);
        if matches!(self.state, WorkerState::Failed { retry_at_ms, .. } if now_ms >= retry_at_ms) {
            self.spawn_next(now_ms);
        }
        while matches!(self.state, WorkerState::Starting) {
            let Some(process) = &self.process else {
                return Err("strategy_worker_unavailable");
            };
            match process.event_rx.recv() {
                Ok(WorkerEvent::Ready { generation }) if generation == self.generation => {
                    self.state = WorkerState::Ready;
                    return Ok(());
                }
                Ok(WorkerEvent::Failed { generation, reason }) if generation == self.generation => {
                    self.record_failure(reason, now_ms);
                    return Err(reason);
                }
                Ok(_) => {}
                Err(_) => {
                    self.record_failure("strategy_exit", now_ms);
                    return Err("strategy_exit");
                }
            }
        }
        match self.state {
            WorkerState::Ready => Ok(()),
            WorkerState::Failed { reason, .. } | WorkerState::Quarantined { reason } => Err(reason),
            WorkerState::Busy { .. } => Err("strategy_worker_busy"),
            WorkerState::Starting => Err("strategy_worker_setup"),
        }
    }

    /// Synchronous call on the persistent worker (reused across calls in
    /// replay, action-bench, and tests). Zero sleep polling.
    pub(crate) fn call_sync(
        &mut self,
        observation_json: &[u8],
        now_ms: i64,
    ) -> Result<Vec<u8>, &'static str> {
        self.wait_ready(now_ms)?;
        let seq = self.submit(observation_json, now_ms)?;
        loop {
            let Some(process) = &self.process else {
                self.record_failure("strategy_worker_unavailable", now_ms);
                return Err("strategy_worker_unavailable");
            };
            match process.event_rx.recv() {
                Ok(WorkerEvent::Reply {
                    generation,
                    reply,
                    elapsed_us,
                }) if generation == self.generation => {
                    if reply.seq != seq {
                        self.record_failure("strategy_output_invalid", now_ms);
                        return Err("strategy_output_invalid");
                    }
                    return match reply.outcome {
                        Ok(bytes) => {
                            self.last_elapsed_us = Some(elapsed_us);
                            self.state = WorkerState::Ready;
                            Ok(bytes)
                        }
                        Err(reason) => {
                            self.record_failure(reason, now_ms);
                            Err(reason)
                        }
                    };
                }
                Ok(WorkerEvent::Failed { generation, reason }) if generation == self.generation => {
                    self.record_failure(reason, now_ms);
                    return Err(reason);
                }
                Ok(_) => {}
                Err(_) => {
                    self.record_failure("strategy_exit", now_ms);
                    return Err("strategy_exit");
                }
            }
        }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(mut process) = self.process.take() {
            process.terminate();
        }
    }
}

fn build_request_line(generation: u64, seq: u64, observation_json: &[u8]) -> Vec<u8> {
    let header = format!(
        "{{\"v\":{PROTOCOL_VERSION},\"generation\":{generation},\"seq\":{seq},\"observation\":"
    );
    let mut out = Vec::with_capacity(header.len() + observation_json.len() + 2);
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(observation_json);
    out.extend_from_slice(b"}\n");
    out
}
