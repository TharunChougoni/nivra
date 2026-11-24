use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    /// Default-valued blocks are omitted so sealed planner policy hashes stay stable (#66).
    #[serde(
        default,
        skip_serializing_if = "nivra_core::prefilter::PrefilterSettings::is_default"
    )]
    pub prefilter: nivra_core::prefilter::PrefilterSettings,
    #[serde(
        default,
        skip_serializing_if = "nivra_core::feature_store::FeatureStoreSettings::is_default"
    )]
    pub feature_store: nivra_core::feature_store::FeatureStoreSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_selection: Option<StrategySelectionSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow_scorer: Option<ShadowScorerSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scorer_sandbox: Option<nivra_sandbox::ScorerSandboxConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<nivra_core::strategy::StrategySettings>,
    /// Dedicated strategy ledger: approval admission reads and deterministic scan writes.
    /// Orchestrator evidence reads never gain producer-write capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_results_database: Option<std::path::PathBuf>,
    /// Deterministic scan persistence bounds; active only with an enabled strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_results: Option<super::super::strategy_results::Settings>,
    /// Required measured shadow days; absent means five.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_min_shadow_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screens: Option<nivra_core::execution::ScreenPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_feed: Option<super::super::market_feed::Settings>,
    pub max_scan_age_ms: i64,
    pub max_prospects: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_batch_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_interval_ms: Option<i64>,
    /// A scanner prospect wakes Luna only when its sampled move reaches this many bps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_move_bps: Option<f64>,
    /// The same prospect cannot wake Luna again within this window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_cooldown_ms: Option<i64>,
    /// Global minimum gap between scanner triggers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_trigger_gap_ms: Option<i64>,
    /// Fallback: with no triggered digest, run one after this long since the last
    /// digest of any kind. Absent means no idle digests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_digest_interval_ms: Option<i64>,
    /// Owner-policy cadence: compile fresh facts after this long without a
    /// material event. Absent means 60 s for the owner; legacy policies ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_cadence_ms: Option<i64>,
    /// Measure scanner moves against the median move of all fresh scanner samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_market_relative: Option<bool>,
    /// REST ranking rows older than this are dropped. Absent means 60 s (#41).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking_window_ms: Option<i64>,
    /// Financial quote freshness gate; may only tighten the two-second rule (#41).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_freshness_ms: Option<i64>,
}
/// Shadow diagnostics are the default; live applies scores to scanner prospects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum StrategySelectionMode {
    #[default]
    Shadow,
    Live,
}
impl StrategySelectionMode {
    fn is_shadow(&self) -> bool {
        matches!(self, Self::Shadow)
    }
    pub(crate) fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Live => "live",
        }
    }
}
/// Selector diagnostics by default; live mode is a config-gated paper ranking path.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct StrategySelectionSettings {
    pub enabled: bool,
    #[serde(skip_serializing_if = "StrategySelectionMode::is_shadow")]
    pub mode: StrategySelectionMode,
    pub library_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paper_trial_sha256: Option<String>,
    pub evaluate_ms: i64,
    pub regime: nivra_core::regime::RegimeConfig,
    pub selector: nivra_sandbox::selector::SelectorConfig,
}
impl Default for StrategySelectionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: StrategySelectionMode::Shadow,
            library_path: String::new(),
            paper_trial_sha256: None,
            evaluate_ms: 60_000,
            regime: Default::default(),
            selector: Default::default(),
        }
    }
}
impl StrategySelectionSettings {
    fn validate(&self) -> Result<()> {
        if self.evaluate_ms <= 0 || (self.enabled && self.library_path.trim().is_empty()) {
            return Err("invalid strategy_selection path or interval".into());
        }
        // Live ranking changes the scanner shortlist; an enabled Tier 1 champion
        // already owns ranking. Refuse the ambiguous combination.
        if self.mode.is_live() && !self.enabled {
            return Err("strategy_selection_live_requires_enabled".into());
        }
        if self.paper_trial_sha256.as_ref().is_some_and(|hash| {
            !self.mode.is_live()
                || hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err("strategy_selection_paper_trial_invalid".into());
        }
        self.regime.validate()?;
        self.selector.validate()?;
        Ok(())
    }
}
/// Scores are diagnostic only and never enter model projections.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ShadowScorerSettings {
    pub enabled: bool,
    pub shortlist_only: bool,
    pub scorer_path: String,
    pub params_path: String,
    pub evaluate_ms: i64,
    pub top_n: usize,
}
impl Default for ShadowScorerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            shortlist_only: false,
            scorer_path: String::new(),
            params_path: String::new(),
            evaluate_ms: 60_000,
            top_n: 10,
        }
    }
}
impl ShadowScorerSettings {
    fn validate(&self) -> Result<()> {
        if self.evaluate_ms <= 0
            || self.top_n == 0
            || self.top_n > 4096
            || (self.enabled
                && (self.scorer_path.trim().is_empty() || self.params_path.trim().is_empty()))
        {
            return Err("invalid shadow_scorer paths, interval or top_n".into());
        }
        Ok(())
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        self.feature_store.validate()?;
        self.prefilter.validate()?;
        if let Some(limits) = &self.scorer_sandbox {
            limits.validate().map_err(|_| "invalid scorer_sandbox")?;
            if limits.network
                && self
                    .strategy_selection
                    .as_ref()
                    .is_some_and(|selection| selection.paper_trial_sha256.is_some())
            {
                return Err("strategy_selection_paper_trial_requires_no_network".into());
            }
        }
        if let Some(scorer) = &self.shadow_scorer {
            scorer.validate()?;
        }
        if let Some(selection) = &self.strategy_selection {
            selection.validate()?;
            if selection.mode.is_live() && self.strategy.as_ref().is_some_and(|s| s.enabled) {
                return Err("strategy_selection_live_conflicts_with_champion".into());
            }
        }
        if let Some(results) = &self.strategy_results {
            results.validate()?;
        }
        if let Some(strategy) = &self.strategy {
            strategy.validate()?;
            if strategy.enabled {
                let champion = strategy
                    .specs
                    .iter()
                    .find(|s| s.id == strategy.champion)
                    .ok_or("strategy_champion_missing")?;
                let path = self
                    .strategy_results_database
                    .as_deref()
                    .ok_or("strategy_champion_approval_database_required")?;
                let db = nivra_store::strategy_results::ResultsDb::open_read_only(path)?;
                db.require_approved(
                    &nivra_store::strategy_results::proposal_hash(champion)?,
                    self.strategy_min_shadow_days.unwrap_or(5),
                )?;
            }
        }
        if let Some(screens) = &self.screens {
            screens.validate()?;
        }
        if let Some(feed) = &self.market_feed {
            feed.validate()?;
        }
        if self
            .strategy_min_shadow_days
            .is_some_and(|n| !(1..=365).contains(&n))
            || self.max_scan_age_ms <= 0
            || self.max_prospects == 0
            || self.scan_batch_size.is_some_and(|n| n == 0 || n > 200)
            || self.scan_interval_ms.is_some_and(|n| n <= 0)
            || self
                .trigger_move_bps
                .is_some_and(|n| !(n.is_finite() && n > 0.0))
            || self.trigger_cooldown_ms.is_some_and(|n| n < 0)
            || self.min_trigger_gap_ms.is_some_and(|n| n < 0)
            || self.idle_digest_interval_ms.is_some_and(|n| n <= 0)
            || self
                .digest_cadence_ms
                .is_some_and(|n| !(1_000..=3_600_000).contains(&n))
            || self.ranking_window_ms.is_some_and(|n| n <= 0)
            || self
                .quote_freshness_ms
                .is_some_and(|n| !(1..=MAX_QUOTE_FRESHNESS_MS).contains(&n))
        {
            return Err("planner_discovery_settings_invalid".into());
        }
        Ok(())
    }
}
pub(crate) const DEFAULT_SCAN_BATCH_SIZE: usize = 200;
pub(crate) const DEFAULT_RANKING_WINDOW_MS: i64 = 60_000;
/// AGENTS.md: the two-second financial freshness gate is a hard upper bound.
pub(crate) const MAX_QUOTE_FRESHNESS_MS: i64 = nivra_core::market::MAX_FINANCIAL_AGE_MS;
