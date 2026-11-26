//! Runtime ranker replay for candidate backtests; no providers or paper writer.
use crate::strategy_lab::{
    self, CostAdjustedMetrics, ForwardReturn, MissingMarkReason, ReplayConfig,
};
use nivra_core::{
    finance::Money,
    strategy::{FeatureSnapshot, ScanSnapshot, StrategySpec, evaluate},
};
use nivra_sandbox::{BwrapLeaderReceipt, Failure, Features, ScorerSandboxConfig, Scores};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

fn default_notional() -> Money {
    Money::parse("5000").expect("constant notional")
}
fn default_horizons() -> Vec<i64> {
    vec![5, 15, 30]
}
/// Runtime scorer replay policy. It is also the stable report format for the
/// post-close trigger; this module never publishes or activates a strategy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScorerReplaySettings {
    pub max_snapshots: usize,
    pub min_valid_snapshots: usize,
    pub horizons_minutes: Vec<i64>,
    pub top_n: usize,
    pub max_replays: usize,
    pub max_age_ms: i64,
    pub lookback_samples: usize,
    /// Maximum UTF-8 bytes of the prior replay summary fed back to the model.
    pub max_feedback_chars: usize,
    /// Retained in the serialized replay policy for existing configurations.
    pub notional: Money,
}
impl Default for ScorerReplaySettings {
    fn default() -> Self {
        Self {
            max_snapshots: 60,
            min_valid_snapshots: 3,
            horizons_minutes: default_horizons(),
            top_n: 10,
            max_replays: 3,
            max_age_ms: 2000,
            lookback_samples: 20,
            max_feedback_chars: 4096,
            notional: default_notional(),
        }
    }
}
impl ScorerReplaySettings {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=10_000).contains(&self.max_snapshots)
            || !(1..=self.max_snapshots).contains(&self.min_valid_snapshots)
            || !(1..=200).contains(&self.top_n)
            || !(1..=32).contains(&self.max_replays)
            || !(1..=300_000).contains(&self.max_age_ms)
            || !(1..=20).contains(&self.lookback_samples)
            || !(256..=65_536).contains(&self.max_feedback_chars)
            || self.notional <= Money::ZERO
            || self.horizons_minutes.is_empty()
            || self.horizons_minutes.len() > 8
            || self
                .horizons_minutes
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self
                .horizons_minutes
                .iter()
                .any(|minutes| !(1..=1440).contains(minutes))
        {
            return Err("scorer_replay_config_invalid".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScorerTickReport {
    pub observed_ms: i64,
    pub status: String,
    pub failure: Option<Failure>,
    pub top_n_overlap: Option<f64>,
    pub kendall: Option<f64>,
    pub spearman: Option<f64>,
    pub default_top_n: Vec<String>,
    pub candidate_top_n: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScorerHorizonReport {
    pub horizon_minutes: i64,
    pub ticks: usize,
    pub selected_rows: usize,
    pub forward_returns: Vec<ForwardReturn>,
    pub missing_forward_marks: usize,
    pub missing_mark_reasons: BTreeMap<MissingMarkReason, usize>,
    pub mean_forward_return_bps: Option<f64>,
    pub cost_adjusted: Option<CostAdjustedMetrics>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScorerReplayReport {
    pub schema: u32,
    pub status: String,
    pub reason: Option<String>,
    pub scorer_sha256: String,
    pub params_sha256: String,
    pub params_effective_sha256: String,
    pub dataset_hash: String,
    pub snapshots: usize,
    pub valid_snapshots: usize,
    pub failed_snapshots: usize,
    pub min_valid_snapshots: usize,
    pub top_n: usize,
    pub mean_top_n_overlap: Option<f64>,
    pub mean_kendall: Option<f64>,
    pub mean_spearman: Option<f64>,
    pub ticks: Vec<ScorerTickReport>,
    pub horizons: Vec<ScorerHorizonReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_sha256: Option<String>,
    pub report_hash: String,
}
impl ScorerReplayReport {
    /// Publication requires every explicit qualification condition, not just a
    /// successful process exit.
    pub fn qualified(&self) -> bool {
        self.status == "ok"
            && self.failed_snapshots == 0
            && self.valid_snapshots >= self.min_valid_snapshots
            && self.valid_snapshots > 0
    }
    fn hash(mut self) -> Result<Self, String> {
        self.report_hash.clear();
        self.report_hash = nivra_core::replay::digest(
            &serde_json::to_vec(&self).map_err(|_| "scorer_replay_report_encoding")?,
        );
        Ok(self)
    }
    /// Recompute the self-hash over the report with its hash field cleared.
    /// Durable candidate results use this to refuse a tampered or re-bound
    /// report instead of trusting a stored hash string.
    pub fn verify_hash(&self) -> bool {
        let mut copy = self.clone();
        copy.report_hash.clear();
        serde_json::to_vec(&copy)
            .ok()
            .is_some_and(|bytes| nivra_core::replay::digest(&bytes) == self.report_hash)
    }
}
fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut count = 0usize;
    let mut total = 0.0;
    for value in values {
        count += 1;
        total += value;
    }
    (count > 0).then(|| total / count as f64)
}
fn scorer_input(snapshot: &FeatureSnapshot, params: &Value) -> Result<Features, Failure> {
    let mut features: Features = snapshot
        .rows
        .iter()
        .map(|row| {
            (
                row.key.clone(),
                serde_json::to_value(&row.features).unwrap_or(Value::Null),
            )
        })
        .collect();
    if features.contains_key("params") {
        return Err(Failure::Input);
    }
    features.insert("params".into(), params.clone());
    Ok(features)
}
fn candidate_scan(
    default: ScanSnapshot,
    scores: &Scores,
    k: usize,
) -> Result<ScanSnapshot, Failure> {
    if scores
        .iter()
        .any(|(key, value)| !value.is_finite() || !default.rows.iter().any(|row| &row.key == key))
    {
        return Err(Failure::OutputInvalid);
    }
    let mut candidate = default;
    for row in &mut candidate.rows {
        row.score = scores.get(&row.key).copied();
        row.reason = "candidate_score".into();
        row.trigger_eligible = None;
        row.selected = false;
    }
    candidate.rows.sort_by(|a, b| {
        b.score
            .unwrap_or(f64::NEG_INFINITY)
            .total_cmp(&a.score.unwrap_or(f64::NEG_INFINITY))
            .then(a.key.cmp(&b.key))
    });
    for row in candidate
        .rows
        .iter_mut()
        .filter(|row| row.score.is_some())
        .take(k)
    {
        row.selected = true;
    }
    Ok(candidate)
}
/// The enclosing owned unit enforces the task deadline. The last local sandbox
/// receipt is advisory and does not release the candidate lease.
pub fn replay_candidate(
    settings: &ScorerReplaySettings,
    sandbox: &ScorerSandboxConfig,
    scorer_source: &[u8],
    params_bytes: &[u8],
    snapshots: &[FeatureSnapshot],
) -> Result<(ScorerReplayReport, BwrapLeaderReceipt), String> {
    settings.validate()?;
    sandbox
        .validate()
        .map_err(|_| "scorer_replay_sandbox_invalid".to_string())?;
    let env_sha256 =
        nivra_sandbox::env_sha256(sandbox).map_err(|_| "ranker_replay_env_invalid".to_string())?;
    if snapshots.is_empty() {
        return Err("scorer_replay_snapshots_unavailable".into());
    }
    if snapshots.len() > 10_000
        || snapshots
            .windows(2)
            .any(|pair| pair[0].observed_ms >= pair[1].observed_ms)
    {
        return Err("scorer_replay_input_invalid".into());
    }
    let params: Value =
        serde_json::from_slice(params_bytes).map_err(|_| "scorer_replay_params_invalid")?;
    if !params.is_object() {
        return Err("scorer_replay_params_invalid".into());
    }
    let written_params_sha256 = nivra_core::replay::digest(params_bytes);
    let params_effective_sha256 = nivra_core::replay::digest(
        &serde_json::to_vec(&params).map_err(|_| "scorer_replay_params_invalid")?,
    );
    let spec = StrategySpec::default();
    let mut ticks = Vec::with_capacity(snapshots.len());
    let mut scans = Vec::with_capacity(snapshots.len());
    let mut advisory = BwrapLeaderReceipt::not_started();
    for snapshot in snapshots {
        let default = evaluate(&spec, snapshot)?;
        let scores = scorer_input(snapshot, &params).and_then(|features| {
            let execution =
                nivra_sandbox::score_source_with_receipt(sandbox, scorer_source, &features);
            advisory = execution.receipt;
            execution.result
        });
        let (candidate, failure) = match scores
            .and_then(|scores| candidate_scan(default.clone(), &scores, settings.top_n))
        {
            Ok(candidate) => (candidate, None),
            Err(failure) => {
                let mut failed = default.clone();
                failed.rows.iter_mut().for_each(|row| row.selected = false);
                (failed, Some(failure))
            }
        };
        let old = ranking(&default);
        let new = ranking(&candidate);
        let metrics = metrics(&old, &new, settings.top_n);
        ticks.push(ScorerTickReport {
            observed_ms: snapshot.observed_ms,
            status: String::from(if failure.is_none() { "ok" } else { "failed" }),
            failure,
            top_n_overlap: metrics.top_k_overlap,
            kendall: metrics.kendall,
            spearman: metrics.spearman,
            default_top_n: old.into_iter().take(settings.top_n).collect(),
            candidate_top_n: new.into_iter().take(settings.top_n).collect(),
        });
        scans.push(candidate);
    }
    let failed_snapshots = ticks.iter().filter(|tick| tick.failure.is_some()).count();
    let valid_snapshots = ticks.len() - failed_snapshots;
    let reason = if valid_snapshots == 0 {
        Some("scorer_no_valid_snapshots")
    } else if failed_snapshots > 0 {
        Some("scorer_snapshot_failures")
    } else if valid_snapshots < settings.min_valid_snapshots {
        Some("scorer_min_valid_snapshots")
    } else {
        None
    };
    let mut horizons = Vec::with_capacity(settings.horizons_minutes.len());
    for minutes in &settings.horizons_minutes {
        let config = ReplayConfig {
            horizon_ms: minutes.saturating_mul(60_000),
            ..ReplayConfig::default()
        };
        let mut cached = scans.iter().cloned();
        let report = strategy_lab::replay_with(&spec, snapshots, &config, |_| {
            cached
                .next()
                .ok_or_else(|| "scorer_replay_scan_missing".to_string())
        })?;
        horizons.push(ScorerHorizonReport {
            horizon_minutes: *minutes,
            ticks: report.ticks,
            selected_rows: report.selected_rows,
            forward_returns: report.forward_returns,
            missing_forward_marks: report.missing_forward_marks,
            missing_mark_reasons: report.missing_mark_reasons,
            mean_forward_return_bps: report.mean_forward_return_bps,
            cost_adjusted: report.cost_adjusted,
        });
    }
    let report = ScorerReplayReport {
        schema: 1,
        status: String::from(if reason.is_none() { "ok" } else { "failed" }),
        reason: reason.map(str::to_owned),
        scorer_sha256: nivra_core::replay::digest(scorer_source),
        params_sha256: written_params_sha256,
        params_effective_sha256,
        dataset_hash: nivra_core::replay::digest(
            &serde_json::to_vec(snapshots).map_err(|_| "scorer_replay_input_invalid")?,
        ),
        snapshots: snapshots.len(),
        valid_snapshots,
        failed_snapshots,
        min_valid_snapshots: settings.min_valid_snapshots,
        top_n: settings.top_n,
        mean_top_n_overlap: mean(ticks.iter().filter_map(|tick| tick.top_n_overlap)),
        mean_kendall: mean(ticks.iter().filter_map(|tick| tick.kendall)),
        mean_spearman: mean(ticks.iter().filter_map(|tick| tick.spearman)),
        ticks,
        horizons,
        env_sha256,
        report_hash: String::new(),
    }
    .hash()?;
    Ok((report, advisory))
}

#[derive(Serialize)]
struct Metrics {
    top_k_common: usize,
    top_k_overlap: Option<f64>,
    common_ranked: usize,
    kendall: Option<f64>,
    spearman: Option<f64>,
}
// Rankings are total orders: score descending, key ascending breaks ties.
// Correlations use the intersection only and compress ranks after intersection.
fn metrics(old: &[String], new: &[String], k: usize) -> Metrics {
    let common: Vec<_> = old.iter().filter(|key| new.contains(key)).collect();
    let other: Vec<_> = new.iter().filter(|key| old.contains(key)).collect();
    let positions: BTreeMap<_, _> = other.iter().enumerate().map(|(i, key)| (*key, i)).collect();
    let ranks: Vec<_> = common.iter().map(|key| positions[key]).collect();
    let n = ranks.len();
    let inversions: usize = ranks
        .iter()
        .enumerate()
        .map(|(i, rank)| ranks[i + 1..].iter().filter(|r| *r < rank).count())
        .sum();
    let squared: f64 = ranks
        .iter()
        .enumerate()
        .map(|(i, rank)| (i as f64 - *rank as f64).powi(2))
        .sum();
    let top_k_common = old
        .iter()
        .take(k)
        .filter(|key| new.iter().take(k).any(|v| v == *key))
        .count();
    let denominator = k.min(old.len()).max(k.min(new.len()));
    Metrics {
        top_k_common,
        top_k_overlap: (denominator > 0).then(|| top_k_common as f64 / denominator as f64),
        common_ranked: n,
        kendall: (n > 1).then(|| 1.0 - 4.0 * inversions as f64 / (n * (n - 1)) as f64),
        spearman: (n > 1).then(|| 1.0 - 6.0 * squared / (n as f64 * ((n * n) as f64 - 1.0))),
    }
}
fn ranking(scan: &ScanSnapshot) -> Vec<String> {
    scan.rows
        .iter()
        .filter(|r| r.score.is_some())
        .map(|r| r.key.clone())
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use nivra_core::strategy::{Feature, FeatureRow};
    fn keys(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }
    fn fixture_snapshots() -> Vec<FeatureSnapshot> {
        [0, 300_000]
            .into_iter()
            .map(|observed_ms| FeatureSnapshot {
                observed_ms,
                lookback_samples: 20,
                rows: ["a", "b", "c"]
                    .into_iter()
                    .enumerate()
                    .map(|(i, key)| FeatureRow {
                        key: key.into(),
                        features: BTreeMap::from([
                            (Feature::Price, if observed_ms == 0 { 100.0 } else { 110.0 }),
                            (Feature::AbsoluteMoveBps, 300.0 + i as f64),
                            (Feature::TradedValue, 2_000_000.0),
                            (Feature::CircuitDistanceBps, 500.0),
                        ]),
                    })
                    .collect(),
            })
            .collect()
    }
    #[test]
    fn rank_math_handles_reverse_partial_ties_and_empty() {
        let old = keys(&["a", "b", "c"]);
        let same = metrics(&old, &old, 2);
        assert_eq!(
            (same.top_k_overlap, same.kendall, same.spearman),
            (Some(1.0), Some(1.0), Some(1.0))
        );
        let reversed = metrics(&old, &keys(&["c", "b", "a"]), 2);
        assert_eq!(
            (reversed.top_k_overlap, reversed.kendall, reversed.spearman),
            (Some(0.5), Some(-1.0), Some(-1.0))
        );
        let partial = metrics(&old, &keys(&["b", "z", "a"]), 2);
        assert_eq!(partial.common_ranked, 2);
        assert_eq!(partial.kendall, Some(-1.0));
        assert_eq!(partial.spearman, Some(-1.0));
        let empty = metrics(&[], &[], 10);
        assert!(
            empty.top_k_overlap.is_none() && empty.kendall.is_none() && empty.spearman.is_none()
        );
        assert_eq!(metrics(&old, &[], 10).top_k_overlap, Some(0.0));
        assert!(metrics(&old, &keys(&["a"]), 10).kendall.is_none());
        let swapped = metrics(&old, &keys(&["b", "a", "c"]), 3);
        assert!((swapped.kendall.unwrap() - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!(swapped.spearman, Some(0.5));
    }

    #[test]
    fn replay_records_env_sha256() {
        let probe = nivra_sandbox::SandboxConfig::default();
        if !probe.bwrap_path.is_file()
            || !probe.python_path.is_file()
            || nivra_sandbox::run_document(&probe, b"print('{}')", &serde_json::json!({})).is_err()
        {
            return;
        }
        let env_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            env_dir.path().join("numpy.py"),
            b"def scale(v):\n    return v * 2.0\n",
        )
        .unwrap();
        let expected_hash = nivra_sandbox::env::compute_dir_hash(env_dir.path()).unwrap();
        let sandbox = nivra_sandbox::SandboxConfig {
            env_dir: Some(env_dir.path().to_path_buf()),
            env_sha256: Some(expected_hash.clone()),
            memory_mb: 512,
            cpu_seconds: 10,
            wall_ms: 5000,
            ..probe
        };
        let snaps = fixture_snapshots();
        let src = br#"import json, sys, numpy
with open(sys.argv[1]) as f:
    rows = json.load(f)
print(json.dumps({k: numpy.scale(v.get("absolute_move_bps", 1.0)) for k, v in rows.items() if k != "params"}))
"#;
        let (report, _) = replay_candidate(
            &serde_json::from_str(r#"{"min_valid_snapshots":2}"#).unwrap(),
            &sandbox,
            src,
            b"{}",
            &snaps,
        )
        .unwrap();
        assert_eq!(report.status, "ok");
        assert_eq!(report.env_sha256.as_deref(), Some(expected_hash.as_str()));
        assert!(report.verify_hash());
    }
}
