//! Offline pipeline inspection and policy-gated artifacts; no runtime activation.
//! The workspace must not be modified concurrently during writes (including symlink swaps).
use crate::strategy_lab::{ReplayConfig, ReplayReport, replay_configured};
use sadr_core::strategy::{FeatureSnapshot, StrategySpec};
use sadr_sandbox::{
    library::{Library, RegimeResult, RunResult},
    orchestrator_policy::{OrchestratorPolicyConfig, OrchestratorWritePolicy, PolicyError},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrchestratorConfig {
    pub policy: OrchestratorPolicyConfig,
    /// Optional existing strategy source library, relative to the workspace or absolute.
    pub library_path: Option<PathBuf>,
    pub replay: ReplayConfig,
    pub max_proposals: usize,
    /// Maximum bytes read from each JSON proposal.
    pub max_proposal_bytes: u64,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            policy: OrchestratorPolicyConfig::default(),
            library_path: None,
            replay: ReplayConfig::default(),
            max_proposals: 100,
            max_proposal_bytes: 1_048_576,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ProposalEntry {
    /// Workspace-relative path.
    pub path: PathBuf,
    pub size_bytes: u64,
    pub modified_ms: Option<u128>,
    pub kind: Option<serde_json::Value>,
    pub strategy: Option<serde_json::Value>,
    /// Redacted read, size-limit or JSON parse failure, local to this entry.
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum Error {
    Policy(PolicyError),
    Io(std::io::Error),
    Json(serde_json::Error),
    Library(sadr_sandbox::library::Error),
    Replay,
    RegimeInputsLength { snapshots: usize, inputs: usize },
    Symlink,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Do not expose input bodies, database contents or filesystem paths.
        f.write_str(match self {
            Self::Policy(_) => "orchestrator write policy rejected path",
            Self::Io(_) => "orchestrator filesystem error",
            Self::Json(_) => "orchestrator JSON error",
            Self::Library(_) => "orchestrator library error",
            Self::RegimeInputsLength { .. } => {
                "orchestrator regime inputs length differs from snapshots"
            }
            Self::Replay => "orchestrator replay rejected input",
            Self::Symlink => "orchestrator write path contains a symlink",
        })
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Serialize)]
pub struct LibraryEntry {
    pub sha256: String,
    pub name: String,
    pub results: Vec<RunResult>,
}
#[derive(Debug, Serialize)]
pub struct PipelineView {
    /// Workspace-relative regular files, recursively listed in sorted order.
    pub strategy_files: Vec<PathBuf>,
    pub library_entries: Vec<LibraryEntry>,
    pub proposals: Vec<ProposalEntry>,
}

/// Explicit offline recording destination and snapshot-aligned market evidence.
/// Callers can construct inputs with `sadr_core::regime::compute_inputs`.
pub struct ReplayRecording<'a> {
    pub library_path: &'a Path,
    pub strategy_id: i64,
    pub version: i64,
    pub inputs: &'a [sadr_core::regime::RegimeInputs],
    pub recorded_at_ms: i64,
}

pub struct Orchestrator {
    workspace_root: PathBuf,
    policy: OrchestratorWritePolicy,
    config: OrchestratorConfig,
}
impl Orchestrator {
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        config: OrchestratorConfig,
    ) -> Result<Self, Error> {
        let policy = OrchestratorWritePolicy::new(&config.policy).map_err(Error::Policy)?;
        Ok(Self {
            workspace_root: workspace_root.into(),
            policy,
            config,
        })
    }

    pub fn pipeline_view(&self) -> Result<PipelineView, Error> {
        let mut strategy_files = Vec::new();
        self.list_files(&self.config.policy.strategies_dir, &mut strategy_files)?;
        strategy_files.sort();
        let mut library_entries = Vec::new();
        if let Some(path) = &self.config.library_path {
            let path = self.workspace_root.join(path);
            match fs::metadata(&path) {
                Ok(_) => {
                    let library = Library::open_read_only(path).map_err(Error::Library)?;
                    for (sha256, name, results) in library
                        .all_versions_with_results()
                        .map_err(Error::Library)?
                    {
                        library_entries.push(LibraryEntry {
                            sha256,
                            name,
                            results,
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(PipelineView {
            strategy_files,
            library_entries,
            proposals: self.list_proposals()?,
        })
    }

    /// Read regular proposal files recursively, skipping symlinks. Missing roots
    /// are empty; directory errors propagate, JSON errors remain per-entry.
    /// The count cap stops traversal; the returned subset is sorted by path.
    pub fn list_proposals(&self) -> Result<Vec<ProposalEntry>, Error> {
        let mut proposals = Vec::new();
        let root = &self.config.policy.proposals_dir;
        // Do not traverse a symlink in a configured multi-component root.
        for ancestor in self.workspace_root.join(root).ancestors() {
            match fs::symlink_metadata(ancestor) {
                Ok(meta) if meta.is_symlink() => return Ok(proposals),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let mut pending = vec![root.clone()];
        while let Some(relative) = pending.pop() {
            if proposals.len() >= self.config.max_proposals {
                break;
            }
            let entries = match fs::read_dir(self.workspace_root.join(&relative)) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            for entry in entries {
                if proposals.len() >= self.config.max_proposals {
                    break;
                }
                let entry = entry?;
                let file_type = entry.file_type()?;
                let path = relative.join(entry.file_name());
                if file_type.is_dir() {
                    pending.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let metadata = entry.metadata()?;
                let mut proposal = ProposalEntry {
                    path,
                    size_bytes: metadata.len(),
                    modified_ms: metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|duration| duration.as_millis()),
                    kind: None,
                    strategy: None,
                    error: None,
                };
                if proposal.path.extension().is_some_and(|ext| ext == "json") {
                    let limit = self.config.max_proposal_bytes;
                    let mut bytes = Vec::new();
                    if proposal.size_bytes > limit {
                        proposal.error = Some("proposal exceeds byte limit".into());
                    } else if fs::File::open(entry.path())
                        .and_then(|file| file.take(limit.saturating_add(1)).read_to_end(&mut bytes))
                        .is_err()
                    {
                        proposal.error = Some("proposal read error".into());
                    } else if bytes.len() as u64 > limit {
                        proposal.error = Some("proposal exceeds byte limit".into());
                    } else {
                        match serde_json::from_slice::<serde_json::Value>(&bytes) {
                            Ok(value) => {
                                proposal.kind = value.get("kind").cloned();
                                proposal.strategy = value.get("strategy").cloned();
                            }
                            Err(_) => proposal.error = Some("proposal JSON parse error".into()),
                        }
                    }
                }
                proposals.push(proposal);
            }
        }
        proposals.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(proposals)
    }

    fn list_files(&self, relative: &Path, files: &mut Vec<PathBuf>) -> Result<(), Error> {
        let entries = match fs::read_dir(self.workspace_root.join(relative)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = relative.join(entry.file_name());
            if kind.is_dir() {
                self.list_files(&path, files)?;
            } else if kind.is_file() {
                files.push(path);
            }
        }
        Ok(())
    }

    /// Preflight a workspace write without creating anything. Used by callers
    /// that must refuse a whole batch before executing its first action.
    pub fn validate_write(&self, relative_path: impl AsRef<Path>) -> Result<(), Error> {
        self.policy
            .check_write(relative_path.as_ref())
            .map_err(Error::Policy)?;
        self.input_path(relative_path.as_ref())?;
        Ok(())
    }

    /// Preflight a policy-root artifact read without reading its contents.
    pub fn validate_read(&self, relative_path: impl AsRef<Path>) -> Result<(), Error> {
        self.policy
            .check_write(relative_path.as_ref())
            .map_err(Error::Policy)?;
        self.input_path(relative_path.as_ref())?;
        Ok(())
    }

    /// Preflight a replay's input reads and output write without running it.
    pub fn validate_replay(
        &self,
        spec_path: impl AsRef<Path>,
        snapshots_path: impl AsRef<Path>,
        out_relative_path: impl AsRef<Path>,
    ) -> Result<(), Error> {
        self.validate_write(out_relative_path)?;
        self.input_path(spec_path.as_ref())?;
        self.input_path(snapshots_path.as_ref())?;
        Ok(())
    }

    pub fn run_replay(
        &self,
        spec_path: impl AsRef<Path>,
        snapshots_path: impl AsRef<Path>,
        out_relative_path: impl AsRef<Path>,
    ) -> Result<ReplayReport, Error> {
        self.policy
            .check_write(&out_relative_path)
            .map_err(Error::Policy)?;
        let spec: StrategySpec =
            serde_json::from_slice(&fs::read(self.input_path(spec_path.as_ref())?)?)?;
        let snapshots: Vec<FeatureSnapshot> =
            serde_json::from_slice(&fs::read(self.input_path(snapshots_path.as_ref())?)?)?;
        let report =
            replay_configured(&spec, &snapshots, &self.config.replay).map_err(|_| Error::Replay)?;
        self.write(
            out_relative_path.as_ref(),
            &serde_json::to_vec_pretty(&report)?,
        )?;
        Ok(report)
    }

    /// Read a policy-root artifact produced by this workspace. The path is
    /// workspace-relative and may not escape an allowed root or a symlink.
    pub fn read_artifact(&self, relative_path: impl AsRef<Path>) -> Result<Vec<u8>, Error> {
        let relative = relative_path.as_ref();
        self.policy.check_write(relative).map_err(Error::Policy)?;
        let path = self.input_path(relative)?;
        let meta = fs::metadata(&path)?;
        if !meta.is_file() || meta.len() > self.config.max_proposal_bytes {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "artifact invalid",
            )));
        }
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(self.config.max_proposal_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > self.config.max_proposal_bytes {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "artifact oversized",
            )));
        }
        Ok(bytes)
    }

    fn input_path(&self, relative: &Path) -> Result<PathBuf, Error> {
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(Error::Policy(PolicyError::ParentTraversal));
        }
        let path = self.workspace_root.join(relative);
        for ancestor in path.ancestors() {
            match fs::symlink_metadata(ancestor) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(Error::Symlink),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(path)
    }

    /// Record descriptive independent round-trip marks, never executable trades.
    /// Inputs align one-to-one with snapshots; missing evidence classifies unknown.
    /// Nonempty mark batches require explicitly configured replay costs.
    pub fn run_replay_recorded(
        &self,
        spec_path: impl AsRef<Path>,
        snapshots_path: impl AsRef<Path>,
        out_relative_path: impl AsRef<Path>,
        recording: ReplayRecording<'_>,
    ) -> Result<ReplayReport, Error> {
        self.policy
            .check_write(&out_relative_path)
            .map_err(Error::Policy)?;
        self.check_symlinks(&self.workspace_root.join(&out_relative_path))?;
        if self.config.library_path.as_deref() != Some(recording.library_path) {
            self.policy
                .check_write(recording.library_path)
                .map_err(Error::Policy)?;
        }
        let library_path = self.workspace_root.join(recording.library_path);
        self.check_symlinks(&library_path)?;
        let spec: StrategySpec =
            serde_json::from_slice(&fs::read(self.input_path(spec_path.as_ref())?)?)?;
        let snapshots: Vec<FeatureSnapshot> =
            serde_json::from_slice(&fs::read(self.input_path(snapshots_path.as_ref())?)?)?;
        if recording.inputs.len() != snapshots.len() {
            return Err(Error::RegimeInputsLength {
                snapshots: snapshots.len(),
                inputs: recording.inputs.len(),
            });
        }
        if recording.strategy_id <= 0 || recording.version <= 0 || recording.recorded_at_ms < 0 {
            return Err(Error::Replay);
        }
        let report =
            replay_configured(&spec, &snapshots, &self.config.replay).map_err(|_| Error::Replay)?;
        let mut slices = std::collections::BTreeMap::new();
        for (snapshot, inputs) in snapshots.iter().zip(recording.inputs) {
            let regime = sadr_core::regime::classify(inputs, &Default::default());
            let batch = slices
                .entry(regime.label())
                .or_insert_with(|| (regime, Vec::new()));
            batch.1.extend(
                report
                    .forward_returns
                    .iter()
                    .filter(|mark| mark.scan_ms == snapshot.observed_ms),
            );
        }
        let mut results = Vec::new();
        for (_, (regime, marks)) in slices {
            if marks.is_empty() {
                continue;
            }
            let costs = self.config.replay.costs.as_ref().ok_or(Error::Replay)?;
            let marks = marks.into_iter().cloned().collect::<Vec<_>>();
            let metrics =
                crate::strategy_lab::cost_adjusted(&marks, costs).map_err(|_| Error::Replay)?;
            let notional: f64 = costs
                .notional
                .amount()
                .to_string()
                .parse()
                .map_err(|_| Error::Replay)?;
            let scale = notional / 10000.0;
            results.push(RegimeResult {
                strategy_id: recording.strategy_id,
                version: recording.version,
                regime,
                source: "replay".into(),
                sample_count: marks.len() as i64,
                metrics: serde_json::json!({
                    "trades": marks.len(),
                    "net_pnl_after_costs": metrics.net_expectancy_bps * marks.len() as f64 * scale,
                    "expectancy": metrics.net_expectancy_bps * scale,
                    "max_drawdown": metrics.cumulative_drawdown_bps * scale,
                    "label": metrics.label,
                }),
                recorded_at_ms: recording.recorded_at_ms,
            });
        }
        if let Some(parent) = library_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let library = Library::open(library_path).map_err(Error::Library)?;
        for result in results {
            library
                .record_regime_result(&result)
                .map_err(Error::Library)?;
        }
        self.write(
            out_relative_path.as_ref(),
            &serde_json::to_vec_pretty(&report)?,
        )?;
        Ok(report)
    }

    fn check_symlinks(&self, path: &Path) -> Result<(), Error> {
        for component in path.ancestors() {
            match fs::symlink_metadata(component) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(Error::Symlink),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    pub fn write_note(&self, rel_path: impl AsRef<Path>, text: &str) -> Result<(), Error> {
        self.write(rel_path.as_ref(), text.as_bytes())
    }

    fn write(&self, relative: &Path, bytes: &[u8]) -> Result<(), Error> {
        self.policy.check_write(relative).map_err(Error::Policy)?;
        let path = self.workspace_root.join(relative);
        // Inspect ancestors as well as the destination before creating anything.
        for component in path.ancestors() {
            match fs::symlink_metadata(component) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(Error::Symlink),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_replay_appends_regimes_and_rejects_paths_before_writes() {
        use sadr_core::{
            regime::RegimeInputs,
            strategy::{Feature, FeatureRow, Normalization, Weight},
        };
        let dir = tempfile::tempdir().unwrap();
        let config = OrchestratorConfig {
            replay: ReplayConfig {
                horizon_ms: 1000,
                max_mark_delay_ms: 0,
                costs: Some(
                    serde_json::from_value(serde_json::json!({
                        "notional":"5000", "round_trip_spread_bps":10,
                        "fees":{"per_order":"0.01","buy_bps":"5","sell_bps":"7","rounding_increment":"0.01"}
                    }))
                    .unwrap(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        let spec = StrategySpec {
            filters: vec![],
            weights: vec![Weight {
                feature: Feature::MoveBps,
                weight: 1.0,
            }],
            normalization: Normalization::None,
            shortlist_size: 1,
            trigger_score: 0.0,
            trigger: None,
            ..Default::default()
        };
        let snapshots: Vec<_> = (0..3)
            .map(|i| FeatureSnapshot {
                observed_ms: 1000 + i * 1000,
                lookback_samples: 20,
                rows: (0..2)
                    .map(|j| FeatureRow {
                        key: j.to_string(),
                        features: std::collections::BTreeMap::from([
                            (Feature::Price, 100.0 + i as f64),
                            (Feature::MoveBps, if i == j { 10.0 } else { -1.0 }),
                        ]),
                    })
                    .collect(),
            })
            .collect();
        fs::write(
            dir.path().join("spec.json"),
            serde_json::to_vec(&spec).unwrap(),
        )
        .unwrap();
        fs::write(
            dir.path().join("snapshots.json"),
            serde_json::to_vec(&snapshots).unwrap(),
        )
        .unwrap();
        let mut inputs = vec![
            RegimeInputs {
                session_return: 0.0,
                opening_gap: 0.0,
                realised_volatility: 0.001,
                breadth: 0.5,
                instrument_count: 200,
                minute_count: 15,
            };
            3
        ];
        inputs[1].session_return = 0.01;
        let orchestrator = Orchestrator::new(dir.path(), config.clone()).unwrap();
        let recording = |path| ReplayRecording {
            library_path: path,
            strategy_id: 1,
            version: 1,
            inputs: &inputs,
            recorded_at_ms: 4000,
        };
        for path in ["denied/library.sqlite", "strategies/../escape.sqlite"] {
            assert!(matches!(
                orchestrator.run_replay_recorded(
                    "missing",
                    "missing",
                    "proposals/report.json",
                    recording(Path::new(path))
                ),
                Err(Error::Policy(_))
            ));
        }
        assert!(!dir.path().join("proposals").exists());
        assert!(!dir.path().join("denied").exists());
        assert!(!dir.path().join("strategies").exists());
        assert!(matches!(
            orchestrator.run_replay_recorded(
                "spec.json",
                "snapshots.json",
                "proposals/report.json",
                ReplayRecording {
                    inputs: &inputs[..2],
                    ..recording(Path::new("strategies/library.sqlite"))
                }
            ),
            Err(Error::RegimeInputsLength {
                snapshots: 3,
                inputs: 2
            })
        ));
        assert!(!dir.path().join("proposals").exists());
        assert!(!dir.path().join("strategies").exists());
        let path = Path::new("strategies/library.sqlite");
        fs::create_dir(dir.path().join("strategies")).unwrap();
        let mut library = Library::open(dir.path().join(path)).unwrap();
        library
            .add_version(sadr_sandbox::library::NewVersion {
                name: "fixture",
                file_body: &serde_json::to_string(&spec).unwrap(),
                author_model: "fixture",
                intent: "offline",
                inputs: &serde_json::json!({}),
                created_at_ms: 1,
            })
            .unwrap();
        drop(library);
        for _ in 0..2 {
            let report = orchestrator
                .run_replay_recorded(
                    "spec.json",
                    "snapshots.json",
                    "proposals/report.json",
                    recording(path),
                )
                .unwrap();
            assert_eq!(report.forward_returns.len(), 2);
        }
        let rows = Library::open_read_only(dir.path().join(path))
            .unwrap()
            .regime_performance()
            .unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row.source, "replay");
            assert_eq!(row.sample_count, 2);
            assert_eq!(row.metrics["trades"], 2);
            let report: ReplayReport = serde_json::from_slice(
                &fs::read(dir.path().join("proposals/report.json")).unwrap(),
            )
            .unwrap();
            let index = usize::from(row.regime.starts_with("up_trend"));
            let expected = crate::strategy_lab::cost_adjusted(
                &report.forward_returns[index..index + 1],
                config.replay.costs.as_ref().unwrap(),
            )
            .unwrap()
            .net_expectancy_bps
                * 0.5;
            assert!((row.metrics["expectancy"].as_f64().unwrap() - expected).abs() < 1e-9);
            assert!(
                (row.metrics["net_pnl_after_costs"].as_f64().unwrap() - expected * 2.0).abs()
                    < 1e-9
            );
        }
        assert!(dir.path().join("proposals/report.json").exists());
        let mut explicit = config;
        explicit.library_path = Some(path.into());
        explicit.policy.strategies_dir = "other".into();
        Orchestrator::new(dir.path(), explicit)
            .unwrap()
            .run_replay_recorded(
                "spec.json",
                "snapshots.json",
                "proposals/explicit.json",
                recording(path),
            )
            .unwrap();
    }

    #[test]
    fn proposals_are_bounded_read_only_and_parse_failures_are_local() {
        let dir = tempfile::tempdir().unwrap();
        let mut config: OrchestratorConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.max_proposals, 100);
        assert_eq!(config.max_proposal_bytes, 1_048_576);
        config.policy.proposals_dir = "ideas".into();
        config.max_proposal_bytes = 80;
        let orchestrator = Orchestrator::new(dir.path(), config.clone()).unwrap();
        assert!(orchestrator.list_proposals().unwrap().is_empty());
        assert!(!dir.path().join("ideas").exists());
        fs::create_dir_all(dir.path().join("ideas/nested")).unwrap();
        let fixtures = [
            (
                "a.json",
                r#"{"kind":"replay","strategy":{"name":"fixture"}}"#.to_owned(),
            ),
            ("bad.json", "{private invalid body".to_owned()),
            ("large.json", " ".repeat(81)),
            ("nested/note.txt", "plain text".to_owned()),
            ("empty.json", "{}".to_owned()),
        ];
        for (path, body) in &fixtures {
            fs::write(dir.path().join("ideas").join(path), body).unwrap();
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), dir.path().join("ideas/link")).unwrap();
            std::os::unix::fs::symlink(
                dir.path().join("ideas/a.json"),
                dir.path().join("ideas/link.json"),
            )
            .unwrap();
        }
        let view = orchestrator.pipeline_view().unwrap();
        assert_eq!(view.proposals.len(), 5);
        let entries = &view.proposals;
        assert_eq!(entries[0].path, PathBuf::from("ideas/a.json"));
        assert_eq!(entries[0].kind, Some(serde_json::json!("replay")));
        assert_eq!(
            entries[0].strategy,
            Some(serde_json::json!({"name":"fixture"}))
        );
        assert_eq!(entries[0].size_bytes, fixtures[0].1.len() as u64);
        assert_eq!(
            entries[0].modified_ms,
            fs::metadata(dir.path().join("ideas/a.json"))
                .unwrap()
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis())
        );
        assert_eq!(
            entries[1].error.as_deref(),
            Some("proposal JSON parse error")
        );
        assert!(entries[2].kind.is_none() && entries[2].error.is_none());
        assert_eq!(
            entries[3].error.as_deref(),
            Some("proposal exceeds byte limit")
        );
        assert!(entries[4].error.is_none());
        serde_json::to_value(view).unwrap();
        config.max_proposals = 2;
        assert_eq!(
            Orchestrator::new(dir.path(), config.clone())
                .unwrap()
                .list_proposals()
                .unwrap()
                .len(),
            2
        );
        config.max_proposals = 0;
        assert!(
            Orchestrator::new(dir.path(), config)
                .unwrap()
                .list_proposals()
                .unwrap()
                .is_empty()
        );
        for (path, body) in &fixtures {
            assert_eq!(
                fs::read_to_string(dir.path().join("ideas").join(path)).unwrap(),
                *body
            );
        }
    }

    #[test]
    fn offline_replay_and_notes_obey_policy_without_rejected_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let orchestrator = Orchestrator::new(dir.path(), OrchestratorConfig::default()).unwrap();
        fs::write(
            dir.path().join("spec.json"),
            serde_json::to_vec(&StrategySpec::default()).unwrap(),
        )
        .unwrap();
        let snapshots = vec![FeatureSnapshot {
            observed_ms: 1000,
            lookback_samples: 20,
            rows: vec![],
        }];
        fs::write(
            dir.path().join("snapshots.json"),
            serde_json::to_vec(&snapshots).unwrap(),
        )
        .unwrap();
        let report = orchestrator
            .run_replay("spec.json", "snapshots.json", "proposals/run/report.json")
            .unwrap();
        assert_eq!(report.ticks, 1);
        let stored: ReplayReport = serde_json::from_slice(
            &fs::read(dir.path().join("proposals/run/report.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(stored.dataset_hash, report.dataset_hash);
        orchestrator
            .write_note("orchestrator_notes/run/note.txt", "offline")
            .unwrap();
        for path in ["denied/new/report.json", "strategies/../escape/report.json"] {
            assert!(matches!(
                orchestrator.run_replay("missing", "missing", path),
                Err(Error::Policy(_))
            ));
            assert!(matches!(
                orchestrator.write_note(path, "denied"),
                Err(Error::Policy(_))
            ));
        }
        assert!(!dir.path().join("denied").exists());
        assert!(!dir.path().join("strategies").exists());
        assert!(!dir.path().join("escape").exists());
        assert!(
            orchestrator
                .pipeline_view()
                .unwrap()
                .strategy_files
                .is_empty()
        );
    }

    #[test]
    fn pipeline_reads_existing_library_without_initializing_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let config = OrchestratorConfig {
            library_path: Some("library.sqlite".into()),
            ..Default::default()
        };
        let orchestrator = Orchestrator::new(dir.path(), config).unwrap();
        assert!(
            orchestrator
                .pipeline_view()
                .unwrap()
                .library_entries
                .is_empty()
        );
        assert!(!dir.path().join("library.sqlite").exists());
        fs::create_dir_all(dir.path().join("strategies/nested")).unwrap();
        fs::write(dir.path().join("strategies/nested/a.json"), "{}").unwrap();
        let mut library = Library::open(dir.path().join("library.sqlite")).unwrap();
        library
            .add_version(sadr_sandbox::library::NewVersion {
                name: "fixture",
                file_body: "{}",
                author_model: "fixture",
                intent: "offline",
                inputs: &serde_json::json!({}),
                created_at_ms: 1,
            })
            .unwrap();
        drop(library);
        let before = fs::read(dir.path().join("library.sqlite")).unwrap();
        let view = orchestrator.pipeline_view().unwrap();
        assert_eq!(
            view.strategy_files,
            vec![PathBuf::from("strategies/nested/a.json")]
        );
        assert_eq!(view.library_entries[0].name, "fixture");
        serde_json::to_value(view).unwrap();
        assert_eq!(fs::read(dir.path().join("library.sqlite")).unwrap(), before);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), dir.path().join("proposals")).unwrap();
            assert!(matches!(
                orchestrator.write_note("proposals/escape.txt", "denied"),
                Err(Error::Symlink)
            ));
            assert!(!dir.path().join("escape.txt").exists());
        }
    }
}
