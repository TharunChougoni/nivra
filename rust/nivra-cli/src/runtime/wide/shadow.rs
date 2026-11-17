//! Shadow strategy selection and the sandboxed shadow scorer. Neither one
//! places paper orders; each only annotates the discovery projection.
use super::*;

pub(super) fn shadow_regime_inputs(
    scan: &Scanner,
    now: i64,
    max_age: i64,
    minute_returns: &[f64],
) -> nivra_core::regime::RegimeInputs {
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
    nivra_core::regime::compute_inputs(&prior, &opening, &current, minute_returns)
        .expect("aligned regime slices")
}

fn bounded_name(name: &str) -> String {
    name.chars().take(128).collect()
}

#[derive(Clone, Debug)]
pub(super) struct LiveFallback {
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
    Sandbox(nivra_sandbox::Failure),
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
    fn from_library(error: nivra_sandbox::library::Error) -> Self {
        use nivra_sandbox::library::Error::*;
        match error {
            NotFound => Self::MissingSource,
            SizeLimit => Self::SourceTooLarge,
            InvalidInput | InvalidUtf8 | HashMismatch | Ambiguous => Self::InvalidSource,
            Storage | WrongDatabase | HashNameConflict => Self::Library,
        }
    }
}
pub(super) fn sandbox_failure_detail(failure: nivra_sandbox::Failure) -> &'static str {
    use nivra_sandbox::Failure::*;
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
    reason: nivra_sandbox::selector::SelectionReason,
) -> (&'static str, &'static str) {
    use nivra_sandbox::selector::SelectionReason::*;
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
pub(super) struct ShadowSelection {
    pub(super) last_evaluation: Option<i64>,
    pub(super) last_live: Option<i64>,
    pub(super) state: nivra_sandbox::selector::SelectorState,
    pub(super) reason: Option<nivra_sandbox::selector::SelectionReason>,
    pub(super) regime: Option<String>,
    pub(super) status: Option<serde_json::Value>,
    pub(super) mode: super::super::discovery::StrategySelectionMode,
    pub(super) chosen_name: Option<String>,
    pub(super) previous_active: Option<String>,
    pub(super) live: Option<super::super::discovery::LiveRanking>,
    pub(super) source: Option<nivra_sandbox::library::Version>,
    pub(super) fallback: Option<LiveFallback>,
    pub(super) fallback_count: u64,
    pub(super) recent: std::collections::VecDeque<serde_json::Value>,
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
    pub(super) fn regime(&self) -> serde_json::Value {
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
    pub(super) fn evaluate(
        &mut self,
        settings: &super::super::discovery::Settings,
        scan: &Scanner,
        now: i64,
        stale: bool,
        minute_returns: &[f64],
    ) {
        use nivra_sandbox::{
            library::Library,
            selector::{CandidateEvidence, ScorerHealth, select},
        };
        let Some(cfg) = settings.strategy_selection.as_ref().filter(|s| s.enabled) else {
            self.mode = super::super::discovery::StrategySelectionMode::Shadow;
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
        let regime = nivra_core::regime::classify(&inputs, &cfg.regime);
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
            (regime != nivra_core::regime::Regime::Unknown).then_some(label.as_str()),
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
        if cfg.mode.is_live() && self.state.active == nivra_sandbox::selector::DEFAULT_ID {
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
    ) -> std::result::Result<nivra_sandbox::library::Version, LiveFailure> {
        if let Some(version) = self.source.as_ref().filter(|version| version.sha256 == sha) {
            return Ok(version.clone());
        }
        let db = nivra_sandbox::library::Library::open_read_only(path)
            .map_err(|_| LiveFailure::Library)?;
        let version = db.version_by_sha(sha).map_err(LiveFailure::from_library)?;
        self.source = Some(version.clone());
        Ok(version)
    }
    pub(super) fn live_ranking(
        &mut self,
        settings: &super::super::discovery::Settings,
        scan: &Scanner,
        instruments: &[Instrument],
        projection: &serde_json::Value,
        now: i64,
    ) -> Option<super::super::discovery::LiveRanking> {
        let Some(cfg) = settings.strategy_selection.as_ref().filter(|s| s.enabled) else {
            self.mode = super::super::discovery::StrategySelectionMode::Shadow;
            self.live = None;
            return None;
        };
        if !cfg.mode.is_live() {
            self.mode = super::super::discovery::StrategySelectionMode::Shadow;
            self.live = None;
            return None;
        }
        self.mode = super::super::discovery::StrategySelectionMode::Live;
        if self.state.active == nivra_sandbox::selector::DEFAULT_ID {
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
    pub(super) fn cached_live(&self) -> Option<&super::super::discovery::LiveRanking> {
        self.live.as_ref()
    }
    pub(super) fn attach(&self, discovery: &mut Option<serde_json::Value>) {
        if let (Some(discovery), Some(status)) = (discovery, &self.status) {
            discovery["strategy_selection"] = status.clone();
        }
    }
}

fn live_overlaps(
    live: &super::super::discovery::LiveRanking,
    projection: &serde_json::Value,
) -> bool {
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
    settings: &super::super::discovery::Settings,
    version: &nivra_sandbox::library::Version,
    scan: &Scanner,
    instruments: &[Instrument],
    projection: &serde_json::Value,
    now: i64,
) -> std::result::Result<super::super::discovery::LiveRanking, LiveFailure> {
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
    let snapshot = nivra_core::strategy::extract_features_with_settings(
        &cache,
        now,
        settings.max_scan_age_ms,
        metadata_age,
        nivra_core::strategy::default_lookback_samples(),
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
    let scores = nivra_sandbox::score_source(
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
    let ranking = super::super::discovery::LiveRanking {
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
pub(super) struct ShadowScorer {
    pub(super) pending: Option<std::sync::mpsc::Receiver<serde_json::Value>>,
    #[cfg(test)]
    pub(super) synchronous: bool,
    pub(super) last_evaluation: Option<i64>,
    pub(super) status: Option<serde_json::Value>,
}
impl ShadowScorer {
    pub(super) fn poll(&mut self) {
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
    pub(super) fn evaluate(
        &mut self,
        settings: &super::super::discovery::Settings,
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
            super::super::discovery::project(&cache, instruments, &default_settings, now);
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
        settings: super::super::discovery::Settings,
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
            let snapshot = nivra_core::strategy::extract_features_with_settings(
                &cache,
                now,
                settings.max_scan_age_ms,
                metadata_age,
                nivra_core::strategy::default_lookback_samples(),
                &strategy.feature_settings,
            )
            .map_err(|_| nivra_sandbox::Failure::Input)?;
            let mut features: nivra_sandbox::Features = snapshot
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
                return Err(nivra_sandbox::Failure::Input);
            }
            let file =
                std::fs::File::open(&cfg.params_path).map_err(|_| nivra_sandbox::Failure::Input)?;
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(262145)
                .read_to_end(&mut bytes)
                .map_err(|_| nivra_sandbox::Failure::Input)?;
            if bytes.len() > 262144 {
                return Err(nivra_sandbox::Failure::Input);
            }
            let params: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| nivra_sandbox::Failure::Input)?;
            if !params.is_object() {
                return Err(nivra_sandbox::Failure::Input);
            }
            features.insert("params".into(), params);
            let scores = nivra_sandbox::score(
                &settings.scorer_sandbox.clone().unwrap_or_default(),
                std::path::Path::new(&cfg.scorer_path),
                &features,
            )?;
            if scores.contains_key("params") {
                return Err(nivra_sandbox::Failure::OutputInvalid);
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
    pub(super) fn restrict_input(
        features: &mut nivra_sandbox::Features,
        shortlist: Option<&BTreeSet<String>>,
    ) {
        if let Some(keys) = shortlist {
            features.retain(|key, _| keys.contains(key));
        }
    }
    pub(super) fn comparison(
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
    pub(super) fn attach(&self, discovery: &mut Option<serde_json::Value>) {
        if let (Some(discovery), Some(status)) = (discovery, &self.status) {
            discovery["shadow_scorer"] = status.clone();
        }
    }
}
