//! Rig research tools with no provider transport, model grant or trading authority.
//! The trusted host supplies a frozen catalog and the original reviewed job queue.
pub mod archive_bars;
pub mod bubblewrap;
pub mod drill;
pub mod strategy_sandbox;
pub mod worker;
use nivra_store::research_jobs::{self as jobs, Spec, State};
use rig_core::tool::PortableTool;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredAnalysis {
    pub id: String,
    pub script_hash: String,
    pub runtime_hash: String,
    pub memory_bytes: u64,
    pub output_bytes: u64,
    pub wall_millis: u32,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevelopmentSplit {
    Training,
    Development,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dataset {
    pub id: String,
    pub input_hash: String,
    pub split: DevelopmentSplit,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisArgs {
    pub experiment_id: String,
    pub analysis_id: String,
    pub dataset_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnalysisResult {
    pub job_id: String,
    pub state: State,
    pub execution_started: bool,
    pub promotion_authorized: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolError {
    Arguments,
    Catalog,
    Queue,
}
impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Arguments => "analysis_arguments_rejected",
            Self::Catalog => "analysis_catalog_unavailable",
            Self::Queue => "analysis_queue_unavailable",
        })
    }
}
impl std::error::Error for ToolError {}

pub struct AnalysisTool {
    review_id: String,
    queue: Arc<Mutex<Connection>>,
    analyses: BTreeMap<String, RegisteredAnalysis>,
    datasets: BTreeMap<String, Dataset>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}
fn alias(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl AnalysisTool {
    /// Only the trusted host can construct catalogs. Model arguments cannot add entries.
    pub fn bind(
        review_id: String,
        queue: Arc<Mutex<Connection>>,
        analyses: Vec<RegisteredAnalysis>,
        datasets: Vec<Dataset>,
        clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Result<Self, ToolError> {
        if !hash(&review_id)
            || analyses.is_empty()
            || analyses.len() > 32
            || datasets.is_empty()
            || datasets.len() > 32
        {
            return Err(ToolError::Catalog);
        }
        let mut registered = BTreeMap::new();
        let mut data = BTreeMap::new();
        for analysis in analyses {
            if !alias(&analysis.id)
                || !hash(&analysis.script_hash)
                || !hash(&analysis.runtime_hash)
                || registered.insert(analysis.id.clone(), analysis).is_some()
            {
                return Err(ToolError::Catalog);
            }
        }
        for dataset in datasets {
            if !alias(&dataset.id)
                || !hash(&dataset.input_hash)
                || data.insert(dataset.id.clone(), dataset).is_some()
            {
                return Err(ToolError::Catalog);
            }
        }
        Ok(Self {
            review_id,
            queue,
            analyses: registered,
            datasets: data,
            clock,
        })
    }
    /// Queue only. The model selects aliases; this never starts the worker.
    pub fn enqueue(&self, args: AnalysisArgs) -> Result<AnalysisResult, ToolError> {
        if !alias(&args.experiment_id) {
            return Err(ToolError::Arguments);
        }
        let analysis = self
            .analyses
            .get(&args.analysis_id)
            .ok_or(ToolError::Arguments)?;
        let dataset = self
            .datasets
            .get(&args.dataset_id)
            .ok_or(ToolError::Arguments)?;
        match dataset.split {
            DevelopmentSplit::Training | DevelopmentSplit::Development => {}
        }
        let identity=serde_json::to_vec(&serde_json::json!({"review":self.review_id,"experiment":args.experiment_id,"analysis":args.analysis_id,"dataset":args.dataset_id,"code":analysis.script_hash,"runtime":analysis.runtime_hash,"inputs":dataset.input_hash})).map_err(|_|ToolError::Arguments)?;
        let job_id: String = Sha256::digest(identity)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let spec = Spec {
            id: job_id.clone(),
            review_id: self.review_id.clone(),
            script_hash: analysis.script_hash.clone(),
            input_hash: dataset.input_hash.clone(),
            runtime_hash: analysis.runtime_hash.clone(),
            memory_bytes: analysis.memory_bytes,
            output_bytes: analysis.output_bytes,
            wall_millis: analysis.wall_millis,
            retry_safe_compute: true,
            max_attempts: 2,
            capability: jobs::SpecCapability::RegisteredAnalysis,
        };
        let mut queue = self.queue.lock().map_err(|_| ToolError::Queue)?;
        let job = jobs::enqueue(&mut queue, &spec, (self.clock)()).map_err(|_| ToolError::Queue)?;
        Ok(AnalysisResult {
            job_id,
            state: job.state,
            execution_started: false,
            promotion_authorized: false,
        })
    }
    pub(crate) fn connection(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.queue)
    }
}
impl PortableTool for AnalysisTool {
    const NAME: &'static str = "request_registered_analysis";
    type Args = AnalysisArgs;
    type Output = AnalysisResult;
    type Error = ToolError;
    fn description(&self) -> String {
        "Queue a preregistered analysis against a development dataset. Worker execution is separately gated; this tool never executes code or promotes a strategy.".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","additionalProperties":false,"required":["experiment_id","analysis_id","dataset_id"],"properties":{
            "experiment_id":{"type":"string","pattern":"^[a-z0-9_-]{1,64}$"},
            "analysis_id":{"type":"string","enum":self.analyses.keys().collect::<Vec<_>>()},
            "dataset_id":{"type":"string","enum":self.datasets.keys().collect::<Vec<_>>()}}})
    }
    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        self.enqueue(args)
    }
}
