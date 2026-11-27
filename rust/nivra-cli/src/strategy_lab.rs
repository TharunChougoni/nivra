//! Explicit offline strategy laboratory. No session, credentials or provider clients.
use clap::Subcommand;
use nivra_core::strategy::{FeatureSnapshot, StrategySpec, evaluate};
use nivra_store::strategy_results::{
    Evidence, EvidenceKind, Orchestrator, Proposal, Query, ResultsDb,
};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
const INPUT_LIMIT: u64 = crate::strategy_archive::MAX_EXECUTION_TAPE_BYTES as u64;
#[derive(Subcommand)]
pub enum Commands {
    /// Simulate explicit entry sizes and holding periods against archived executable quotes.
    ReplayActions {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        parameters: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        events: PathBuf,
        /// New report file; existing reports are never replaced.
        #[arg(long)]
        output: PathBuf,
    },
    /// Reconcile recorded fills against a captured archive without calling a model.
    ReconcileArchive {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        events: PathBuf,
        #[arg(long)]
        fills: PathBuf,
        /// New report file; existing reports are never replaced.
        #[arg(long)]
        output: PathBuf,
    },
    /// Compare captured catalog timings to a cadence without performing a capture.
    MeasureCatalog {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        previous: Option<PathBuf>,
    },
    /// Export a bounded, read-only strategy leaderboard and approval history.
    Report {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    /// Print the versioned legacy scanner composition for operator review.
    DefaultSpec,
    /// Record trusted measured qualification evidence; does not approve or activate.
    Qualify {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        input: PathBuf,
    },
    /// Record Tharun's explicit approval between sessions; never edits bot configuration.
    Approve {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        proposal: String,
        #[arg(long)]
        approval_ref: String,
        #[arg(long)]
        between_sessions: bool,
        #[arg(long)]
        config: PathBuf,
    },
    /// Execute a bounded transcript of typed orchestrator tools, with no model/provider call.
    Tools {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        run: String,
        #[arg(long)]
        requests: PathBuf,
        #[arg(long)]
        session: Option<PathBuf>,
        /// Paper config for books that archive their quote bodies.
        #[arg(long)]
        archive_config: Option<PathBuf>,
        #[arg(long)]
        recorded: Option<PathBuf>,
        #[arg(long)]
        replay_config: Option<PathBuf>,
    },
    /// Extract deterministic features from an archived capture manifest/events.
    ExportFeatures {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        events: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 2000)]
        max_age_ms: i64,
        #[arg(long, default_value_t = 20)]
        lookback_samples: usize,
        /// Bounded archive size, observation allowance and optional replay window.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Read bounded source journals with no write authority.
    Source {
        #[arg(long)]
        session: PathBuf,
        /// Paper config for books that archive their quote bodies.
        #[arg(long)]
        archive_config: Option<PathBuf>,
        #[arg(long)]
        query: PathBuf,
        #[arg(long, default_value_t = 16384)]
        budget_bytes: usize,
    },
    /// Create a separate strategy-results database (never a paper book).
    Init {
        #[arg(long)]
        database: PathBuf,
    },
    /// Import trusted, attributed evidence from a JSON array, preserving original sources.
    Import {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        input: PathBuf,
    },
    /// Paginated read-only research evidence query, with bounded token output.
    Query {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        query: PathBuf,
        #[arg(long, default_value_t = 4096)]
        token_budget: usize,
    },
    /// Store a schema-validated proposal; does not approve or activate it.
    Propose {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        input: PathBuf,
    },
    /// Append an orchestrator journal entry; does not modify evidence.
    Note {
        #[arg(long)]
        database: PathBuf,
        #[arg(long)]
        run: String,
        #[arg(long)]
        text: String,
    },
    /// Replay recorded feature snapshots. No fills, profitability or executable prices inferred.
    Replay {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        database: Option<PathBuf>,
        #[arg(long)]
        run: String,
        /// JSON laboratory settings; omitted settings use conservative defaults.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Box<dyn Error>> {
    let f = File::open(path)?;
    if f.metadata()?.len() > INPUT_LIMIT {
        return Err("strategy_input_too_large".into());
    }
    let mut bytes = Vec::new();
    f.take(INPUT_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > INPUT_LIMIT {
        return Err("strategy_input_too_large".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}
#[derive(Debug, Serialize, Deserialize)]
pub struct ReplayReport {
    pub spec: StrategySpec,
    pub spec_id: String,
    pub spec_version: u32,
    pub spec_hash: String,
    pub dataset_hash: String,
    pub replay_config: ReplayConfig,
    pub ticks: usize,
    pub failed_ticks: usize,
    pub selected_rows: usize,
    pub filtered_rows: usize,
    pub input_label: String,
    pub scans: Vec<nivra_core::strategy::ScanSnapshot>,
    pub forward_returns: Vec<ForwardReturn>,
    pub missing_forward_marks: usize,
    #[serde(default)]
    pub missing_mark_reasons: std::collections::BTreeMap<MissingMarkReason, usize>,
    pub mean_forward_return_bps: Option<f64>,
    pub cost_adjusted: Option<CostAdjustedMetrics>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkCosts {
    /// Independent hypothetical round trips, not portfolio sizing or executable fills.
    pub notional: nivra_core::finance::Money,
    pub round_trip_spread_bps: f64,
    pub fees: ReplayFees,
}
/// Offline fee assumptions, calculated by the shared exact-decimal arithmetic.
/// Spread is separately declared; no executable fill or broker fee is inferred.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayFees {
    pub per_order: nivra_core::finance::Money,
    pub buy_bps: nivra_core::finance::Money,
    pub sell_bps: nivra_core::finance::Money,
    pub rounding_increment: Option<nivra_core::finance::Price>,
}
impl ReplayFees {
    fn arithmetic(&self) -> nivra_core::finance::FeeArithmetic {
        nivra_core::finance::FeeArithmetic {
            per_order: self.per_order,
            buy_bps: self.buy_bps,
            sell_bps: self.sell_bps,
            rounding_increment: self.rounding_increment,
            slippage_bps: nivra_core::finance::Money::ZERO,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingMarkReason {
    InitialPriceUnavailable,
    HorizonOverflow,
    RecordingEndsBeforeHorizon,
    NoSnapshotInWindow,
    OnlyFailedSnapshotsInWindow,
    InstrumentAbsentInWindow,
    PriceUnavailableInWindow,
    NonfiniteReturn,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CostAdjustedMetrics {
    pub samples: usize,
    pub hit_rate: f64,
    pub net_expectancy_bps: f64,
    pub cumulative_drawdown_bps: f64,
    pub turnover: nivra_core::finance::Amount,
    pub fees: nivra_core::finance::Amount,
    pub label: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardReturn {
    pub key: String,
    pub scan_ms: i64,
    pub mark_ms: i64,
    pub return_bps: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReplayConfig {
    pub horizon_ms: i64,
    pub max_mark_delay_ms: i64,
    pub costs: Option<MarkCosts>,
    /// Inclusive selection window; all supplied snapshots remain available as marks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_from_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_to_ms: Option<i64>,
}
impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            horizon_ms: 300_000,
            max_mark_delay_ms: 2000,
            costs: None,
            selection_from_ms: None,
            selection_to_ms: None,
        }
    }
}
pub fn replay(spec: &StrategySpec, snapshots: &[FeatureSnapshot]) -> Result<ReplayReport, String> {
    replay_configured(spec, snapshots, &ReplayConfig::default())
}
pub fn replay_configured(
    spec: &StrategySpec,
    snapshots: &[FeatureSnapshot],
    config: &ReplayConfig,
) -> Result<ReplayReport, String> {
    replay_with(spec, snapshots, config, |snapshot| evaluate(spec, snapshot))
}
pub(crate) fn replay_with(
    spec: &StrategySpec,
    snapshots: &[FeatureSnapshot],
    config: &ReplayConfig,
    mut scorer: impl FnMut(&FeatureSnapshot) -> Result<nivra_core::strategy::ScanSnapshot, String>,
) -> Result<ReplayReport, String> {
    if !(1..=86_400_000).contains(&config.horizon_ms)
        || !(0..=60_000).contains(&config.max_mark_delay_ms)
        || matches!((config.selection_from_ms, config.selection_to_ms), (Some(from), Some(to)) if from > to)
        || config.costs.as_ref().is_some_and(|c| {
            c.notional <= nivra_core::finance::Money::ZERO
                || !c.round_trip_spread_bps.is_finite()
                || !(0.0..=10_000.0).contains(&c.round_trip_spread_bps)
                || [
                    nivra_core::finance::Side::Buy,
                    nivra_core::finance::Side::Sell,
                ]
                .iter()
                .any(|side| c.fees.arithmetic().charge(*side, c.notional).is_err())
        })
    {
        return Err("strategy_replay_config_invalid".into());
    }
    spec.validate()?;
    if snapshots.is_empty()
        || snapshots.len() > 10_000
        || snapshots
            .windows(2)
            .any(|w| w[0].observed_ms >= w[1].observed_ms)
    {
        return Err("strategy_replay_input_invalid".into());
    }
    let mut report = ReplayReport {
        spec: spec.clone(),
        spec_id: spec.id.clone(),
        spec_version: spec.version,
        spec_hash: nivra_core::replay::digest(
            &serde_json::to_vec(spec).map_err(|_| "strategy_spec_encoding")?,
        ),
        dataset_hash: nivra_core::replay::digest(
            &serde_json::to_vec(snapshots).map_err(|_| "strategy_dataset_encoding")?,
        ),
        replay_config: config.clone(),
        ticks: snapshots.len(),
        failed_ticks: 0,
        selected_rows: 0,
        filtered_rows: 0,
        input_label: "recorded_marks_forward_returns_not_executable_fills".into(),
        scans: Vec::new(),
        forward_returns: Vec::new(),
        missing_forward_marks: 0,
        missing_mark_reasons: std::collections::BTreeMap::new(),
        mean_forward_return_bps: None,
        cost_adjusted: None,
    };
    let mut last_trigger = std::collections::BTreeMap::<String, i64>::new();
    let mut disarmed = std::collections::BTreeSet::new();
    for snapshot in snapshots {
        match scorer(snapshot) {
            Ok(mut scan) => {
                let selecting = config
                    .selection_from_ms
                    .is_none_or(|from| snapshot.observed_ms >= from)
                    && config
                        .selection_to_ms
                        .is_none_or(|to| snapshot.observed_ms <= to);
                for row in &mut scan.rows {
                    if !selecting {
                        if row.selected {
                            row.selected = false;
                            row.reason = "outside_selection_window".into();
                        }
                        continue;
                    }
                    if !row.selected || row.trigger_eligible == Some(false) {
                        row.selected = false;
                        // Missing/invalid evidence cannot establish that a signal reset.
                        if (row.score.is_some()
                            && row.reason != "missing_trigger_feature"
                            && row.trigger_eligible != Some(true))
                            || row.reason.starts_with("filtered:")
                        {
                            disarmed.remove(&row.key);
                        }
                        continue;
                    }
                    let cooling = last_trigger.get(&row.key).is_some_and(|last| {
                        snapshot.observed_ms.saturating_sub(*last) < spec.cooldown_ms
                    });
                    if cooling || disarmed.contains(&row.key) {
                        row.selected = false;
                        row.reason = if cooling {
                            "cooldown"
                        } else {
                            "awaiting_rearm"
                        }
                        .into();
                    } else {
                        last_trigger.insert(row.key.clone(), snapshot.observed_ms);
                        disarmed.insert(row.key.clone());
                    }
                }
                scan.seal()?;
                report.selected_rows += scan.rows.iter().filter(|r| r.selected).count();
                report.filtered_rows += scan.rows.iter().filter(|r| r.score.is_none()).count();
                report.scans.push(scan);
            }
            Err(_) => report.failed_ticks += 1,
        }
    }
    for scan in &report.scans {
        for row in scan.rows.iter().filter(|r| r.selected) {
            match forward_mark(scan.observed_ms, row, &report.scans, snapshots, config) {
                Ok(mark) => report.forward_returns.push(mark),
                Err(reason) => {
                    report.missing_forward_marks += 1;
                    *report.missing_mark_reasons.entry(reason).or_default() += 1;
                }
            }
        }
    }
    if !report.forward_returns.is_empty() {
        report.mean_forward_return_bps = Some(
            report
                .forward_returns
                .iter()
                .map(|r| r.return_bps)
                .sum::<f64>()
                / report.forward_returns.len() as f64,
        );
    }
    if let Some(costs) = &config.costs
        && !report.forward_returns.is_empty()
    {
        report.cost_adjusted = Some(cost_adjusted(&report.forward_returns, costs)?);
    }
    Ok(report)
}
fn forward_mark(
    scan_ms: i64,
    row: &nivra_core::strategy::ScanRow,
    valid: &[nivra_core::strategy::ScanSnapshot],
    recorded: &[FeatureSnapshot],
    config: &ReplayConfig,
) -> Result<ForwardReturn, MissingMarkReason> {
    use MissingMarkReason::*;
    use nivra_core::strategy::Feature;
    let initial = row
        .features
        .get(&Feature::Price)
        .copied()
        .filter(|p| *p > 0.0)
        .ok_or(InitialPriceUnavailable)?;
    let target = scan_ms
        .checked_add(config.horizon_ms)
        .ok_or(HorizonOverflow)?;
    if recorded.last().is_none_or(|s| s.observed_ms < target) {
        return Err(RecordingEndsBeforeHorizon);
    }
    let window = &valid[valid.partition_point(|s| s.observed_ms < target)..];
    let mut any_snapshot = false;
    let mut any_key = false;
    for snapshot in window
        .iter()
        .take_while(|s| s.observed_ms.saturating_sub(target) <= config.max_mark_delay_ms)
    {
        any_snapshot = true;
        if let Some(future) = snapshot.rows.iter().find(|r| r.key == row.key) {
            any_key = true;
            if let Some(price) = future.features.get(&Feature::Price).filter(|p| **p > 0.0) {
                let return_bps = (*price / initial - 1.0) * 10000.0;
                return if return_bps.is_finite() {
                    Ok(ForwardReturn {
                        key: row.key.clone(),
                        scan_ms,
                        mark_ms: snapshot.observed_ms,
                        return_bps,
                    })
                } else {
                    Err(NonfiniteReturn)
                };
            }
        }
    }
    if any_key {
        return Err(PriceUnavailableInWindow);
    }
    if any_snapshot {
        return Err(InstrumentAbsentInWindow);
    }
    let next = recorded.get(recorded.partition_point(|s| s.observed_ms < target));
    if next.is_some_and(|s| s.observed_ms.saturating_sub(target) <= config.max_mark_delay_ms) {
        Err(OnlyFailedSnapshotsInWindow)
    } else {
        Err(NoSnapshotInWindow)
    }
}
pub(crate) fn cost_adjusted(
    marks: &[ForwardReturn],
    costs: &MarkCosts,
) -> Result<CostAdjustedMetrics, String> {
    use nivra_core::finance::{Amount, Money, Side};
    let mut metrics = CostAdjustedMetrics {
        samples: marks.len(),
        hit_rate: 0.0,
        net_expectancy_bps: 0.0,
        cumulative_drawdown_bps: 0.0,
        turnover: Amount::ZERO,
        fees: Amount::ZERO,
        label: "independent_fixed_notional_marks_declared_fees_and_spread_not_executable_fills"
            .into(),
    };
    let notional: f64 = costs
        .notional
        .amount()
        .to_string()
        .parse()
        .map_err(|_| "strategy_cost_notional")?;
    if !notional.is_finite() || notional <= 0.0 {
        return Err("strategy_cost_notional".into());
    }
    let mut wins = 0usize;
    let mut cumulative = 0.0_f64;
    let mut peak = 0.0_f64;
    for mark in marks {
        let sale = notional * (1.0 + mark.return_bps / 10000.0);
        if !sale.is_finite() || sale <= 0.0 {
            return Err("strategy_cost_mark".into());
        }
        // Explicit decimal conversion for descriptive marks; original financial state is never used.
        let sale = Money::try_from(
            Amount::parse(&format!("{sale:.8}")).map_err(|_| "strategy_cost_range")?,
        )
        .map_err(|_| "strategy_cost_range")?;
        let buy_fee = costs
            .fees
            .arithmetic()
            .charge(Side::Buy, costs.notional)
            .map_err(|_| "strategy_cost_range")?;
        let sell_fee = costs
            .fees
            .arithmetic()
            .charge(Side::Sell, sale)
            .map_err(|_| "strategy_cost_range")?;
        let fees = buy_fee
            .amount()
            .checked_add(sell_fee.amount())
            .map_err(|_| "strategy_cost_range")?;
        metrics.fees = metrics
            .fees
            .checked_add(fees)
            .map_err(|_| "strategy_cost_range")?;
        metrics.turnover = metrics
            .turnover
            .checked_add(costs.notional.amount())
            .and_then(|v| v.checked_add(sale.amount()))
            .map_err(|_| "strategy_cost_range")?;
        let fee_rupees: f64 = fees
            .to_string()
            .parse()
            .map_err(|_| "strategy_cost_range")?;
        let net = mark.return_bps - fee_rupees / notional * 10000.0 - costs.round_trip_spread_bps;
        wins += usize::from(net > 0.0);
        cumulative += net;
        peak = peak.max(cumulative);
        metrics.cumulative_drawdown_bps = metrics.cumulative_drawdown_bps.max(peak - cumulative);
    }
    if !marks.is_empty() {
        metrics.hit_rate = wins as f64 / marks.len() as f64;
        metrics.net_expectancy_bps = cumulative / marks.len() as f64;
    }
    Ok(metrics)
}
pub fn run(
    command: Commands,
    connect_archive: impl Fn(
        &Path,
    ) -> Result<nivra_store::market_archive::ArchiveClient, Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    match command {
        Commands::ReplayActions {
            source,
            parameters,
            manifest,
            events,
            output,
        } => {
            use std::os::unix::fs::OpenOptionsExt;
            let report = crate::strategy_actions::evaluate(
                &read_bytes_limit(&source, 2 * 1024 * 1024)?,
                &read_bytes_limit(&parameters, 16 * 1024)?,
                &read_bytes_limit(&manifest, 512 * 1024)?,
                &read_bytes(&events)?,
            )?;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(output)?;
            serde_json::to_writer_pretty(&file, &report)?;
            file.sync_all()?;
        }
        Commands::ReconcileArchive {
            manifest,
            events,
            fills,
            output,
        } => {
            use std::os::unix::fs::OpenOptionsExt;
            let report = crate::archive_reconcile::reconcile(
                &read_bytes_limit(&manifest, 512 * 1024)?,
                &read_bytes(&events)?,
                &read_bytes_limit(&fills, 2 * 1024 * 1024)?,
            )?;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(output)?;
            serde_json::to_writer_pretty(&file, &report)?;
            file.sync_all()?;
        }
        Commands::MeasureCatalog {
            input,
            output,
            previous,
        } => {
            use std::os::unix::fs::OpenOptionsExt;
            let current: serde_json::Value =
                serde_json::from_slice(&read_bytes_limit(&input, 1024 * 1024)?)?;
            let previous = match previous {
                Some(path) => Some(serde_json::from_slice(&read_bytes_limit(
                    &path,
                    1024 * 1024,
                )?)?),
                None => None,
            };
            let report = crate::catalog_measure::evaluate(&current, previous.as_ref())?;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(output)?;
            serde_json::to_writer_pretty(&file, &report)?;
            file.sync_all()?;
        }
        Commands::Report { database, output } => crate::strategy_report::write(&database, &output)?,
        Commands::DefaultSpec => println!(
            "{}",
            serde_json::to_string_pretty(&StrategySpec::default())?
        ),
        Commands::Qualify { database, input } => {
            crate::strategy_approval::qualify(&database, &input)?
        }
        Commands::Approve {
            database,
            proposal,
            approval_ref,
            between_sessions,
            config,
        } => {
            crate::strategy_approval::approve(
                &database,
                &proposal,
                &approval_ref,
                between_sessions,
                &config,
            )?;
        }
        Commands::Tools {
            config,
            database,
            run,
            requests,
            session,
            archive_config,
            recorded,
            replay_config,
        } => {
            let config: nivra_store::strategy_results::LabConfig = read(&config)?;
            config.validate()?;
            if !config.enabled {
                return Err("strategy_orchestrator_disabled".into());
            }
            let requests: Vec<crate::strategy_pipeline::ToolRequest> = read(&requests)?;
            if requests.is_empty() || requests.len() > 100 {
                return Err("strategy_tool_request_limit".into());
            }
            let recorded: Vec<FeatureSnapshot> = recorded
                .as_deref()
                .map(read)
                .transpose()?
                .unwrap_or_default();
            let replay_config = replay_config
                .as_deref()
                .map(read)
                .transpose()?
                .unwrap_or_default();
            let db = ResultsDb::open(&database)?;
            let connect =
                || connect_archive(archive_config.as_deref().ok_or("archive_config_missing")?);
            let mut tools = crate::strategy_pipeline::PipelineTools::new(
                &db,
                run,
                &config,
                session.as_deref(),
                &recorded,
                replay_config,
            )?
            .with_archive(Some(&connect));
            for request in requests {
                println!("{}", serde_json::to_string(&tools.call(request)?)?);
            }
        }
        Commands::ExportFeatures {
            manifest,
            events,
            output,
            max_age_ms,
            lookback_samples,
            config,
        } => {
            let config: crate::strategy_archive::ArchiveConfig =
                config.as_deref().map(read).transpose()?.unwrap_or_default();
            config.validate()?;
            let manifest = read_bytes(&manifest)?;
            let events = read_bytes_limit(&events, config.max_archive_bytes)?;
            let features = crate::strategy_archive::export_features_configured(
                &manifest,
                &events,
                max_age_ms,
                lookback_samples,
                &config,
            )?;
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output)?;
            serde_json::to_writer(file, &features)?;
        }
        Commands::Source {
            session,
            archive_config,
            query,
            budget_bytes,
        } => {
            let query = read(&query)?;
            let connect =
                || connect_archive(archive_config.as_deref().ok_or("archive_config_missing")?);
            let page = crate::strategy_archive::read_source_with_archive(
                &session,
                &query,
                budget_bytes,
                Some(&connect),
            )?;
            println!("{}", serde_json::to_string(&page)?);
        }
        Commands::Init { database } => {
            ResultsDb::open(&database)?;
            println!("strategy_results_initialized");
        }
        Commands::Import { database, input } => {
            let rows: Vec<Evidence> = read(&input)?;
            let db = ResultsDb::open(&database)?;
            db.record_batch(&rows)?;
            println!("strategy_evidence_imported");
        }
        Commands::Query {
            database,
            query,
            token_budget,
        } => {
            let q: Query = read(&query)?;
            let db = ResultsDb::open_read_only(&database)?;
            let page = Orchestrator::new(&db, "offline_query".into(), token_budget)?.read(&q)?;
            println!("{}", serde_json::to_string(&page)?);
        }
        Commands::Propose { database, input } => {
            let p: Proposal = read(&input)?;
            let db = ResultsDb::open(&database)?;
            Orchestrator::new(&db, "offline_proposal".into(), 4096)?.propose(&p)?;
            println!("strategy_proposal_recorded_pending_qualification_and_approval");
        }
        Commands::Note {
            database,
            run,
            text,
        } => {
            let db = ResultsDb::open(&database)?;
            Orchestrator::new(&db, run, 4096)?.note(&text)?;
            println!("strategy_note_recorded");
        }
        Commands::Replay {
            spec,
            input,
            database,
            run,
            config,
        } => {
            let config: ReplayConfig = match config {
                Some(path) => read(&path)?,
                None => ReplayConfig::default(),
            };
            let spec: StrategySpec = read(&spec)?;
            let snapshots: Vec<FeatureSnapshot> = read(&input)?;
            let report = replay_configured(&spec, &snapshots, &config)?;
            if let Some(path) = database {
                let db = ResultsDb::open(&path)?;
                db.register_spec(&spec)?;
                let mut records = vec![Evidence {
                    id: run.clone(),
                    kind: EvidenceKind::Replay,
                    observed_ms: snapshots.last().ok_or("strategy_replay_empty")?.observed_ms,
                    spec_id: format!("{}:{}", spec.id, spec.version),
                    scan_id: None,
                    digest_id: None,
                    decide_id: None,
                    body: serde_json::json!({"spec": report.spec, "spec_hash": report.spec_hash, "dataset_hash": report.dataset_hash, "replay_config": report.replay_config, "ticks": report.ticks, "failed_ticks": report.failed_ticks, "missing_forward_marks": report.missing_forward_marks, "missing_mark_reasons": report.missing_mark_reasons}),
                }];
                for scan in &report.scans {
                    records.push(Evidence {
                        id: format!("{run}:scan:{}", scan.scan_id),
                        kind: EvidenceKind::Scan,
                        observed_ms: scan.observed_ms,
                        spec_id: format!("{}:{}", spec.id, spec.version),
                        scan_id: Some(scan.scan_id.clone()),
                        digest_id: None,
                        decide_id: None,
                        body: serde_json::to_value(scan)?,
                    });
                }
                for (index, mark) in report.forward_returns.iter().enumerate() {
                    records.push(Evidence {
                        id: format!("{run}:forward:{index}"),
                        kind: EvidenceKind::ForwardReturn,
                        observed_ms: mark.mark_ms,
                        spec_id: format!("{}:{}", spec.id, spec.version),
                        scan_id: report
                            .scans
                            .iter()
                            .find(|s| s.observed_ms == mark.scan_ms)
                            .map(|s| s.scan_id.clone()),
                        digest_id: None,
                        decide_id: None,
                        body: serde_json::to_value(mark)?,
                    });
                }
                records.push(Evidence {
                    id: format!("{run}:scorecard"), kind: EvidenceKind::Scorecard,
                    observed_ms: snapshots.last().ok_or("strategy_replay_empty")?.observed_ms,
                    spec_id: format!("{}:{}", spec.id, spec.version), scan_id: None,
                    digest_id: None, decide_id: None,
                    body: serde_json::json!({"spec_hash": report.spec_hash, "dataset_hash": report.dataset_hash,
                        "ticks": report.ticks, "failed_ticks": report.failed_ticks,
                        "mean_forward_return_bps": report.mean_forward_return_bps,
                        "cost_adjusted": report.cost_adjusted,
                        "missing_forward_marks": report.missing_forward_marks,
                        "missing_mark_reasons": report.missing_mark_reasons,
                        "measured_marks": report.forward_returns.len(), "replay_config": report.replay_config}),
                });
                db.record_batch(&records)?;
            }
            println!("{}", serde_json::to_string(&report)?);
        }
    }
    Ok(())
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, Box<dyn Error>> {
    read_bytes_limit(path, INPUT_LIMIT)
}
fn read_bytes_limit(path: &Path, limit: u64) -> Result<Vec<u8>, Box<dyn Error>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > limit {
        return Err("strategy_input_too_large".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err("strategy_input_too_large".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nivra_core::strategy::{Feature, FeatureRow, Normalization, Weight};
    use std::collections::BTreeMap;
    #[test]
    fn replay_applies_cooldown_and_refuses_reordered_receipts() {
        crate::strategy_actions::verify_action_replay();
        let spec = StrategySpec {
            id: "recorded".into(),
            version: 1,
            lookback_samples: 20,
            filters: vec![],
            weights: vec![Weight {
                feature: Feature::MoveBps,
                weight: 1.0,
            }],
            normalization: Normalization::None,
            shortlist_size: 1,
            trigger_score: 0.0,
            cooldown_ms: 1000,
            trigger: None,
        };
        let row = FeatureRow {
            key: "fixture".into(),
            features: BTreeMap::from([(Feature::MoveBps, 10.0)]),
        };
        let snapshots = vec![
            FeatureSnapshot {
                observed_ms: 1000,
                lookback_samples: 20,
                rows: vec![row.clone()],
            },
            FeatureSnapshot {
                observed_ms: 1500,
                lookback_samples: 20,
                rows: vec![row.clone()],
            },
            FeatureSnapshot {
                observed_ms: 2000,
                lookback_samples: 20,
                rows: vec![row],
            },
        ];
        let report = replay(&spec, &snapshots).unwrap();
        assert_eq!(report.selected_rows, 1);
        assert_eq!(
            report.missing_mark_reasons[&MissingMarkReason::InitialPriceUnavailable],
            1
        );
        assert_eq!(report.scans[1].rows[0].reason, "cooldown");
        assert_eq!(report.failed_ticks, 0);
        assert_eq!(report.scans[2].rows[0].reason, "awaiting_rearm");
        let signal = |at, a, b| FeatureSnapshot {
            observed_ms: at,
            lookback_samples: 20,
            rows: vec![
                FeatureRow {
                    key: "A".into(),
                    features: BTreeMap::from([(Feature::MoveBps, a)]),
                },
                FeatureRow {
                    key: "B".into(),
                    features: BTreeMap::from([(Feature::MoveBps, b)]),
                },
            ],
        };
        let switched = replay(
            &spec,
            &[
                signal(1000, 10.0, -1.0),
                signal(1500, -1.0, 10.0),
                signal(2000, 10.0, -1.0),
            ],
        )
        .unwrap();
        assert_eq!(switched.selected_rows, 3);
        assert_eq!(
            switched.scans[1]
                .rows
                .iter()
                .find(|r| r.selected)
                .unwrap()
                .key,
            "B"
        );
        let mut reordered = snapshots;
        reordered.reverse();
        assert!(replay(&spec, &reordered).is_err());
        let mut marked = reordered;
        marked.reverse();
        for (i, snapshot) in marked.iter_mut().enumerate() {
            snapshot.rows[0]
                .features
                .insert(Feature::Price, 100.0 + i as f64);
        }
        let config = ReplayConfig {
            horizon_ms: 1000,
            max_mark_delay_ms: 0,
            costs: None,
            ..ReplayConfig::default()
        };
        let measured = replay_configured(&spec, &marked, &config).unwrap();
        assert_eq!(measured.forward_returns.len(), 1);
        assert!((measured.forward_returns[0].return_bps - 200.0).abs() < 0.00001);
        assert_eq!(measured.missing_forward_marks, 0);
        let with_costs = replay_configured(
            &spec,
            &marked,
            &ReplayConfig {
                costs: Some(MarkCosts {
                    notional: nivra_core::finance::Money::try_from(
                        nivra_core::finance::Amount::from_integer(5000),
                    )
                    .unwrap(),
                    round_trip_spread_bps: 10.0,
                    fees: serde_json::from_value(serde_json::json!({"per_order":"0.01","buy_bps":"5","sell_bps":"7","rounding_increment":"0.01"})).unwrap(),
                }),
                ..config.clone()
            },
        )
        .unwrap();
        let costs = with_costs.cost_adjusted.unwrap();
        assert_eq!(costs.samples, 1);
        assert!(costs.fees > nivra_core::finance::Amount::ZERO);
        assert!(costs.net_expectancy_bps < measured.mean_forward_return_bps.unwrap() - 10.0);
        assert!(costs.turnover > nivra_core::finance::Amount::from_integer(10000));
        // A cost-adjusted replay must declare its fee policy.
        assert!(
            serde_json::from_value::<MarkCosts>(serde_json::json!({
                "notional":"5000", "round_trip_spread_bps":10
            }))
            .is_err()
        );
        let configured: MarkCosts = serde_json::from_value(serde_json::json!({
            "notional":"5000", "round_trip_spread_bps":10,
            "fees":{"per_order":"1","buy_bps":"1","sell_bps":"2","rounding_increment":"0.01"}
        }))
        .unwrap();
        let fee_report = replay_configured(
            &spec,
            &marked,
            &ReplayConfig {
                costs: Some(configured),
                ..config.clone()
            },
        )
        .unwrap();
        let fee_metrics = fee_report.cost_adjusted.as_ref().unwrap();
        assert_eq!(fee_metrics.fees.to_string(), "3.52");
        assert!((fee_metrics.net_expectancy_bps - 182.96).abs() < 0.00001);
        assert_eq!(
            serde_json::to_value(fee_report).unwrap()["replay_config"]["costs"]["fees"]["per_order"],
            "1"
        );
        for invalid in [
            serde_json::json!({"per_order":"-1","buy_bps":"1","sell_bps":"2"}),
            serde_json::json!({"per_order":1.0,"buy_bps":"1","sell_bps":"2"}),
            serde_json::json!({"per_order":"1","buy_bps":"1","sell_bps":"2","rounding_increment":"0"}),
            serde_json::json!({"per_order":"1","buy_bps":"1","sell_bps":"2","slippage_bps":"5"}),
        ] {
            assert!(serde_json::from_value::<ReplayFees>(invalid).is_err());
        }
        let mut invalid_mark = marked.clone();
        let duplicate = invalid_mark[2].rows[0].clone();
        invalid_mark[2].rows.push(duplicate);
        let refused = replay_configured(&spec, &invalid_mark, &config).unwrap();
        assert_eq!(refused.failed_ticks, 1);
        assert_eq!(refused.missing_forward_marks, 1);
        assert!(refused.forward_returns.is_empty());
        assert_eq!(
            refused.missing_mark_reasons[&MissingMarkReason::OnlyFailedSnapshotsInWindow],
            1
        );
        let mut rotated = marked.clone();
        rotated[2].observed_ms = 2100;
        rotated.insert(
            2,
            FeatureSnapshot {
                observed_ms: 2000,
                lookback_samples: 20,
                rows: vec![FeatureRow {
                    key: "other".into(),
                    features: BTreeMap::from([(Feature::Price, 999.0)]),
                }],
            },
        );
        let delayed = replay_configured(
            &spec,
            &rotated,
            &ReplayConfig {
                max_mark_delay_ms: 100,
                ..config.clone()
            },
        )
        .unwrap();
        assert_eq!(delayed.forward_returns.len(), 1);
        assert_eq!(delayed.forward_returns[0].mark_ms, 2100);
        let selected_window = replay_configured(
            &spec,
            &marked,
            &ReplayConfig {
                selection_from_ms: Some(1000),
                selection_to_ms: Some(1000),
                ..config.clone()
            },
        )
        .unwrap();
        assert_eq!(selected_window.selected_rows, 1);
        assert_eq!(selected_window.forward_returns.len(), 1);
        assert_eq!(
            selected_window.scans[2].rows[0].reason,
            "outside_selection_window"
        );
        let no_selections = replay_configured(
            &spec,
            &marked,
            &ReplayConfig {
                selection_from_ms: Some(2001),
                ..config.clone()
            },
        )
        .unwrap();
        assert_eq!(no_selections.selected_rows, 0);
        assert!(no_selections.missing_mark_reasons.is_empty());
        assert!(
            replay_configured(
                &spec,
                &marked,
                &ReplayConfig {
                    selection_from_ms: Some(1001),
                    selection_to_ms: Some(1000),
                    ..config.clone()
                }
            )
            .is_err()
        );
        let mut no_price = rotated.clone();
        no_price[2].rows[0] = FeatureRow {
            key: "fixture".into(),
            features: BTreeMap::from([(Feature::MoveBps, 10.0)]),
        };
        let mut gap = marked.clone();
        gap[2].observed_ms = 2200;
        let mut overflow = marked[..1].to_vec();
        overflow[0].observed_ms = i64::MAX - 500;
        let mut extreme = marked.clone();
        extreme[0].rows[0]
            .features
            .insert(Feature::Price, f64::MIN_POSITIVE);
        extreme[2].rows[0].features.insert(Feature::Price, f64::MAX);
        for (snapshots, reason) in [
            (rotated, MissingMarkReason::InstrumentAbsentInWindow),
            (no_price, MissingMarkReason::PriceUnavailableInWindow),
            (gap, MissingMarkReason::NoSnapshotInWindow),
            (
                marked[..2].to_vec(),
                MissingMarkReason::RecordingEndsBeforeHorizon,
            ),
            (overflow, MissingMarkReason::HorizonOverflow),
            (extreme, MissingMarkReason::NonfiniteReturn),
        ] {
            let result = replay_configured(&spec, &snapshots, &config).unwrap();
            assert_eq!(result.missing_forward_marks, 1);
            assert_eq!(result.missing_mark_reasons[&reason], 1);
            assert_eq!(
                result.missing_mark_reasons.values().sum::<usize>(),
                result.missing_forward_marks
            );
            assert!(result.forward_returns.is_empty());
        }
        let hash = measured.dataset_hash;
        marked[2].rows[0].features.insert(Feature::Price, 110.0);
        assert_ne!(
            hash,
            replay_configured(&spec, &marked, &config)
                .unwrap()
                .dataset_hash
        );
        assert!(
            replay_configured(
                &spec,
                &marked,
                &ReplayConfig {
                    horizon_ms: 0,
                    max_mark_delay_ms: 0,
                    costs: None,
                    ..ReplayConfig::default()
                }
            )
            .is_err()
        );
    }
}
