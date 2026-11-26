//! Offline comparison and the runtime scorer replay; no providers or paper writer.
use crate::strategy_lab::{
    self, CostAdjustedMetrics, ForwardReturn, MissingMarkReason, ReplayConfig, ReplayReport,
};
use clap::Args as ClapArgs;
use nivra_core::{
    finance::{Amount, FeeArithmetic, Money},
    strategy::{FeatureSnapshot, ScanSnapshot, StrategySpec, evaluate},
};
use nivra_sandbox::{BwrapLeaderReceipt, Failure, Features, ScorerSandboxConfig, Scores};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    error::Error,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

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

const PAPER_COSTS_MISSING: &str = "scorer_compare_paper_costs_missing";
const PAPER_COSTS_INVALID: &str = "scorer_compare_paper_costs_invalid";
const PAPER_COSTS_CONFLICT: &str = "scorer_compare_paper_costs_conflict";
const PAPER_COSTS_RESERVED: &str = "scorer_compare_paper_costs_reserved";
const PAPER_COSTS_SOURCE_EXPLICIT: &str = "explicit_file";
const PAPER_COSTS_SOURCE_PARAMS: &str = "params_file";
const PAPER_COSTS_SOURCE_ABSENT: &str = "absent";
const COST_PARAM_KEYS: [&str; 4] = ["per_order_fee", "buy_bps", "sell_bps", "slippage_bps"];
// Runtime-only names and report evidence names cannot be mixed into scorer params.
const RESERVED_PARAM_KEYS: [&str; 5] = [
    "params",
    "per_order",
    "rounding_increment",
    "paper_costs",
    "paper_costs_source",
];

#[derive(ClapArgs)]
pub struct Args {
    #[arg(long)]
    pub snapshots: PathBuf,
    #[arg(long)]
    pub candidate: PathBuf,
    /// JSON file containing the scorer's params object.
    #[arg(long)]
    pub params: Option<PathBuf>,
    /// Explicit paper-cost policy JSON, using the runtime paper_costs schema.
    #[arg(long)]
    pub paper_costs: Option<PathBuf>,
    /// JSON file containing comparison settings.
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub out: PathBuf,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    k: usize,
    sandbox: ScorerSandboxConfig,
    replay: ReplayConfig,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            k: 10,
            sandbox: ScorerSandboxConfig::default(),
            replay: ReplayConfig::default(),
        }
    }
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
#[derive(Serialize)]
struct Tick {
    observed_ms: i64,
    failure: Option<Failure>,
    metrics: Metrics,
    default_ranking: Vec<String>,
    candidate_ranking: Vec<String>,
}
#[derive(Clone, Serialize)]
struct CostEvidence {
    source: &'static str,
    policy: Option<FeeArithmetic>,
    params: BTreeMap<String, f64>,
}
impl Default for CostEvidence {
    fn default() -> Self {
        Self {
            source: PAPER_COSTS_SOURCE_ABSENT,
            policy: None,
            params: BTreeMap::new(),
        }
    }
}
impl CostEvidence {
    fn new(policy: Option<&FeeArithmetic>, params: Option<&serde_json::Value>) -> Self {
        let object = params.and_then(serde_json::Value::as_object);
        let source = if policy.is_some() {
            PAPER_COSTS_SOURCE_EXPLICIT
        } else if object.is_some_and(|o| COST_PARAM_KEYS.iter().any(|key| o.contains_key(*key))) {
            PAPER_COSTS_SOURCE_PARAMS
        } else {
            PAPER_COSTS_SOURCE_ABSENT
        };
        let params = COST_PARAM_KEYS
            .iter()
            .filter_map(|key| {
                object
                    .and_then(|o| o.get(*key))
                    .and_then(serde_json::Value::as_f64)
                    .map(|value| ((*key).to_string(), value))
            })
            .collect();
        Self {
            source,
            policy: policy.cloned(),
            params,
        }
    }
}
#[derive(Serialize)]
struct Report {
    config: Config,
    paper_costs_source: &'static str,
    paper_costs: Option<FeeArithmetic>,
    scorer_cost_params: BTreeMap<String, f64>,
    failed_ticks: usize,
    ticks: Vec<Tick>,
    default_replay: ReplayReport,
    candidate_replay: ReplayReport,
    interpretation: &'static str,
}
fn compare(
    snapshots: &[FeatureSnapshot],
    config: &Config,
    costs: &CostEvidence,
    mut scorer: impl FnMut(&FeatureSnapshot) -> Result<Scores, Failure>,
) -> Result<Report, String> {
    if !(1..=200).contains(&config.k) {
        return Err("scorer_compare_k_invalid".into());
    }
    let spec = StrategySpec::default();
    let default_replay = strategy_lab::replay_configured(&spec, snapshots, &config.replay)?;
    let mut ticks = Vec::new();
    let mut scans = Vec::new();
    for snapshot in snapshots {
        let default = evaluate(&spec, snapshot)?;
        let mut candidate = default.clone();
        let failure = match scorer(snapshot) {
            Ok(scores) => {
                if scores.iter().any(|(key, value)| {
                    !value.is_finite() || !snapshot.rows.iter().any(|r| &r.key == key)
                }) {
                    Some(Failure::OutputInvalid)
                } else {
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
                        .filter(|r| r.score.is_some())
                        .take(config.k)
                    {
                        row.selected = true;
                    }
                    None
                }
            }
            Err(reason) => Some(reason),
        };
        let old = ranking(&default);
        let new = ranking(&candidate);
        ticks.push(Tick {
            observed_ms: snapshot.observed_ms,
            failure,
            metrics: metrics(&old, &new, config.k),
            default_ranking: old,
            candidate_ranking: new,
        });
        scans.push(candidate);
    }
    let mut scans = scans.into_iter();
    let candidate_replay = strategy_lab::replay_with(&spec, snapshots, &config.replay, |_| {
        scans.next().ok_or_else(|| "comparison_scan_missing".into())
    })?;
    Ok(Report {
        config: config.clone(),
        paper_costs_source: costs.source,
        paper_costs: costs.policy.clone(),
        scorer_cost_params: costs.params.clone(),
        failed_ticks: ticks.iter().filter(|t| t.failure.is_some()).count(),
        ticks,
        default_replay,
        candidate_replay,
        interpretation: "offline_rank_comparison_and_independent_forward_marks_not_executable_fills_or_promotion",
    })
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Box<dyn Error>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("scorer_compare_input_too_large".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn parse_paper_costs(bytes: &[u8]) -> Result<FeeArithmetic, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(PAPER_COSTS_INVALID.into());
    }
    let costs: FeeArithmetic =
        serde_json::from_slice(bytes).map_err(|_| PAPER_COSTS_INVALID.to_string())?;
    costs
        .validate()
        .map_err(|_| PAPER_COSTS_INVALID.to_string())?;
    Ok(costs)
}
fn load_paper_costs(path: &Path) -> Result<FeeArithmetic, String> {
    let bytes = std::fs::read(path).map_err(|_| PAPER_COSTS_MISSING.to_string())?;
    parse_paper_costs(&bytes)
}
fn cost_params(costs: &FeeArithmetic) -> [(&'static str, Money); 4] {
    [
        ("per_order_fee", costs.per_order),
        ("buy_bps", costs.buy_bps),
        ("sell_bps", costs.sell_bps),
        ("slippage_bps", costs.slippage_bps),
    ]
}
fn number_matches(value: &serde_json::Value, expected: Money) -> bool {
    value
        .as_number()
        .and_then(|number| Amount::parse(&number.to_string()).ok())
        .is_some_and(|amount| amount == expected.amount())
}
fn json_number(value: Money) -> Result<serde_json::Value, String> {
    let number = value
        .amount()
        .to_string()
        .parse::<f64>()
        .map_err(|_| PAPER_COSTS_INVALID.to_string())?;
    serde_json::Number::from_f64(number)
        .map(serde_json::Value::Number)
        .ok_or_else(|| PAPER_COSTS_INVALID.to_string())
}
// Merge runtime paper costs into the scorer's params object. Existing matching
// values are accepted unchanged; conflicts and runtime-only names refuse before
// any mutation so the caller never sees a partially applied policy.
fn merge_paper_costs(params: &mut serde_json::Value, costs: &FeeArithmetic) -> Result<(), String> {
    let object = params
        .as_object_mut()
        .ok_or_else(|| "scorer_compare_params_invalid".to_string())?;
    if RESERVED_PARAM_KEYS
        .iter()
        .any(|key| object.contains_key(*key))
    {
        return Err(PAPER_COSTS_RESERVED.into());
    }
    let mapped = cost_params(costs);
    if mapped.iter().any(|(key, expected)| {
        object
            .get(*key)
            .is_some_and(|value| !number_matches(value, *expected))
    }) {
        return Err(PAPER_COSTS_CONFLICT.into());
    }
    for (key, expected) in mapped {
        if !object.contains_key(key) {
            object.insert(key.into(), json_number(expected)?);
        }
    }
    Ok(())
}
pub fn run(args: Args) -> Result<(), Box<dyn Error>> {
    let snapshots: Vec<FeatureSnapshot> = read(&args.snapshots)?;
    let config = args
        .config
        .as_deref()
        .map(read)
        .transpose()?
        .unwrap_or_default();
    let mut params: Option<serde_json::Value> = args.params.as_deref().map(read).transpose()?;
    if params.as_ref().is_some_and(|p| !p.is_object()) {
        return Err("scorer_compare_params_invalid".into());
    }
    let paper_costs = args
        .paper_costs
        .as_deref()
        .map(load_paper_costs)
        .transpose()?;
    if let Some(costs) = &paper_costs {
        let params = params.get_or_insert_with(|| serde_json::Value::Object(Default::default()));
        merge_paper_costs(params, costs)?;
    }
    let evidence = CostEvidence::new(paper_costs.as_ref(), params.as_ref());
    let report = compare(&snapshots, &config, &evidence, |snapshot| {
        let mut features: Features = snapshot
            .rows
            .iter()
            .map(|r| {
                (
                    r.key.clone(),
                    serde_json::to_value(&r.features).expect("numeric features"),
                )
            })
            .collect();
        if let Some(params) = &params {
            if features.contains_key("params") {
                return Err(Failure::Input);
            }
            features.insert("params".into(), params.clone());
        }
        nivra_sandbox::score(&config.sandbox, &args.candidate, &features)
    })?;
    // Never overwrite a recorded input or an existing report.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(args.out)?;
    serde_json::to_writer_pretty(file, &report)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nivra_core::strategy::{Feature, FeatureRow};
    fn keys(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }
    fn fixture_costs() -> FeeArithmetic {
        parse_paper_costs(include_bytes!("../../../fixtures/paper-costs.json")).unwrap()
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
        let defaults: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.k, 10);
        assert!(defaults.replay.costs.is_none());
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
    fn stub_comparison_reuses_marks_costs_and_failure_fallback() {
        let snapshots = fixture_snapshots();
        let mut config = Config::default();
        config.replay.costs = Some(serde_json::from_value(serde_json::json!({"notional":"5000","round_trip_spread_bps":10,"fees":{"per_order":"0.01","buy_bps":"5","sell_bps":"7","rounding_increment":"0.01"}})).unwrap());
        let report = compare(&snapshots, &config, &CostEvidence::default(), |snapshot| {
            if snapshot.observed_ms == 0 {
                Ok(BTreeMap::from([("a".into(), 1.0), ("b".into(), 1.0)]))
            } else {
                Err(Failure::WallTimeout)
            }
        })
        .unwrap();
        assert_eq!(report.paper_costs_source, PAPER_COSTS_SOURCE_ABSENT);
        assert!(report.paper_costs.is_none());
        assert!(report.scorer_cost_params.is_empty());
        assert_eq!(report.failed_ticks, 1);
        assert_eq!(report.ticks[0].candidate_ranking, keys(&["a", "b"]));
        assert_eq!(
            report.ticks[1].candidate_ranking,
            report.ticks[1].default_ranking
        );
        assert_eq!(report.ticks[1].metrics.kendall, Some(1.0));
        let default = report.default_replay.cost_adjusted.unwrap();
        let candidate = report.candidate_replay.cost_adjusted.unwrap();
        assert_eq!((default.samples, candidate.samples), (3, 2));
        assert!(candidate.net_expectancy_bps < 1000.0);
        assert!(candidate.fees > nivra_core::finance::Amount::ZERO);
        let failed = compare(
            &snapshots,
            &Config::default(),
            &CostEvidence::default(),
            |_| Err(Failure::Exit),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&failed.default_replay.forward_returns).unwrap(),
            serde_json::to_value(&failed.candidate_replay.forward_returns).unwrap()
        );
        let invalid = compare(
            &snapshots,
            &Config::default(),
            &CostEvidence::default(),
            |_| Ok(BTreeMap::from([("unknown".into(), 1.0)])),
        )
        .unwrap();
        assert_eq!(invalid.failed_ticks, 2);
        assert_eq!(invalid.ticks[0].failure, Some(Failure::OutputInvalid));
        let missing = compare(
            &snapshots[..1],
            &Config::default(),
            &CostEvidence::default(),
            |_| Ok(Scores::new()),
        )
        .unwrap();
        assert!(missing.default_replay.cost_adjusted.is_none());
        assert_eq!(missing.default_replay.missing_forward_marks, 3);
        assert!(
            compare(
                &snapshots.iter().rev().cloned().collect::<Vec<_>>(),
                &Config::default(),
                &CostEvidence::default(),
                |_| Ok(Scores::new())
            )
            .is_err()
        );
    }
    #[test]
    fn paper_costs_map_to_scorer_params_and_accept_matching_values() {
        let costs = fixture_costs();
        let mut matching = serde_json::json!({
            "min_price": 10.0,
            "per_order_fee": 0.01,
            "buy_bps": 5,
            "sell_bps": 7.0,
            "slippage_bps": 5
        });
        let original = matching.clone();
        merge_paper_costs(&mut matching, &costs).unwrap();
        assert_eq!(matching, original);
        let mut empty = serde_json::json!({});
        merge_paper_costs(&mut empty, &costs).unwrap();
        assert_eq!(empty["per_order_fee"], 0.01);
        assert_eq!(empty["buy_bps"], 5.0);
        assert_eq!(empty["sell_bps"], 7.0);
        assert_eq!(empty["slippage_bps"], 5.0);
        for ((key, expected), param_key) in cost_params(&costs).into_iter().zip(COST_PARAM_KEYS) {
            assert_eq!(key, param_key);
            assert!(number_matches(&empty[key], expected));
        }
    }
    #[test]
    fn paper_costs_refuse_conflicts_and_reserved_names_before_mutating() {
        let costs = fixture_costs();
        let mut conflicting = serde_json::json!({"buy_bps": 6});
        let original = conflicting.clone();
        assert_eq!(
            merge_paper_costs(&mut conflicting, &costs).unwrap_err(),
            PAPER_COSTS_CONFLICT
        );
        // The error must not leave the missing per-order key partially merged.
        assert_eq!(conflicting, original);
        for reserved in RESERVED_PARAM_KEYS {
            let mut params = serde_json::json!({});
            params
                .as_object_mut()
                .unwrap()
                .insert(reserved.into(), serde_json::json!("0.01"));
            assert_eq!(
                merge_paper_costs(&mut params, &costs).unwrap_err(),
                PAPER_COSTS_RESERVED,
                "{reserved}"
            );
        }
        let mut not_object = serde_json::json!([]);
        assert_eq!(
            merge_paper_costs(&mut not_object, &costs).unwrap_err(),
            "scorer_compare_params_invalid"
        );
    }
    #[test]
    fn paper_costs_refuse_missing_malformed_unknown_negative_and_out_of_range_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_paper_costs(&dir.path().join("absent.json")).unwrap_err(),
            PAPER_COSTS_MISSING
        );
        for bytes in [
            b"not json".as_slice(),
            br#"{"per_order":"0.01","buy_bps":"5","sell_bps":"7"}"#,
            br#"{"per_order":"0.01","buy_bps":"5","sell_bps":"7","slippage_bps":"5","unknown":"1"}"#,
            br#"{"per_order":"-1","buy_bps":"5","sell_bps":"7","slippage_bps":"5"}"#,
            br#"{"per_order":"0.01","buy_bps":"5","sell_bps":"7","slippage_bps":"-5"}"#,
            br#"{"per_order":"0.01","buy_bps":"10001","sell_bps":"7","slippage_bps":"5"}"#,
            br#"{"per_order":"0.01","buy_bps":"5","sell_bps":"7","slippage_bps":"10000"}"#,
            br#"{"per_order":"0.01","buy_bps":"5","sell_bps":"7","slippage_bps":"5","rounding_increment":"0"}"#,
            br#"{"per_order":0.01,"buy_bps":"5","sell_bps":"7","slippage_bps":"5"}"#,
        ] {
            assert_eq!(
                parse_paper_costs(bytes).unwrap_err(),
                PAPER_COSTS_INVALID,
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }
    #[test]
    fn checked_in_params_match_agreed_paper_cost_policy() {
        let costs = fixture_costs();
        let mut params: serde_json::Value = serde_json::from_str(include_str!(
            "../../../strategies/cost_aware_momentum.params.json"
        ))
        .unwrap();
        let original = params.clone();
        merge_paper_costs(&mut params, &costs).unwrap();
        assert_eq!(params, original);
        assert_eq!(params["per_order_fee"], 0.01);
        assert_eq!(params["buy_bps"], 5.0);
        assert_eq!(params["sell_bps"], 7.0);
        assert_eq!(params["slippage_bps"], 5.0);
    }
    #[test]
    fn report_records_explicit_paper_cost_source_and_values() {
        let snapshots = fixture_snapshots();
        let costs = fixture_costs();
        let mut params = serde_json::json!({"min_price": 10.0});
        merge_paper_costs(&mut params, &costs).unwrap();
        let evidence = CostEvidence::new(Some(&costs), Some(&params));
        let report = compare(&snapshots, &Config::default(), &evidence, |_| {
            Ok(Scores::new())
        })
        .unwrap();
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["paper_costs_source"], "explicit_file");
        for (key, expected) in [
            ("per_order", costs.per_order),
            ("buy_bps", costs.buy_bps),
            ("sell_bps", costs.sell_bps),
            ("slippage_bps", costs.slippage_bps),
        ] {
            let actual = value["paper_costs"][key]
                .as_str()
                .expect("serialized exact decimal");
            assert_eq!(Amount::parse(actual).unwrap(), expected.amount(), "{key}");
        }
        for (key, expected) in cost_params(&costs) {
            assert_eq!(
                value["scorer_cost_params"][key],
                expected.amount().to_string().parse::<f64>().unwrap(),
                "{key}"
            );
        }
        assert_eq!(
            CostEvidence::new(None, Some(&params)).source,
            PAPER_COSTS_SOURCE_PARAMS
        );
        assert_eq!(
            CostEvidence::new(None, None).source,
            PAPER_COSTS_SOURCE_ABSENT
        );
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
