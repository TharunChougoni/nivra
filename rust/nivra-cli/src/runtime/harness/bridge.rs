//! Executes pi agent tool calls through the harness's own validation and
//! executors, under the tools' existing Rust names.
use super::*;
use std::os::unix::fs::OpenOptionsExt;

/// A bounded reason code safe to hand back to a model.
pub(crate) fn failure_reason(error: &(dyn std::error::Error + 'static)) -> String {
    tool_failure_reason(error)
}

/// One lane turn's tool executor. The bundle's trigger and summary carry the
/// lane binding that the existing authority checks read.
pub(crate) struct Bridge<'a> {
    pub c: &'a Config,
    pub cfg: &'a PlannerConfig,
    pub role: &'a ModelRole,
    pub bundle: &'a Value,
    pub backend: &'a mut dyn Backend,
    pub guard: &'a CallGuard<'a>,
    pub archive: &'a Path,
    pub run_id: &'a str,
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
        role: &'a ModelRole,
        bundle: &'a Value,
        backend: &'a mut dyn Backend,
        guard: &'a CallGuard<'a>,
        archive: &'a Path,
        run_id: &'a str,
        web_transport: &'a mut dyn super::web_tools::Transport,
    ) -> Result<Self> {
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(archive)?;
        Ok(Self {
            c,
            cfg,
            role,
            bundle,
            backend,
            guard,
            archive,
            run_id,
            definitions: bridge_definitions(cfg)?,
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
        let value = execute_tool(
            &mut ToolScope {
                c: self.c,
                cfg: self.cfg,
                name: "supervisor",
                role: self.role,
                bundle: self.bundle,
                backend: &mut *self.backend,
                guard: self.guard,
                archive: self.archive,
                web_budget: &mut self.web_budget,
                web_transport: &mut *self.web_transport,
                run_id: self.run_id,
                turn: self.seen.len(),
                tool_limits: &self.limits,
            },
            &id,
            &key,
            tool,
        )?;
        if serde_json::to_string(&value)?.len() > TOOL_OUTPUT_MAX_BYTES {
            return Err("harness_tool_output_cap".into());
        }
        rollover::atomic(
            &self.archive.join(format!("tool-{key}.result.json")),
            &value,
        )?;
        Ok(value)
    }
}
