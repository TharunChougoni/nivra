//! Typed orchestrator tools. Operator-supplied sources are read-only; model
//! arguments contain no paths, SQL, credentials, approval or session controls.
use crate::{
    strategy_archive::{ArchiveConnection, SourceQuery, read_source_with_archive},
    strategy_lab::{ReplayConfig, replay_configured},
};
use nivra_core::strategy::{FeatureSnapshot, StrategySpec};
use nivra_store::strategy_results::{
    LabConfig, Orchestrator, Proposal, Query, RegistryQuery, ResultsDb,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Debug, Deserialize)]
#[serde(
    tag = "tool",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ToolRequest {
    ReadResults(Query),
    ReadRegistry(RegistryQuery),
    ReadPipeline(SourceQuery),
    ReplaySpec(StrategySpec),
    Propose(Proposal),
    Note(String),
}

pub struct PipelineTools<'a> {
    results: Orchestrator<'a>,
    session: Option<&'a Path>,
    recorded: &'a [FeatureSnapshot],
    replay: ReplayConfig,
    read_sources: bool,
    archive: Option<ArchiveConnection<'a>>,
}
impl<'a> PipelineTools<'a> {
    pub fn new(
        db: &'a ResultsDb,
        run: String,
        config: &LabConfig,
        session: Option<&'a Path>,
        recorded: &'a [FeatureSnapshot],
        replay: ReplayConfig,
    ) -> Result<Self, String> {
        Ok(Self {
            results: Orchestrator::configured(db, run, config)?,
            session,
            recorded,
            replay,
            read_sources: config.read_sources,
            archive: None,
        })
    }
    pub(crate) fn with_archive(mut self, archive: Option<ArchiveConnection<'a>>) -> Self {
        self.archive = archive;
        self
    }
    pub fn call(&mut self, request: ToolRequest) -> Result<Value, String> {
        match request {
            ToolRequest::ReadRegistry(query) => self.results.read_registry(&query),
            ToolRequest::ReadResults(query) => serde_json::to_value(self.results.read(&query)?)
                .map_err(|_| "strategy_tool_encode".into()),
            ToolRequest::ReadPipeline(query) => {
                if !self.read_sources {
                    return Err("strategy_source_access_disabled".into());
                }
                let budget = self
                    .results
                    .remaining_tokens()
                    .saturating_sub(256)
                    .min(1_000_000);
                if budget < 1024 {
                    return Err("strategy_tool_budget_exhausted".into());
                }
                let page = read_source_with_archive(
                    self.session.ok_or("strategy_session_not_configured")?,
                    &query,
                    budget,
                    self.archive,
                )?;
                let value = serde_json::to_value(page).map_err(|_| "strategy_tool_encode")?;
                self.results.charge(&value)?;
                Ok(value)
            }
            ToolRequest::ReplaySpec(spec) => {
                let report = replay_configured(&spec, self.recorded, &self.replay)?;
                let value = json!({"spec_id":report.spec_id,"spec_version":report.spec_version,"spec_hash":report.spec_hash,"dataset_hash":report.dataset_hash,"ticks":report.ticks,"failed_ticks":report.failed_ticks,"selected_rows":report.selected_rows,"filtered_rows":report.filtered_rows,"missing_forward_marks":report.missing_forward_marks,"missing_mark_reasons":report.missing_mark_reasons,"mean_forward_return_bps":report.mean_forward_return_bps,"cost_adjusted":report.cost_adjusted,"input_label":report.input_label,"replay_config":report.replay_config,"scan_count":report.scans.len(),"forward_return_count":report.forward_returns.len()});
                self.results.charge(&value)?;
                Ok(value)
            }
            ToolRequest::Propose(proposal) => {
                let value = json!({"status":"pending_qualification_and_explicit_Tharun_approval"});
                self.results.charge(&value)?;
                self.results.propose(&proposal)?;
                Ok(value)
            }
            ToolRequest::Note(note) => {
                let value = json!({"status":"recorded"});
                self.results.charge(&value)?;
                self.results.note(&note)?;
                Ok(value)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_tools_preserve_operator_boundary_and_shared_budget() {
        let directory = tempfile::tempdir().unwrap();
        let db = ResultsDb::open(&directory.path().join("results.sqlite")).unwrap();
        let config = LabConfig {
            enabled: true,
            token_budget: 128,
            ..Default::default()
        };
        let mut tools = PipelineTools::new(
            &db,
            "fixture".into(),
            &config,
            None,
            &[],
            ReplayConfig::default(),
        )
        .unwrap();
        assert!(tools.call(ToolRequest::Note("first".into())).is_ok());
        assert!(tools.call(ToolRequest::Note("second".into())).is_ok());
        assert!(
            tools
                .call(ToolRequest::Note("budget exceeded".into()))
                .is_err()
        );
        for name in ["approve", "order", "sql", "config", "import", "qualify"] {
            assert!(
                serde_json::from_value::<ToolRequest>(json!({"tool":name,"input":{}})).is_err()
            );
        }
        assert!(
            PipelineTools::new(
                &db,
                "fixture".into(),
                &LabConfig::default(),
                None,
                &[],
                ReplayConfig::default()
            )
            .is_err()
        );
    }
}
