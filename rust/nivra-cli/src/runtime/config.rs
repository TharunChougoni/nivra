use super::operational_health::CalendarCoverage;
use super::*;

/// Operator-chosen warning horizon for the verified embedded calendar. It is
/// not an exchange guarantee, a risk cap or a holiday forecast.
pub(crate) const CALENDAR_EXPIRY_WARNING_DAYS: i64 = 30;

pub(crate) fn default_history_preparation_max_fetch_failures() -> usize {
    25
}
pub(crate) fn default_history_avg_daily_volume_min_sessions() -> u32 {
    10
}
pub(crate) fn default_history_preparation_max_attempts() -> usize {
    3
}
pub(crate) fn default_history_preparation_retry_ms() -> i64 {
    60_000
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MarketDataTransportSettings {
    pub http_timeout_ms: u64,
}
impl Default for MarketDataTransportSettings {
    fn default() -> Self {
        Self {
            http_timeout_ms: 7000,
        }
    }
}
impl MarketDataTransportSettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct StorageSettings {
    pub read_journal_busy_timeout_ms: u64,
}
impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            read_journal_busy_timeout_ms: 1000,
        }
    }
}
impl StorageSettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DashboardSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument_catalog: Option<instrument_display::Settings>,
    #[serde(skip_serializing_if = "DashboardSettings::default_backtest_timeout_ms")]
    pub backtest_timeout_ms: u64,
    #[serde(skip_serializing_if = "DashboardSettings::default_history_max_files")]
    pub history_max_files: usize,
    pub refresh_interval_ms: u64,
    /// Retired Preact setting; retained only for deployed config/hash round trips.
    /// The v1/React workspace does not read this value.
    #[serde(skip_serializing_if = "DashboardSettings::default_model_feed_unavailable_grace_ms")]
    pub model_feed_unavailable_grace_ms: u64,
    #[serde(skip_serializing_if = "DashboardSettings::default_workers")]
    pub workers: usize,
    #[serde(skip_serializing_if = "DashboardSettings::default_queue")]
    pub queue: usize,
    /// Retired static-asset max-age; retained only for deployed config/hash
    /// round trips. The workspace shell always revalidates with its ETag.
    #[serde(skip_serializing_if = "DashboardSettings::default_static_max_age_s")]
    pub static_max_age_s: u64,
    #[serde(skip_serializing_if = "DashboardSettings::default_gzip_min_bytes")]
    pub gzip_min_bytes: usize,
    #[serde(skip_serializing_if = "DashboardSettings::default_history_read_max_bytes")]
    pub history_read_max_bytes: usize,
    /// Retired legacy listing cache switch; retained for config/hash compatibility.
    /// The transcript index owns v1 listing and does not read this value.
    #[serde(skip_serializing_if = "DashboardSettings::default_history_list_cache")]
    pub history_list_cache: bool,
    #[serde(skip_serializing_if = "DashboardSettings::default_sse_max_streams")]
    pub sse_max_streams: usize,
    #[serde(skip_serializing_if = "DashboardSettings::default_sse_heartbeat_ms")]
    pub sse_heartbeat_ms: u64,
    #[serde(skip_serializing_if = "DashboardSettings::default_sse_write_timeout_ms")]
    pub sse_write_timeout_ms: u64,
    #[serde(skip_serializing_if = "DashboardSettings::default_longpoll_hold_ms")]
    pub longpoll_hold_ms: u64,
    #[serde(skip_serializing_if = "DashboardSettings::default_transcript_max_bytes")]
    pub transcript_max_bytes: usize,
    #[serde(skip_serializing_if = "DashboardSettings::default_transcript_text_max_bytes")]
    pub transcript_text_max_bytes: usize,
    #[serde(skip_serializing_if = "DashboardSettings::default_transcript_args_max_bytes")]
    pub transcript_args_max_bytes: usize,
    #[serde(skip_serializing_if = "DashboardSettings::default_blob_slice_max_bytes")]
    pub blob_slice_max_bytes: usize,
    #[serde(skip_serializing_if = "DashboardSettings::default_series_max_points")]
    pub series_max_points: usize,
    #[serde(skip_serializing_if = "DashboardSettings::default_series_max_range_ms")]
    pub series_max_range_ms: u64,
    #[serde(skip_serializing_if = "DashboardSettings::default_equity_max_points")]
    pub equity_max_points: usize,
    #[serde(skip_serializing_if = "DashboardSettings::default_equity_sample_interval_ms")]
    pub equity_sample_interval_ms: u64,
    #[serde(skip_serializing_if = "DashboardSettings::default_price_batch_instruments")]
    pub price_batch_instruments: usize,
    #[serde(skip_serializing_if = "DashboardSettings::default_price_batch_interval_ms")]
    pub price_batch_interval_ms: u64,
    #[serde(skip_serializing_if = "DashboardSettings::default_failed_auth_per_minute")]
    pub failed_auth_per_minute: usize,
    #[serde(default, skip_serializing_if = "RetentionSettings::is_default")]
    pub retention: RetentionSettings,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub model_prices: std::collections::BTreeMap<String, ModelPrice>,
}
impl Default for DashboardSettings {
    fn default() -> Self {
        Self {
            instrument_catalog: None,
            refresh_interval_ms: 5000,
            backtest_timeout_ms: 3000,
            history_max_files: 256,
            model_feed_unavailable_grace_ms: 5000,
            workers: 4,
            queue: 16,
            static_max_age_s: 86_400,
            gzip_min_bytes: 1024,
            history_read_max_bytes: 8_000_000,
            history_list_cache: true,
            sse_max_streams: 3,
            sse_heartbeat_ms: 15_000,
            sse_write_timeout_ms: 30_000,
            longpoll_hold_ms: 25_000,
            transcript_max_bytes: 2_097_152,
            transcript_text_max_bytes: 16_384,
            transcript_args_max_bytes: 4096,
            blob_slice_max_bytes: 262_144,
            series_max_points: 2_000,
            series_max_range_ms: 86_400_000,
            equity_max_points: 4_096,
            equity_sample_interval_ms: 15_000,
            price_batch_instruments: 24,
            price_batch_interval_ms: 2_000,
            failed_auth_per_minute: 30,
            retention: RetentionSettings::default(),
            model_prices: std::collections::BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_after_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_after_days: Option<u32>,
    /// Newest `wide_quotes` bodies left intact in a closed sibling session.
    #[serde(default = "default_keep_full_quote_bodies")]
    pub keep_full_quote_bodies: usize,
    /// Older `wide_quotes` bodies above this size become sha256 receipts.
    #[serde(default = "default_max_quote_body_bytes")]
    pub max_quote_body_bytes: usize,
    /// `plan.json`, `output.json` and `input.json` above this size become receipts.
    #[serde(default = "default_max_model_file_bytes")]
    pub max_model_file_bytes: u64,
}
pub(crate) fn default_keep_full_quote_bodies() -> usize {
    32
}
pub(crate) fn default_max_quote_body_bytes() -> usize {
    4096
}
pub(crate) fn default_max_model_file_bytes() -> u64 {
    65_536
}
impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            pack_after_days: None,
            delete_after_days: None,
            keep_full_quote_bodies: default_keep_full_quote_bodies(),
            max_quote_body_bytes: default_max_quote_body_bytes(),
            max_model_file_bytes: default_max_model_file_bytes(),
        }
    }
}
impl RetentionSettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelPrice {
    pub input_inr_per_mtok: String,
    pub output_inr_per_mtok: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_inr_per_mtok: Option<String>,
}
impl DashboardSettings {
    fn default_backtest_timeout_ms(v: &u64) -> bool {
        *v == 3000
    }
    fn default_history_max_files(v: &usize) -> bool {
        *v == 256
    }
    fn default_model_feed_unavailable_grace_ms(v: &u64) -> bool {
        *v == 5000
    }
    fn default_workers(v: &usize) -> bool {
        *v == 4
    }
    fn default_queue(v: &usize) -> bool {
        *v == 16
    }
    fn default_static_max_age_s(v: &u64) -> bool {
        *v == 86_400
    }
    fn default_gzip_min_bytes(v: &usize) -> bool {
        *v == 1024
    }
    fn default_history_read_max_bytes(v: &usize) -> bool {
        *v == 8_000_000
    }
    fn default_history_list_cache(v: &bool) -> bool {
        *v
    }
    fn default_sse_max_streams(v: &usize) -> bool {
        *v == 3
    }
    fn default_sse_heartbeat_ms(v: &u64) -> bool {
        *v == 15_000
    }
    fn default_sse_write_timeout_ms(v: &u64) -> bool {
        *v == 30_000
    }
    fn default_longpoll_hold_ms(v: &u64) -> bool {
        *v == 25_000
    }
    fn default_transcript_max_bytes(v: &usize) -> bool {
        *v == 2_097_152
    }
    fn default_transcript_text_max_bytes(v: &usize) -> bool {
        *v == 16_384
    }
    fn default_transcript_args_max_bytes(v: &usize) -> bool {
        *v == 4096
    }
    fn default_blob_slice_max_bytes(v: &usize) -> bool {
        *v == 262_144
    }
    fn default_series_max_points(v: &usize) -> bool {
        *v == 2_000
    }
    fn default_series_max_range_ms(v: &u64) -> bool {
        *v == 86_400_000
    }
    fn default_equity_max_points(v: &usize) -> bool {
        *v == 4_096
    }
    fn default_equity_sample_interval_ms(v: &u64) -> bool {
        *v == 15_000
    }
    fn default_price_batch_instruments(v: &usize) -> bool {
        *v == 24
    }
    fn default_price_batch_interval_ms(v: &u64) -> bool {
        *v == 2_000
    }
    fn default_failed_auth_per_minute(v: &usize) -> bool {
        *v == 30
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=3000).contains(&self.backtest_timeout_ms)
            || !(1..=256).contains(&self.history_max_files)
            || self.model_feed_unavailable_grace_ms > 60_000
            || !(1..=16).contains(&self.workers)
            || !(1..=256).contains(&self.queue)
            || self.static_max_age_s > 31_536_000
            || self.gzip_min_bytes > 1_048_576
            || !(1_000_000..=64_000_000).contains(&self.history_read_max_bytes)
            || self.sse_max_streams > 16
            || !(1_000..=60_000).contains(&self.sse_heartbeat_ms)
            || !(1_000..=60_000).contains(&self.sse_write_timeout_ms)
            || !(1_000..=60_000).contains(&self.longpoll_hold_ms)
            || !(65_536..=16_777_216).contains(&self.transcript_max_bytes)
            || !(1_024..=262_144).contains(&self.transcript_text_max_bytes)
            || !(256..=65_536).contains(&self.transcript_args_max_bytes)
            || !(4_096..=4_194_304).contains(&self.blob_slice_max_bytes)
            || !(64..=10_000).contains(&self.series_max_points)
            || !(60_000..=604_800_000).contains(&self.series_max_range_ms)
            || !(64..=65_536).contains(&self.equity_max_points)
            || !(1_000..=300_000).contains(&self.equity_sample_interval_ms)
            || !(1..=256).contains(&self.price_batch_instruments)
            || !(500..=60_000).contains(&self.price_batch_interval_ms)
            || !(1..=600).contains(&self.failed_auth_per_minute)
            || !(1..=4_096).contains(&self.retention.keep_full_quote_bodies)
            || !(256..=1_048_576).contains(&self.retention.max_quote_body_bytes)
            || !(1_024..=16_777_216).contains(&self.retention.max_model_file_bytes)
        {
            return Err("dashboard_settings_invalid".into());
        }
        if let Some(settings) = &self.instrument_catalog {
            settings.validate()?;
        }
        for days in [
            self.retention.pack_after_days,
            self.retention.delete_after_days,
        ]
        .into_iter()
        .flatten()
        {
            if !(1..=365).contains(&days) {
                return Err("dashboard_settings_invalid".into());
            }
        }
        if let (Some(pack), Some(delete)) = (
            self.retention.pack_after_days,
            self.retention.delete_after_days,
        ) && delete <= pack
        {
            return Err("dashboard_settings_invalid".into());
        }
        for price in self.model_prices.values() {
            if nivra_core::finance::Amount::parse(&price.input_inr_per_mtok).is_err()
                || nivra_core::finance::Amount::parse(&price.output_inr_per_mtok).is_err()
                || price
                    .cached_inr_per_mtok
                    .as_deref()
                    .is_some_and(|text| nivra_core::finance::Amount::parse(text).is_err())
            {
                return Err("dashboard_settings_invalid".into());
            }
        }
        Ok(())
    }
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryDownloadSettings {
    pub quota_wait_max_attempts: usize,
    pub quota_wait_poll_ms: u64,
    #[serde(skip_serializing_if = "HistoryDownloadSettings::default_reserve_second")]
    pub reserve_second_reads: u32,
    #[serde(skip_serializing_if = "HistoryDownloadSettings::default_reserve_minute")]
    pub reserve_minute_reads: u32,
    #[serde(skip_serializing_if = "HistoryDownloadSettings::default_reserve")]
    pub reserve_half_hour_reads: u32,
}
impl Default for HistoryDownloadSettings {
    fn default() -> Self {
        Self {
            quota_wait_max_attempts: 1800,
            quota_wait_poll_ms: 1000,
            reserve_second_reads: 2,
            reserve_minute_reads: 30,
            reserve_half_hour_reads: 300,
        }
    }
}
impl HistoryDownloadSettings {
    fn default_reserve_second(value: &u32) -> bool {
        *value == 2
    }
    fn default_reserve_minute(value: &u32) -> bool {
        *value == 30
    }
    fn default_reserve(value: &u32) -> bool {
        *value == 300
    }
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if self.quota_wait_max_attempts > 1800
            || !(1..=60_000).contains(&self.quota_wait_poll_ms)
            || self.reserve_half_hour_reads >= 1500
            || self.reserve_second_reads >= 5
            || self.reserve_minute_reads >= 120
        {
            return Err("history_download_settings_invalid".into());
        }
        Ok(())
    }
}
pub(crate) fn default_universe_prepare_retry_ms() -> u64 {
    300000
}
pub(crate) fn is_default_universe_prepare_retry_ms(value: &u64) -> bool {
    *value == default_universe_prepare_retry_ms()
}
/// Longest idle loop sleep the config accepts. Operational liveness derives its
/// deadline from this bound, so the two cannot drift (#234).
pub(crate) const MAX_IDLE_POLL_INTERVAL_MS: u64 = 300_000;
pub(crate) fn default_idle_poll_interval_ms() -> u64 {
    15000
}
pub(crate) fn is_default_idle_poll_interval_ms(value: &u64) -> bool {
    *value == default_idle_poll_interval_ms()
}
pub(crate) fn default_daily_history_sessions() -> usize {
    21
}
pub(crate) fn is_default_daily_history_sessions(v: &usize) -> bool {
    *v == 21
}
pub(crate) fn is_default_preparation_start(v: &u32) -> bool {
    *v == 480
}
pub(crate) fn is_default_preparation_cutoff(v: &u32) -> bool {
    *v == 550
}
pub(crate) fn default_preparation_start() -> u32 {
    480
}
pub(crate) fn default_preparation_cutoff() -> u32 {
    550
}

/// Config-driven quote-key allowlist. The default admits only the NSE cash
/// equity prefix, so behaviour is unchanged until an operator configures more.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct QuoteKeySettings {
    /// Instrument-key prefixes including the trailing `|`, e.g. `NSE_EQ|`.
    pub prefixes: Vec<String>,
    /// Request cap for one quote call; the provider ceiling stays 200.
    pub max_keys: usize,
}
impl Default for QuoteKeySettings {
    fn default() -> Self {
        Self {
            prefixes: vec!["NSE_EQ|".into()],
            max_keys: 200,
        }
    }
}
impl QuoteKeySettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if self.max_keys == 0
            || self.max_keys > 200
            || self.prefixes.is_empty()
            || self
                .prefixes
                .iter()
                .any(|prefix| !admissible_prefix(prefix))
        {
            return Err("quote_key_settings_invalid".into());
        }
        Ok(())
    }
    /// Prefix plus a bounded instrument id. Spaces are allowed inside the id
    /// (`NSE_INDEX|Nifty 50`); empty ids and path/query metacharacters are not.
    pub(crate) fn admits(&self, key: &str) -> bool {
        self.prefixes.iter().any(|prefix| {
            key.strip_prefix(prefix.as_str())
                .is_some_and(valid_instrument_id)
        })
    }
}
fn admissible_prefix(prefix: &str) -> bool {
    prefix.strip_suffix('|').is_some_and(|segment| {
        nivra_core::market::Segment::from_key(&format!("{segment}|X")).is_some()
    })
}
fn valid_instrument_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with(' ')
        && !id.ends_with(' ')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b' ')
}

/// Non-tradeable NSE index feeds. Disabled by default; enabling requires the
/// operator to also allow the `NSE_INDEX|` quote-key prefix (fail closed).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct IndexFeedSettings {
    pub enabled: bool,
    /// Upstox instrument keys, never invented: the three documented NSE indices.
    pub keys: Vec<String>,
    /// Optional advisory regime thresholds for index-level digest context only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regime: Option<nivra_core::regime::RegimeConfig>,
}
pub(crate) fn default_index_feed_keys() -> Vec<String> {
    vec![
        "NSE_INDEX|Nifty 50".into(),
        "NSE_INDEX|Nifty Bank".into(),
        "NSE_INDEX|India VIX".into(),
    ]
}
impl Default for IndexFeedSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            keys: default_index_feed_keys(),
            regime: None,
        }
    }
}
impl IndexFeedSettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub(crate) fn validate(&self, quote_keys: &QuoteKeySettings) -> Result<()> {
        if self.keys.len() > 64
            || self
                .keys
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.keys.len()
            || self.keys.iter().any(|key| {
                nivra_core::market::Segment::from_key(key)
                    != Some(nivra_core::market::Segment::NseIndex)
            })
        {
            return Err("index_feeds_settings_invalid".into());
        }
        if self.enabled {
            if self.keys.is_empty() {
                return Err("index_feeds_settings_invalid".into());
            }
            if self.keys.iter().any(|key| !quote_keys.admits(key)) {
                return Err("index_feeds_prefix_not_allowed".into());
            }
        }
        if let Some(regime) = &self.regime {
            regime
                .validate()
                .map_err(|_| "index_feeds_regime_invalid")?;
            if regime.min_instruments > self.keys.len() {
                return Err("index_feeds_regime_invalid".into());
            }
        }
        Ok(())
    }
}

/// Opt-in buy-only Nifty weekly options paper settings. Default off and
/// skip-serialized at defaults so existing session JSON keeps parsing.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct NiftyOptionSettings {
    pub enabled: bool,
    /// Spot instrument key; the existing quote map must carry a fresh quote.
    pub underlying_key: String,
    pub underlying_symbol: String,
    /// Sanity check against the contract/BOD payload; never used to invent a lot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_lot_size: Option<u64>,
    /// Only `current_week` is supported in Phase 2.
    pub expiries: Vec<String>,
    /// Strikes kept on each side of spot.
    pub strike_window: u32,
    /// Phase 2 is buy-only; an explicit false is refused rather than ignored.
    pub buy_only: bool,
}
impl Default for NiftyOptionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            underlying_key: "NSE_INDEX|Nifty 50".into(),
            underlying_symbol: "NIFTY".into(),
            expected_lot_size: Some(65),
            expiries: vec!["current_week".into()],
            strike_window: 20,
            buy_only: true,
        }
    }
}
impl NiftyOptionSettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub(crate) fn validate(&self) -> Result<()> {
        let spot_id = self
            .underlying_key
            .strip_prefix("NSE_INDEX|")
            .filter(|id| !id.is_empty() && id.len() <= 118);
        if spot_id.is_none()
            || self.underlying_symbol.is_empty()
            || self.underlying_symbol.len() > 64
            || !self
                .underlying_symbol
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b' ' || byte == b'&')
            || self.expected_lot_size == Some(0)
            || !(1..=100).contains(&self.strike_window)
            || self.expiries.is_empty()
            || self.expiries.len() > 2
            || self.expiries.iter().any(|expiry| expiry != "current_week")
            || !self.buy_only
        {
            return Err("nifty_options_settings_invalid".into());
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaperCostTransition {
    pub source_session: NaiveDate,
    pub prior_config: PathBuf,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(
        default = "default_daily_history_sessions",
        skip_serializing_if = "is_default_daily_history_sessions"
    )]
    pub daily_history_sessions: usize,
    #[serde(
        default,
        skip_serializing_if = "nivra_core::execution::ExecutionSettings::is_default"
    )]
    pub execution: nivra_core::execution::ExecutionSettings,
    #[serde(
        default = "default_preparation_start",
        skip_serializing_if = "is_default_preparation_start"
    )]
    pub history_preparation_start_minute: u32,
    #[serde(
        default = "default_preparation_cutoff",
        skip_serializing_if = "is_default_preparation_cutoff"
    )]
    pub history_preparation_cutoff_minute: u32,
    #[serde(default, skip_serializing)]
    pub exit_backtest: exit_backtest::Settings,
    #[serde(default, skip_serializing_if = "HistoryDownloadSettings::is_default")]
    pub history_download: HistoryDownloadSettings,
    #[serde(
        default = "default_universe_prepare_retry_ms",
        skip_serializing_if = "is_default_universe_prepare_retry_ms"
    )]
    pub universe_prepare_retry_ms: u64,
    #[serde(
        default = "default_idle_poll_interval_ms",
        skip_serializing_if = "is_default_idle_poll_interval_ms"
    )]
    pub idle_poll_interval_ms: u64,
    #[serde(
        default,
        skip_serializing_if = "MarketDataTransportSettings::is_default"
    )]
    pub market_data_transport: MarketDataTransportSettings,
    #[serde(default, skip_serializing_if = "StorageSettings::is_default")]
    pub storage: StorageSettings,
    #[serde(default, skip_serializing_if = "DashboardSettings::is_default")]
    pub dashboard: DashboardSettings,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper_costs: Option<nivra_core::finance::FeeArithmetic>,
    /// Optional per-segment fee schedules; `paper_costs` stays the cash schedule.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub paper_costs_by_segment:
        std::collections::BTreeMap<nivra_core::market::Segment, nivra_core::finance::FeeArithmetic>,
    /// Quote-key prefix allowlist; default is the NSE cash equity prefix only.
    #[serde(default, skip_serializing_if = "QuoteKeySettings::is_default")]
    pub quote_keys: QuoteKeySettings,
    /// Non-tradeable NSE index data feeds; off by default.
    #[serde(default, skip_serializing_if = "IndexFeedSettings::is_default")]
    pub index_feeds: IndexFeedSettings,
    /// Opt-in buy-only Nifty weekly options paper settings.
    #[serde(default, skip_serializing_if = "NiftyOptionSettings::is_default")]
    pub nifty_options: NiftyOptionSettings,
    /// Explicit prior config evidence for a closed source session's cost-only rollover.
    #[serde(default, skip_serializing)]
    pub paper_cost_transition: Option<PaperCostTransition>,
    #[serde(default, skip_serializing)]
    pub planner_config: Option<PathBuf>,
    #[serde(default, skip_serializing)]
    pub alert_webhook_file: Option<PathBuf>,
    #[serde(default = "default_stale_minutes", skip_serializing)]
    pub alert_stale_minutes: u32,
    #[serde(
        default = "default_model_readiness_check_interval_ms",
        skip_serializing
    )]
    pub model_readiness_check_interval_ms: i64,
    pub account: String,
    pub session: NaiveDate,
    pub previous_session: NaiveDate,
    pub calendar_source: String,
    pub directory: PathBuf,
    #[serde(default, skip_serializing)]
    pub sessions_root: Option<PathBuf>,
    #[serde(skip)]
    pub legacy_directory: Option<PathBuf>,
    #[serde(default, skip_serializing)]
    #[allow(dead_code)] // Archived reviewer setting, retained for config compatibility.
    pub history_directory: Option<PathBuf>,
    /// Opt-in bounded previous-session acquisition before the market opens.
    #[serde(default, skip_serializing)]
    pub history_preparation_enabled: bool,
    #[serde(
        default = "default_history_avg_daily_volume_min_sessions",
        skip_serializing
    )]
    pub history_avg_daily_volume_min_sessions: u32,
    /// Retain explicit provider HTTP 400 exclusions and continue other histories.
    #[serde(default, skip_serializing)]
    pub history_skip_unavailable: bool,
    /// Pre-open preparation skips up to this many instruments whose fetch fails
    /// (timeout/5xx) and keeps downloading the rest; one more aborts it (#35).
    #[serde(
        default = "default_history_preparation_max_fetch_failures",
        skip_serializing
    )]
    pub history_preparation_max_fetch_failures: usize,
    /// Bounded pre-open passes over missing history; zero disables acquisition.
    #[serde(default = "default_history_preparation_max_attempts", skip_serializing)]
    pub history_preparation_max_attempts: usize,
    #[serde(default = "default_history_preparation_retry_ms", skip_serializing)]
    pub history_preparation_retry_ms: i64,
    /// Positive delay after failed attempts, including non-transient failures.
    #[serde(default = "default_history_preparation_retry_ms", skip_serializing)]
    pub history_preparation_failed_retry_ms: i64,
    #[serde(default = "default_history_preparation_retry_ms", skip_serializing)]
    pub history_preparation_check_interval_ms: i64,
    pub token_file: PathBuf,
    pub dashboard_token_file: PathBuf,
    #[serde(default, skip_serializing)]
    pub dashboard_public_host: Option<DashboardPublicHost>,
    pub read_journal: PathBuf,
    pub read_scope: String,
    pub read_journal_device: u64,
    pub read_journal_inode: u64,
    pub strategy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capital: Option<nivra_core::finance::Amount>,
    #[serde(default, skip_serializing)]
    pub prior_config: Option<PathBuf>,
}
pub(crate) fn default_model_readiness_check_interval_ms() -> i64 {
    60_000
}
pub(crate) fn default_stale_minutes() -> u32 {
    5
}
impl Config {
    fn embedded_calendar(&self) -> Result<nivra_core::market::Calendar> {
        Ok(serde_json::from_str(include_str!(
            "nse-calendar-2026.json"
        ))?)
    }
    pub(crate) fn calendar(&self) -> Result<nivra_core::market::Calendar> {
        self.calendar_at(local(millis()).date_naive())
    }
    pub(crate) fn calendar_at(&self, today: NaiveDate) -> Result<nivra_core::market::Calendar> {
        let calendar = self.embedded_calendar()?;
        calendar.hours(nivra_core::time::ExchangeDate::parse(&today.to_string())?)?;
        Ok(calendar)
    }
    pub(crate) fn calendar_coverage_at(&self, today: NaiveDate) -> Result<CalendarCoverage> {
        let calendar = self.embedded_calendar()?;
        let from = calendar.valid_from();
        let to = calendar.valid_to();
        let today_exchange = nivra_core::time::ExchangeDate::parse(&today.to_string())?;
        let requested = nivra_core::time::ExchangeDate::parse(&self.session.to_string())?;
        let historical = nivra_core::time::ExchangeDate::parse(&self.previous_session.to_string())?;
        let to_date = NaiveDate::parse_from_str(&to.to_string(), "%Y-%m-%d")?;
        let days_remaining = to_date.signed_duration_since(today).num_days();
        Ok(CalendarCoverage {
            source: calendar.source().to_owned(),
            covered_from: from.to_string(),
            covered_to: to.to_string(),
            current_operational_date: today.to_string(),
            covered: today_exchange >= from && today_exchange <= to,
            requested_session: self.session.to_string(),
            requested_session_covered: requested >= from && requested <= to,
            historical_session: self.previous_session.to_string(),
            historical_session_covered: historical >= from && historical <= to,
            days_remaining,
            expiring: (0..=CALENDAR_EXPIRY_WARNING_DAYS).contains(&days_remaining),
            expired: days_remaining < 0,
            expiry_warning_days: CALENDAR_EXPIRY_WARNING_DAYS,
            expiry_warning_basis: "operator_chosen_default_not_exchange_guarantee",
            update_prerequisite: "verified_exchange_calendar_update_required",
        })
    }
    pub(crate) fn is_trading_day(&self, day: NaiveDate) -> Result<bool> {
        Ok(self
            .calendar_at(day)?
            .hours(nivra_core::time::ExchangeDate::parse(&day.to_string())?)?
            .is_some())
    }
    pub(crate) fn next_sessions(&self, today: NaiveDate) -> Result<(NaiveDate, NaiveDate)> {
        let mut session = self.session.max(today);
        while !self.is_trading_day(session)? {
            session = session.succ_opt().ok_or("session_date_range")?;
        }
        let mut previous = session.pred_opt().ok_or("session_date_range")?;
        while !self.is_trading_day(previous)? {
            previous = previous.pred_opt().ok_or("session_date_range")?;
        }
        Ok((session, previous))
    }
    pub(crate) fn allocation(&self) -> Result<nivra_core::finance::Amount> {
        self.capital
            .filter(|v| *v > nivra_core::finance::Amount::ZERO)
            .ok_or_else(|| Fail::new("positive_paper_capital_required").into())
    }
    /// Build the per-segment fee lookup. `paper_costs` remains the cash
    /// schedule; `paper_costs_by_segment` only adds non-cash entries.
    pub(crate) fn fee_schedule(&self) -> Result<nivra_core::finance::FeeSchedule> {
        let cash = self.paper_costs.clone().ok_or("paper_costs_required")?;
        let mut schedule = nivra_core::finance::FeeSchedule::new(cash);
        for (segment, fees) in &self.paper_costs_by_segment {
            fees.validate()?;
            schedule = schedule.with_segment(*segment, fees.clone());
        }
        Ok(schedule)
    }
    pub(crate) fn validate(&self) -> Result<()> {
        self.execution.validate()?;
        if self.history_preparation_start_minute >= self.history_preparation_cutoff_minute
            || self.history_preparation_cutoff_minute >= 555
        {
            return Err("invalid_preparation_window".into());
        }
        if self.daily_history_sessions < 21 {
            return Err("invalid_daily_history_sessions".into());
        }
        self.history_download.validate()?;
        self.dashboard.validate()?;
        if !(1..=86_400_000).contains(&self.universe_prepare_retry_ms)
            || !(1..=MAX_IDLE_POLL_INTERVAL_MS).contains(&self.idle_poll_interval_ms)
            || !(1..=120_000).contains(&self.market_data_transport.http_timeout_ms)
            || !(1..=60_000).contains(&self.storage.read_journal_busy_timeout_ms)
            || !(1..=300_000).contains(&self.dashboard.refresh_interval_ms)
        {
            return Err(Fail::new("invalid_runtime_timing_settings").into());
        }
        self.paper_costs
            .as_ref()
            .ok_or("paper_costs_required")?
            .validate()?;
        if self
            .paper_costs_by_segment
            .contains_key(&nivra_core::market::Segment::NseEq)
        {
            // Cash always uses `paper_costs`; a duplicate NSE_EQ schedule would
            // be silently ignored, so refuse it instead.
            return Err("paper_costs_by_segment_invalid".into());
        }
        for fees in self.paper_costs_by_segment.values() {
            fees.validate()?;
        }
        self.quote_keys.validate()?;
        self.index_feeds.validate(&self.quote_keys)?;
        self.nifty_options.validate()?;
        if self.nifty_options.enabled {
            // Options need their own F&O fee schedule and an admitted NSE_FO
            // quote-key prefix; never borrow the cash 5/7 bps policy.
            if !self
                .paper_costs_by_segment
                .contains_key(&nivra_core::market::Segment::NseFo)
            {
                return Err("nifty_options_fee_schedule_required".into());
            }
            if !self
                .quote_keys
                .prefixes
                .iter()
                .any(|prefix| prefix == "NSE_FO|")
            {
                return Err("nifty_options_quote_keys_required".into());
            }
        }
        if self
            .paper_cost_transition
            .as_ref()
            .is_some_and(|transition| {
                !transition.prior_config.is_absolute() || transition.source_session > self.session
            })
        {
            return Err("paper_cost_transition_invalid".into());
        }
        // Preserve account/source authority while accepting configured session dates.
        if self.model_readiness_check_interval_ms <= 0
            || self.history_preparation_check_interval_ms <= 0
            || self.history_preparation_retry_ms < 0
            || self.history_preparation_failed_retry_ms <= 0
            || self.alert_stale_minutes == 0
            || self.alert_stale_minutes > 1440
            || self.account != "tharun-upstox"
            || self.previous_session >= self.session
            || self.calendar_source != "NSE/CMTR/71775"
            || !["model_planner_v1", "model_abstain"].contains(&self.strategy.as_str())
            || self.read_scope != "upstox-market-tharun-upstox"
        {
            return Err(Fail::new("invalid_paper_mandate").into());
        }
        Ok(())
    }
}
