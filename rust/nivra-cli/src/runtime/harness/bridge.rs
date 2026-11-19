//! Executes pi agent tool calls through the harness's own validation and
//! executors, under the tools' existing Rust names.
use super::*;
use std::os::unix::fs::OpenOptionsExt;

/// A bounded reason code safe to hand back to a model.
pub(crate) fn failure_reason(error: &(dyn std::error::Error + 'static)) -> String {
    tool_failure_reason(error)
}

/// The research requests one lane set posts and answers. Ids are the queue's
/// message ids, which `research_status` and `research_result` also accept.
pub(crate) trait LaneRequests {
    fn post(&self, turn_id: &str, question: &str, refs: Vec<String>, now: i64) -> Result<Value>;
    /// `None` when `id` is not a request of this lane set.
    fn status(&self, id: &str) -> Result<Option<Value>>;
    fn result(&self, id: &str) -> Result<Option<Value>>;
    fn cancel(&self, id: &str, now: i64) -> Result<Option<Value>>;
}

/// What a lane turn may do, decided by the lane itself. It replaces the
/// harness's role-and-bundle bindings for every tool whose authority they gate.
pub(crate) struct LaneGrant<'a> {
    pub workspace: super::workspace::WorkspaceScope,
    /// Model identity recorded on the research tasks this turn starts.
    pub model_id: &'a str,
    pub requests: &'a dyn LaneRequests,
}

/// One lane turn's tool executor.
pub(crate) struct Bridge<'a> {
    pub c: &'a Config,
    pub cfg: &'a PlannerConfig,
    pub bundle: &'a Value,
    pub backend: &'a mut dyn Backend,
    pub guard: &'a CallGuard<'a>,
    pub archive: &'a Path,
    pub run_id: &'a str,
    grant: LaneGrant<'a>,
    definitions: Vec<Value>,
    limits: ToolsSettings,
    web_budget: super::web_tools::WebToolBudget,
    web_transport: &'a mut dyn super::web_tools::Transport,
    seen: BTreeSet<String>,
}

impl<'a> Bridge<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        c: &'a Config,
        cfg: &'a PlannerConfig,
        bundle: &'a Value,
        backend: &'a mut dyn Backend,
        guard: &'a CallGuard<'a>,
        archive: &'a Path,
        run_id: &'a str,
        grant: LaneGrant<'a>,
        web_transport: &'a mut dyn super::web_tools::Transport,
    ) -> Result<Self> {
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(archive)?;
        Ok(Self {
            c,
            cfg,
            bundle,
            backend,
            guard,
            archive,
            run_id,
            grant,
            definitions: bridge_definitions(cfg, true)?,
            limits: cfg.effective_tools(),
            web_budget: Default::default(),
            web_transport,
            seen: BTreeSet::new(),
        })
    }

    /// Validate and run one call. Errors are bounded reason codes for the model.
    pub(crate) fn call(
        &mut self,
        call_id: &str,
        name: &str,
        args: &Value,
    ) -> std::result::Result<Value, String> {
        self.run(call_id, name, args)
            .map_err(|error| tool_failure_reason(error.as_ref()))
    }

    fn run(&mut self, call_id: &str, name: &str, args: &Value) -> Result<Value> {
        let item = json!({"type":"function_call","call_id":call_id,"name":name,"arguments":args.to_string()});
        let (id, tool) = checked_call_with(&item, &self.definitions, &self.limits)?;
        if !self.seen.insert(id.clone()) {
            return Err("harness_duplicate_call_no_replay".into());
        }
        self.guard.check()?;
        // A durable claim precedes any tool effect. No automatic replay.
        let key = digest(id.as_bytes());
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(self.archive.join(format!("tool-{key}.claim")))?
            .sync_all()?;
        let value = match self.lane_tool(&id, tool)? {
            Ok(value) => value,
            Err(tool) => execute_tool(
                &mut ToolScope {
                    c: self.c,
                    bundle: self.bundle,
                    backend: &mut *self.backend,
                    guard: self.guard,
                    archive: self.archive,
                    tool_limits: &self.limits,
                },
                &key,
                tool,
            )?,
        };
        if serde_json::to_string(&value)?.len() > TOOL_OUTPUT_MAX_BYTES {
            return Err("harness_tool_output_cap".into());
        }
        rollover::atomic(
            &self.archive.join(format!("tool-{key}.result.json")),
            &value,
        )?;
        Ok(value)
    }

    /// The tools whose authority the lane grants. Research tasks carry no
    /// decision-owner evidence: a lane turn is not an owner claim and never
    /// fabricates one. Every other tool goes back to `execute_tool`.
    fn lane_tool(&mut self, id: &str, tool: Tool) -> Result<std::result::Result<Value, Tool>> {
        let (c, cfg, turn) = (self.c, self.cfg, self.run_id);
        let now = DomainClock::new(self.guard).wall_ms();
        let model = self.grant.model_id;
        let requests = self.grant.requests;
        let candidate = |operation: Result<Value>| {
            if candidate_tools_enabled(cfg) {
                operation
            } else {
                Err("harness_candidate_compute_disabled".into())
            }
        };
        Ok(Ok(match tool {
            Tool::ResearchRequest(args) => requests.post(turn, &args.question, args.refs, now)?,
            Tool::ResearchEnqueue(args) => research::task_enqueue_with_evidence(
                c,
                cfg,
                model,
                0,
                turn,
                &args.task_key,
                &args.goal,
                &args.analysis_id,
                &args.dataset_id,
                now,
                None,
            )?,
            Tool::ResearchTaskStatus(task_id, revision) => match requests.status(&task_id)? {
                Some(request) => request,
                None => research::research_task_status(c, cfg, &task_id, revision, now)?,
            },
            Tool::ResearchTaskResult(task_id, revision) => match requests.result(&task_id)? {
                Some(answer) => answer,
                None => research::research_task_result(c, cfg, &task_id, revision, now)?,
            },
            Tool::ResearchTaskCancel(task_id, revision) => match requests.cancel(&task_id, now)? {
                Some(request) => request,
                None => research::research_task_cancel(c, cfg, &task_id, revision, now)?,
            },
            Tool::CandidateSubmit(args) => candidate(research::candidate_submit(
                c, cfg, model, 0, turn, args, now, None,
            ))?,
            Tool::CandidateStatus(task_id, revision) => {
                candidate(research::candidate_status(c, cfg, &task_id, revision, now))?
            }
            Tool::CandidateResult(task_id, revision, reference) => candidate(match reference {
                Some(raw) => {
                    let reference: Value = serde_json::from_str(&raw)
                        .map_err(|_| "harness_owner_research_args_invalid")?;
                    research::candidate_result_for_observation_ref(c, cfg, &reference, now)
                }
                None => research::candidate_result(
                    c,
                    cfg,
                    task_id
                        .as_deref()
                        .ok_or("harness_owner_research_args_invalid")?,
                    revision.ok_or("harness_owner_research_args_invalid")?,
                    now,
                ),
            })?,
            Tool::CandidateCancel(task_id, revision) => {
                candidate(research::candidate_cancel(c, cfg, &task_id, revision, now))?
            }
            Tool::QuoteCapture(keys, from_ms, to_ms) => {
                research_quote_capture(c, cfg, keys, from_ms, to_ms, now)?
            }
            Tool::Workspace(op) => super::workspace::execute_scoped(
                &c.directory,
                cfg.workspace.as_ref().ok_or("harness_workspace_disabled")?,
                self.grant.workspace,
                op,
            )?,
            Tool::Web(op) => {
                let settings = cfg
                    .web_tools
                    .as_ref()
                    .filter(|web| web.enabled)
                    .ok_or("harness_web_tools_disabled")?;
                super::web_tools::execute_with(
                    &mut *self.web_transport,
                    settings,
                    &mut self.web_budget,
                    &super::web_tools::CallContext {
                        session: c.session.to_string(),
                        run: turn.to_string(),
                        turn: self.seen.len(),
                        call: id.to_owned(),
                    },
                    op,
                    now,
                    Some(&c.directory.join("model")),
                )?
            }
            other => return Ok(Err(other)),
        }))
    }
}
