//! Lane turns: binding, budgets, journal and the research request loop.
use super::client::{AgentClient, Allowance, TurnOutcome, TurnRequest};
use super::protocol::{SessionRef, TurnStatus, Usage};
use super::tools::{LaneHost, lane_definitions};
use super::{AgentSettings, Lane};
use crate::runtime::harness::Bridge;
use crate::runtime::lane_queue::{self, MessageBody, MessageKind, MessageState, TransitionReason};
use crate::runtime::orchestrator_model::Trigger;
use crate::runtime::planner::{Backend, CallGuard, PlannerConfig, preview_guard};
use crate::runtime::web_tools::Transport;
use crate::runtime::{Config, Result, digest, rollover};
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

/// What a lane turn's tools read. The controller builds it in Phase 2;
/// agent-check and tests build it from fixtures.
pub(crate) struct TurnContext {
    /// Frozen evidence the read tools serve, without a lane binding.
    pub bundle: Value,
    pub backend: Box<dyn Backend>,
    pub portfolio: Portfolio,
    pub instruments: Vec<Instrument>,
    /// HTTP for the web tools; tests pass a fake so no socket opens.
    pub web: Box<dyn Transport>,
}

#[derive(Default, Serialize, Deserialize)]
struct Spend {
    date: String,
    lanes: BTreeMap<Lane, Usage>,
}

/// A finished turn plus the plan Rust validated in it, if any.
pub(crate) struct LaneTurn {
    pub outcome: TurnOutcome,
    pub plan: Option<Value>,
}

pub(crate) struct LaneRunner<'a> {
    pub client: &'a mut AgentClient,
    pub settings: &'a AgentSettings,
    pub c: &'a Config,
    pub cfg: &'a PlannerConfig,
    pub clock: &'a dyn Fn() -> i64,
    /// Set while a decision turn waits; a running research or review turn yields.
    pub preempt: Arc<AtomicBool>,
    /// Overrides every lane's model, for offline checks against the fake provider.
    pub model_override: Option<super::ModelSpec>,
    sequence: u64,
}

fn private_dir(path: &Path) -> Result<()> {
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

/// A queued research answer must fit the lane message bounds and stay free of
/// credential-shaped text; anything else is left to the artifact.
fn summary(text: &str) -> String {
    let bounded: String = text
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .take(MAX_SUMMARY_CHARS)
        .collect();
    if bounded.trim().is_empty() {
        "The research turn returned no text.".into()
    } else if crate::runtime::transcript::redact(&json!(bounded)).as_str() != Some(&bounded) {
        "Research summary withheld; read the artifact.".into()
    } else {
        bounded
    }
}

impl<'a> LaneRunner<'a> {
    pub(crate) fn new(
        client: &'a mut AgentClient,
        settings: &'a AgentSettings,
        c: &'a Config,
        cfg: &'a PlannerConfig,
        clock: &'a dyn Fn() -> i64,
    ) -> Self {
        Self {
            client,
            settings,
            c,
            cfg,
            clock,
            preempt: Arc::new(AtomicBool::new(false)),
            model_override: None,
            sequence: 0,
        }
    }

    fn root(&self) -> PathBuf {
        self.c.directory.join("model").join("agent")
    }

    fn load_spend(&self) -> Result<Spend> {
        let path = self.root().join("usage.json");
        let date = self.c.session.to_string();
        let spend = match std::fs::read(&path) {
            Ok(bytes) => {
                serde_json::from_slice::<Spend>(&bytes).map_err(|_| "agent_usage_corrupt")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Spend::default(),
            Err(error) => return Err(error.into()),
        };
        Ok(if spend.date == date {
            spend
        } else {
            Spend {
                date,
                lanes: BTreeMap::new(),
            }
        })
    }

    fn journal(&self, line: &Value) -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.root().join("turns.ndjson"))?;
        writeln!(file, "{line}")?;
        Ok(file.sync_all()?)
    }

    /// Run one lane turn. The bundle gets the lane binding the existing tool
    /// authority checks read; usage is charged to the lane's daily allowance.
    pub(crate) fn turn(
        &mut self,
        lane: Lane,
        trigger: Value,
        brief: Value,
        context: &mut TurnContext,
    ) -> Result<LaneTurn> {
        let now = (self.clock)();
        self.sequence += 1;
        let id = format!("{}-{now}-{}", lane.as_str(), self.sequence);
        let lane_settings = self.settings.lane(lane)?;
        private_dir(&self.root())?;
        let mut spend = self.load_spend()?;
        let spent = spend.lanes.get(&lane).cloned().unwrap_or_default();
        let mut bundle = context.bundle.clone();
        if lane == Lane::Decision {
            bundle["trigger"]["kind"] = json!("decision");
            bundle["summary"]["host_decision"] = json!({"turn_id":id});
        } else {
            // Review reads through the research binding, narrowed by its allowlist.
            bundle["trigger"]["kind"] = json!("research");
            bundle["summary"]["owner_task"] = json!({"kind":"research"});
            bundle["summary"]["turn_id"] = json!(id);
            bundle["summary"]["research_id"] = json!(id);
        }
        let role = self.cfg.role(Trigger::Supervisor);
        let guard = CallGuard {
            invocation_id: Some(&id),
            configuration: Some((self.c, self.cfg)),
            clock: self.clock,
            ..preview_guard()
        };
        let archive = self.root().join("turns").join(&id);
        let mut host = LaneHost {
            lane,
            bridge: Bridge::new(
                self.c,
                self.cfg,
                &role,
                &bundle,
                context.backend.as_mut(),
                &guard,
                &archive,
                &id,
                context.web.as_mut(),
            )?,
            plan_inputs: (lane == Lane::Decision)
                .then_some((&context.portfolio, context.instruments.as_slice())),
            plan: None,
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
            model: self
                .model_override
                .clone()
                .unwrap_or_else(|| lane_settings.model.clone()),
            trigger: trigger.clone(),
            brief,
            tools: lane_definitions(self.cfg, lane)?,
        };
        let allowance = Allowance {
            calls: lane_settings.daily_calls,
            cost_usd: lane_settings.daily_cost_usd,
            max_tool_calls: lane_settings.max_tool_calls,
            timeout: Duration::from_millis(lane_settings.turn_timeout_ms),
        };
        let preempt = self.preempt.clone();
        let yields = lane != Lane::Decision;
        let result = self
            .client
            .run_turn(&request, &mut host, &spent, allowance, &|| {
                yields && preempt.load(Ordering::Acquire)
            });
        let plan = host.plan.take();
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                self.journal(&json!({"turn_id":id,"lane":lane,"trigger":trigger,"at_ms":now,"status":"refused","error":error.to_string()}))?;
                return Err(error);
            }
        };
        add(spend.lanes.entry(lane).or_default(), &outcome.usage);
        rollover::atomic(
            &self.root().join("usage.json"),
            &serde_json::to_value(&spend)?,
        )?;
        let mut events = Vec::new();
        for event in &outcome.events {
            events.extend_from_slice(serde_json::to_string(event)?.as_bytes());
            events.push(b'\n');
        }
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(archive.join("events.ndjson"))?
            .write_all(&events)?;
        self.journal(&json!({
            "turn_id":id,"lane":lane,"trigger":trigger,"at_ms":now,
            "status":outcome.status,"error":outcome.error,"model":outcome.model,"provider":outcome.provider,
            "usage":outcome.usage,"session_file":outcome.session_file,"prompt_sha256":outcome.prompt_sha256,
            "tool_calls":outcome.tool_calls,"plan":plan,"elapsed_ms":outcome.elapsed_ms,
            "text_sha256":digest(outcome.text.as_bytes())
        }))?;
        Ok(LaneTurn { outcome, plan })
    }

    /// A decision turn whose brief carries undelivered research answers; they
    /// are marked delivered once the turn completed.
    pub(crate) fn decision(
        &mut self,
        trigger: Value,
        facts: Value,
        context: &mut TurnContext,
    ) -> Result<LaneTurn> {
        let now = (self.clock)();
        let updates = if lane_queue::lane_settings(self.cfg).is_some() {
            lane_queue::trading_updates(self.c, now)?
        } else {
            Vec::new()
        };
        let brief = json!({
            "facts":facts,
            "research_updates":updates.iter().map(lane_queue::update_fact).collect::<Vec<_>>()
        });
        let turn = self.turn(Lane::Decision, trigger, brief, context)?;
        if turn.outcome.status == TurnStatus::Ok {
            let ids: Vec<String> = updates.into_iter().map(|update| update.id).collect();
            lane_queue::mark_delivered(self.c, &ids, &turn.outcome.id, (self.clock)())?;
        }
        Ok(turn)
    }

    /// Answer queued research requests, one research turn each. A finished
    /// turn posts its summary for the next decision brief; a failed or
    /// pre-empted turn fails the request, which the decision lane may repeat.
    pub(crate) fn research_requests(&mut self, context: &mut TurnContext) -> Result<Vec<LaneTurn>> {
        let lane = lane_queue::lane_settings(self.cfg).ok_or("research_lane_disabled")?;
        let (ttl_ms, max_open) = (lane.request_ttl_ms, lane.max_open_requests);
        let session = self.c.session.to_string();
        let mut db = lane_queue::open(&self.c.directory)?;
        let now = (self.clock)();
        lane_queue::expire(&mut db, &session, now)?;
        let queued = lane_queue::list(
            &db,
            &session,
            &[MessageKind::ResearchRequest],
            MessageState::Queued,
            now,
            MAX_REQUESTS_PER_PASS,
        )?;
        let mut turns = Vec::new();
        for request in queued {
            if self.preempt.load(Ordering::Acquire) {
                break;
            }
            let MessageBody::ResearchRequest { question, refs } = &request.body else {
                continue;
            };
            let claim = format!("research-claim-{}", request.id);
            if !lane_queue::transition(
                &mut db,
                &request.id,
                MessageState::Queued,
                MessageState::Running,
                TransitionReason::Claimed,
                Some(&claim),
                (self.clock)(),
            )? {
                continue;
            }
            let trigger = json!({"kind":"request","reason":"decision lane research request","request_id":request.id});
            let brief = json!({"request_id":request.id,"question":question,"refs":refs});
            let turn = self.turn(Lane::Research, trigger, brief, context);
            let at = (self.clock)();
            match turn {
                Ok(turn) if turn.outcome.status == TurnStatus::Ok => {
                    let artifact = format!("agent/research/{}.json", request.id);
                    let path = self.c.directory.join("model").join(&artifact);
                    private_dir(path.parent().ok_or("agent_research_path")?)?;
                    rollover::atomic(
                        &path,
                        &json!({"request_id":request.id,"question":question,"turn_id":turn.outcome.id,
                            "text":turn.outcome.text,"usage":turn.outcome.usage,"model":turn.outcome.model}),
                    )?;
                    let body = MessageBody::ResearchResult {
                        request_id: Some(request.id.clone()),
                        research_id: turn.outcome.id.clone(),
                        summary: summary(&turn.outcome.text),
                        artifact_ref: artifact,
                        output_sha256: digest(turn.outcome.text.as_bytes()),
                    };
                    lane_queue::post(&mut db, &session, &request.id, &body, at, ttl_ms, max_open)?;
                    lane_queue::transition(
                        &mut db,
                        &request.id,
                        MessageState::Running,
                        MessageState::Done,
                        TransitionReason::Completed,
                        Some(&turn.outcome.id),
                        at,
                    )?;
                    turns.push(turn);
                }
                other => {
                    let reason = match &other {
                        Ok(turn) if turn.outcome.error.as_deref() == Some("agent_preempted") => {
                            TransitionReason::Interrupted
                        }
                        _ => TransitionReason::TurnFailed,
                    };
                    lane_queue::transition(
                        &mut db,
                        &request.id,
                        MessageState::Running,
                        MessageState::Failed,
                        reason,
                        None,
                        at,
                    )?;
                    turns.extend(other.ok());
                }
            }
        }
        Ok(turns)
    }
}
