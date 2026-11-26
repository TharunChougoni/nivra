//! Lane turns: grants, budgets, journal and the research request loop.
use super::brief::Brief;
use super::client::{Agent, Allowance, TurnOutcome, TurnRequest};
use super::protocol::{SessionRef, TurnStatus, Usage};
use super::requests::Requests;
use super::tools::{LaneHost, PlanAttempt, Verdict, lane_definitions};
use super::{AgentSettings, Lane, ModelSpec};
use crate::runtime::harness::{Bridge, LaneGrant};
use crate::runtime::lane_queue::{LaneMessage, MessageBody, TransitionReason};
use crate::runtime::planner::{Backend, CallGuard, PlannerConfig};
use crate::runtime::web_tools::Transport;
use crate::runtime::workspace::WorkspaceScope;
use crate::runtime::{Config, Result, digest, rollover};
use chrono::NaiveDate;
use nivra_core::execution::{Instrument, Portfolio};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const MAX_SUMMARY_CHARS: usize = 600;
const MAX_REQUESTS_PER_PASS: usize = 4;
const MAX_MEMORY_CHARS: usize = 8_000;

/// What a lane turn's tools read. The controller builds it from its own state;
/// agent-check and tests build it from fixtures.
pub(crate) struct TurnContext {
    /// Frozen evidence the read tools serve.
    pub bundle: Value,
    pub backend: Box<dyn Backend + Send>,
    pub portfolio: Portfolio,
    pub instruments: Vec<Instrument>,
    /// HTTP for the web tools; tests pass a fake so no socket opens.
    pub web: Box<dyn Transport + Send>,
}

/// A turn's usage so far, kept until the turn settles. An entry whose process
/// is gone was cut off by a crash and is charged once.
#[derive(Clone, Serialize, Deserialize)]
struct InFlight {
    lane: Lane,
    pid: u32,
    usage: Usage,
}

#[derive(Default, Serialize, Deserialize)]
struct Spend {
    date: String,
    lanes: BTreeMap<Lane, Usage>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    in_flight: BTreeMap<String, InFlight>,
}

/// Today's lane spend in `model/agent/usage.json`. The controller's worker
/// and `nivra agent-turn` may both charge it, so every change is a locked
/// read-modify-write.
struct Ledger {
    path: PathBuf,
    date: String,
}

impl Ledger {
    fn new(c: &Config) -> Self {
        Self {
            path: root(c).join("usage.json"),
            date: c.session.to_string(),
        }
    }

    fn read(&self) -> Result<Spend> {
        let spend = match std::fs::read(&self.path) {
            Ok(bytes) => {
                serde_json::from_slice::<Spend>(&bytes).map_err(|_| "agent_usage_corrupt")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Spend::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(if spend.date == self.date {
            spend
        } else {
            Spend {
                date: self.date.clone(),
                ..Spend::default()
            }
        })
    }

    /// Apply one change under the ledger lock. Crashed turns are charged
    /// first, so no reader sees an allowance that forgot them.
    fn update<T>(&self, change: impl FnOnce(&mut Spend) -> T) -> Result<T> {
        let parent = self.path.parent().ok_or("agent_usage_path")?;
        private_dir(parent)?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(parent.join("usage.lock"))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        let mut spend = self.read()?;
        let crashed: Vec<String> = spend
            .in_flight
            .iter()
            .filter(|(_, turn)| !Path::new(&format!("/proc/{}", turn.pid)).exists())
            .map(|(id, _)| id.clone())
            .collect();
        for id in crashed {
            if let Some(turn) = spend.in_flight.remove(&id) {
                add(spend.lanes.entry(turn.lane).or_default(), &turn.usage);
            }
        }
        let value = change(&mut spend);
        rollover::atomic(&self.path, &serde_json::to_value(&spend)?)?;
        Ok(value)
    }

    fn spent(&self, lane: Lane) -> Result<Usage> {
        self.update(|spend| spend.lanes.get(&lane).cloned().unwrap_or_default())
    }

    fn checkpoint(&self, turn_id: &str, lane: Lane, usage: &Usage) -> Result<()> {
        self.update(|spend| {
            spend.in_flight.insert(
                turn_id.to_owned(),
                InFlight {
                    lane,
                    pid: std::process::id(),
                    usage: usage.clone(),
                },
            );
        })
    }

    fn settle(&self, turn_id: &str, lane: Lane, usage: &Usage) -> Result<()> {
        self.update(|spend| {
            spend.in_flight.remove(turn_id);
            add(spend.lanes.entry(lane).or_default(), usage);
        })
    }
}

/// A finished turn plus the last plan it submitted, if any.
pub(crate) struct LaneTurn {
    pub outcome: TurnOutcome,
    pub plan: Option<PlanAttempt>,
    pub verdict: Option<Verdict>,
}

pub(crate) struct LaneRunner<'a> {
    pub agent: &'a mut Agent,
    pub settings: &'a AgentSettings,
    pub c: &'a Config,
    pub cfg: &'a PlannerConfig,
    pub clock: &'a dyn Fn() -> i64,
    /// Set while a decision turn waits; a running research or review turn yields.
    pub preempt: Arc<AtomicBool>,
    /// Overrides every lane's model, for offline checks against the fake provider.
    pub model_override: Option<ModelSpec>,
    pub requests: Requests,
    sequence: u64,
}

pub(crate) fn private_dir(path: &Path) -> Result<()> {
    Ok(std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?)
}

fn add(total: &mut Usage, usage: &Usage) {
    total.input += usage.input;
    total.output += usage.output;
    total.cache_read += usage.cache_read;
    total.cache_write += usage.cache_write;
    total.cost_usd += usage.cost_usd;
    total.calls += usage.calls;
}

fn bounded(text: &str, max: usize) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .take(max)
        .collect()
}

/// A queued research answer must fit the lane message bounds and stay free of
/// credential-shaped text; anything else is left to the artifact.
pub(crate) fn summary(text: &str) -> String {
    let bounded = bounded(text, MAX_SUMMARY_CHARS);
    if bounded.trim().is_empty() {
        "The research turn returned no text.".into()
    } else if crate::runtime::transcript::redact(&json!(bounded)).as_str() != Some(&bounded) {
        "Research summary withheld; read the artifact.".into()
    } else {
        bounded
    }
}

pub(crate) fn root(c: &Config) -> PathBuf {
    c.directory.join("model").join("agent")
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ReviewMemory {
    pub session: NaiveDate,
    pub turn_id: String,
    pub text: String,
}

pub(crate) fn sealed_review_source(c: &Config, directory: &Path, session: NaiveDate) -> bool {
    #[derive(Deserialize)]
    struct Report {
        account: String,
        session: NaiveDate,
        broker: String,
        paper_only: bool,
        live_enabled: bool,
        portfolio: Closed,
    }

    #[derive(Deserialize)]
    struct Closed {
        closed: bool,
    }

    let report = crate::read_bounded(&directory.join("report.json"), 16_000_000)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Report>(&bytes).ok());
    report.is_some_and(|report| {
        report.account == c.account
            && report.session == session
            && report.broker == "upstox"
            && report.paper_only
            && !report.live_enabled
            && report.portfolio.closed
    })
}

/// Marker of the session's review turn.
pub(crate) const REVIEW_MARKER: &str = "review-claimed.json";
/// Marker of the session's after-close research turn.
pub(crate) const AFTER_CLOSE_MARKER: &str = "research-after-close.json";

/// Claim a once-a-session turn. The marker is created durably before the turn
/// starts, so a restart never runs the turn twice; `false` means it ran.
pub(crate) fn claim(c: &Config, name: &str) -> Result<bool> {
    let directory = root(c);
    private_dir(&directory)?;
    let marker = directory.join(name);
    let mut file = match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(marker)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    serde_json::to_writer(&mut file, &c.session)?;
    file.sync_all()?;
    std::fs::File::open(&directory)?.sync_all()?;
    Ok(true)
}

fn read_memory(c: &Config, directory: &Path, session: NaiveDate) -> Option<ReviewMemory> {
    let path = directory.join("model/agent/memory.json");
    let bytes = crate::read_bounded(&path, 64_000).ok()?;
    let mut memory: ReviewMemory = serde_json::from_slice(&bytes).ok()?;
    if memory.session != session
        || !memory.turn_id.starts_with("review-")
        || !super::super::model_history::valid_id(&memory.turn_id)
        || !sealed_review_source(c, directory, session)
    {
        return None;
    }
    let safe = crate::runtime::transcript::redact(&json!(bounded(&memory.text, MAX_MEMORY_CHARS)));
    memory.text = safe.as_str()?.to_owned();
    (!memory.text.trim().is_empty()).then_some(memory)
}

pub(crate) fn memory(c: &Config, lane: Lane) -> Option<ReviewMemory> {
    if lane == Lane::Research
        && let Some(memory) = read_memory(c, &c.directory, c.session)
    {
        return Some(memory);
    }
    let prior = c.previous_session;
    if prior >= c.session {
        return None;
    }
    let dated = rollover::root(c)
        .ok()
        .map(|root| root.join(prior.to_string()));
    dated
        .as_deref()
        .and_then(|directory| read_memory(c, directory, prior))
        .or_else(|| read_memory(c, c.legacy_directory.as_deref()?, prior))
}

fn model_id(model: &ModelSpec) -> String {
    format!("{}/{}", model.provider, model.id)
        .chars()
        .take(128)
        .collect()
}

impl<'a> LaneRunner<'a> {
    pub(crate) fn new(
        agent: &'a mut Agent,
        settings: &'a AgentSettings,
        c: &'a Config,
        cfg: &'a PlannerConfig,
        clock: &'a dyn Fn() -> i64,
    ) -> Self {
        Self {
            agent,
            settings,
            c,
            cfg,
            clock,
            preempt: Arc::new(AtomicBool::new(false)),
            model_override: None,
            requests: Requests::new(c, &settings.triggers().research),
            sequence: 0,
        }
    }

    fn root(&self) -> PathBuf {
        root(self.c)
    }

    fn journal(&self, line: &Value) -> Result<()> {
        private_dir(&self.root())?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.root().join("turns.ndjson"))?;
        writeln!(file, "{line}")?;
        Ok(file.sync_all()?)
    }

    fn next_id(&mut self, lane: Lane) -> String {
        self.sequence += 1;
        format!(
            "{}-{}-{}-{}",
            lane.as_str(),
            (self.clock)(),
            std::process::id(),
            self.sequence
        )
    }

    fn model(&self, lane: Lane) -> Result<ModelSpec> {
        Ok(self
            .model_override
            .clone()
            .unwrap_or(self.settings.lane(lane)?.model.clone()))
    }

    /// Run one lane turn. The lane grants its tools' authority; usage is
    /// charged to the lane's daily allowance as pi reports it.
    pub(crate) fn turn(
        &mut self,
        lane: Lane,
        trigger: Value,
        brief: Value,
        context: &mut TurnContext,
    ) -> Result<LaneTurn> {
        let id = self.next_id(lane);
        self.turn_as(id, lane, trigger, brief, context, None)
    }

    fn turn_as(
        &mut self,
        id: String,
        lane: Lane,
        trigger: Value,
        brief: Value,
        context: &mut TurnContext,
        research_request: Option<&LaneMessage>,
    ) -> Result<LaneTurn> {
        let now = (self.clock)();
        let lane_settings = self.settings.lane(lane)?.clone();
        let ledger = Ledger::new(self.c);
        let spent = ledger.spent(lane)?;
        let model = self.model(lane)?;
        let guard = CallGuard {
            book: self.c,
            policy: self.cfg,
            clock: self.clock,
        };
        let archive = self.root().join("turns").join(&id);
        let checkpoints = Ledger::new(self.c);
        let (turn_id, model_id) = (id.clone(), model_id(&model));
        let requests_served =
            lane == Lane::Decision && self.settings.lanes.contains_key(&Lane::Research);
        let mut host = LaneHost {
            lane,
            clock: self.clock,
            bridge: Bridge::new(
                self.c,
                self.cfg,
                &context.bundle,
                context.backend.as_mut(),
                &guard,
                &archive,
                &id,
                LaneGrant {
                    workspace: match lane {
                        Lane::Research => WorkspaceScope::ResearchSandbox,
                        Lane::Decision | Lane::Review | Lane::Gate => WorkspaceScope::Decision,
                    },
                    model_id: &model_id,
                    requests: &self.requests,
                },
                context.web.as_mut(),
            )?,
            plan_inputs: (lane == Lane::Decision)
                .then_some((&context.portfolio, context.instruments.as_slice())),
            plan: None,
            verdict: None,
            on_usage: Some(Box::new(move |usage: &Usage| {
                // A failed checkpoint only loses crash accounting; the turn's
                // final usage is still charged when it settles.
                let _ = checkpoints.checkpoint(&turn_id, lane, usage);
            })),
        };
        let request = TurnRequest {
            id: id.clone(),
            lane,
            session: SessionRef {
                date: self.c.session.to_string(),
                dir: self
                    .root()
                    .join("pi")
                    .join(lane.as_str())
                    .to_string_lossy()
                    .into_owned(),
            },
            model,
            // The offline fake answers every turn; it never needs a fallback.
            fallback: match self.model_override {
                Some(_) => None,
                None => lane_settings.fallback.clone(),
            },
            trigger: trigger.clone(),
            brief,
            tools: lane_definitions(self.cfg, lane, requests_served)?,
            compact_at_tokens: lane_settings.compact_at_tokens,
        };
        let allowance = Allowance {
            calls: lane_settings.daily_calls,
            cost_usd: lane_settings.daily_cost_usd,
            max_tool_calls: lane_settings.max_tool_calls,
            timeout: Duration::from_millis(lane_settings.turn_timeout_ms),
        };
        let preempt = self.preempt.clone();
        let result = self
            .agent
            .run_turn(now, &request, &mut host, &spent, allowance, &|| {
                preempt.load(Ordering::Acquire)
            });
        let plan = host.plan.take();
        let verdict = host.verdict.take();
        drop(host);
        let mut outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                self.journal(&json!({"turn_id":id,"lane":lane,"trigger":trigger,"at_ms":now,"status":"refused","error":error.to_string()}))?;
                return Err(error);
            }
        };
        ledger.settle(&id, lane, &outcome.usage)?;
        let mut events = Vec::new();
        for event in &outcome.events {
            events.extend_from_slice(
                serde_json::to_string(&crate::runtime::transcript::redact(event))?.as_bytes(),
            );
            events.push(b'\n');
        }
        private_dir(&archive)?;
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(archive.join("events.ndjson"))?
            .write_all(&events)?;
        if let Some(request) = research_request
            && outcome.status == TurnStatus::Ok
            && self
                .requests
                .answer(
                    request,
                    &id,
                    &outcome.text,
                    &json!(outcome.usage),
                    (self.clock)(),
                )
                .is_err()
        {
            outcome.status = TurnStatus::Error;
            outcome.error = Some("agent_research_answer_failed".into());
        }
        let mut line = json!({
            "turn_id":id,"lane":lane,"trigger":trigger,"at_ms":now,
            "status":outcome.status,"error":outcome.error,"model":outcome.model,"provider":outcome.provider,
            "usage":outcome.usage,"session_file":outcome.session_file,"prompt_sha256":outcome.prompt_sha256,
            "tool_calls":outcome.tool_calls,"plan":plan,"verdict":verdict,"elapsed_ms":outcome.elapsed_ms,
            "restarts":self.agent.restarts(),"text_sha256":digest(outcome.text.as_bytes())
        });
        if let Some(fallback) = &outcome.fallback {
            line["fallback"] = json!(fallback);
            eprintln!(
                "agent_fallback turn={id} lane={} from={}/{} to={}/{} reason={}",
                lane.as_str(),
                fallback.from.provider,
                fallback.from.id,
                fallback.to.provider,
                fallback.to.id,
                json!(fallback.reason).as_str().unwrap_or_default(),
            );
        }
        self.journal(&crate::runtime::transcript::redact(&line))?;
        Ok(LaneTurn {
            outcome,
            plan,
            verdict,
        })
    }

    /// A decision turn. Its brief carries undelivered research answers; they
    /// are marked delivered once the turn completed.
    pub(crate) fn decision(
        &mut self,
        trigger: Value,
        brief: Brief,
        context: &mut TurnContext,
    ) -> Result<LaneTurn> {
        let id = self.next_id(Lane::Decision);
        self.decision_as(id, trigger, brief, context)
    }

    pub(super) fn decision_as(
        &mut self,
        id: String,
        trigger: Value,
        mut brief: Brief,
        context: &mut TurnContext,
    ) -> Result<LaneTurn> {
        let now = (self.clock)();
        let updates = if self.settings.lanes.contains_key(&Lane::Research) {
            self.requests.updates(now)?
        } else {
            Vec::new()
        };
        brief.research_updates = updates
            .iter()
            .map(crate::runtime::lane_queue::update_fact)
            .collect();
        let turn = self.turn_as(
            id,
            Lane::Decision,
            trigger,
            json!(brief.render()?),
            context,
            None,
        )?;
        if turn.outcome.status == TurnStatus::Ok {
            let ids: Vec<String> = updates.into_iter().map(|update| update.id).collect();
            self.requests
                .mark_delivered(&ids, &turn.outcome.id, (self.clock)())?;
        }
        Ok(turn)
    }

    /// Answer queued research requests, one research turn each. A finished
    /// turn posts its answer for the next decision brief; any other ending
    /// fails the request, which is never run again. Requests a crashed pass
    /// left running are failed first.
    pub(crate) fn research_requests(
        &mut self,
        trigger: &Value,
        brief: &Brief,
        context: &mut TurnContext,
    ) -> Result<Vec<LaneTurn>> {
        let now = (self.clock)();
        self.requests.recover(now)?;
        let queued = self.requests.queued(now, MAX_REQUESTS_PER_PASS)?;
        let mut turns = Vec::new();
        for request in queued {
            if self.preempt.load(Ordering::Acquire) {
                break;
            }
            let MessageBody::ResearchRequest { question, refs } = &request.body else {
                continue;
            };
            let id = self.next_id(Lane::Research);
            if !self.requests.claim(&request.id, &id, (self.clock)())? {
                continue;
            }
            let mut brief = brief.clone();
            brief.task =
                json!({"kind":"request","request_id":request.id,"question":question,"refs":refs});
            let turn = brief.render().and_then(|text| {
                self.turn_as(
                    id,
                    Lane::Research,
                    trigger.clone(),
                    json!(text),
                    context,
                    Some(&request),
                )
            });
            let at = (self.clock)();
            match turn {
                Ok(turn) => {
                    if turn.outcome.status != TurnStatus::Ok {
                        let reason = if turn.outcome.error.as_deref() == Some("agent_preempted") {
                            TransitionReason::Interrupted
                        } else {
                            TransitionReason::TurnFailed
                        };
                        self.requests.fail(&request.id, reason, at)?;
                    }
                    turns.push(turn);
                }
                Err(error) => {
                    self.requests
                        .fail(&request.id, TransitionReason::TurnFailed, at)?;
                    return if turns.is_empty() {
                        Err(error)
                    } else {
                        Ok(turns)
                    };
                }
            }
        }
        Ok(turns)
    }

    /// A review turn after the seal. Its text becomes the memory the next
    /// decision brief carries.
    pub(crate) fn review(
        &mut self,
        trigger: Value,
        brief: &Brief,
        context: &mut TurnContext,
    ) -> Result<LaneTurn> {
        let turn = self.turn(Lane::Review, trigger, json!(brief.render()?), context)?;
        if turn.outcome.status == TurnStatus::Ok && !turn.outcome.text.trim().is_empty() {
            let text = crate::runtime::transcript::redact(&json!(bounded(
                &turn.outcome.text,
                MAX_MEMORY_CHARS
            )));
            rollover::atomic(
                &self.root().join("memory.json"),
                &json!({"session":self.c.session.to_string(),"turn_id":turn.outcome.id,"text":text}),
            )?;
        }
        Ok(turn)
    }
}

/// Today's spend per lane for the controller's budget reads, counting turns
/// still in flight. Read only.
pub(crate) fn spend(c: &Config) -> BTreeMap<Lane, Usage> {
    read_spend(c).unwrap_or_default()
}

pub(crate) fn read_spend(c: &Config) -> Result<BTreeMap<Lane, Usage>> {
    let mut spend = Ledger::new(c).read()?;
    for turn in spend.in_flight.values() {
        add(spend.lanes.entry(turn.lane).or_default(), &turn.usage);
    }
    Ok(spend.lanes)
}

/// The fields of one `turns.ndjson` line that readiness reports as evidence.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct JournaledTurn {
    pub turn_id: String,
    pub lane: Lane,
    pub at_ms: i64,
    /// `ok`, `error`, `aborted`, or `refused` when the turn never ran.
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub fallback: Option<super::protocol::Fallback>,
}

const MAX_TURN_JOURNAL_BYTES: u64 = 16_000_000;

/// Each lane's latest journaled turn this session. The journal is appended in
/// turn order, so the last line per lane wins. A missing journal holds no
/// turns; an unreadable or malformed one is an error, never an empty map. A
/// final line without its newline is a write a crash cut off and is not
/// evidence.
pub(crate) fn latest_turns(c: &Config) -> Result<BTreeMap<Lane, JournaledTurn>> {
    let path = root(c).join("turns.ndjson");
    let bytes = match crate::read_bounded(&path, MAX_TURN_JOURNAL_BYTES) {
        Ok(bytes) => bytes,
        Err(_) if !path.exists() => return Ok(BTreeMap::new()),
        Err(_) => return Err("agent_turns_unreadable".into()),
    };
    let mut latest = BTreeMap::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !line.ends_with(b"\n") {
            break;
        }
        let turn: JournaledTurn =
            serde_json::from_slice(line).map_err(|_| "agent_turns_unreadable")?;
        let day = turn
            .at_ms
            .checked_add(19_800_000)
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|time| time.date_naive());
        if day == Some(c.session) {
            latest.insert(turn.lane, turn);
        }
    }
    Ok(latest)
}
