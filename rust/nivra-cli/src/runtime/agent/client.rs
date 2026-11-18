//! One long-lived agent process, driven over its stdin/stdout.
use super::protocol::{FromAgent, SessionRef, ToAgent, TurnStatus, Usage, VERSION};
use super::{AgentSettings, Lane, ModelSpec};
use crate::runtime::Result;
use serde::Serialize;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long an aborted turn may take to report before the agent is presumed hung.
const ABORT_GRACE: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(50);
const MAX_LINE_BYTES: usize = 8_000_000;
const MAX_EVENTS: usize = 4_096;

/// Runs one bridged tool call for the turn in progress.
pub(crate) trait ToolHost {
    fn call(
        &mut self,
        call_id: &str,
        name: &str,
        args: &Value,
    ) -> std::result::Result<Value, String>;
}

pub(crate) struct TurnRequest {
    pub id: String,
    pub lane: Lane,
    pub session: SessionRef,
    pub model: ModelSpec,
    pub trigger: Value,
    pub brief: Value,
    pub tools: Vec<Value>,
}

/// What a turn may still spend: model requests and US dollars.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Allowance {
    pub calls: u64,
    pub cost_usd: f64,
    pub max_tool_calls: usize,
    pub timeout: Duration,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ToolCallRecord {
    pub call_id: String,
    pub name: String,
    pub ok: bool,
    pub error: Option<String>,
    pub duration_ms: u128,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct TurnOutcome {
    pub id: String,
    pub lane: Lane,
    pub status: TurnStatus,
    pub error: Option<String>,
    pub text: String,
    pub usage: Usage,
    pub model: String,
    pub provider: String,
    pub session_file: Option<String>,
    pub prompt_sha256: String,
    pub tool_calls: Vec<ToolCallRecord>,
    pub events: Vec<Value>,
    pub elapsed_ms: u128,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Ready {
    pub pi_version: String,
    pub node: String,
    pub pid: u32,
    pub startup_ms: u128,
}

/// What a turn produced before its result arrived, kept if the agent dies.
#[derive(Default)]
struct Partial {
    usage: Usage,
    events: Vec<Value>,
    tool_calls: Vec<ToolCallRecord>,
}

impl Partial {
    fn charge(&mut self, usage: &Value) {
        let count = |field: &str| usage[field].as_u64().unwrap_or(0);
        self.usage.input += count("input");
        self.usage.output += count("output");
        self.usage.cache_read += count("cache_read");
        self.usage.cache_write += count("cache_write");
        self.usage.cost_usd += usage["cost_usd"].as_f64().unwrap_or(0.0);
        self.usage.calls += 1;
    }
}

pub(crate) struct AgentClient {
    writer: Box<dyn Write + Send>,
    lines: Receiver<std::io::Result<String>>,
    child: Option<Child>,
    pub ready: Ready,
}

fn read_lines(reader: impl Read + Send + 'static) -> Receiver<std::io::Result<String>> {
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut line = String::new();
            match (&mut reader)
                .take(MAX_LINE_BYTES as u64 + 1)
                .read_line(&mut line)
            {
                Ok(0) => return,
                Ok(n) if n > MAX_LINE_BYTES => {
                    let _ = sender.send(Err(std::io::Error::other("agent_line_too_long")));
                    return;
                }
                Ok(_) => {
                    if sender.send(Ok(line)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                    return;
                }
            }
        }
    });
    receiver
}

impl AgentClient {
    /// Start the agent process. `extra` adds arguments such as an offline `--fake` script.
    pub(crate) fn spawn(settings: &AgentSettings, extra: &[std::ffi::OsString]) -> Result<Self> {
        let started = Instant::now();
        let mut command = Command::new(&settings.node);
        command
            .arg(&settings.entry)
            .arg("--agent-dir")
            .arg(&settings.agent_dir);
        if let Some(prompts) = &settings.prompts {
            command.arg("--prompts").arg(prompts);
        }
        let mut child = command
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|_| "agent_spawn_failed")?;
        let writer = child.stdin.take().ok_or("agent_spawn_failed")?;
        let reader = child.stdout.take().ok_or("agent_spawn_failed")?;
        Self::connect(reader, writer, Some(child), started)
    }

    /// Wait for the agent's `ready` line on an already connected stream.
    pub(crate) fn connect(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        child: Option<Child>,
        started: Instant,
    ) -> Result<Self> {
        let lines = read_lines(reader);
        let mut client = Self {
            writer: Box::new(writer),
            lines,
            child,
            ready: Ready {
                pi_version: String::new(),
                node: String::new(),
                pid: 0,
                startup_ms: 0,
            },
        };
        match client.next(READY_TIMEOUT.saturating_sub(started.elapsed()))? {
            Some(FromAgent::Ready {
                pi_version,
                node,
                pid,
                ..
            }) => {
                client.ready = Ready {
                    pi_version,
                    node,
                    pid,
                    startup_ms: started.elapsed().as_millis(),
                };
                Ok(client)
            }
            Some(_) => Err("agent_protocol_invalid".into()),
            None => Err("agent_ready_timeout".into()),
        }
    }

    pub(crate) fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    fn send(&mut self, message: &ToAgent<'_>) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .and_then(|()| self.writer.flush())
            .map_err(|_| "agent_exited".into())
    }

    /// The next message within `wait`; `None` when none arrived.
    fn next(&mut self, wait: Duration) -> Result<Option<FromAgent>> {
        match self.lines.recv_timeout(wait) {
            Ok(Ok(line)) => {
                let message: FromAgent =
                    serde_json::from_str(&line).map_err(|_| "agent_protocol_invalid")?;
                if message.version() != VERSION {
                    return Err("agent_protocol_version".into());
                }
                Ok(Some(message))
            }
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => Err("agent_exited".into()),
            Err(RecvTimeoutError::Timeout) => Ok(None),
        }
    }

    /// Run one turn to its result. `spent` is what the lane already used this
    /// session date; the turn is aborted as soon as pi's usage reports cross
    /// the allowance, and `preempt` aborts it for a higher-priority turn.
    pub(crate) fn run_turn(
        &mut self,
        request: &TurnRequest,
        host: &mut dyn ToolHost,
        spent: &Usage,
        allowance: Allowance,
        preempt: &dyn Fn() -> bool,
    ) -> Result<TurnOutcome> {
        if spent.calls >= allowance.calls || spent.cost_usd >= allowance.cost_usd {
            return Err("agent_budget_exhausted".into());
        }
        let started = Instant::now();
        let mut partial = Partial::default();
        match self.drive(
            request,
            host,
            spent,
            allowance,
            preempt,
            started,
            &mut partial,
        ) {
            Ok(outcome) => Ok(outcome),
            // The agent is gone or hung after the turn started: what pi already
            // reported is still charged, and the turn ends as an error.
            Err(error) => Ok(TurnOutcome {
                id: request.id.clone(),
                lane: request.lane,
                status: TurnStatus::Error,
                error: Some(error.to_string()),
                text: String::new(),
                usage: partial.usage,
                model: request.model.id.clone(),
                provider: request.model.provider.clone(),
                session_file: None,
                prompt_sha256: String::new(),
                tool_calls: partial.tool_calls,
                events: partial.events,
                elapsed_ms: started.elapsed().as_millis(),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn drive(
        &mut self,
        request: &TurnRequest,
        host: &mut dyn ToolHost,
        spent: &Usage,
        allowance: Allowance,
        preempt: &dyn Fn() -> bool,
        started: Instant,
        partial: &mut Partial,
    ) -> Result<TurnOutcome> {
        self.send(&ToAgent::Turn {
            v: VERSION,
            id: &request.id,
            lane: request.lane,
            session: &request.session,
            model: &request.model,
            trigger: &request.trigger,
            brief: &request.brief,
            tools: &request.tools,
        })?;
        let mut deadline = started + allowance.timeout;
        let mut aborted: Option<&'static str> = None;
        loop {
            let calls = spent.calls + partial.usage.calls;
            let cost_usd = spent.cost_usd + partial.usage.cost_usd;
            if aborted.is_none() {
                let reason = if Instant::now() >= deadline {
                    Some("agent_turn_timeout")
                } else if calls >= allowance.calls || cost_usd >= allowance.cost_usd {
                    Some("agent_budget_exhausted")
                } else if preempt() {
                    Some("agent_preempted")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    self.send(&ToAgent::Abort {
                        v: VERSION,
                        turn_id: &request.id,
                        reason,
                    })?;
                    aborted = Some(reason);
                    deadline = Instant::now() + ABORT_GRACE;
                }
            }
            let Some(message) = self.next(POLL)? else {
                if aborted.is_some() && Instant::now() >= deadline {
                    return Err("agent_turn_unresponsive".into());
                }
                continue;
            };
            match message {
                FromAgent::ToolCall {
                    turn_id,
                    call_id,
                    name,
                    args,
                    ..
                } if turn_id == request.id => {
                    let tool_started = Instant::now();
                    let result = if aborted.is_some() {
                        Err("agent_turn_aborted".to_owned())
                    } else if partial.tool_calls.len() >= allowance.max_tool_calls {
                        Err("agent_tool_budget_exhausted".to_owned())
                    } else {
                        host.call(&call_id, &name, &args)
                    };
                    let (ok, output, error) = match &result {
                        Ok(value) => (true, Some(value), None),
                        Err(error) => (false, None, Some(error.as_str())),
                    };
                    self.send(&ToAgent::ToolResult {
                        v: VERSION,
                        turn_id: &request.id,
                        call_id: &call_id,
                        ok,
                        output,
                        error,
                    })?;
                    partial.tool_calls.push(ToolCallRecord {
                        call_id,
                        name,
                        ok,
                        error: result.err(),
                        duration_ms: tool_started.elapsed().as_millis(),
                    });
                }
                FromAgent::Event { turn_id, event, .. } if turn_id == request.id => {
                    if event["kind"] == "message" {
                        partial.charge(&event["usage"]);
                    }
                    if partial.events.len() < MAX_EVENTS {
                        partial.events.push(event);
                    }
                }
                FromAgent::TurnResult {
                    id,
                    status,
                    error,
                    output,
                    usage,
                    model,
                    provider,
                    session_file,
                    prompt_sha256,
                    ..
                } if id == request.id => {
                    return Ok(TurnOutcome {
                        id,
                        lane: request.lane,
                        status,
                        error: error.or(aborted
                            .filter(|_| status != TurnStatus::Ok)
                            .map(str::to_owned)),
                        text: output.text,
                        usage,
                        model,
                        provider,
                        session_file,
                        prompt_sha256,
                        tool_calls: std::mem::take(&mut partial.tool_calls),
                        events: std::mem::take(&mut partial.events),
                        elapsed_ms: started.elapsed().as_millis(),
                    });
                }
                FromAgent::Error { error, .. } => {
                    if partial.events.len() < MAX_EVENTS {
                        partial
                            .events
                            .push(json!({"kind":"protocol_error","error":error}));
                    }
                }
                _ => return Err("agent_protocol_invalid".into()),
            }
        }
    }

    /// Ask the agent to exit and reap it.
    pub(crate) fn shutdown(mut self) -> Result<()> {
        let _ = self.send(&ToAgent::Shutdown { v: VERSION });
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if child.try_wait()?.is_some() {
                    return Ok(());
                }
                std::thread::sleep(POLL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }
}

impl Drop for AgentClient {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
