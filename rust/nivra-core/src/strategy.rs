//! Deterministic, bounded strategy composition. Missing features fail closed.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    Price,
    MoveBps,
    AbsoluteMoveBps,
    MarketRelativeBps,
    AbsoluteMarketRelativeBps,
    TradedValue,
    CircuitDistanceBps,
    VolatilityBps,
    RangePosition,
    BestBid,
    BestAsk,
    BestBidQty,
    BestAskQty,
    SpreadFraction,
    BookImbalance,
    SessionVwap,
    MinutesSinceOpen,
    RelVolumeTod,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureRow {
    pub key: String,
    pub features: BTreeMap<Feature, f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureSnapshot {
    pub observed_ms: i64,
    /// Legacy captures used the full twenty-sample retained scanner window.
    #[serde(default = "default_lookback_samples")]
    pub lookback_samples: usize,
    pub rows: Vec<FeatureRow>,
}
pub fn default_lookback_samples() -> usize {
    20
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    GreaterEqual,
    LessEqual,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub feature: Feature,
    pub op: Operator,
    pub threshold: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Weight {
    pub feature: Feature,
    pub weight: f64,
}
/// Optional wake condition, independent of the ranked shortlist presented to Luna.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerRule {
    pub feature: Feature,
    pub threshold: f64,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    #[default]
    None,
    CrossSectionalRank,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategySpec {
    pub id: String,
    pub version: u32,
    #[serde(default = "default_lookback_samples")]
    pub lookback_samples: usize,
    pub filters: Vec<Filter>,
    pub weights: Vec<Weight>,
    #[serde(default)]
    pub normalization: Normalization,
    pub shortlist_size: usize,
    pub trigger_score: f64,
    pub cooldown_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<TriggerRule>,
}
impl Default for StrategySpec {
    /// Versioned recommended scanner: absolute retained move ranking and an
    /// independent 200 bps wake threshold. Operators serialize this into config
    /// and match shortlist/cooldown/screens to their existing discovery settings.
    fn default() -> Self {
        Self {
            id: "legacy_scanner".into(),
            version: 1,
            lookback_samples: default_lookback_samples(),
            filters: vec![
                Filter {
                    feature: Feature::Price,
                    op: Operator::GreaterEqual,
                    threshold: 10.0,
                },
                Filter {
                    feature: Feature::TradedValue,
                    op: Operator::GreaterEqual,
                    threshold: 1_000_000.0,
                },
                Filter {
                    feature: Feature::CircuitDistanceBps,
                    op: Operator::GreaterEqual,
                    threshold: 100.0,
                },
            ],
            weights: vec![Weight {
                feature: Feature::AbsoluteMoveBps,
                weight: 1.0,
            }],
            normalization: Normalization::None,
            shortlist_size: 10,
            trigger_score: 0.0,
            cooldown_ms: 900_000,
            trigger: Some(TriggerRule {
                feature: Feature::AbsoluteMoveBps,
                threshold: 200.0,
            }),
        }
    }
}
impl StrategySpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 128
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || self.version == 0
            || !(1..=20).contains(&self.lookback_samples)
            || self.filters.len() > 32
            || self.weights.is_empty()
            || self.weights.len() > 32
            || !(1..=200).contains(&self.shortlist_size)
            || !self.trigger_score.is_finite()
            || self.trigger_score.abs() > 1e12
            || !(0..=86_400_000).contains(&self.cooldown_ms)
            || self
                .trigger
                .as_ref()
                .is_some_and(|t| !t.threshold.is_finite() || !(0.0..=1e12).contains(&t.threshold))
            || self
                .filters
                .iter()
                .any(|f| !f.threshold.is_finite() || f.threshold.abs() > 1e15)
            || self
                .weights
                .iter()
                .any(|w| !w.weight.is_finite() || w.weight.abs() > 1e6)
        {
            return Err("strategy_spec_invalid".into());
        }
        Ok(())
    }
}
/// Configurable admission ceilings. Defaults preserve the existing accepted domain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpecAdmissionBounds {
    pub max_specs: usize,
    pub max_features: usize,
    pub max_filters: usize,
    pub max_weights: usize,
    pub max_lookback_samples: usize,
    pub max_shortlist_size: usize,
    pub max_cooldown_ms: i64,
    pub max_abs_weight: f64,
    pub max_abs_filter_threshold: f64,
    pub max_abs_trigger_score: f64,
    pub max_trigger_threshold: f64,
}
impl Default for SpecAdmissionBounds {
    fn default() -> Self {
        Self {
            max_specs: 16,
            max_features: 9,
            max_filters: 32,
            max_weights: 32,
            max_lookback_samples: 20,
            max_shortlist_size: 200,
            max_cooldown_ms: 86_400_000,
            max_abs_weight: 1e6,
            max_abs_filter_threshold: 1e15,
            max_abs_trigger_score: 1e12,
            max_trigger_threshold: 1e12,
        }
    }
}
impl SpecAdmissionBounds {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub fn validate(&self) -> Result<(), String> {
        let defaults = Self::default();
        if self.max_specs == 0
            || self.max_features == 0
            || self.max_features > 9
            || self.max_filters == 0
            || self.max_weights == 0
            || !(1..=20).contains(&self.max_lookback_samples)
            || self.max_shortlist_size == 0
            || self.max_cooldown_ms < 0
            || [
                self.max_abs_weight,
                self.max_abs_filter_threshold,
                self.max_abs_trigger_score,
                self.max_trigger_threshold,
            ]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0)
            || self.max_specs > defaults.max_specs
            || self.max_filters > defaults.max_filters
            || self.max_weights > defaults.max_weights
            || self.max_shortlist_size > defaults.max_shortlist_size
            || self.max_cooldown_ms > defaults.max_cooldown_ms
            || self.max_abs_weight > defaults.max_abs_weight
            || self.max_abs_filter_threshold > defaults.max_abs_filter_threshold
            || self.max_abs_trigger_score > defaults.max_abs_trigger_score
            || self.max_trigger_threshold > defaults.max_trigger_threshold
        {
            return Err("strategy_admission_bounds_invalid".into());
        }
        Ok(())
    }
    pub fn admit(&self, spec: &StrategySpec) -> Result<(), String> {
        self.validate()?;
        spec.validate()?;
        let features: std::collections::BTreeSet<_> = spec
            .filters
            .iter()
            .map(|f| f.feature)
            .chain(spec.weights.iter().map(|w| w.feature))
            .chain(spec.trigger.iter().map(|t| t.feature))
            .collect();
        if features.len() > self.max_features
            || spec.filters.len() > self.max_filters
            || spec.weights.len() > self.max_weights
            || spec.lookback_samples > self.max_lookback_samples
            || spec.shortlist_size > self.max_shortlist_size
            || spec.cooldown_ms > self.max_cooldown_ms
            || spec.trigger_score.abs() > self.max_abs_trigger_score
            || spec
                .filters
                .iter()
                .any(|f| f.threshold.abs() > self.max_abs_filter_threshold)
            || spec
                .weights
                .iter()
                .any(|w| w.weight.abs() > self.max_abs_weight)
            || spec
                .trigger
                .as_ref()
                .is_some_and(|t| t.threshold > self.max_trigger_threshold)
        {
            return Err("strategy_spec_admission_limit".into());
        }
        Ok(())
    }
}
/// Time-of-day volume proxy; no per-minute historical volume curve is assumed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeatureSettings {
    pub session_minutes: f64,
    pub rel_volume_curve_exponent: f64,
    pub rel_volume_min_minutes: f64,
}
impl Default for FeatureSettings {
    fn default() -> Self {
        Self {
            session_minutes: 375.0,
            rel_volume_curve_exponent: 0.7,
            rel_volume_min_minutes: 5.0,
        }
    }
}
impl FeatureSettings {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub fn validate(&self) -> Result<(), String> {
        if !self.session_minutes.is_finite()
            || self.session_minutes <= 0.0
            || !self.rel_volume_curve_exponent.is_finite()
            || self.rel_volume_curve_exponent <= 0.0
            || !self.rel_volume_min_minutes.is_finite()
            || self.rel_volume_min_minutes < 0.0
            || self.rel_volume_min_minutes > self.session_minutes
        {
            return Err("strategy_feature_settings_invalid".into());
        }
        Ok(())
    }
    fn relative_volume(
        &self,
        volume: u64,
        average: Option<f64>,
        minutes: Option<f64>,
    ) -> Option<f64> {
        let average = average.filter(|v| v.is_finite() && *v > 0.0)?;
        let minutes = minutes.filter(|v| v.is_finite() && *v >= self.rel_volume_min_minutes)?;
        let fraction = (minutes / self.session_minutes)
            .powf(self.rel_volume_curve_exponent)
            .min(1.0);
        let denominator = average * fraction;
        let value = volume as f64 / denominator;
        (denominator.is_finite() && denominator > 0.0 && value.is_finite()).then_some(value)
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StrategySettings {
    #[serde(skip_serializing_if = "FeatureSettings::is_default")]
    pub feature_settings: FeatureSettings,
    #[serde(skip_serializing_if = "SpecAdmissionBounds::is_default")]
    pub admission_bounds: SpecAdmissionBounds,
    pub enabled: bool,
    pub champion: String,
    pub specs: Vec<StrategySpec>,
}
impl StrategySettings {
    pub fn validate(&self) -> Result<(), String> {
        self.feature_settings.validate()?;
        self.admission_bounds.validate()?;
        if self.specs.len() > self.admission_bounds.max_specs {
            return Err("strategy_spec_limit".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for spec in &self.specs {
            self.admission_bounds.admit(spec)?;
            if !ids.insert(&spec.id) {
                return Err("strategy_duplicate_id".into());
            }
        }
        if self.enabled && !ids.contains(&self.champion) {
            return Err("strategy_champion_missing".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanRow {
    pub key: String,
    pub features: BTreeMap<Feature, f64>,
    pub score: Option<f64>,
    pub reason: String,
    pub selected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_eligible: Option<bool>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSnapshot {
    pub scan_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scorer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scorer_source_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scorer_failure: Option<String>,
    pub spec_hash: String,
    pub spec_id: String,
    pub spec_version: u32,
    pub observed_ms: i64,
    pub rows: Vec<ScanRow>,
}
impl ScanSnapshot {
    /// Bind the complete retained evidence, including rejected and unavailable rows.
    pub fn seal(&mut self) -> Result<(), String> {
        self.scan_id.clear();
        self.scan_id =
            crate::replay::digest(&serde_json::to_vec(self).map_err(|_| "strategy_scan_encoding")?);
        Ok(())
    }
}
pub fn evaluate(spec: &StrategySpec, snapshot: &FeatureSnapshot) -> Result<ScanSnapshot, String> {
    spec.validate()?;
    if snapshot.lookback_samples != spec.lookback_samples {
        return Err("strategy_feature_window_mismatch".into());
    }
    if snapshot.rows.len() > 100_000 {
        return Err("strategy_input_limit".into());
    }
    let mut keys = std::collections::BTreeSet::new();
    if snapshot.rows.iter().any(|r| {
        r.key.is_empty() || !keys.insert(&r.key) || r.features.values().any(|v| !v.is_finite())
    }) {
        return Err("strategy_input_invalid".into());
    }
    let mut ranks = BTreeMap::new();
    for w in &spec.weights {
        let mut values: Vec<f64> = snapshot
            .rows
            .iter()
            .filter_map(|r| r.features.get(&w.feature).copied())
            .collect();
        values.sort_by(f64::total_cmp);
        ranks.insert(w.feature, values);
    }
    let mut rows = Vec::new();
    for input in &snapshot.rows {
        let mut reason = "ranked".to_string();
        let mut score = 0.0;
        for f in &spec.filters {
            match input.features.get(&f.feature) {
                None => {
                    reason = "missing_feature".into();
                    break;
                }
                Some(v) => {
                    if !match f.op {
                        Operator::GreaterEqual => *v >= f.threshold,
                        Operator::LessEqual => *v <= f.threshold,
                    } {
                        reason = format!("filtered:{:?}", f.feature);
                        break;
                    }
                }
            }
        }
        if reason == "ranked" {
            for w in &spec.weights {
                let Some(value) = input.features.get(&w.feature) else {
                    reason = "missing_feature".into();
                    break;
                };
                let normalized = match spec.normalization {
                    Normalization::None => *value,
                    Normalization::CrossSectionalRank => {
                        let values = &ranks[&w.feature];
                        values.partition_point(|v| v < value) as f64 / values.len().max(1) as f64
                    }
                };
                score += normalized * w.weight;
            }
        }
        if !score.is_finite() {
            reason = "nonfinite_score".into();
        }
        rows.push(ScanRow {
            key: input.key.clone(),
            features: input.features.clone(),
            score: (reason == "ranked").then_some(score),
            reason,
            selected: false,
            trigger_eligible: None,
        });
    }
    rows.sort_by(|a, b| {
        b.score
            .unwrap_or(f64::NEG_INFINITY)
            .total_cmp(&a.score.unwrap_or(f64::NEG_INFINITY))
            .then(a.key.cmp(&b.key))
    });
    for row in rows
        .iter_mut()
        .filter(|r| r.score.is_some_and(|v| v >= spec.trigger_score))
        .take(spec.shortlist_size)
    {
        row.selected = true;
    }
    for row in rows.iter_mut().filter(|row| row.score.is_some()) {
        if let Some(trigger) = &spec.trigger {
            if let Some(value) = row.features.get(&trigger.feature) {
                row.trigger_eligible = Some(*value >= trigger.threshold);
            } else {
                row.selected = false;
                row.reason = "missing_trigger_feature".into();
            }
        }
    }
    let mut result = ScanSnapshot {
        scan_id: String::new(),
        scorer_id: None,
        scorer_source_hash: None,
        scorer_failure: None,
        spec_hash: crate::replay::digest(
            &serde_json::to_vec(spec).map_err(|_| "strategy_spec_encoding")?,
        ),
        spec_id: spec.id.clone(),
        spec_version: spec.version,
        observed_ms: snapshot.observed_ms,
        rows,
    };
    result.seal()?;
    Ok(result)
}
/// Exact default-scanner compatibility projection. Membership is explicit;
/// cached screening reasons, rather than metadata receipt age, match legacy discovery.
/// This preserves legacy discovery semantics while sharing the feature library.
pub fn evaluate_default_scanner(
    spec: &StrategySpec,
    scanner: &crate::execution::Scanner,
    universe: &[String],
    now: i64,
    max_age: i64,
) -> Result<ScanSnapshot, String> {
    use crate::{execution::ScreenPolicy, finance::Amount};
    // Validate before indexing the default composition's three filters.
    validate_default_composition(spec)?;
    let amount = |v: f64| Amount::parse(&v.to_string()).map_err(|_| "strategy_screen_decimal");
    let policy = ScreenPolicy {
        min_price: amount(spec.filters[0].threshold)?,
        min_traded_value: amount(spec.filters[1].threshold)?,
        min_circuit_distance_bps: amount(spec.filters[2].threshold)?,
    };
    policy.validate().map_err(|_| "strategy_screen_policy")?;
    evaluate_default_scanner_with_policy(spec, scanner, universe, now, max_age, &policy)
}

fn validate_default_composition(spec: &StrategySpec) -> Result<(), String> {
    // Admit the composition independently of the exact decimal runtime policy.
    spec.validate()?;
    let mut expected = StrategySpec::default();
    expected.id.clone_from(&spec.id);
    expected.version = spec.version;
    expected.shortlist_size = spec.shortlist_size;
    expected.cooldown_ms = spec.cooldown_ms;
    for (target, input) in expected.filters.iter_mut().zip(&spec.filters) {
        target.threshold = input.threshold;
    }
    if serde_json::to_value(&expected).map_err(|_| "strategy_spec_encoding")?
        != serde_json::to_value(spec).map_err(|_| "strategy_spec_encoding")?
    {
        return Err("strategy_default_composition_required".into());
    }
    Ok(())
}

/// Runtime compatibility path with the operator's exact decimal screening policy.
/// Screen thresholds must never round-trip through floating-point spec fields.
pub fn evaluate_default_scanner_with_policy(
    spec: &StrategySpec,
    scanner: &crate::execution::Scanner,
    universe: &[String],
    now: i64,
    max_age: i64,
    policy: &crate::execution::ScreenPolicy,
) -> Result<ScanSnapshot, String> {
    validate_default_composition(spec)?;
    let members: std::collections::BTreeSet<_> = universe.iter().collect();
    if universe.len() > 100_000
        || members.len() != universe.len()
        || universe.iter().any(|key| key.is_empty())
    {
        return Err("strategy_universe_invalid".into());
    }
    policy.validate().map_err(|_| "strategy_screen_policy")?;
    let mut cache = scanner.clone();
    cache.samples.retain(|key, _| members.contains(key));
    let mut rejected = BTreeMap::new();
    for (key, sample) in &cache.samples {
        let reason = if sample.reason == "missing_market_fields" {
            "missing_market_fields"
        } else if let Some(fields) = &sample.market_fields {
            sample
                .current
                .as_ref()
                .or_else(|| sample.history.last())
                .and_then(|p| policy.reason(p.amount(), fields).ok())
                .unwrap_or("missing_market_fields")
        } else {
            "missing_market_fields"
        };
        if reason != "eligible_no_signal" {
            rejected.insert(key.clone(), reason);
        }
    }
    let mut input = extract_window(&cache, now, max_age, max_age, 20, None);
    for row in &mut input.rows {
        row.features.entry(Feature::AbsoluteMoveBps).or_insert(0.0);
    }
    // Exact decimal policy has already decided all three screens. Avoid a second,
    // lossy floating-point comparison at a decimal boundary.
    let mut ranking = spec.clone();
    ranking.filters.clear();
    let mut result = evaluate(&ranking, &input)?;
    for row in &mut result.rows {
        if let Some(reason) = rejected.get(&row.key) {
            row.score = None;
            row.reason = (*reason).into();
            row.trigger_eligible = None;
        }
        row.selected = false;
    }
    result.rows.sort_by(|a, b| {
        b.score
            .unwrap_or(f64::NEG_INFINITY)
            .total_cmp(&a.score.unwrap_or(f64::NEG_INFINITY))
            .then(a.key.cmp(&b.key))
    });
    for row in result
        .rows
        .iter_mut()
        .filter(|r| r.score.is_some())
        .take(spec.shortlist_size)
    {
        row.selected = true;
    }
    for key in universe {
        if !result.rows.iter().any(|r| r.key == *key) {
            result.rows.push(ScanRow {
                key: key.clone(),
                features: BTreeMap::new(),
                score: None,
                reason: "unavailable_or_stale".into(),
                selected: false,
                trigger_eligible: None,
            });
        }
    }
    result.spec_hash =
        crate::replay::digest(&serde_json::to_vec(spec).map_err(|_| "strategy_spec_encoding")?);
    result.seal()?;
    Ok(result)
}

/// Retained scanner prices have no individual timestamps: move is over the retained
/// sample window, never labelled as a fabricated N-minute return.
pub fn extract_features(
    scanner: &crate::execution::Scanner,
    now: i64,
    max_age: i64,
) -> FeatureSnapshot {
    extract_features_with_metadata_age(scanner, now, max_age, max_age)
}
/// Streaming prices and REST metadata retain independent original receipts and
/// configured expiry windows; a fresh price never renews metadata.
pub fn extract_features_with_metadata_age(
    scanner: &crate::execution::Scanner,
    now: i64,
    max_age: i64,
    metadata_max_age: i64,
) -> FeatureSnapshot {
    extract_window(
        scanner,
        now,
        max_age,
        metadata_max_age,
        default_lookback_samples(),
        Some(&FeatureSettings::default()),
    )
}
/// Window length is part of both the spec and frozen input provenance. A window
/// counts retained observations, not elapsed minutes; available history may be shorter.
pub fn extract_features_with_lookback(
    scanner: &crate::execution::Scanner,
    now: i64,
    max_age: i64,
    metadata_max_age: i64,
    lookback_samples: usize,
) -> Result<FeatureSnapshot, String> {
    extract_features_with_settings(
        scanner,
        now,
        max_age,
        metadata_max_age,
        lookback_samples,
        &FeatureSettings::default(),
    )
}
/// Extract optional features with validated operator-configured proxy parameters.
pub fn extract_features_with_settings(
    scanner: &crate::execution::Scanner,
    now: i64,
    max_age: i64,
    metadata_max_age: i64,
    lookback_samples: usize,
    settings: &FeatureSettings,
) -> Result<FeatureSnapshot, String> {
    settings.validate()?;
    if !(1..=20).contains(&lookback_samples) {
        return Err("strategy_lookback_invalid".into());
    }
    Ok(extract_window(
        scanner,
        now,
        max_age,
        metadata_max_age,
        lookback_samples,
        Some(settings),
    ))
}
fn extract_window(
    scanner: &crate::execution::Scanner,
    now: i64,
    max_age: i64,
    metadata_max_age: i64,
    lookback_samples: usize,
    settings: Option<&FeatureSettings>,
) -> FeatureSnapshot {
    let legacy_screening = settings.is_none();
    let mut rows = Vec::new();
    for (key, s) in &scanner.samples {
        let age = now.checked_sub(s.scan_activity.unwrap_or(s.seen));
        let freshness = if s.scan_activity.is_some() {
            s.fresh_for_ms.unwrap_or(max_age)
        } else {
            max_age.min(s.fresh_for_ms.unwrap_or(max_age))
        };
        if matches!(
            s.reason.as_str(),
            "stale" | "invalid_quote" | "no_quote_returned"
        ) || (!legacy_screening && s.diagnostic.is_some())
        {
            continue;
        }
        if s.seen <= 0
            || age.is_none_or(|a| a < 0 || a > freshness)
            || s.source
                .filter(|_| s.scan_activity.is_none())
                .is_some_and(|t| now.checked_sub(t).is_none_or(|a| a < 0 || a > freshness))
        {
            continue;
        }
        let prices: Vec<f64> = s.history[s.history.len().saturating_sub(lookback_samples)..]
            .iter()
            .filter_map(|p| p.amount().to_string().parse().ok())
            .collect();
        let Some(current) = s
            .current
            .as_ref()
            .or_else(|| s.history.last())
            .and_then(|p| p.amount().to_string().parse::<f64>().ok())
            .filter(|p| *p > 0.0)
        else {
            continue;
        };
        let mut features = BTreeMap::from([(Feature::Price, current)]);
        if !legacy_screening
            && let Some(minutes) =
                crate::execution::minutes_since_open(s.seen, scanner.settings.session_start_minute)
        {
            features.insert(Feature::MinutesSinceOpen, minutes);
        }
        if let Some(first) = prices.first().filter(|p| **p > 0.0) {
            let change = (current / first - 1.0) * 10000.0;
            features.insert(Feature::MoveBps, change);
            features.insert(Feature::AbsoluteMoveBps, change.abs());
        }
        if prices.len() > 1 {
            let lo = prices.iter().copied().fold(current, f64::min);
            let hi = prices.iter().copied().fold(current, f64::max);
            if hi > lo {
                features.insert(Feature::RangePosition, (current - lo) / (hi - lo));
            }
            let returns: Vec<f64> = prices
                .windows(2)
                .filter(|w| w[0] > 0.0)
                .map(|w| (w[1] / w[0] - 1.0) * 10000.0)
                .collect();
            if !returns.is_empty() {
                let mean = returns.iter().sum::<f64>() / returns.len() as f64;
                features.insert(
                    Feature::VolatilityBps,
                    (returns.iter().map(|v| (v - mean).powi(2)).sum::<f64>()
                        / returns.len() as f64)
                        .sqrt(),
                );
            }
        }
        let intraday = s
            .intraday
            .as_ref()
            .filter(|data| data.fresh(now, metadata_max_age));
        if let Some(fields) = s.market_fields.as_ref().filter(|_| {
            s.reason != "missing_market_fields"
                && (legacy_screening
                    || intraday.is_some_and(|_| {
                        s.market_fields_receipt.is_some_and(|receipt| {
                            crate::intraday::Clock {
                                source: receipt,
                                receipt,
                            }
                            .same_session(now)
                        })
                    })
                    || s.market_fields_receipt.is_some_and(|t| {
                        now.checked_sub(t)
                            .is_some_and(|a| a >= 0 && a <= metadata_max_age)
                    }))
        }) {
            if !legacy_screening
                && s.intraday.is_none()
                && let Some(vwap) = fields.session_vwap.filter(|v| v.is_finite() && *v > 0.0)
            {
                features.insert(Feature::SessionVwap, vwap);
            }
            if s.intraday.is_none()
                && let Some(settings) = settings
                && let Some(value) = settings.relative_volume(
                    fields.volume,
                    s.avg_daily_volume,
                    crate::execution::minutes_since_open(
                        s.seen,
                        scanner.settings.session_start_minute,
                    ),
                )
            {
                features.insert(Feature::RelVolumeTod, value);
            }
            if !legacy_screening && s.intraday.is_none() {
                let bid = fields.best_bid.filter(|v| v.is_finite() && *v > 0.0);
                let ask = fields.best_ask.filter(|v| v.is_finite() && *v > 0.0);
                for (feature, value) in [
                    (Feature::BestBid, bid),
                    (Feature::BestAsk, ask),
                    (
                        Feature::BestBidQty,
                        fields.best_bid_qty.filter(|v| *v > 0).map(|v| v as f64),
                    ),
                    (
                        Feature::BestAskQty,
                        fields.best_ask_qty.filter(|v| *v > 0).map(|v| v as f64),
                    ),
                ] {
                    if let Some(value) = value {
                        features.insert(feature, value);
                    }
                }
                if let (Some(bid), Some(ask)) = (bid, ask)
                    && ask >= bid
                {
                    let mid = bid / 2.0 + ask / 2.0;
                    features.insert(Feature::SpreadFraction, (ask - bid) / mid);
                }
                if let (Some(buy), Some(sell)) =
                    (fields.total_buy_quantity, fields.total_sell_quantity)
                {
                    let sum = buy as f64 + sell as f64;
                    if sum > 0.0 {
                        features.insert(Feature::BookImbalance, buy as f64 / sum);
                    }
                }
            }
            if legacy_screening || s.intraday.is_none() {
                features.insert(Feature::TradedValue, current * fields.volume as f64);
            }
            if let (Ok(lo), Ok(hi)) = (
                fields.lower_circuit_limit.to_string().parse::<f64>(),
                fields.upper_circuit_limit.to_string().parse::<f64>(),
            ) && lo > 0.0
                && hi > lo
            {
                features.insert(
                    Feature::CircuitDistanceBps,
                    ((current - lo).min(hi - current)) / current * 10000.0,
                );
            }
        }
        if let (Some(data), Some(settings)) = (intraday, settings) {
            features.extend(data.touch_features());
            features.extend(data.vwap.map(|value| (Feature::SessionVwap, value)));
            features.extend(data.imbalance.map(|value| (Feature::BookImbalance, value)));
            if let Some(volume) = data.volume {
                features.insert(Feature::TradedValue, current * volume as f64);
                if let Some(value) = settings.relative_volume(
                    volume,
                    s.avg_daily_volume,
                    crate::execution::minutes_since_open(
                        data.clock.source,
                        scanner.settings.session_start_minute,
                    ),
                ) {
                    features.insert(Feature::RelVolumeTod, value);
                }
            }
        }
        features.retain(|_, v| v.is_finite());
        rows.push(FeatureRow {
            key: key.clone(),
            features,
        });
    }
    let mut moves: Vec<f64> = rows
        .iter()
        .filter_map(|r| r.features.get(&Feature::MoveBps).copied())
        .collect();
    moves.sort_by(f64::total_cmp);
    if !moves.is_empty() {
        let n = moves.len();
        let median = if n.is_multiple_of(2) {
            (moves[n / 2 - 1] + moves[n / 2]) / 2.0
        } else {
            moves[n / 2]
        };
        for row in &mut rows {
            if let Some(v) = row.features.get(&Feature::MoveBps).copied() {
                row.features.insert(Feature::MarketRelativeBps, v - median);
                row.features
                    .insert(Feature::AbsoluteMarketRelativeBps, (v - median).abs());
            }
        }
    }
    FeatureSnapshot {
        observed_ms: now,
        lookback_samples,
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec() -> StrategySpec {
        StrategySpec {
            id: "momentum".into(),
            version: 1,
            lookback_samples: 20,
            filters: vec![Filter {
                feature: Feature::TradedValue,
                op: Operator::GreaterEqual,
                threshold: 100.0,
            }],
            weights: vec![Weight {
                feature: Feature::MoveBps,
                weight: 1.0,
            }],
            normalization: Normalization::None,
            shortlist_size: 1,
            trigger_score: 0.0,
            cooldown_ms: 1000,
            trigger: None,
        }
    }
    #[test]
    fn ranks_filters_and_retains_rejected_evidence() {
        let snapshot = FeatureSnapshot {
            observed_ms: 42,
            lookback_samples: 20,
            rows: vec![
                FeatureRow {
                    key: "A".into(),
                    features: BTreeMap::from([
                        (Feature::MoveBps, 20.0),
                        (Feature::TradedValue, 200.0),
                    ]),
                },
                FeatureRow {
                    key: "B".into(),
                    features: BTreeMap::from([
                        (Feature::MoveBps, 40.0),
                        (Feature::TradedValue, 20.0),
                    ]),
                },
                FeatureRow {
                    key: "C".into(),
                    features: BTreeMap::new(),
                },
            ],
        };
        let result = evaluate(&spec(), &snapshot).unwrap();
        assert_eq!(result.rows.len(), 3);
        assert_eq!(result.rows[0].key, "A");
        assert!(result.rows[0].selected);
        assert!(!result.rows[1].selected);
        assert!(result.rows.iter().any(|r| r.reason == "missing_feature"));
        let mut ranked = spec();
        ranked.normalization = Normalization::CrossSectionalRank;
        assert_eq!(
            evaluate(&ranked, &snapshot).unwrap().rows[0].score,
            Some(0.0)
        );
        // The default preserves two-sided ranking without making every
        // shortlisted prospect a wake signal.
        let legacy = StrategySpec::default();
        let default_input = FeatureSnapshot {
            observed_ms: 42,
            lookback_samples: 20,
            rows: [("down", -300.0), ("quiet", 20.0), ("up", 250.0)]
                .into_iter()
                .map(|(key, change)| FeatureRow {
                    key: key.into(),
                    features: BTreeMap::from([
                        (Feature::Price, 100.0),
                        (Feature::TradedValue, 2_000_000.0),
                        (Feature::CircuitDistanceBps, 1000.0),
                        (Feature::AbsoluteMoveBps, f64::abs(change)),
                    ]),
                })
                .collect(),
        };
        let result = evaluate(&legacy, &default_input).unwrap();
        assert_eq!(
            result
                .rows
                .iter()
                .map(|r| r.key.as_str())
                .collect::<Vec<_>>(),
            ["down", "up", "quiet"]
        );
        assert!(result.rows.iter().all(|r| r.selected));
        assert_eq!(
            result
                .rows
                .iter()
                .map(|r| r.trigger_eligible)
                .collect::<Vec<_>>(),
            [Some(true), Some(true), Some(false)]
        );
        let mut missing_trigger = legacy;
        missing_trigger.trigger.as_mut().unwrap().feature = Feature::RangePosition;
        let unavailable = evaluate(&missing_trigger, &default_input).unwrap();
        assert!(
            unavailable
                .rows
                .iter()
                .all(|row| !row.selected && row.reason == "missing_trigger_feature")
        );
    }
    #[test]
    fn scan_identity_binds_spec_and_all_evidence() {
        let input = FeatureSnapshot {
            observed_ms: 42,
            lookback_samples: 20,
            rows: vec![FeatureRow {
                key: "A".into(),
                features: BTreeMap::from([(Feature::MoveBps, 20.0)]),
            }],
        };
        let original = evaluate(&spec(), &input).unwrap();
        assert_eq!(original.scan_id, evaluate(&spec(), &input).unwrap().scan_id);
        let mut changed = spec();
        changed.weights[0].weight = 2.0;
        let mutated = evaluate(&changed, &input).unwrap();
        assert_ne!(original.spec_hash, mutated.spec_hash);
        assert_ne!(original.scan_id, mutated.scan_id);
        let mut augmented = original.clone();
        augmented.rows[0].reason = "unavailable_or_stale".into();
        augmented.seal().unwrap();
        assert_ne!(original.scan_id, augmented.scan_id);
        let id = augmented.scan_id.clone();
        augmented.seal().unwrap();
        assert_eq!(id, augmented.scan_id);
    }
    #[test]
    fn configured_spec_admission_is_fail_closed() {
        let legacy: StrategySettings = serde_json::from_str(r#"{"specs":[]}"#).unwrap();
        assert!(legacy.validate().is_ok());
        assert!(
            legacy
                .admission_bounds
                .admit(&StrategySpec::default())
                .is_ok()
        );
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("admission_bounds")
                .is_none()
        );
        let defaults = serde_json::to_value(SpecAdmissionBounds::default()).unwrap();
        // Every ceiling admits its exact boundary and rejects one step beyond it.
        for (field, boundary) in [
            ("max_features", 4.0),
            ("max_filters", 3.0),
            ("max_weights", 1.0),
            ("max_lookback_samples", 20.0),
            ("max_shortlist_size", 10.0),
            ("max_cooldown_ms", 900_000.0),
            ("max_abs_weight", 1.0),
            ("max_abs_filter_threshold", 1_000_000.0),
            ("max_abs_trigger_score", 0.0),
            ("max_trigger_threshold", 200.0),
        ] {
            let mut config = defaults.clone();
            config[field] = serde_json::json!(if field.starts_with("max_abs")
                || field == "max_trigger_threshold"
            {
                serde_json::json!(boundary)
            } else {
                serde_json::json!(boundary as u64)
            });
            let bounds: SpecAdmissionBounds = serde_json::from_value(config.clone()).unwrap();
            assert!(bounds.admit(&StrategySpec::default()).is_ok(), "{field}");
            if boundary > 1.0 {
                config[field] = if field.starts_with("max_abs") || field == "max_trigger_threshold"
                {
                    serde_json::json!(boundary - 1.0)
                } else {
                    serde_json::json!((boundary - 1.0) as u64)
                };
                let tighter: SpecAdmissionBounds = serde_json::from_value(config).unwrap();
                assert!(tighter.admit(&StrategySpec::default()).is_err(), "{field}");
            } else {
                let mut spec = StrategySpec::default();
                match field {
                    "max_weights" => spec.weights.push(spec.weights[0].clone()),
                    "max_abs_weight" => spec.weights[0].weight = 1.01,
                    "max_abs_trigger_score" => spec.trigger_score = -0.01,
                    _ => unreachable!(),
                }
                assert!(bounds.admit(&spec).is_err(), "{field}");
            }
        }
        for field in defaults.as_object().unwrap().keys() {
            let mut invalid = defaults.clone();
            invalid[field] = serde_json::json!(-1);
            assert!(
                serde_json::from_value::<SpecAdmissionBounds>(invalid)
                    .map_or(true, |b| b.validate().is_err()),
                "{field}"
            );
        }
        for (field, value) in defaults.as_object().unwrap() {
            let mut invalid = defaults.clone();
            invalid[field] = if value.is_u64() {
                serde_json::json!(value.as_u64().unwrap() + 1)
            } else {
                serde_json::json!(value.as_f64().unwrap() * 2.0)
            };
            assert!(
                serde_json::from_value::<SpecAdmissionBounds>(invalid)
                    .unwrap()
                    .validate()
                    .is_err(),
                "{field}"
            );
        }
        let mut invalid = SpecAdmissionBounds {
            max_abs_weight: f64::NAN,
            ..Default::default()
        };
        assert!(invalid.validate().is_err());
        invalid.max_abs_weight = f64::INFINITY;
        assert!(invalid.validate().is_err());
        assert!(
            serde_json::from_str::<StrategySettings>(r#"{"admission_bounds":{"typo":1}}"#).is_err()
        );
        let mut settings = StrategySettings {
            specs: vec![StrategySpec::default()],
            ..Default::default()
        };
        settings.admission_bounds.max_specs = 1;
        assert!(settings.validate().is_ok());
        settings.specs.push(StrategySpec {
            id: "other".into(),
            ..Default::default()
        });
        assert_eq!(settings.validate().unwrap_err(), "strategy_spec_limit");
        settings.specs.pop();
        settings.admission_bounds.max_features = 3;
        assert_eq!(
            settings.validate().unwrap_err(),
            "strategy_spec_admission_limit"
        );
        settings.specs.clear();
        settings.admission_bounds.max_lookback_samples = 0;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn invalid_specs_and_nonfinite_inputs_fail_closed() {
        let mut invalid = spec();
        invalid.weights[0].weight = f64::INFINITY;
        assert!(invalid.validate().is_err());
        assert!(serde_json::from_str::<Feature>("\"invented\"").is_err());
        let snapshot = FeatureSnapshot {
            observed_ms: 0,
            lookback_samples: 20,
            rows: vec![FeatureRow {
                key: "A".into(),
                features: BTreeMap::from([(Feature::MoveBps, f64::NAN)]),
            }],
        };
        assert!(evaluate(&spec(), &snapshot).is_err());
        assert!(!StrategySettings::default().enabled);
        let mut scanner = crate::execution::Scanner::default();
        scanner.samples.insert(
            "A".into(),
            crate::execution::Sample {
                seen: 10_000,
                source: Some(10_000),
                current: Some(crate::finance::Price::parse("100").unwrap()),
                history: vec![crate::finance::Price::parse("100").unwrap()],
                reason: "eligible_no_signal".into(),
                market_fields: Some(crate::execution::MarketFields {
                    session_vwap: None,
                    best_bid: None,
                    best_bid_qty: None,
                    best_ask: None,
                    best_ask_qty: None,
                    total_buy_quantity: None,
                    total_sell_quantity: None,
                    lower_circuit_limit: crate::finance::Amount::parse("80").unwrap(),
                    upper_circuit_limit: crate::finance::Amount::parse("120").unwrap(),
                    volume: 100_000,
                }),
                market_fields_receipt: Some(1000),
                ..Default::default()
            },
        );
        assert!(
            !extract_features(&scanner, 10_001, 2000).rows[0]
                .features
                .contains_key(&Feature::TradedValue)
        );
        let streaming = extract_features_with_metadata_age(&scanner, 10_001, 2000, 300_000);
        assert!(
            streaming.rows[0]
                .features
                .contains_key(&Feature::TradedValue)
        );
        for (value, expected) in [
            (Some(99.5), Some(99.5)),
            (None, None),
            (Some(f64::NAN), None),
            (Some(f64::INFINITY), None),
            (Some(0.0), None),
            (Some(-1.0), None),
        ] {
            scanner
                .samples
                .get_mut("A")
                .unwrap()
                .market_fields
                .as_mut()
                .unwrap()
                .session_vwap = value;
            let snapshot = extract_features_with_metadata_age(&scanner, 10_001, 2000, 300_000);
            assert_eq!(
                snapshot.rows[0]
                    .features
                    .get(&Feature::SessionVwap)
                    .copied(),
                expected
            );
        }
        // Proxy boundaries use the original receipt, never the extraction clock.
        let open = 1_792_986_300_000; // 2026-10-26 09:15 IST
        let settings: FeatureSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, FeatureSettings::default());
        for (minutes, expected) in [
            (0.0, None),
            (4.999, None),
            (5.0, Some(5.0)),
            (375.0, Some(375.0)),
            (400.0, Some(375.0)),
        ] {
            let sample = scanner.samples.get_mut("A").unwrap();
            sample.seen = open + (minutes * 60_000.0) as i64;
            sample.source = Some(sample.seen);
            sample.market_fields_receipt = Some(sample.seen);
            sample.avg_daily_volume = Some(100_000.0);
            let now = sample.seen;
            let snapshot = extract_features(&scanner, now, 2000);
            let actual = snapshot.rows[0]
                .features
                .get(&Feature::RelVolumeTod)
                .copied();
            let expected = expected.map(|m: f64| 1.0 / (m / 375.0).powf(0.7));
            match (actual, expected) {
                (Some(a), Some(b)) => assert!((a - b).abs() < 1e-9),
                (None, None) => {}
                pair => panic!("unexpected proxy: {pair:?}"),
            }
        }
        let now = scanner.samples["A"].seen;
        scanner.samples.get_mut("A").unwrap().market_fields_receipt = Some(now - 2001);
        assert!(
            !extract_features(&scanner, now, 2000).rows[0]
                .features
                .contains_key(&Feature::RelVolumeTod)
        );
        scanner.samples.get_mut("A").unwrap().market_fields_receipt = Some(now);
        let custom = FeatureSettings {
            session_minutes: 800.0,
            ..settings.clone()
        };
        let custom_value = extract_features_with_settings(&scanner, now, 2000, 2000, 20, &custom)
            .unwrap()
            .rows[0]
            .features[&Feature::RelVolumeTod];
        assert!((custom_value - 1.0 / 0.5_f64.powf(0.7)).abs() < 1e-9);
        for average in [
            None,
            Some(0.0),
            Some(-1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            scanner.samples.get_mut("A").unwrap().avg_daily_volume = average;
            assert!(
                !extract_features(&scanner, now, 2000).rows[0]
                    .features
                    .contains_key(&Feature::RelVolumeTod)
            );
        }
        assert_eq!(settings.relative_volume(10, Some(100.0), None), None);
        let linear = FeatureSettings {
            rel_volume_curve_exponent: 1.0,
            ..settings.clone()
        };
        assert!(
            linear
                .relative_volume(100, Some(100.0), Some(100.0))
                .unwrap()
                > settings
                    .relative_volume(100, Some(100.0), Some(100.0))
                    .unwrap()
        );
        assert_eq!(
            settings.relative_volume(0, Some(100.0), Some(5.0)),
            Some(0.0)
        );
        for invalid in [
            FeatureSettings {
                session_minutes: 0.0,
                ..settings.clone()
            },
            FeatureSettings {
                rel_volume_curve_exponent: f64::NAN,
                ..settings.clone()
            },
            FeatureSettings {
                rel_volume_curve_exponent: 0.0,
                ..settings.clone()
            },
            FeatureSettings {
                rel_volume_min_minutes: -1.0,
                ..settings.clone()
            },
            FeatureSettings {
                rel_volume_min_minutes: 376.0,
                ..settings.clone()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        // Restore the existing metadata/window scenarios below.
        let sample = scanner.samples.get_mut("A").unwrap();
        sample.seen = 10_000;
        sample.source = Some(10_000);
        sample.market_fields_receipt = Some(1000);
        assert_eq!(scanner.samples["A"].market_fields_receipt, Some(1000));
        scanner.samples.get_mut("A").unwrap().reason = "missing_market_fields".into();
        assert!(
            !extract_features_with_metadata_age(&scanner, 10_001, 2000, 300_000).rows[0]
                .features
                .contains_key(&Feature::TradedValue)
        );
        let sample = scanner.samples.get_mut("A").unwrap();
        sample.reason = "eligible_no_signal".into();
        sample.history = ["80", "90", "100"]
            .into_iter()
            .map(|price| crate::finance::Price::parse(price).unwrap())
            .collect();
        let full = extract_features_with_metadata_age(&scanner, 10_001, 2000, 300_000);
        let short = extract_features_with_lookback(&scanner, 10_001, 2000, 300_000, 2).unwrap();
        assert_eq!(short.lookback_samples, 2);
        assert!(
            full.rows[0].features[&Feature::MoveBps] > short.rows[0].features[&Feature::MoveBps]
        );
        assert_eq!(scanner.samples["A"].history.len(), 3);
        let mut windowed = spec();
        assert_eq!(
            evaluate(&windowed, &short).unwrap_err(),
            "strategy_feature_window_mismatch"
        );
        windowed.lookback_samples = 2;
        assert!(evaluate(&windowed, &short).is_ok());
        assert!(extract_features_with_lookback(&scanner, 10_001, 2000, 300_000, 0).is_err());
    }
}
