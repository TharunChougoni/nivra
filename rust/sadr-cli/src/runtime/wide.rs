//! Dated cash-only scanner and single durable portfolio owner.
use super::orchestrator_model::Trigger;
use super::*;
use super::{Fail, LIVE_ENABLED, failure_code};
use crate::alert::{Alerts, Event};
use sadr_cli::lock::AdvisoryLock;
use sadr_core::finance::{Amount, Price};
use std::collections::{BTreeMap, BTreeSet};
mod alerts;
mod controller;
pub(super) use controller::run;
mod projection;
use projection::{execution_projection, usage_projection};
mod universe;
use alerts::{scanner_fresh_drop, scanner_fresh_drop_feed_ready};
use sadr_core::execution::{Portfolio, Scanner, a};
use sadr_core::feature_store::FeatureStore;
use universe::{Instrument, Universe};

fn session_features(c: &Config, config: Option<&planner::PlannerConfig>) -> Result<FeatureStore> {
    let settings = config
        .and_then(|c| c.discovery.as_ref())
        .map(|d| d.feature_store.clone())
        .unwrap_or_default();
    let mut store = FeatureStore::new(settings)?;
    let session_open_ms = chrono::NaiveTime::from_num_seconds_from_midnight_opt(
        c.execution.session_start_minute.saturating_mul(60),
        0,
    )
    .and_then(|time| {
        c.session
            .and_time(time)
            .and_local_timezone(chrono::FixedOffset::east_opt(19_800)?)
            .single()
    })
    .map(|stamp| stamp.timestamp_millis());
    store.reset_for_session_at(c.session, Some(millis()), session_open_ms);
    Ok(store)
}

pub(super) fn observe_feature(store: &mut FeatureStore, key: &str, price: Price, receipt: i64) {
    if let Ok(price) = price.amount().to_string().parse::<f64>() {
        // Shadow-only rejection must never interrupt the financial path.
        let _ = store.observe(key, price, receipt);
    }
}

/// Read-only NSE index context for scanner/digest use. Index levels are never
/// tradeable: they never enter `Universe::instruments`, the cash scanner
/// ranking, or paper execution admission.
#[derive(Clone, Debug, Default)]
pub(super) struct IndexContext {
    observations: BTreeMap<String, IndexObservation>,
    regime: Option<IndexRegime>,
}

#[derive(Clone, Debug)]
struct IndexObservation {
    price: Price,
    source: i64,
    receipt: i64,
    prior_close: Option<Price>,
    change_bps: Option<f64>,
    fresh: bool,
}

#[derive(Clone, Debug)]
struct IndexRegime {
    label: String,
    inputs: sadr_core::regime::RegimeInputs,
}

impl IndexContext {
    /// Capture configured index points from the existing feed snapshot. Missing
    /// keys stay missing; nothing is invented or promoted to a trade candidate.
    pub(super) fn capture(
        c: &Config,
        points: &BTreeMap<String, super::market_feed::Point>,
        store: &FeatureStore,
        settings: &super::market_feed::Settings,
        now: i64,
        activity: Option<i64>,
    ) -> Self {
        let fresh = activity.is_some_and(|at| {
            now.checked_sub(at)
                .is_some_and(|age| (0..=settings.scan_activity_timeout_ms).contains(&age))
        });
        let mut observations = BTreeMap::new();
        for key in &c.index_feeds.keys {
            let Some(point) = points.get(key) else {
                continue;
            };
            let change_bps = point.prior_close.and_then(|prior| {
                let price = price_f64(point.price)?;
                let change = (price / price_f64(prior)? - 1.0) * 10_000.0;
                change.is_finite().then_some(change)
            });
            observations.insert(
                key.clone(),
                IndexObservation {
                    price: point.price,
                    source: point.source,
                    receipt: point.receipt,
                    prior_close: point.prior_close,
                    change_bps,
                    fresh,
                },
            );
        }
        let regime = c
            .index_feeds
            .regime
            .as_ref()
            .and_then(|cfg| index_regime(cfg, &c.index_feeds.keys, points, store));
        Self {
            observations,
            regime,
        }
    }

    /// Additive digest/status projection. `tradeable: false` is explicit.
    pub(super) fn json(&self) -> serde_json::Value {
        let observations = self
            .observations
            .iter()
            .map(|(key, o)| {
                serde_json::json!({
                    "instrument_key": key,
                    "price": o.price.amount().to_string(),
                    "source_ms": o.source,
                    "receipt_ms": o.receipt,
                    "prior_close": o.prior_close.map(|p| p.amount().to_string()),
                    "change_bps": o.change_bps,
                    "fresh": o.fresh,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "paper_only": true,
            "tradeable": false,
            "observations": observations,
            "regime": self.regime.as_ref().map(|r| serde_json::json!({
                "label": r.label,
                "advisory_only": true,
                "session_return": r.inputs.session_return,
                "opening_gap": r.inputs.opening_gap,
                "realised_volatility": r.inputs.realised_volatility,
                "breadth": r.inputs.breadth,
                "instrument_count": r.inputs.instrument_count,
                "minute_count": r.inputs.minute_count,
            })),
            "limitations": [
                "Index levels are non-tradeable context; they never enter the cash universe or paper execution",
                "Regime labels are advisory digest context and grant no trade authority"
            ]
        })
    }
}

fn price_f64(price: Price) -> Option<f64> {
    price
        .amount()
        .to_string()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

/// Advisory index regime: session return/gap from the prior close and observed
/// session open, realised volatility from index minute returns. Context only.
fn index_regime(
    cfg: &sadr_core::regime::RegimeConfig,
    keys: &[String],
    points: &BTreeMap<String, super::market_feed::Point>,
    store: &FeatureStore,
) -> Option<IndexRegime> {
    let mut prior = Vec::new();
    let mut opening = Vec::new();
    let mut current = Vec::new();
    for key in keys {
        let point = points.get(key)?;
        let bars = store.bars(key)?;
        prior.push(price_f64(point.prior_close?)?);
        opening.push(bars.front()?.open);
        current.push(bars.back()?.close);
    }
    let minute_returns = store.market_minute_returns(1);
    let inputs =
        sadr_core::regime::compute_inputs(&prior, &opening, &current, &minute_returns).ok()?;
    let label = sadr_core::regime::classify(&inputs, cfg).label();
    Some(IndexRegime { label, inputs })
}
fn note_feed_health(store: &Arc<Mutex<FeatureStore>>, at: i64, healthy: bool) {
    if let Ok(mut store) = store.lock() {
        store.record_feed_health(at, healthy);
    }
}
fn shadow_regime_inputs(
    scan: &Scanner,
    now: i64,
    max_age: i64,
    minute_returns: &[f64],
) -> sadr_core::regime::RegimeInputs {
    let mut prior = Vec::new();
    let mut opening = Vec::new();
    let mut current = Vec::new();
    for sample in scan.samples.values() {
        let Some((receipt, close, open)) = sample.regime_references else {
            continue;
        };
        if local(receipt).date_naive() != local(now).date_naive()
            || sample.seen <= 0
            || !now
                .checked_sub(sample.scan_activity.unwrap_or(sample.seen))
                .is_some_and(|age| {
                    (0..=sample.fresh_for_ms.unwrap_or(max_age).min(max_age)).contains(&age)
                })
        {
            continue;
        }
        let Some(price) = sample.current else {
            continue;
        };
        let as_f64 = |p: Price| p.amount().to_string().parse::<f64>().unwrap_or(f64::NAN);
        prior.push(as_f64(close));
        opening.push(as_f64(open));
        current.push(as_f64(price));
    }
    sadr_core::regime::compute_inputs(&prior, &opening, &current, minute_returns)
        .expect("aligned regime slices")
}

fn bounded_name(name: &str) -> String {
    name.chars().take(128).collect()
}

#[derive(Clone, Debug)]
struct LiveFallback {
    reason: String,
    detail: String,
    from: String,
    at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LiveFailure {
    Library,
    MissingSource,
    SourceTooLarge,
    InvalidSource,
    Sandbox(sadr_sandbox::Failure),
    InvalidScores,
    NoEligibleCandidates,
    Input,
}
impl LiveFailure {
    fn identity(self) -> (&'static str, &'static str) {
        match self {
            Self::Library => ("ScorerFailure", "library_unavailable"),
            Self::MissingSource => ("ScorerFailure", "missing_source"),
            Self::SourceTooLarge => ("ScorerFailure", "source_too_large"),
            Self::InvalidSource => ("ScorerFailure", "invalid_source"),
            Self::Sandbox(failure) => ("ScorerFailure", sandbox_failure_detail(failure)),
            Self::InvalidScores => ("ScorerFailure", "invalid_scores"),
            Self::NoEligibleCandidates => ("NoEligibleCandidates", "no_scored_candidates"),
            Self::Input => ("ScorerFailure", "invalid_input"),
        }
    }
    fn from_library(error: sadr_sandbox::library::Error) -> Self {
        use sadr_sandbox::library::Error::*;
        match error {
            NotFound => Self::MissingSource,
            SizeLimit => Self::SourceTooLarge,
            InvalidInput | InvalidUtf8 | HashMismatch | Ambiguous => Self::InvalidSource,
            Storage | WrongDatabase | HashNameConflict => Self::Library,
        }
    }
}
fn sandbox_failure_detail(failure: sadr_sandbox::Failure) -> &'static str {
    use sadr_sandbox::Failure::*;
    match failure {
        Config => "sandbox_config",
        Input => "sandbox_input",
        Setup => "sandbox_setup",
        Unavailable => "sandbox_unavailable",
        Exit => "sandbox_exit",
        WallTimeout => "sandbox_wall_timeout",
        OutputLimit => "sandbox_output_limit",
        OutputInvalid => "sandbox_output_invalid",
    }
}
fn selection_default_fallback(
    reason: sadr_sandbox::selector::SelectionReason,
) -> (&'static str, &'static str) {
    use sadr_sandbox::selector::SelectionReason::*;
    match reason {
        UnknownRegime => ("UnknownRegime", "unknown_regime"),
        NoEligibleCandidates => ("NoEligibleCandidates", "no_eligible_candidates"),
        StaleData => ("StaleData", "stale_data"),
        ScorerFailure => ("ScorerFailure", "library_unavailable"),
        PaperFloorBreach => ("PaperFloorBreach", "paper_floor_breach"),
        InvalidConfig => ("InvalidConfig", "invalid_config"),
        BestEvidence | Cooldown | Hysteresis => ("DefaultSelected", "default_selected"),
    }
}

#[derive(Default)]
struct ShadowSelection {
    last_evaluation: Option<i64>,
    last_live: Option<i64>,
    state: sadr_sandbox::selector::SelectorState,
    reason: Option<sadr_sandbox::selector::SelectionReason>,
    regime: Option<String>,
    status: Option<serde_json::Value>,
    mode: super::discovery::StrategySelectionMode,
    chosen_name: Option<String>,
    previous_active: Option<String>,
    live: Option<super::discovery::LiveRanking>,
    source: Option<sadr_sandbox::library::Version>,
    fallback: Option<LiveFallback>,
    fallback_count: u64,
    recent: std::collections::VecDeque<serde_json::Value>,
}
impl ShadowSelection {
    fn push_event(
        &mut self,
        kind: &str,
        from: &str,
        to: &str,
        reason: &str,
        detail: Option<&str>,
        now: i64,
    ) {
        let mut event =
            serde_json::json!({"at_ms":now,"kind":kind,"from":from,"to":to,"reason":reason});
        if let Some(detail) = detail {
            event["detail"] = serde_json::json!(detail);
        }
        if self.recent.back().is_some_and(|last| last == &event) {
            return;
        }
        if self.recent.len() >= 8 {
            self.recent.pop_front();
        }
        self.recent.push_back(event);
    }
    fn set_fallback(&mut self, reason: &str, detail: &str, now: i64, from: &str) -> bool {
        let changed = self.fallback.as_ref().is_none_or(|fallback| {
            fallback.reason != reason || fallback.detail != detail || fallback.from != from
        });
        self.fallback = Some(LiveFallback {
            reason: reason.into(),
            detail: detail.into(),
            from: from.into(),
            at_ms: now,
        });
        if changed {
            self.fallback_count = self.fallback_count.saturating_add(1);
            self.push_event("fallback", from, "default", reason, Some(detail), now);
            eprintln!(
                "strategy_selection_fallback {}",
                serde_json::json!({
                    "paper_only":true,"at_ms":now,"chosen_id":from,
                    "chosen_name":self.chosen_name,"active":self.state.active,
                    "previous_active":self.previous_active,"regime":self.regime(),
                    "mode":self.mode.as_str(),"ranking":"default",
                    "reason":reason,"detail":detail
                })
            );
        }
        changed
    }
    fn clear_fallback(&mut self, now: i64) {
        if self.fallback.take().is_some() {
            let active = self.state.active.clone();
            let reason = self
                .reason
                .map(|reason| format!("{reason:?}"))
                .unwrap_or_else(|| "BestEvidence".into());
            self.push_event("recovered", "default", &active, &reason, None, now);
            eprintln!(
                "strategy_selection_recovered {}",
                serde_json::json!({
                    "paper_only":true,"at_ms":now,"chosen_id":active,
                    "chosen_name":self.chosen_name,"previous_active":self.previous_active,
                    "regime":self.regime(),"mode":self.mode.as_str(),
                    "ranking":"selected","reason":reason
                })
            );
        }
    }
    fn regime(&self) -> serde_json::Value {
        self.regime
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null)
    }
    fn refresh_status(&mut self) {
        let Some(status) = self.status.as_mut() else {
            return;
        };
        status["mode"] = serde_json::json!(self.mode.as_str());
        status["shadow_only"] = serde_json::json!(!self.mode.is_live());
        status["active"] = serde_json::json!(self.state.active);
        status["previous_active"] = serde_json::json!(self.previous_active);
        status["chosen_name"] = serde_json::json!(self.chosen_name);
        status["live_applied"] = serde_json::json!(self.live.is_some());
        status["live_scored_count"] =
            serde_json::json!(self.live.as_ref().map_or(0, |live| live.scores.len()));
        status["fallback_count"] = serde_json::json!(self.fallback_count);
        status["fallback"] = self
            .fallback
            .as_ref()
            .map_or(serde_json::Value::Null, |fallback| {
                serde_json::json!({
                    "reason":fallback.reason,"detail":fallback.detail,"from":fallback.from,
                    "at_ms":fallback.at_ms
                })
            });
        status["recent"] = serde_json::json!(self.recent.iter().cloned().collect::<Vec<_>>());
    }
    fn evaluate(
        &mut self,
        settings: &super::discovery::Settings,
        scan: &Scanner,
        now: i64,
        stale: bool,
        minute_returns: &[f64],
    ) {
        use sadr_sandbox::{
            library::Library,
            selector::{CandidateEvidence, ScorerHealth, select},
        };
        let Some(cfg) = settings.strategy_selection.as_ref().filter(|s| s.enabled) else {
            self.mode = super::discovery::StrategySelectionMode::Shadow;
            self.live = None;
            return;
        };
        if self
            .last_evaluation
            .is_some_and(|at| now.saturating_sub(at) < cfg.evaluate_ms)
        {
            return;
        }
        self.last_evaluation = Some(now);
        self.last_live = None;
        self.mode = cfg.mode;
        let previous = self.state.active.clone();
        let inputs = shadow_regime_inputs(scan, now, settings.max_scan_age_ms, minute_returns);
        let regime = sadr_core::regime::classify(&inputs, &cfg.regime);
        let label = regime.label();
        self.regime = Some(label.clone());
        // Shadow retains its permissive initialize-on-read behavior. Live ranking
        // must never create or migrate a library while deciding paper candidates.
        let loaded = if cfg.mode.is_live() {
            Library::open_read_only(&cfg.library_path).and_then(|db| db.all_versions_with_results())
        } else {
            Library::open(&cfg.library_path).and_then(|db| db.all_versions_with_results())
        };
        let health = ScorerHealth {
            stale_data: stale,
            scorer_failure: loaded.is_err(),
        };
        let rows = loaded.unwrap_or_default();
        let candidates = rows
            .iter()
            .map(|(sha, name, results)| CandidateEvidence {
                sha256: sha,
                name,
                results,
            })
            .collect::<Vec<_>>();
        let selection = select(
            &candidates,
            (regime != sadr_core::regime::Regime::Unknown).then_some(label.as_str()),
            &self.state,
            &health,
            now,
            &cfg.selector,
        );
        let changed = selection.switched || self.reason != Some(selection.reason);
        let chosen_name = rows
            .iter()
            .find(|(sha, _, _)| sha == &selection.chosen_id)
            .map(|(_, name, _)| bounded_name(name));
        self.reason = Some(selection.reason);
        self.previous_active = Some(previous.clone());
        self.chosen_name = chosen_name.clone();
        self.state = selection.state.clone();
        self.live = None;
        if self
            .source
            .as_ref()
            .is_some_and(|version| version.sha256 != self.state.active)
        {
            self.source = None;
        }
        if cfg.mode.is_live() && self.state.active == sadr_sandbox::selector::DEFAULT_ID {
            let (reason, detail) = selection_default_fallback(selection.reason);
            self.set_fallback(reason, detail, now, &previous);
        }
        if changed {
            self.push_event(
                if selection.switched {
                    "switch"
                } else {
                    "reason"
                },
                &previous,
                &selection.chosen_id,
                &format!("{:?}", selection.reason),
                None,
                now,
            );
        }
        let status = serde_json::json!({"shadow_only":cfg.mode.as_str()=="shadow","mode":cfg.mode.as_str(),"paper_only":true,"as_of_ms":now,
            "regime":label,"instrument_count":inputs.instrument_count,"minute_count":inputs.minute_count,
            "chosen_id":selection.chosen_id,"chosen_name":chosen_name,"previous_active":previous,
            "active":self.state.active,"reason":format!("{:?}", selection.reason),
            "switched":selection.switched,"state":self.state,
            "scorer_failure":health.scorer_failure,"stale_data":health.stale_data,
            "live_applied":false,"live_scored_count":0,"fallback_count":self.fallback_count,
            "fallback":self.fallback.as_ref().map_or(serde_json::Value::Null, |fallback| serde_json::json!({
                "reason":fallback.reason,"detail":fallback.detail,"from":fallback.from,"at_ms":fallback.at_ms
            })),
            "recent":self.recent.iter().cloned().collect::<Vec<_>>()});
        if changed {
            eprintln!("strategy_selection {status}");
        }
        self.status = Some(status);
    }
    fn version(
        &mut self,
        path: &str,
        sha: &str,
    ) -> std::result::Result<sadr_sandbox::library::Version, LiveFailure> {
        if let Some(version) = self.source.as_ref().filter(|version| version.sha256 == sha) {
            return Ok(version.clone());
        }
        let db = sadr_sandbox::library::Library::open_read_only(path)
            .map_err(|_| LiveFailure::Library)?;
        let version = db.version_by_sha(sha).map_err(LiveFailure::from_library)?;
        self.source = Some(version.clone());
        Ok(version)
    }
    fn live_ranking(
        &mut self,
        settings: &super::discovery::Settings,
        scan: &Scanner,
        instruments: &[Instrument],
        projection: &serde_json::Value,
        now: i64,
    ) -> Option<super::discovery::LiveRanking> {
        let Some(cfg) = settings.strategy_selection.as_ref().filter(|s| s.enabled) else {
            self.mode = super::discovery::StrategySelectionMode::Shadow;
            self.live = None;
            return None;
        };
        if !cfg.mode.is_live() {
            self.mode = super::discovery::StrategySelectionMode::Shadow;
            self.live = None;
            return None;
        }
        self.mode = super::discovery::StrategySelectionMode::Live;
        if self.state.active == sadr_sandbox::selector::DEFAULT_ID {
            self.live = None;
            self.refresh_status();
            return None;
        }
        // Reuse the last score set between selection evaluations; the sandbox
        // runs at the selector interval, not on every scanner rank tick.
        if self
            .last_live
            .is_some_and(|at| now.saturating_sub(at) < cfg.evaluate_ms)
        {
            if let Some(live) = self.live.as_ref() {
                if live_overlaps(live, projection) {
                    return Some(live.clone());
                }
            } else {
                return None;
            }
        }
        self.last_live = Some(now);
        let sha = self.state.active.clone();
        let version = match self.version(&cfg.library_path, &sha) {
            Ok(version) => version,
            Err(failure) => {
                self.live = None;
                let (reason, detail) = failure.identity();
                self.set_fallback(reason, detail, now, &sha);
                self.refresh_status();
                return None;
            }
        };
        match score_live(settings, &version, scan, instruments, projection, now) {
            Ok(ranking) => {
                self.live = Some(ranking.clone());
                self.clear_fallback(now);
                self.refresh_status();
                Some(ranking)
            }
            Err(failure) => {
                self.live = None;
                let (reason, detail) = failure.identity();
                self.set_fallback(reason, detail, now, &sha);
                self.refresh_status();
                None
            }
        }
    }
    fn cached_live(&self) -> Option<&super::discovery::LiveRanking> {
        self.live.as_ref()
    }
    fn attach(&self, discovery: &mut Option<serde_json::Value>) {
        if let (Some(discovery), Some(status)) = (discovery, &self.status) {
            discovery["strategy_selection"] = status.clone();
        }
    }
}

fn live_overlaps(live: &super::discovery::LiveRanking, projection: &serde_json::Value) -> bool {
    let Some(prospects) = projection["prospects"].as_array() else {
        return true;
    };
    prospects.is_empty()
        || prospects.iter().any(|p| {
            p["instrument_key"]
                .as_str()
                .is_some_and(|key| live.scores.contains_key(key))
        })
}

fn score_live(
    settings: &super::discovery::Settings,
    version: &sadr_sandbox::library::Version,
    scan: &Scanner,
    instruments: &[Instrument],
    projection: &serde_json::Value,
    now: i64,
) -> std::result::Result<super::discovery::LiveRanking, LiveFailure> {
    let mut cache = scan.clone();
    cache
        .samples
        .retain(|key, _| instruments.iter().any(|i| &i.key == key));
    let strategy = settings.strategy.clone().unwrap_or_default();
    let metadata_age = settings
        .market_feed
        .as_ref()
        .map(|feed| feed.metadata_stale_ms)
        .unwrap_or(settings.max_scan_age_ms);
    let snapshot = sadr_core::strategy::extract_features_with_settings(
        &cache,
        now,
        settings.max_scan_age_ms,
        metadata_age,
        sadr_core::strategy::default_lookback_samples(),
        &strategy.feature_settings,
    )
    .map_err(|_| LiveFailure::Input)?;
    let mut features = BTreeMap::new();
    for row in snapshot.rows {
        if row.key == "params" {
            return Err(LiveFailure::InvalidScores);
        }
        features.insert(
            row.key,
            serde_json::to_value(&row.features).map_err(|_| LiveFailure::Input)?,
        );
    }
    if features.is_empty() {
        return Err(LiveFailure::NoEligibleCandidates);
    }
    if version.inputs.is_object() {
        features.insert("params".into(), version.inputs.clone());
    }
    let scores = sadr_sandbox::score_source(
        &settings.scorer_sandbox.clone().unwrap_or_default(),
        version.file_body.as_bytes(),
        &features,
    )
    .map_err(LiveFailure::Sandbox)?;
    if scores.contains_key("params")
        || scores.is_empty()
        || scores
            .iter()
            .any(|(key, score)| key.is_empty() || !features.contains_key(key) || !score.is_finite())
    {
        return Err(if scores.is_empty() {
            LiveFailure::NoEligibleCandidates
        } else {
            LiveFailure::InvalidScores
        });
    }
    let ranking = super::discovery::LiveRanking {
        sha256: version.sha256.clone(),
        name: bounded_name(&version.name),
        scores,
    };
    if !live_overlaps(&ranking, projection) {
        return Err(LiveFailure::NoEligibleCandidates);
    }
    Ok(ranking)
}

#[derive(Default)]
struct ShadowScorer {
    pending: Option<std::sync::mpsc::Receiver<serde_json::Value>>,
    #[cfg(test)]
    synchronous: bool,
    last_evaluation: Option<i64>,
    status: Option<serde_json::Value>,
}
impl ShadowScorer {
    fn poll(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(status) => {
                self.pending = None;
                self.record(status);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending = None,
        }
    }
    fn record(&mut self, status: serde_json::Value) {
        eprintln!("shadow_scorer {status}");
        self.status = Some(status);
    }
    fn evaluate(
        &mut self,
        settings: &super::discovery::Settings,
        scan: &Scanner,
        instruments: &[Instrument],
        now: i64,
        shortlist: Option<&BTreeSet<String>>,
    ) {
        let Some(cfg) = settings.shadow_scorer.as_ref().filter(|c| c.enabled) else {
            return;
        };
        if self.pending.is_some()
            || self
                .last_evaluation
                .is_some_and(|at| now.saturating_sub(at) < cfg.evaluate_ms)
        {
            return;
        }
        self.last_evaluation = Some(now);
        // Freeze the same dated cache and rank timestamp; never mutate live evidence.
        let mut cache = scan.clone();
        cache
            .samples
            .retain(|key, _| instruments.iter().any(|i| i.key == *key));
        let shortlist = shortlist
            .filter(|_| cfg.shortlist_only && settings.prefilter.enabled)
            .cloned();
        let mut default_settings = settings.clone();
        default_settings.strategy = None;
        default_settings.max_prospects = default_settings.max_prospects.max(cfg.top_n);
        let default_projection =
            super::discovery::project(&cache, instruments, &default_settings, now);
        let default_top = default_projection["prospects"]
            .as_array()
            .into_iter()
            .flatten()
            .take(cfg.top_n)
            .filter_map(|row| row["instrument_key"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        let settings = settings.clone();
        let instrument_count = instruments.len();
        let run = move || {
            Self::score_snapshot(
                settings,
                cache,
                instrument_count,
                now,
                shortlist,
                default_top,
            )
        };
        #[cfg(test)]
        if self.synchronous {
            self.record(run());
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        self.pending = Some(receiver);
        std::thread::spawn(move || {
            let _ = sender.send(run());
        });
    }
    fn score_snapshot(
        settings: super::discovery::Settings,
        cache: Scanner,
        instrument_count: usize,
        now: i64,
        shortlist: Option<BTreeSet<String>>,
        default_top: Vec<String>,
    ) -> serde_json::Value {
        let cfg = settings
            .shadow_scorer
            .as_ref()
            .expect("enabled scorer snapshot");
        let mut input_rows = 0;
        let result = (|| {
            let strategy = settings.strategy.clone().unwrap_or_default();
            let metadata_age = settings
                .market_feed
                .as_ref()
                .map(|f| f.metadata_stale_ms)
                .unwrap_or(settings.max_scan_age_ms);
            let snapshot = sadr_core::strategy::extract_features_with_settings(
                &cache,
                now,
                settings.max_scan_age_ms,
                metadata_age,
                sadr_core::strategy::default_lookback_samples(),
                &strategy.feature_settings,
            )
            .map_err(|_| sadr_sandbox::Failure::Input)?;
            let mut features: sadr_sandbox::Features = snapshot
                .rows
                .iter()
                .map(|row| {
                    (
                        row.key.clone(),
                        serde_json::to_value(&row.features).unwrap_or(serde_json::Value::Null),
                    )
                })
                .collect();
            Self::restrict_input(&mut features, shortlist.as_ref());
            input_rows = features.len();
            if features.contains_key("params") {
                return Err(sadr_sandbox::Failure::Input);
            }
            let file =
                std::fs::File::open(&cfg.params_path).map_err(|_| sadr_sandbox::Failure::Input)?;
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(262145)
                .read_to_end(&mut bytes)
                .map_err(|_| sadr_sandbox::Failure::Input)?;
            if bytes.len() > 262144 {
                return Err(sadr_sandbox::Failure::Input);
            }
            let params: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| sadr_sandbox::Failure::Input)?;
            if !params.is_object() {
                return Err(sadr_sandbox::Failure::Input);
            }
            features.insert("params".into(), params);
            let scores = sadr_sandbox::score(
                &settings.scorer_sandbox.clone().unwrap_or_default(),
                std::path::Path::new(&cfg.scorer_path),
                &features,
            )?;
            if scores.contains_key("params") {
                return Err(sadr_sandbox::Failure::OutputInvalid);
            }
            Ok(scores)
        })();
        let mut status =
            serde_json::json!({"as_of_ms":now,"top":[],"ineligible_count":instrument_count});
        match result {
            Ok(scores) => {
                status["ineligible_count"] =
                    serde_json::json!(instrument_count.saturating_sub(scores.len()));
                let mut scores = scores.into_iter().collect::<Vec<_>>();
                scores.sort_by(|(ak, av), (bk, bv)| bv.total_cmp(av).then(ak.cmp(bk)));
                status["top"] = serde_json::json!(
                    scores
                        .into_iter()
                        .take(cfg.top_n)
                        .map(|(key, score)| serde_json::json!({"instrument_key":key,"score":score}))
                        .collect::<Vec<_>>()
                );
            }
            Err(reason) => status["fallback_reason"] = serde_json::json!(reason),
        }
        status["comparison"] = Self::comparison(&status["top"], &default_top, input_rows);
        status
    }
    fn restrict_input(features: &mut sadr_sandbox::Features, shortlist: Option<&BTreeSet<String>>) {
        if let Some(keys) = shortlist {
            features.retain(|key, _| keys.contains(key));
        }
    }
    fn comparison(
        top: &serde_json::Value,
        default_top: &[String],
        input_rows: usize,
    ) -> serde_json::Value {
        let overlap = top
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| {
                row["instrument_key"]
                    .as_str()
                    .is_some_and(|key| default_top.iter().any(|k| k == key))
            })
            .count();
        serde_json::json!({"overlap_top_n":overlap,"default_top":default_top,"input_rows":input_rows})
    }
    fn attach(&self, discovery: &mut Option<serde_json::Value>) {
        if let (Some(discovery), Some(status)) = (discovery, &self.status) {
            discovery["shadow_scorer"] = status.clone();
        }
    }
}

pub(super) fn load_scanner(
    db: &Connection,
    preload_max_age: i64,
    now: i64,
    settings: &sadr_core::execution::ExecutionSettings,
) -> Result<Scanner> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS scanner(id INTEGER PRIMARY KEY CHECK(id=1),body TEXT NOT NULL);")?;
    let raw: Option<String> = db
        .query_row("SELECT body FROM scanner WHERE id=1", [], |r| r.get(0))
        .optional()?;
    match raw {
        Some(raw) => {
            if raw.len() > 16_000_000 {
                return Err("scanner_size".into());
            }
            let mut s: Scanner = serde_json::from_str(&raw)?;
            s.settings = settings.clone();
            if s.samples.len() > 20000
                || s.ranking.len() > 20000
                || s.samples
                    .values()
                    .any(|v| v.history.len() > settings.history_max_samples)
            {
                return Err("scanner_bounds".into());
            }
            for sample in s.samples.values_mut() {
                sample.metadata_preloaded = preload_max_age > 0
                    && sample.market_fields.is_some()
                    && sample.market_fields_receipt.is_some_and(|at| {
                        now.checked_sub(at)
                            .is_some_and(|age| (0..=preload_max_age).contains(&age))
                    });
                if !sample.metadata_preloaded {
                    sample.market_fields = None;
                    sample.market_fields_receipt = None;
                }
                // A restored connection cannot supply current screening activity.
                sample.scan_activity = None;
            }
            Ok(s)
        }
        None => Ok(Scanner {
            settings: settings.clone(),
            ..Default::default()
        }),
    }
}
pub(super) fn checkpoint_scanner(db: &Connection, s: &Scanner) -> Result<()> {
    // In-process rollover opens a new book without the start-of-process load.
    // Creating the journal here keeps the first open-market scan from crashing.
    db.execute_batch("CREATE TABLE IF NOT EXISTS scanner(id INTEGER PRIMARY KEY CHECK(id=1),body TEXT NOT NULL);")?;
    db.execute(
        "INSERT INTO scanner VALUES(1,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
        [serde_json::to_string(s)?],
    )?;
    Ok(())
}
pub(super) fn load_wide(db: &mut Connection, c: &Config) -> Result<Portfolio> {
    load_wide_at(db, c, millis())
}
fn load_wide_at(db: &mut Connection, c: &Config, now: i64) -> Result<Portfolio> {
    let record: Option<(String, String)> = db
        .query_row("SELECT binding,body FROM state WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let expected_binding = book_binding(c)?;
    if let Some((binding, _)) = &record
        && binding != &expected_binding
    {
        if super::cost_policy_only_change(c, db, binding) {
            return Err("paper_cost_policy_changed".into());
        }
        return Err("state_binding_changed".into());
    }
    let body = record.map(|(_, body)| body);
    if let Some(body) = body {
        if let Ok(mut p) = serde_json::from_str::<Portfolio>(&body) {
            p.settings = c.execution.clone();
            p.reconcile()?;
            if c.paper_costs.is_none() || c.paper_costs != p.costs {
                return Err("paper_cost_policy_changed".into());
            }
            return Ok(p);
        }
        let old: Book = serde_json::from_str(&body)?;
        old.reconcile()?;
        if old.stopped
            || old.halted
            || old.closed
            || old.units != 0
            || !old.fills.is_empty()
            || super::local(now).date_naive() >= c.session
        {
            return Err("legacy_book_not_pristine_pre_session".into());
        }
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_history(id INTEGER PRIMARY KEY,body TEXT NOT NULL);",
        )?;
        db.execute("INSERT INTO schema_history(body) VALUES(?)", [body])?;
    }
    let carry = c.directory.join("carry-over.json");
    if carry.exists() {
        let value: serde_json::Value = serde_json::from_slice(
            &crate::read_bounded(&carry, 16_000_000).map_err(|_| "carry_unreadable")?,
        )?;
        rollover::verify_carry(c, &value)?;
        if value["destination_binding"] != expected_binding {
            return Err("carry_binding".into());
        }
        let mut p: Portfolio = serde_json::from_value(value["portfolio"].clone())?;
        let halted_carry = !value["halted_carry"].is_null();
        if !p.model_led
            || p.closed
            || p.stopped
            || p.halted != halted_carry
            || (p.halted && (p.health != rollover::PROTECTIVE_HEALTH || p.positions.is_empty()))
        {
            return Err("carry_invalid".into());
        }
        if halted_carry {
            // One durable adoption record; protection-only entries stay blocked
            // until the reviewed planner-recover clears the carried halt.
            rollover::record_protective_carry(db, &value, millis())?;
        }
        p.settings = c.execution.clone();
        p.reconcile()?;
        // Cash keeps `paper_costs`; the schedule lookup is the single fee source.
        p.costs = Some(c.fee_schedule()?.cash().clone());
        return Ok(p);
    }
    if rollover::published_decision(c)["carry_required"] == true {
        return Err("carry_missing_no_capital_reset".into());
    }
    let mut p = Portfolio::with_capital(c.allocation()?);
    let schedule = c.fee_schedule()?;
    p.settings = c.execution.clone();
    p.costs = Some(schedule.cash().clone());
    // Runtime authority comes from the explicit selector, not the offline legacy constructor.
    p.model_led = c.strategy == "model_planner_v1"
        && c.planner_config
            .as_deref()
            .map(|path| planner::PlannerConfig::load_for_session(path, c.session))
            .transpose()?
            .is_none_or(|cfg| cfg.model_led != Some(false));
    Ok(p)
}
use rusqlite::OptionalExtension;
fn record_halt_transition(db: &Connection, p: &Portfolio, at: i64) -> Result<()> {
    if !p.halted {
        return Ok(());
    }
    let prior: Option<String> = db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .optional()?;
    let already_halted = prior
        .as_deref()
        .map(serde_json::from_str::<Portfolio>)
        .transpose()?
        .is_some_and(|p| p.halted);
    if !already_halted {
        db.execute_batch("CREATE TABLE IF NOT EXISTS paper_halt_history(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,reason TEXT NOT NULL,source TEXT NOT NULL);")?;
        db.execute(
            "INSERT INTO paper_halt_history(at,reason,source) VALUES(?,?,'state_transition')",
            rusqlite::params![at, p.health],
        )?;
    }
    Ok(())
}
pub(super) fn last_halt_reason(db: &Connection) -> Result<(String, &'static str)> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='paper_halt_history')", [], |r|r.get(0))?;
    if exists
        && let Some(reason) = db
            .query_row(
                "SELECT reason FROM paper_halt_history ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
    {
        return Ok((reason, "state_transition"));
    }
    // Old releases recorded halt events but let shutdown overwrite health. Keep
    // those rows intact. Any unrelated halt since the last reviewed recovery
    // takes precedence over repeated planner incidents on an already halted book.
    let events = db.prepare("SELECT event FROM ticks WHERE rowid>COALESCE((SELECT MAX(rowid) FROM ticks WHERE event IN ('operator_planner_recovery_confirmed_paper_review','operator_resume_confirmed_paper_review')),0) AND (event='planner_authority_invalid' OR event LIKE '%halt%') ORDER BY rowid")?.query_map([], |r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
    if let Some(other) = events
        .iter()
        .rev()
        .find(|e| e.as_str() != "planner_authority_invalid")
    {
        return Ok((other.clone(), "legacy_halt_events"));
    }
    if !events.is_empty() {
        return Ok((
            "planner_authority_invalid_recovery_required".into(),
            "legacy_halt_events",
        ));
    }
    Err("planner_recover_halt_not_eligible_halt_history_missing".into())
}
fn stopped_restart_warning(p: &Portfolio) -> Option<&'static str> {
    if !p.stopped || p.closed {
        return None;
    }
    Some(if p.halted {
        "stopped_open_session_restart_requires_review classification=halt_requires_review: paper book remains halted and stopped; inspect original halt before reviewed recovery"
    } else {
        "stopped_open_session_restart_requires_review classification=expected_not_failure: paper book remains operator-stopped; inspect and run resume --confirm-paper-review before starting the service"
    })
}
fn mark_operator_stopped(p: &mut Portfolio) {
    if !p.closed {
        p.stopped = true;
        if !p.halted {
            p.health = "operator_stopped".into();
        }
    }
}
pub(super) fn close_portfolio(p: &mut Portfolio) -> bool {
    if p.closed {
        return false;
    }
    p.closed = true;
    if !p.halted {
        p.health = if p.positions.is_empty() {
            "closed"
        } else if p.model_led {
            "closed_with_carried_positions"
        } else {
            "closed_with_unresolved_positions"
        }
        .into();
    }
    true
}
pub(super) fn persist(
    db: &mut Connection,
    c: &Config,
    p: &Portfolio,
    at: i64,
    receipt: Option<i64>,
    raw: Option<&str>,
    event: &str,
) -> Result<()> {
    p.reconcile()?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    record_halt_transition(&tx, p, at)?;
    tx.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,?,?,?)",
        rusqlite::params![at, receipt, raw, event],
    )?;
    tx.execute(
        "INSERT INTO state VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
        rusqlite::params![book_binding(c)?, serde_json::to_string(p)?],
    )?;
    tx.commit()?;
    Ok(())
}
/// Verify one capture clock sample against the VERIFIED session trading day and
/// configured cash-session window. The admission, request, receipt and actual
/// completion are separate samples; a completion after the close is refused
/// even when admission happened inside the window.
fn verified_capture_session_clock(c: &Config, at: i64) -> Result<()> {
    let local = local(at);
    if local.date_naive() != c.session {
        return Err("observation_capture_session_date_mismatch".into());
    }
    if !c.is_trading_day(c.session)? {
        return Err("observation_capture_not_trading_day".into());
    }
    let minute = local
        .hour()
        .saturating_mul(60)
        .saturating_add(local.minute());
    if !(c.execution.session_start_minute..c.execution.session_end_minute).contains(&minute) {
        return Err("observation_capture_market_closed".into());
    }
    Ok(())
}

/// Tick event for controller research-observation receipts. They are kept apart
/// from trading `wide_quotes` so retention never compacts the original receipt a
/// later session re-derives an observation from (#240).
pub(crate) const RESEARCH_QUOTES_EVENT: &str = "research_quotes";

/// Append one original controller research-observation quote receipt. It writes
/// only the original `ticks` row with purpose `research_observation`; it never
/// applies quotes, changes portfolio/state, creates a plan/order or opens a new
/// financial book. The receipt body is preserved exactly and never rewritten.
pub(super) fn commit_research_quotes(
    db: &mut Connection,
    c: &Config,
    requested: i64,
    receipt: i64,
    raw: &str,
    now: i64,
    max_rows: usize,
) -> Result<()> {
    if requested < 0
        || receipt < requested
        || receipt > now
        || raw.is_empty()
        || raw.len() > super::owner_tasks::OBSERVATION_CAPTURE_MAX_BYTES
        || !(1..=super::owner_tasks::OBSERVATION_CAPTURE_MAX_ROWS).contains(&max_rows)
    {
        return Err("observation_capture_invalid".into());
    }
    // Re-check the request start and the ACTUAL completion against the verified
    // session window before the INSERT. A stale admission sample is never
    // reused as the completion clock.
    verified_capture_session_clock(c, requested)?;
    verified_capture_session_clock(c, receipt)?;
    verified_capture_session_clock(c, now)?;
    let body: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| "observation_capture_invalid")?;
    if body["status"] != "success" || !body["data"].is_object() {
        return Err("observation_capture_invalid".into());
    }
    if body["data"]
        .as_object()
        .is_none_or(|data| data.is_empty() || data.len() > max_rows)
    {
        return Err("observation_capture_row_budget_exceeded".into());
    }
    let archive = serde_json::json!({
        "source":"upstox-readonly-v1",
        "purpose":"research_observation",
        "requested_ms":requested,
        "received_ms":receipt,
        "body":raw
    })
    .to_string();
    // `now` is the actual completion observation clock sampled after the I/O
    // returned; the original data receipt is stored unchanged and never
    // replaced by a later clock sample or an artificial max().
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,?,?,?)",
        rusqlite::params![now, receipt, archive, RESEARCH_QUOTES_EVENT],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // Explicit transaction inputs, including the injectable post-lock clock.
pub(super) fn commit_quotes(
    db: &mut Connection,
    c: &Config,
    p: &mut Portfolio,
    quotes: &BTreeMap<String, Quote>,
    entries: &[Instrument],
    receipt: i64,
    raw: &str,
    clock: impl FnOnce() -> i64,
) -> Result<()> {
    commit_quotes_screened(db, c, p, quotes, entries, receipt, raw, clock, None, false)
}
#[allow(clippy::too_many_arguments)] // Runtime supplies the explicit revalidated buy screen; protection/sells bypass it.
fn commit_quotes_screened(
    db: &mut Connection,
    c: &Config,
    p: &mut Portfolio,
    quotes: &BTreeMap<String, Quote>,
    entries: &[Instrument],
    receipt: i64,
    raw: &str,
    clock: impl FnOnce() -> i64,
    allowed_buys: Option<&BTreeSet<String>>,
    held_positions: bool,
) -> Result<()> {
    if !entries.is_empty() {
        return Err(Fail::new(super::planner::NO_DECISION_WORK).into());
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let now = clock();
    let mut next = p.clone();
    next.apply_at(c.session, now, quotes, &[])?;
    // Expiry is not cash rollover: only the held-position read carries every
    // open position, so only that read may flatten or halt an expiring option.
    if held_positions && next.options.is_some() {
        let cutoff = c.execution.session_end_minute.saturating_sub(30);
        super::options::expiry::flatten_expiring(&mut next, quotes, now, cutoff)?;
    }
    let protected = next.clone();
    tx.execute_batch("SAVEPOINT planner_decision")?;
    let action_result = (|| -> Result<Option<serde_json::Value>> {
        ensure_actions(&tx)?;
        let saved_plan = planner::saved_plan(&tx, c)?;
        if let Some(plan) = &saved_plan
            && local(now).date_naive() == c.session
            && (c.execution.session_start_minute..c.execution.session_end_minute)
                .contains(&minute(now))
        {
            let decision = plan["decision_id"]
                .as_str()
                .ok_or("planner_saved_plan_invalid")?;
            if plan["session_binding"] != book_binding(c)?
                || plan["input_hash"] != digest(&serde_json::to_vec(&plan["input"])?)
            {
                return Err("planner_saved_binding_invalid".into());
            }
            if plan["fake"] != false || plan["dry_run"] != false {
                return Err("planner_fake_plan_not_executable".into());
            }
            // A1 intent binding: revalidate the book and policy revision the
            // intent was produced against. Missing values keep archived legacy
            // plans executable; present values must match exactly.
            let stale_portfolio = plan["portfolio_revision"].as_str().is_some_and(|expected| {
                redecision::portfolio_hash(&next).map_or(true, |current| current != expected)
            });
            let current_cfg = if plan.get("policy_sha256").and_then(|v| v.as_str()).is_some()
                || plan
                    .get("profile_sha256")
                    .and_then(|v| v.as_str())
                    .is_some()
            {
                c.planner_config
                    .as_deref()
                    .and_then(|path| planner::PlannerConfig::load_for_session(path, c.session).ok())
            } else {
                None
            };
            let policy_changed = plan["policy_sha256"].as_str().is_some_and(|expected| {
                current_cfg
                    .as_ref()
                    .and_then(|cfg| planner::policy_bytes(cfg).ok())
                    .map(|bytes| digest(&bytes))
                    .as_deref()
                    != Some(expected)
            });
            let profile_changed = plan.get("profile_sha256").is_some_and(|expected| {
                let actual = current_cfg
                    .as_ref()
                    .and_then(|cfg| cfg.profile_sha256())
                    .map_or(serde_json::Value::Null, |v| serde_json::json!(v));
                *expected != actual
            });
            let binding_refusal = if stale_portfolio {
                Some("portfolio_changed")
            } else if policy_changed {
                Some("policy_changed")
            } else if profile_changed {
                Some("profile_changed")
            } else {
                None
            };
            let verdicts: Vec<planner::Verdict> = serde_json::from_value(plan["verdicts"].clone())?;
            for verdict in verdicts {
                let Some(action) = verdict.action else {
                    continue;
                };
                let done: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM planner_action WHERE decision=? AND idx=?)",
                    rusqlite::params![decision, verdict.index as i64],
                    |r| r.get(0),
                )?;
                if done {
                    continue;
                }
                if let Some(reason) = binding_refusal {
                    tx.execute(
                        "INSERT INTO planner_action VALUES(?,?,?)",
                        rusqlite::params![decision, verdict.index as i64, reason],
                    )?;
                    continue;
                }
                if plan["valid_until_ms"]
                    .as_i64()
                    .is_some_and(|deadline| now > deadline)
                {
                    tx.execute(
                        "INSERT INTO planner_action VALUES(?,?,'plan_expired')",
                        rusqlite::params![decision, verdict.index as i64],
                    )?;
                    continue;
                }
                let instrument: Instrument = serde_json::from_value(
                    plan["input"]["allowlist"]
                        .as_array()
                        .ok_or("planner_allowlist_missing")?
                        .iter()
                        .find(|v| v["key"] == action.instrument_key)
                        .ok_or("planner_instrument_missing")?
                        .clone(),
                )?;
                if matches!(action.side, planner::PlanSide::ReviseExit) {
                    let revised = next.revise_exit(&instrument, &action.exit_intent, decision)?;
                    tx.execute(
                        "INSERT INTO planner_action VALUES(?,?,?)",
                        rusqlite::params![
                            decision,
                            verdict.index as i64,
                            if revised {
                                "exit_revised"
                            } else {
                                "exit_revision_rejected"
                            }
                        ],
                    )?;
                    continue;
                }
                let Some(q) = quotes.get(&action.instrument_key) else {
                    continue;
                };
                if matches!(action.side, planner::PlanSide::Buy)
                    && allowed_buys.is_some_and(|keys| !keys.contains(&action.instrument_key))
                {
                    tx.execute(
                        "INSERT INTO planner_action VALUES(?,?,'screen_rejected')",
                        rusqlite::params![decision, verdict.index as i64],
                    )?;
                    continue;
                }
                let filled = next.execute_plan(
                    now,
                    &instrument,
                    q,
                    matches!(action.side, planner::PlanSide::Buy),
                    verdict.units,
                    action.max_price.ok_or("planner_action_max_price_missing")?,
                    action.exit_intent,
                    decision,
                )?;
                tx.execute(
                    "INSERT INTO planner_action VALUES(?,?,?)",
                    rusqlite::params![
                        decision,
                        verdict.index as i64,
                        if filled {
                            "filled"
                        } else {
                            "execution_gate_rejected"
                        }
                    ],
                )?;
            }
        }
        Ok(saved_plan)
    })();
    let saved_plan = match action_result {
        Ok(plan) => {
            tx.execute_batch("RELEASE planner_decision")?;
            plan
        }
        Err(_) => {
            tx.execute_batch("ROLLBACK TO planner_decision; RELEASE planner_decision")?;
            next = protected;
            if !next.halted {
                next.health = "planner_authority_invalid_recovery_required".into();
            }
            next.halted = true;
            tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,NULL,'planner_authority_invalid')",[now])?;
            None
        }
    };
    record_halt_transition(&tx, &next, now)?;
    tx.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,?,?,'wide_quotes')",
        rusqlite::params![now, receipt, raw],
    )?;
    tx.execute(
        "INSERT INTO state VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
        rusqlite::params![book_binding(c)?, serde_json::to_string(&next)?],
    )?;
    tx.commit()?;
    *p = next;
    let audit_result = (|| -> Result<()> {
        if let Some(plan) = &saved_plan {
            let id = plan["decision_id"]
                .as_str()
                .ok_or("planner_saved_plan_invalid")?;
            let rows = db
            .prepare("SELECT idx,verdict FROM planner_action WHERE decision=? ORDER BY idx")?
            .query_map([id], |r| {
                Ok(serde_json::json!({"index":r.get::<_,i64>(0)?,"verdict":r.get::<_,String>(1)?}))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
            let audit = c.directory.join("model").join(id);
            if audit.is_dir() {
                rollover::atomic(&audit.join("execution.json"), &serde_json::json!(rows))?;
            }
        }
        Ok(())
    })();
    if audit_result.is_err() {
        planner_incident(db, c, p, now)?;
    }
    if saved_plan
        .as_ref()
        .is_some_and(|plan| !plan["strategy_attribution"].is_null())
        && let Some(path) = &c.planner_config
    {
        let config = planner::PlannerConfig::load_for_session(path, c.session)?;
        if let Some(settings) = &config.discovery {
            super::strategy_results::record_pipeline(settings, c, p, saved_plan.as_ref())?;
        }
    }
    Ok(())
}
#[cfg(test)]
pub(super) fn check_closed_incident_guard(
    db: &mut Connection,
    c: &Config,
    p: &mut Portfolio,
    now: i64,
) {
    let original = p.clone();
    let ticks: i64 = db
        .query_row("SELECT count(*) FROM ticks", [], |r| r.get(0))
        .unwrap();
    planner_incident(db, c, p, now).unwrap();
    assert_eq!(*p, original);
    assert_eq!(
        db.query_row("SELECT count(*) FROM ticks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        ticks
    );
}

fn planner_incident(db: &mut Connection, c: &Config, p: &mut Portfolio, now: i64) -> Result<()> {
    planner_incident_cause(db, c, p, now, None)
}
fn planner_incident_cause(
    db: &mut Connection,
    c: &Config,
    p: &mut Portfolio,
    now: i64,
    cause: Option<&str>,
) -> Result<()> {
    if closed_incident::closed_sealed(c, p) {
        return Ok(());
    }
    let mut check = match cause {
        Some("planner_event_configuration_changed" | "planner_configuration_failed") => {
            redecision::configuration_check(db, c)
        }
        Some(code) => serde_json::json!({"failed_check": code}),
        None => serde_json::json!({"failed_check": "planner_authority_invalid"}),
    };
    check["paper_only"] = serde_json::json!(true);
    check["live_enabled"] = serde_json::json!(LIVE_ENABLED);
    check["config_transition_command"] =
        serde_json::json!("sadr-rs config-apply --config PAPER_CONFIG --confirm-paper-review");
    check["recovery_command"] = if p.closed {
        serde_json::json!("sadr-rs planner-recover --config PAPER_CONFIG --confirm-paper-review")
    } else if p.model_led && !p.positions.is_empty() {
        serde_json::json!(
            "sadr-rs planner-recover --config PAPER_CONFIG --protection-only --confirm-paper-review"
        )
    } else {
        serde_json::json!("sadr-rs planner-recover --config PAPER_CONFIG --confirm-paper-review")
    };
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'planner_incident_check')",
        rusqlite::params![now, serde_json::to_string(&check)?],
    )?;
    if p.closed {
        // A closed book cannot trade or require protection. Never convert a
        // config edit into a fresh halt while it is sealed or awaiting review.
        return Ok(());
    }
    if !p.halted {
        p.health = "planner_authority_invalid_recovery_required".into();
    }
    p.halted = true;
    persist(db, c, p, now, None, None, "planner_authority_invalid")
}
pub(super) fn ensure_actions(db: &Connection) -> Result<()> {
    let present: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='planner_action')",
        [],
        |r| r.get(0),
    )?;
    if !present && redecision::has_authority(db)? {
        return Err("planner_action_journal_missing".into());
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS planner_action(decision TEXT,idx INTEGER,verdict TEXT NOT NULL,PRIMARY KEY(decision,idx));")?;
    Ok(())
}
fn block_data_failure(db: &mut Connection, c: &Config, p: &mut Portfolio) {
    // The caller records the original read failure and current halted state next.
    if redecision::block(db, c, redecision::DATA_BLOCK).is_err() {
        p.halted = true;
    }
}
fn block_read_failure(db: &mut Connection, c: &Config, p: &mut Portfolio, reason: &str) {
    let block = if redecision::transient_read_failure(reason) {
        redecision::READ_TRANSIENT_BLOCK
    } else {
        redecision::DATA_BLOCK
    };
    if redecision::block(db, c, block).is_err() {
        p.halted = true;
    }
}
#[allow(clippy::too_many_arguments)] // Contain optional planner failure without replacing the financial book.
fn contain_event_error(
    result: Result<()>,
    db: &mut Connection,
    c: &Config,
    p: &mut Portfolio,
    worker: &mut Option<redecision::Worker>,
    cfg: &mut Option<planner::PlannerConfig>,
    fallback: &mut Option<planner::PlannerConfig>,
    config_reason: &mut Option<&'static str>,
    now: i64,
) -> Result<()> {
    if let Err(error) = result {
        let code = failure_code(error.as_ref());
        if matches!(
            code.as_str(),
            "planner_usage_busy_no_retry" | "planner_usage_lock_timeout"
        ) {
            return Ok(());
        }
        if redecision::recoverable_block(&code)
            || matches!(
                code.as_str(),
                "planner_call_timeout"
                    | "harness_deadline_expired"
                    | "harness_turn_budget_exhausted"
                    | "harness_decision_tool_failed_no_entries"
            )
        {
            redecision::fail_pending(db, c, &code)?;
            *worker = None;
            return Ok(());
        }
        // A live config/prompt edit is an explicit config-apply transition, not
        // an integrity halt. Suspend the planner and keep protection running.
        if code == "planner_event_configuration_changed" {
            *worker = None;
            if let Some(config) = cfg.take() {
                *fallback = Some(config);
            }
            *config_reason = Some("planner_event_configuration_changed");
            redecision::block(db, c, &code)?;
            let mut check = redecision::configuration_check(db, c);
            check["paper_only"] = serde_json::json!(true);
            check["live_enabled"] = serde_json::json!(LIVE_ENABLED);
            check["recovery_command"] = serde_json::json!(
                "sadr-rs config-apply --config PAPER_CONFIG --confirm-paper-review"
            );
            check["resume_command"] =
                serde_json::json!("sadr-rs resume --config PAPER_CONFIG --confirm-paper-review");
            db.execute(
                "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'planner_configuration_changed_apply_required')",
                rusqlite::params![now, serde_json::to_string(&check)?],
            )?;
            return Ok(());
        }
        *worker = None;
        if let Some(config) = cfg.take() {
            *fallback = Some(config);
        }
        *config_reason = Some("planner_integrity_failure");
        db.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'planner_integrity_failure')",
            rusqlite::params![now, serde_json::json!({"reason":code}).to_string()],
        )?;
        planner_incident_cause(db, c, p, now, Some(&code))?;
    }
    Ok(())
}
mod history;
#[cfg(test)]
pub(super) use history::{
    HISTORY_ATTEMPT_INTERRUPTED, HISTORY_ATTEMPT_RECOVERED, HISTORY_RECOVERY_RECEIPT_KIND,
    history_recovery_receipt_path,
};
pub(super) use history::{HistoryAttempts, history_attempt_window, history_recover};

fn load_daily_volumes(c: &Config, instruments: &[Instrument], now: i64, scan: &mut Scanner) {
    // Persisted scanner values may belong to an older session; always clear them.
    for sample in scan.samples.values_mut() {
        sample.avg_daily_volume = None;
        sample.avg_daily_volume_sessions = None;
    }
    let Ok(request) = preparation_request(c, instruments, now) else {
        return;
    };
    let report = preparation::coverage_report(
        &request.history_root,
        instruments,
        &request.required_sessions,
        request.target_session,
        request.previous_session,
    );
    for (key, (average, sessions)) in report.daily_volumes {
        if sessions >= c.history_avg_daily_volume_min_sessions.max(1) {
            let sample = scan.samples.entry(key).or_default();
            sample.avg_daily_volume = Some(average);
            sample.avg_daily_volume_sessions = Some(sessions);
        }
    }
}

fn recovered_history_status(c: &Config, instruments: &[Instrument], now: i64) -> serde_json::Value {
    match preparation_request(c, instruments, now) {
        Ok(request) => serde_json::to_value(preparation::coverage_report(
            &request.history_root,
            instruments,
            &request.required_sessions,
            request.target_session,
            request.previous_session,
        ))
        .unwrap_or_else(|_| serde_json::json!({"status":"UNKNOWN"})),
        Err(_) => {
            serde_json::json!({"status":"UNKNOWN","target_session":c.session,"reason":"history_recovery_coverage_unavailable"})
        }
    }
}

pub(super) fn preparation_request(
    c: &Config,
    instruments: &[Instrument],
    now: i64,
) -> Result<preparation::PreparationRequest> {
    let target = local(now).date_naive();
    let (session, previous) = c.next_sessions(target)?;
    if session != c.session {
        return Err("history_preparation_session_mismatch".into());
    }
    let mut dates = Vec::with_capacity(c.daily_history_sessions);
    let mut day = previous;
    for _ in 0..80 {
        if c.is_trading_day(day)? {
            dates.push(day);
        }
        if dates.len() == c.daily_history_sessions {
            break;
        }
        day = day.pred_opt().ok_or("history_preparation_date_range")?;
    }
    if dates.len() != c.daily_history_sessions {
        return Err("history_preparation_calendar_coverage".into());
    }
    dates.reverse();
    let deadline = DateTime::parse_from_rfc3339(&format!(
        "{session}T{:02}:{:02}:00+05:30",
        c.history_preparation_cutoff_minute / 60,
        c.history_preparation_cutoff_minute % 60
    ))?
    .timestamp_millis();
    Ok(preparation::PreparationRequest {
        config: c.clone(),
        instruments: instruments.to_vec(),
        target_session: session,
        previous_session: previous,
        required_sessions: dates,
        history_root: c
            .history_directory
            .clone()
            .ok_or("history_directory_required")?,
        deadline_ms: deadline,
    })
}
pub(super) fn readiness_snapshot(
    c: &Config,
    p: &Portfolio,
    now: i64,
    history: &serde_json::Value,
) -> serde_json::Value {
    let models=readiness::report(c,now).unwrap_or_else(|_|serde_json::json!({"status":"model_roles_blocked","reason":"model_readiness_unavailable"}));
    let target = models["target_session"]
        .as_str()
        .and_then(|s| s.parse::<NaiveDate>().ok());
    let disposition = if let Some(day) = target.filter(|d| *d > c.session) {
        let at = DateTime::parse_from_rfc3339(&format!("{day}T08:30:00+05:30"))
            .map(|v| v.timestamp_millis());
        match at.ok().and_then(|at| rollover::preview(c, at).ok()) {
            Some(v) => serde_json::json!({"status":v["status"],"reason":v["review"]["status"]}),
            None => {
                serde_json::json!({"status":"blocked","reason":"rollover_readiness_unavailable"})
            }
        }
    } else {
        serde_json::json!({"status":if p.halted||p.stopped||p.closed {"blocked"} else {"current_session"}})
    };
    let lifecycle_ready = matches!(
        disposition["status"].as_str(),
        Some("rollover_ready" | "current_session")
    );
    let history_matches = target.is_some_and(|d| {
        history["target_session"] == d.to_string()
            && history["universe_session"]
                .as_str()
                .is_none_or(|s| s == d.to_string())
    });
    let history_ready = history_matches && history["status"] == "READY";
    let history_partial = history_matches
        && history["status"] == "PARTIAL"
        && history["ready_instruments"].as_u64().is_some_and(|n| n > 0);
    let state = if !lifecycle_ready
        || models["status"] == "model_roles_blocked"
        || (history_matches && history["status"] == "BLOCKED")
    {
        "blocked"
    } else if models["status"] != "model_roles_ready" || (!history_ready && !history_partial) {
        "unknown"
    } else if history_partial {
        "partial"
    } else {
        "ready"
    };
    serde_json::json!({"status":state,"target_session":target,"models":models,"lifecycle":disposition,
        "history":history,"history_preparation_enabled":c.history_preparation_enabled,"checked_at_ms":now})
}

fn refresh_experiment_profile(
    db: &mut Connection,
    c: &Config,
    cfg: &mut planner::PlannerConfig,
    now: i64,
    alerts: &Alerts,
) -> Result<()> {
    if cfg.experiment_profile.is_none() {
        return Ok(());
    }
    let path = cfg.experiment_profile.clone();
    let mut next = cfg.without_experiment_profile();
    next.experiment_profile = path;
    match next.apply_experiment_profile() {
        Err(reason) => {
            if cfg.profile_error.as_deref() != Some(reason.as_str()) {
                eprintln!("experiment_profile_rejected reason={reason}");
                cfg.profile_error = Some(reason);
            }
            Ok(())
        }
        Ok(()) => {
            if next.profile_sha256() == cfg.profile_sha256() {
                return Ok(());
            }
            let changed = next.profile_changed_keys().unwrap_or(&[]).to_vec();
            db.execute(
                "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,?)",
                rusqlite::params![
                    now,
                    serde_json::json!({
                        "old_sha256": cfg.profile_sha256(),
                        "new_sha256": next.profile_sha256(),
                        "changed_keys": changed,
                    })
                    .to_string(),
                    "experiment_profile_applied"
                ],
            )?;
            alerts.emit(
                Event::ExperimentProfileApplied,
                c.session,
                serde_json::json!({"changed_keys": changed}),
            );
            *cfg = next;
            Ok(())
        }
    }
}
#[allow(clippy::too_many_arguments)] // Frozen controller context joins the existing claim/eligibility checks.
fn event_tick(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    instruments: &[Instrument],
    worker: &mut Option<redecision::Worker>,
    scanner: &Scanner,
    store: &FeatureStore,
    index: Option<&IndexContext>,
    now: i64,
    eligible: bool,
    fresh_observation: bool,
) -> Result<()> {
    if let Some(path) = &c.planner_config {
        let mut current = planner::PlannerConfig::load_for_session(path, c.session)?;
        if current.profile_error.is_some() {
            current.retain_previous_profile(cfg);
        }
        if planner::policy_bytes(&current)? != planner::policy_bytes(cfg)? {
            return Err("planner_event_configuration_changed".into());
        }
    }
    if let Some(settings) = &cfg.discovery
        && settings.strategy.as_ref().is_some_and(|s| s.enabled)
    {
        super::strategy_results::record_pipeline(
            settings,
            c,
            p,
            planner::saved_plan(db, c)?.as_ref(),
        )?;
    }
    match redecision::news(cfg, c, instruments) {
        Ok(news) => redecision::observe(db, c, cfg, p, &[], &news, now)?,
        Err(_) => redecision::block(db, c, "planner_news_evidence_invalid")?,
    }
    if let Some(pending) = worker {
        if !eligible
            || p.stopped
            || p.halted
            || p.closed
            || now > pending.claim.deadline_ms
            || redecision::sticky_blocked(db, c)?
        {
            pending
                .cancel
                .store(true, std::sync::atomic::Ordering::Release);
            redecision::block(db, c, "planner_cancelled_no_replay")?;
        }
        let result = match pending.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Some(Err("planner_worker_failed_no_replay".into()))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
        };
        if let Some(result) = result {
            if let Some(plan) =
                redecision::settle(db, c, cfg, p, &pending.claim, result, now, eligible)?
            {
                // Compatibility projection only: SQLite already committed the head.
                rollover::atomic(&c.directory.join("model/session-plan.json"), &plan)?;
            }
            *worker = None;
            return Ok(());
        }
    }
    if worker.is_none()
        && let Some(claim) = redecision::reserve(
            db,
            c,
            cfg,
            p,
            now,
            eligible && fresh_observation && planner::has_eligible_input(c, p, instruments)?,
        )?
    {
        eprintln!(
            "luna_trigger reason={} kind={} event_at_ms={} digest_start_ms={now}",
            redecision::trigger_reason(&claim.trigger),
            claim.trigger["kind"].as_str().unwrap_or("unknown"),
            claim.trigger["observed_ms"],
        );
        let context = cfg
            .decision_evidence
            .as_ref()
            .map(|settings| {
                super::decision_evidence::Snapshot::capture(
                    settings,
                    c.session,
                    now,
                    instruments,
                    scanner,
                    store,
                    index.map(IndexContext::json),
                )
            })
            .transpose()?;
        *worker = Some(redecision::launch(c, cfg, p, instruments, claim, context));
    }
    Ok(())
}
/// Match the explicit provider key; a renamed symbol is a visible diagnostic.
/// Screening metadata is independent of executable depth availability.
#[allow(clippy::too_many_arguments)] // Preserve original receipt separately from the processing clock.
fn scan_rest_rows(
    scan: &mut Scanner,
    instruments: &[Instrument],
    rows: &serde_json::Map<String, serde_json::Value>,
    receipt: i64,
    now: i64,
    stale_ms: i64,
    metadata_only: bool,
    mut feature_store: Option<&mut FeatureStore>,
) -> Result<()> {
    for instrument in instruments {
        let matched = rows
            .values()
            .filter(|r| r["instrument_token"] == instrument.key)
            .collect::<Vec<_>>();
        if matched.len() > 1 {
            return Err("duplicate_quote".into());
        }
        let Some(row) = matched.first() else {
            if metadata_only {
                scan.samples
                    .entry(instrument.key.clone())
                    .or_default()
                    .diagnostic = Some("no_quote_returned".into());
            } else {
                scan.missing(&instrument.key, "no_quote_returned");
            }
            continue;
        };
        let fields = (|| -> Result<sadr_core::execution::MarketFields> {
            Ok(sadr_core::execution::MarketFields {
                session_vwap: sadr_core::execution::session_vwap(row),
                best_bid: sadr_core::execution::book_level(row, "buy").0,
                best_bid_qty: sadr_core::execution::book_level(row, "buy").1,
                best_ask: sadr_core::execution::book_level(row, "sell").0,
                best_ask_qty: sadr_core::execution::book_level(row, "sell").1,
                total_buy_quantity: row["total_buy_quantity"].as_u64(),
                total_sell_quantity: row["total_sell_quantity"].as_u64(),
                lower_circuit_limit: sadr_core::execution::number(&row["lower_circuit_limit"])?,
                upper_circuit_limit: sadr_core::execution::number(&row["upper_circuit_limit"])?,
                volume: row["volume"].as_u64().ok_or("volume_missing")?,
            })
        })();
        let sample = scan.samples.entry(instrument.key.clone()).or_default();
        sample.diagnostic = (row["symbol"] != instrument.symbol).then(|| "symbol_mismatch".into());
        if let Ok(fields) = &fields {
            sample.metadata_preloaded = false;
            sample.market_fields = Some(fields.clone());
            sample.market_fields_receipt = Some(receipt);
        }
        // Incomplete REST metadata must not erase retained fields or renew their receipt.
        if metadata_only {
            continue;
        }
        let price =
            sadr_core::execution::number(&row["last_price"]).and_then(|p| Ok(Price::try_from(p)?));
        match (price, data::quote_source(row)) {
            (Ok(price), Ok(source)) => {
                if let Some(store) = feature_store.as_deref_mut() {
                    observe_feature(store, &instrument.key, price, receipt);
                }
                scan.observe_price(
                    now,
                    &instrument.key,
                    price,
                    source,
                    receipt,
                    stale_ms,
                    fields.as_ref().ok(),
                )?;
            }
            _ => scan.missing(&instrument.key, "invalid_quote"),
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)] // Explicit original receipt tracking; never resample it.
fn read_apply(
    client: &data::Client,
    c: &Config,
    db: &mut Connection,
    p: &mut Portfolio,
    instruments: &[Instrument],
    entries: &[Instrument],
    mut scanner: Option<&mut Scanner>,
    last_fresh_receipt: &mut Option<i64>,
    event_config: Option<&planner::PlannerConfig>,
    event_cancel: Option<&std::sync::atomic::AtomicBool>,
    metadata_only: bool,
    depth_sink: Option<&mut Scanner>,
    feature_store: &Arc<Mutex<FeatureStore>>,
    held_positions: bool,
) -> Result<Option<String>> {
    let keys = instruments
        .iter()
        .map(|i| i.key.as_str())
        .collect::<Vec<_>>();
    let (requested, receipt, raw) = match client.quotes(c, &keys) {
        Ok(v) => v,
        Err(error) => {
            if let Some(cancel) = event_cancel {
                cancel.store(true, std::sync::atomic::Ordering::Release);
            }
            let reason = failure_code(error.as_ref());
            if event_config.is_some() {
                block_read_failure(db, c, p, &reason);
            }
            p.health = "read_unavailable_no_financial_action".into();
            persist(db, c, p, millis(), None, None, "read_unavailable")?;
            return Ok(Some(reason));
        }
    };
    let body: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            if let Some(cancel) = event_cancel {
                cancel.store(true, std::sync::atomic::Ordering::Release);
            }
            if event_config.is_some() {
                block_data_failure(db, c, p);
            }
            persist(
                db,
                c,
                p,
                millis(),
                Some(receipt),
                Some(&raw),
                "invalid_quote_json",
            )?;
            return Ok(Some("invalid_quote_json".into()));
        }
    };
    if body["status"] != "success" {
        if let Some(cancel) = event_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
        if event_config.is_some() {
            block_data_failure(db, c, p);
        }
        persist(
            db,
            c,
            p,
            millis(),
            Some(receipt),
            Some(&raw),
            "invalid_quote_status",
        )?;
        return Ok(Some("invalid_quote_status".into()));
    }
    let Some(rows) = body["data"].as_object() else {
        if let Some(cancel) = event_cancel {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
        if event_config.is_some() {
            block_data_failure(db, c, p);
        }
        persist(
            db,
            c,
            p,
            millis(),
            Some(receipt),
            Some(&raw),
            "invalid_quote_data",
        )?;
        return Ok(Some("invalid_quote_data".into()));
    };
    if metadata_only {
        if let Some(scan) = scanner.as_deref_mut()
            && scan_rest_rows(scan, instruments, rows, receipt, millis(), 2000, true, None).is_err()
        {
            persist(
                db,
                c,
                p,
                millis(),
                Some(receipt),
                Some(&raw),
                "duplicate_metadata_quote",
            )?;
            return Ok(Some("duplicate_quote".into()));
        }
        let archive=serde_json::json!({"source":"upstox-readonly-v1","purpose":"scanner_metadata","requested_ms":requested,"received_ms":receipt,"body":raw}).to_string();
        persist(
            db,
            c,
            p,
            millis(),
            Some(receipt),
            Some(&archive),
            "scanner_metadata",
        )?;
        return Ok(None);
    }
    let mut quotes = BTreeMap::new();
    let mut duplicate = BTreeSet::new();
    for row in rows.values() {
        let Some(i) = instruments
            .iter()
            .find(|i| row["instrument_token"] == i.key)
        else {
            continue;
        };
        if !duplicate.insert(i.key.clone()) {
            if let Some(cancel) = event_cancel {
                cancel.store(true, std::sync::atomic::Ordering::Release);
            }
            if event_config.is_some() {
                block_data_failure(db, c, p);
            }
            persist(
                db,
                c,
                p,
                millis(),
                Some(receipt),
                Some(&raw),
                "duplicate_quote",
            )?;
            return Ok(Some("duplicate_quote".into()));
        }
        if let Ok(q) = data::parse_row(row, receipt, &i.key, &i.symbol, i.tick) {
            quotes.insert(i.key.clone(), q);
        }
    }
    // Shortlist depth reads attach the executable quote to the prospect (#38).
    if let Some(scan) = depth_sink {
        for (key, quote) in &quotes {
            if let Some(sample) = scan.samples.get_mut(key) {
                sample.executable = Some(quote.clone());
            }
        }
    }
    if let Some(scan) = scanner.as_deref_mut() {
        let mut store = feature_store
            .lock()
            .map_err(|_| Fail::new("feature_store_lock"))?;
        scan_rest_rows(
            scan,
            instruments,
            rows,
            receipt,
            millis(),
            event_config
                .and_then(|c| c.discovery.as_ref())
                .map(|d| d.max_scan_age_ms)
                .unwrap_or(2000),
            metadata_only,
            Some(&mut store),
        )?;
        if event_config
            .and_then(|cfg| cfg.discovery.as_ref())
            .and_then(|d| d.strategy_selection.as_ref())
            .is_some_and(|s| s.enabled)
        {
            for row in rows.values() {
                if let Some(sample) = row["instrument_token"]
                    .as_str()
                    .and_then(|key| scan.samples.get_mut(key))
                {
                    sample.regime_references = (|| {
                        let close = Price::try_from(
                            sadr_core::execution::number(&row["ohlc"]["close"]).ok()?,
                        )
                        .ok()?;
                        let open = Price::try_from(
                            sadr_core::execution::number(&row["ohlc"]["open"]).ok()?,
                        )
                        .ok()?;
                        Some((receipt, close, open))
                    })();
                }
            }
        }
    }
    // Entry revalidation repeats dynamic liquidity/band tests on the new receipt.
    let mut checked = Vec::new();
    for i in entries {
        if let (Some(q), Some(row)) = (
            quotes.get(&i.key),
            rows.values().find(|r| r["instrument_token"] == i.key),
        ) && Scanner::market_allows_with_policy(
            millis(),
            i,
            q,
            row,
            &event_config
                .and_then(|cfg| cfg.discovery.as_ref())
                .and_then(|d| d.screens.clone())
                .unwrap_or_else(sadr_core::execution::ScreenPolicy::recommended),
        ) {
            checked.push(i.clone());
        }
    }
    let archive=serde_json::json!({"source":"upstox-readonly-v1","purpose":if scanner.is_some(){"scanner"}else{"execution_or_protection"},"requested_ms":requested,"received_ms":receipt,"body":raw}).to_string();
    let policy = event_config
        .and_then(|cfg| cfg.discovery.as_ref())
        .and_then(|d| d.screens.clone())
        .unwrap_or_else(sadr_core::execution::ScreenPolicy::recommended);
    let allowed_buys = instruments
        .iter()
        .filter(|instrument| {
            // Cash liquidity/circuit screens never apply to option premiums.
            // An option buy is screened only by a fresh executable receipt.
            if sadr_core::market::Segment::from_key(&instrument.key)
                == Some(sadr_core::market::Segment::NseFo)
            {
                return quotes.get(&instrument.key).is_some_and(|quote| {
                    quote.fresh(millis(), sadr_core::market::MAX_FINANCIAL_AGE_MS)
                        && quote.ask >= quote.bid
                });
            }
            quotes
                .get(&instrument.key)
                .zip(
                    rows.values()
                        .find(|row| row["instrument_token"] == instrument.key),
                )
                .is_some_and(|(quote, row)| {
                    Scanner::market_allows_with_policy(millis(), instrument, quote, row, &policy)
                })
        })
        .map(|i| i.key.clone())
        .collect::<BTreeSet<_>>();
    commit_quotes_screened(
        db,
        c,
        p,
        &quotes,
        &checked,
        receipt,
        &archive,
        millis,
        Some(&allowed_buys),
        held_positions,
    )?;
    if let Ok(binding) = book_binding(c) {
        for review in &p.exit_reviews {
            let _ = redecision::enqueue_exit_review(
                db,
                &binding,
                &review.instrument_key,
                &review.reason,
                review.at_ms,
            );
        }
    }
    if !p.halted
        && let Some(cfg) = event_config
    {
        let result =
            redecision::observe_quotes(db, c, cfg, p, instruments, &quotes, rows, millis())
                .and_then(|()| redecision::sticky_blocked(db, c));
        match result {
            Ok(blocked) => {
                if blocked && let Some(cancel) = event_cancel {
                    cancel.store(true, std::sync::atomic::Ordering::Release);
                }
            }
            Err(_) => {
                if let Some(cancel) = event_cancel {
                    cancel.store(true, std::sync::atomic::Ordering::Release);
                }
                planner_incident(db, c, p, millis())?;
            }
        }
    }
    if let Some(receipt) = quotes
        .values()
        .filter(|q| q.fresh(millis(), sadr_core::market::MAX_FINANCIAL_AGE_MS))
        .map(|q| q.receipt)
        .max()
    {
        *last_fresh_receipt = Some(receipt);
    }
    Ok(None)
}
pub(super) fn pending_orders(_p: &Portfolio) -> u64 {
    // This paper runtime fills immediately; there is no broker order pipeline.
    0
}
fn resume_preflight(p: &Portfolio) -> std::result::Result<(), &'static str> {
    if LIVE_ENABLED {
        return Err("resume_live_enabled");
    }
    if !p.stopped {
        return Err("resume_not_operator_stopped");
    }
    if p.halted {
        return Err("resume_risk_halted");
    }
    if p.closed {
        return Err("resume_session_closed");
    }
    // Model-led books continue with their holdings and exit intents under protection.
    if !p.positions.is_empty() && !p.model_led {
        return Err("resume_open_positions");
    }
    if pending_orders(p) != 0 {
        return Err("resume_pending_orders");
    }
    Ok(())
}
pub(super) fn planner_recover(c: &Config, confirm: bool) -> Result<()> {
    planner_recover_checked(c, confirm, false, |c, p, cfg| {
        let instruments =
            history_instruments(c).map_err(|_| "planner_recover_input_unavailable")?;
        if !planner::has_eligible_input(c, p, &instruments)? {
            return Err("planner_recover_no_eligible_input".into());
        }
        redecision::news(cfg, c, &instruments)?;
        planner::require_usage(c)
    })
}
/// Reviewed protection-only recovery. The halt stays set, so entries remain
/// blocked by `Portfolio::halted`; held positions continue under surgical exits.
pub(super) fn planner_recover_protection_only(c: &Config, confirm: bool) -> Result<()> {
    planner_recover_checked(c, confirm, true, |_, _, _| Ok(()))
}
fn planner_recover_checked(
    c: &Config,
    confirm: bool,
    protection_only: bool,
    revalidate: impl FnOnce(&Config, &Portfolio, &planner::PlannerConfig) -> Result<()>,
) -> Result<()> {
    if !confirm {
        return Err("planner_recover_confirm_paper_review_required".into());
    }
    let _root_lock = rollover::controller_lock(c)?;
    let active = rollover::current(c)?;
    let c = &active;
    c.validate()?;
    if LIVE_ENABLED {
        return Err("planner_recover_live_enabled".into());
    }
    if !c.directory.join("paper.sqlite").is_file()
        || !c.directory.join("book-identity.json").is_file()
    {
        return Err("planner_recover_book_missing".into());
    }
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(c.directory.join("controller.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|_| "planner_recover_controller_busy")?;
    let _lock = rollover::ControllerLock(lock);
    let cfg = planner::PlannerConfig::load_for_session(
        c.planner_config
            .as_deref()
            .ok_or("planner_config_required")?,
        c.session,
    )?;
    let recovery_role = cfg.role(Trigger::Supervisor);
    if !cfg.enabled || !recovery_role.enabled {
        return Err("planner_recover_planner_disabled".into());
    }
    let mut db = open_book(c)?;
    let mut p = load_wide(&mut db, c)?;
    p.reconcile()?;
    if p.closed {
        if protection_only {
            return Err("planner_recover_protection_only_requires_open_book".into());
        }
        if !p.halted {
            return Err("planner_recover_halt_not_eligible_book_closed_not_halted".into());
        }
        if !p.positions.is_empty() || pending_orders(&p) != 0 {
            return Err("planner_recover_book_not_flat".into());
        }
        // A carried protective halt may have reached the close flat before the
        // operator cleared it; the same verified evidence still gates the clear.
        let carried_protective = rollover::protective_carry_evidence(c, &db)?.is_some();
        let reviewed_halt = Some(last_halt_reason(&db)?);
        if let Some((reason, _)) = &reviewed_halt
            && reason != "planner_authority_invalid_recovery_required"
            && !(carried_protective && reason == rollover::PROTECTIVE_HEALTH)
        {
            return Err("planner_recover_halt_not_eligible_last_halt_not_planner_authority".into());
        }
        if !redecision::has_authority(&db)? {
            return Err("planner_recover_authority_missing".into());
        }
        let prior = p.clone();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        redecision::recover(&tx, c, &cfg, redecision::RecoveryClearance::RetainIntegrity)?;
        p.halted = false;
        p.health = "closed".into();
        let financial_unchanged = p.cash == prior.cash
            && p.positions == prior.positions
            && p.fills == prior.fills
            && p.high_water == prior.high_water;
        tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_recovery_confirmed_paper_review')",
            rusqlite::params![millis(), serde_json::to_string(&serde_json::json!({"mode":"closed_flat","prior_health":prior.health,"reviewed_halt":reviewed_halt,"financial_state_unchanged":financial_unchanged,"closed":true,"stopped":p.stopped,"halted":false,"carried_protective":carried_protective}))?])?;
        tx.execute(
            "UPDATE state SET body=? WHERE id=1",
            [serde_json::to_string(&p)?],
        )?;
        tx.commit()?;
        println!("planner_recovered_closed_flat_halt_cleared");
        if let Some((reason, source)) = reviewed_halt {
            println!("planner_recovery_reviewed_halt: reason={reason}, source={source}");
        }
        return Ok(());
    }
    if protection_only {
        if !p.halted {
            return Err("planner_recover_protection_only_requires_planner_halt".into());
        }
        let reviewed_halt = Some(last_halt_reason(&db)?);
        if let Some((reason, _)) = &reviewed_halt
            && reason != "planner_authority_invalid_recovery_required"
        {
            return Err("planner_recover_halt_not_eligible_last_halt_not_planner_authority".into());
        }
        if !p.model_led {
            return Err("planner_recover_protection_only_requires_model_led_book".into());
        }
        if p.positions.is_empty() {
            return Err("planner_recover_protection_only_requires_open_position".into());
        }
        if pending_orders(&p) != 0 {
            return Err("planner_recover_pending_orders".into());
        }
        if !redecision::has_authority(&db)? {
            return Err("planner_recover_authority_missing".into());
        }
        let prior = p.clone();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        redecision::recover(&tx, c, &cfg, redecision::RecoveryClearance::RetainIntegrity)?;
        // Halt stays set: entries remain blocked while held positions continue
        // under the frozen protection policy on the next controller start.
        p.stopped = false;
        p.health = "risk_halted_protective_exits_only".into();
        let financial_unchanged = p.cash == prior.cash
            && p.positions == prior.positions
            && p.fills == prior.fills
            && p.high_water == prior.high_water;
        tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_protection_only_confirmed_paper_review')",
            rusqlite::params![millis(), serde_json::to_string(&serde_json::json!({"mode":"protection_only","prior_health":prior.health,"reviewed_halt":reviewed_halt,"financial_state_unchanged":financial_unchanged,"open_positions":p.positions.len(),"entries_blocked":true,"halted":true,"stopped":false}))?])?;
        tx.execute(
            "UPDATE state SET body=? WHERE id=1",
            [serde_json::to_string(&p)?],
        )?;
        tx.commit()?;
        println!("planner_protection_only_active_halt_retained");
        if let Some((reason, source)) = reviewed_halt {
            println!("planner_recovery_reviewed_halt: reason={reason}, source={source}");
        }
        return Ok(());
    }
    // A reviewed halted carry starts a protection-only successor whose halt is
    // cleared only here, and only once the carried position is flat.
    let protective_flat = p.halted && rollover::protective_carry_evidence(c, &db)?.is_some();
    let reviewed_halt = if p.halted {
        Some(last_halt_reason(&db)?)
    } else {
        None
    };
    let reviewed_block = if let Some((reason, _)) = &reviewed_halt {
        if reason == "planner_authority_invalid_recovery_required"
            || (protective_flat && reason == rollover::PROTECTIVE_HEALTH)
        {
            match redecision::recovery_block(&db, c)? {
                redecision::RecoveryBlock::Reviewed(block) => Some(block),
                _ => None,
            }
        } else {
            return Err("planner_recover_halt_not_eligible_last_halt_not_planner_authority".into());
        }
    } else {
        if !p.stopped || p.health != "operator_stopped" {
            return Err("planner_recover_halt_not_eligible_requires_operator_stopped_book".into());
        }
        Some(redecision::reviewed_stopped_block(&db, c)?)
    };
    if !p.positions.is_empty() || pending_orders(&p) != 0 {
        return Err("planner_recover_book_not_flat".into());
    }
    if !redecision::has_authority(&db)? {
        return Err("planner_recover_authority_missing".into());
    }
    // A stopped flat book revalidates every reviewed block. A halted book
    // revalidates only market-data blocks; a configuration block is resolved by
    // the exact rebind and an interrupted claim by exact retirement.
    let inputs_revalidated = if reviewed_halt.is_some() {
        reviewed_block.is_some_and(|block| block.requires_input_revalidation())
    } else {
        reviewed_block.is_some()
    };
    if inputs_revalidated {
        revalidate(c, &p, &cfg)?;
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    redecision::recover(&tx, c, &cfg, redecision::RecoveryClearance::Reviewed)?;
    let prior_health = p.health.clone();
    p.halted = false;
    p.stopped = true;
    p.health = "operator_stopped".into();
    let mode = if protective_flat {
        "flat_carried_protective"
    } else {
        "flat_open"
    };
    let reviewed_block = reviewed_block.map(|block| block.code());
    tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_recovery_confirmed_paper_review')",
        rusqlite::params![millis(), serde_json::to_string(&serde_json::json!({"mode":mode,"prior_health":prior_health,"reviewed_halt":reviewed_halt,"reviewed_block":reviewed_block,"inputs_revalidated":inputs_revalidated,"stopped":true,"entries_blocked_before":protective_flat}))?])?;
    tx.execute(
        "UPDATE state SET body=? WHERE id=1",
        [serde_json::to_string(&p)?],
    )?;
    tx.commit()?;
    println!("planner_recovered_operator_stopped");
    if let Some((reason, source)) = reviewed_halt {
        println!("planner_recovery_reviewed_halt: reason={reason}, source={source}");
    }
    Ok(())
}
pub(super) fn config_apply(c: &Config, confirm: bool) -> Result<()> {
    config_apply_with(c, confirm, None, true)
}
/// Reviewed config transition. `context` is recorded on the audited transition
/// tick for an operator command that names the change beside the policy hashes;
/// `report` prints the CLI line unless the caller owns its own output.
pub(super) fn config_apply_with(
    c: &Config,
    confirm: bool,
    context: Option<serde_json::Value>,
    report: bool,
) -> Result<()> {
    if !confirm {
        return Err("config_apply_confirm_paper_review_required".into());
    }
    let _root_lock = rollover::controller_lock(c)?;
    let active = rollover::current(c)?;
    let c = &active;
    c.validate()?;
    if LIVE_ENABLED {
        return Err("config_apply_live_enabled".into());
    }
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(c.directory.join("controller.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|_| "config_apply_controller_busy")?;
    let _lock = rollover::ControllerLock(lock);
    if !c.directory.join("paper.sqlite").is_file()
        || !c.directory.join("book-identity.json").is_file()
    {
        return Err("config_apply_book_missing".into());
    }
    let cfg = planner::PlannerConfig::load_for_session(
        c.planner_config
            .as_deref()
            .ok_or("planner_config_required")?,
        c.session,
    )?;
    let mut db = open_book(c)?;
    let mut p = load_wide(&mut db, c)?;
    p.reconcile()?;
    if p.halted {
        return Err("config_apply_halted_requires_planner_recover".into());
    }
    if p.closed {
        return Err("config_apply_book_closed".into());
    }
    let desired = cfg.model_led.unwrap_or(c.strategy == "model_planner_v1");
    // A continuing model-led book keeps its holdings, cash and exit intents across
    // a planner change; only the planner policy hash moves. Other books stay flat-only.
    let carried_ok = p.model_led && desired;
    if pending_orders(&p) != 0 || (!p.positions.is_empty() && !carried_ok) {
        return Err("config_apply_book_not_flat".into());
    }
    let before = serde_json::to_string(&p)?;
    if desired && c.strategy != "model_planner_v1" {
        return Err("config_apply_model_led_selector_mismatch".into());
    }
    if desired != p.model_led && (!p.fills.is_empty() || p.cash != c.allocation()?) {
        return Err("config_apply_model_led_requires_pristine_book".into());
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    redecision::apply_config(&tx, c, &cfg)?;
    p.model_led = desired;
    p.stopped = true;
    p.health = "operator_stopped".into();
    p.reconcile()?;
    let after = serde_json::to_string(&p)?;
    let mut audit = serde_json::json!({"prior_state_sha256":digest(before.as_bytes()),"current_state_sha256":digest(after.as_bytes()),"configuration_sha256":digest(&serde_json::to_vec(&cfg)?),"model_led":desired,"usage_preserved":true,"stopped":true,"live_enabled":false});
    if let Some(context) = context {
        audit["operator_command"] = context;
    }
    tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_config_apply_confirmed_paper_review')",
        rusqlite::params![millis(), audit.to_string()])?;
    tx.execute("UPDATE state SET body=? WHERE id=1", [&after])?;
    tx.commit()?;
    if report {
        println!("config_applied_operator_stopped_resume_required");
    }
    Ok(())
}
/// Offline metadata repair for a missing, malformed or stale authority marker.
/// The controller lock proves the controller is stopped; no book state is written.
pub(super) fn planner_authority_repair(
    c: &Config,
    expected_owner: &str,
    confirm: bool,
) -> Result<()> {
    if !confirm {
        return Err("planner_authority_repair_confirm_paper_review_required".into());
    }
    let expected_owner = match expected_owner {
        "orchestrator" => redecision::AuthorityOwner::Orchestrator,
        "legacy" => redecision::AuthorityOwner::Legacy,
        _ => return Err("planner_authority_repair_expected_owner_invalid".into()),
    };
    let _root_lock = rollover::controller_lock(c)?;
    let active = rollover::current(c)?;
    let c = &active;
    c.validate()?;
    if LIVE_ENABLED {
        return Err("planner_authority_repair_live_enabled".into());
    }
    if !c.directory.join("paper.sqlite").is_file()
        || !c.directory.join("book-identity.json").is_file()
    {
        return Err("planner_authority_repair_book_missing".into());
    }
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(c.directory.join("controller.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|_| "planner_authority_repair_controller_busy")?;
    let _lock = rollover::ControllerLock(lock);
    let cfg = planner::PlannerConfig::load_for_session(
        c.planner_config
            .as_deref()
            .ok_or("planner_config_required")?,
        c.session,
    )?;
    let mut db = open_book(c)?;
    let receipt = redecision::repair_authority(&mut db, c, &cfg, expected_owner)?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "authority_repaired":true,
            "kind":receipt.kind,
            "prior_marker_sha256":receipt.prior_marker_sha256,
            "control_config_hash":receipt.control_config_hash,
            "current_config_hash":receipt.current_config_hash,
            "transition":receipt.transition.as_ref().map(|transition| transition.kind.clone())
        }))?
    );
    Ok(())
}

pub(super) fn resume(c: &Config, confirm: bool) -> Result<()> {
    if !confirm {
        return Err(Fail::new("resume_confirm_paper_review_required").into());
    }
    // Same active book as the paper controller. The config directory can be a
    // closed predecessor while sessions_root/current.json names today's book.
    let _root_lock = rollover::controller_lock(c)?;
    let active = rollover::current(c)?;
    let c = &active;
    c.validate()?;
    if LIVE_ENABLED {
        return Err(Fail::new("resume_live_enabled").into());
    }
    if !c.directory.join("paper.sqlite").is_file()
        || !c.directory.join("book-identity.json").is_file()
    {
        return Err(Fail::new("resume_book_missing").into());
    }
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(c.directory.join("controller.lock"))
        .map_err(|_| Fail::new("resume_controller_busy"))?;
    lock.try_lock_exclusive()
        .map_err(|_| Fail::new("resume_controller_busy"))?;
    let _lock = rollover::ControllerLock(lock);
    let mut db = open_book(c)?;
    let mut p = load_wide(&mut db, c)?;
    p.reconcile()?;
    resume_preflight(&p).map_err(Fail::from)?;
    let before = serde_json::to_string(&p)?;
    let planner_cfg = c
        .planner_config
        .as_deref()
        .map(|path| planner::PlannerConfig::load_for_session(path, c.session))
        .transpose()?;
    // Starting with an unapplied planner config halts on planner authority; refuse here.
    if p.model_led
        && let Some(cfg) = &planner_cfg
        && !redecision::config_current(&db, cfg)?
    {
        return Err(Fail::new("resume_planner_config_not_applied").into());
    }
    let migrating = !p.model_led
        && c.strategy == "model_planner_v1"
        && planner_cfg
            .as_ref()
            .is_none_or(|cfg| cfg.model_led != Some(false));
    if migrating {
        if !p.fills.is_empty() || p.cash != c.allocation()? {
            return Err("resume_model_led_requires_pristine_book".into());
        }
        // Keep the serialized config and event authority hash unchanged. This is
        // an audited financial-state transition, not a config/journal replacement.
        redecision::check_model_led_migration(&db, c)?;
        p.model_led = true;
    }
    p.stopped = false;
    p.health = "waiting".into();
    p.reconcile()?;
    let after = serde_json::to_string(&p)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if migrating {
        tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_model_led_enabled_v1')",
            rusqlite::params![millis(), serde_json::json!({"prior_state":serde_json::from_str::<serde_json::Value>(&before)?,"prior_state_sha256":digest(before.as_bytes()),"new_state_sha256":digest(after.as_bytes()),"binding":book_binding(c)?,"financial_values_unchanged":true,"model_led":true}).to_string()])?;
    }
    tx.execute("UPDATE state SET body=? WHERE id=1", [&after])?;
    tx.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,NULL,'operator_resume_confirmed_paper_review')", [millis()])?;
    tx.commit()?;
    println!(
        "{}",
        serde_json::json!({
            "paper_only": true,
            "live_enabled": LIVE_ENABLED,
            "open_positions": p.positions.len(),
            "pending_orders": 0,
            "stopped": false,
            "halted": false,
            "resumed": true,
            "model_led": p.model_led,
            "audit_event": "operator_resume_confirmed_paper_review",
        })
    );
    Ok(())
}
pub(super) fn history_instruments(c: &Config) -> Result<Vec<Instrument>> {
    Ok(universe::cached(c)?.instruments)
}
pub(super) fn preview_instruments(client: &data::Client, c: &Config) -> Result<Vec<Instrument>> {
    Ok(universe::prepare(client, c)?.instruments)
}

#[cfg(test)]
mod tests;
