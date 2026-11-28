use super::*;
use chrono::{Datelike, Months};
use harness::{Admit, Admitted};
use nivra_core::autoresearch::{Verdict, qualification};
use nivra_core::finance::{Amount, FeeArithmetic};
use nivra_store::market_archive::{BarSnapshot, BarWindow};
use nivra_store::research_jobs::BacktestWindows;
use nivra_store::research_memory::{Campaign, NewCampaign, Outcome};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Every bar experiment of a campaign is scored on exactly these inputs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::runtime::autoresearch) struct BarManifest {
    pub protocol: u32,
    pub abi: String,
    pub window: BarWindow,
    pub splits: BacktestWindows,
    pub fold_boundaries_ms: [i64; 2],
    pub embargo_sessions: u32,
    pub snapshot: Option<BarSnapshot>,
    pub runner_sha256: Option<String>,
    pub max_drawdown_bps: i64,
    pub capital: Amount,
    pub costs: FeeArithmetic,
    pub comparator: String,
    pub sandbox: nivra_sandbox::SandboxConfig,
}

/// The bar manifest a campaign ending on `to` freezes.
pub(in crate::runtime::autoresearch) fn campaign_manifest(
    c: &Config,
    cfg: &PlannerConfig,
    settings: &AutoresearchSettings,
    to: NaiveDate,
) -> Result<BarManifest> {
    let campaign = &settings.campaign;
    let next_month = to.succ_opt().ok_or("autoresearch_campaign_invalid")?;
    let end = next_month
        .with_day(1)
        .ok_or("autoresearch_campaign_invalid")?;
    let month_before = |months| {
        end.checked_sub_months(Months::new(months))
            .ok_or("autoresearch_campaign_invalid")
    };
    let from = month_before(11)?;
    let selection_from = month_before(3)?;
    let (_, candidate) = loop_settings(c, cfg)?;
    if c.sleeves.cash.capital <= Amount::ZERO {
        return Err("cash_sleeve_capital_required".into());
    }
    Ok(BarManifest {
        protocol: nivra_core::autoresearch::PROTOCOL_VERSION,
        abi: nivra_research::archive_bars::ABI.into(),
        window: BarWindow {
            unit: campaign.unit,
            from,
            to: end.pred_opt().ok_or("autoresearch_campaign_invalid")?,
            instruments: campaign.instruments.clone().map(|mut keys| {
                keys.sort_unstable();
                keys.dedup();
                keys
            }),
        },
        splits: BacktestWindows {
            training_from_ms: ist_midnight(from),
            training_to_ms: ist_midnight(selection_from) - 1,
            selection_from_ms: ist_midnight(selection_from),
            selection_to_ms: ist_midnight(end) - 1,
        },
        fold_boundaries_ms: [
            ist_midnight(month_before(2)?),
            ist_midnight(month_before(1)?),
        ],
        snapshot: None,
        runner_sha256: None,
        embargo_sessions: campaign.embargo_sessions,
        max_drawdown_bps: rule(cfg, settings, Stage::Bars).max_drawdown_bps,
        capital: c.sleeves.cash.capital,
        costs: c.paper_costs.clone().ok_or("paper_costs_required")?,
        comparator: "cash".into(),
        sandbox: candidate.sandbox,
    })
}

/// The named campaign, created and frozen on first use.
pub(in crate::runtime::autoresearch) fn ensure_campaign(
    store: &mut Store,
    c: &Config,
    cfg: &PlannerConfig,
    settings: &AutoresearchSettings,
    to: NaiveDate,
    now: i64,
) -> Result<Campaign> {
    let mut manifest = campaign_manifest(c, cfg, settings, to)?;
    if store.holdout_overlaps(manifest.window.from, manifest.window.to)? {
        return Err("research_holdout_sealed".into());
    }
    manifest.snapshot = Some(read_snapshot(c, &manifest.window)?);
    manifest.runner_sha256 = Some(harness::runtime_sha256()?);
    if !coverage_complete(
        c,
        &manifest,
        manifest.snapshot.as_ref().expect("frozen snapshot"),
    )? {
        return Err("archive_bars_warmup_or_fold_missing".into());
    }
    let bytes = canonical(&serde_json::to_value(&manifest)?)?;
    Ok(store.ensure_campaign(&NewCampaign {
        name: &settings.campaign.name,
        segment: super::super::SEGMENT,
        manifest: &bytes,
        development_from: manifest.window.from,
        development_to: manifest.window.to,
        selection_from: nivra_core::autoresearch::ist_day(manifest.splits.selection_from_ms),
        trial_cap: settings.campaign.trial_cap,
        now,
    })?)
}

#[derive(Deserialize)]
struct BarReport {
    bar_snapshot: BarSnapshot,
    test: qualification::Fold,
    folds: Vec<qualification::Fold>,
}

impl BarReport {
    fn read(bytes: &[u8]) -> Result<(BarSnapshot, Vec<qualification::Fold>)> {
        let report: Self =
            serde_json::from_slice(bytes).map_err(|_| "archive_bars_report_invalid")?;
        let mut folds = vec![report.test];
        folds.extend(report.folds);
        if folds.len() != 3 {
            return Err("archive_bars_report_invalid".into());
        }
        Ok((report.bar_snapshot, folds))
    }
}

pub(in crate::runtime::autoresearch) fn bar_outcome(
    store: &Store,
    experiment: &Experiment,
    report: &[u8],
) -> Result<Outcome> {
    let invalid = |reason: &str| Outcome::Invalid {
        reason: reason.into(),
    };
    let mut body: Value = serde_json::from_slice(report)?;
    if body["status"] == "unreproducible" {
        return Ok(invalid("archive_bars_snapshot_changed"));
    }
    let Ok((snapshot, folds)) = BarReport::read(report) else {
        return Ok(invalid("archive_bars_report_invalid"));
    };
    let manifest: BarManifest = serde_json::from_slice(&store.object(&experiment.manifest_blob)?)?;
    if manifest.snapshot.as_ref() != Some(&snapshot) {
        return Ok(invalid("archive_bars_snapshot_changed"));
    }
    let score = qualification::pooled(&folds)?;
    let cash = qualification::paired(&folds, None, manifest.capital)?;
    let mut decision = qualification::verdict(&score, cash.as_ref(), manifest.max_drawdown_bps);
    body["qualification"] = json!({"cash":cash,"parent":null,"incumbent":null,
        "cash_initial_equity":manifest.capital,
        "universe_limitation":"archived cash keys; historical membership and adjustments unverified"});
    let idea = store
        .idea(experiment.idea_id)?
        .ok_or("research_idea_unknown")?;
    let incumbent = store
        .cells(experiment.campaign_id)?
        .into_iter()
        .find(|cell| cell.family == idea.family && cell.regime == idea.regime.as_str())
        .map(|cell| cell.incumbent_experiment_id);
    for (role, id) in [("parent", experiment.parent_id), ("incumbent", incumbent)] {
        let Some(id) = id else { continue };
        let other = store.experiment(id)?.filter(|other| {
            other.stage == Stage::Bars
                && other.status == "scored"
                && other.manifest_blob == experiment.manifest_blob
        });
        let comparison = if let Some(blob) = other.and_then(|other| other.report_blob) {
            let (_, baseline) = BarReport::read(&store.object(&blob)?)?;
            qualification::paired(&folds, Some(&baseline), manifest.capital)?
        } else {
            None
        };
        let paired = qualification::verdict(&score, comparison.as_ref(), manifest.max_drawdown_bps);
        if decision.verdict == Verdict::Keep {
            decision = paired;
        }
        body["qualification"][role] = json!({"experiment_id":id,"comparison":comparison});
    }
    let trial_count = store.trials(experiment.campaign_id)?;
    let moments = qualification::daily_moments(&folds, manifest.capital);
    let mut sharpes: Vec<_> = moments.iter().map(|m| m.sharpe).collect();
    for other in store.recent_experiments(experiment.campaign_id, trial_count as usize)? {
        if other.id != experiment.id
            && other.status == "scored"
            && other.manifest_blob == experiment.manifest_blob
            && let Some(blob) = other.report_blob
        {
            let (_, baseline) = BarReport::read(&store.object(&blob)?)?;
            if let Some(moments) = qualification::daily_moments(&baseline, manifest.capital) {
                sharpes.push(moments.sharpe);
            }
        }
    }
    body["qualification"]["deflated_sharpe"] = json!(qualification::deflated_sharpe(
        moments,
        &sharpes,
        trial_count,
        sharpes.len() != trial_count as usize
    ));
    Ok(Outcome::Scored {
        report_blob: store.put_object(&canonical(&body)?)?,
        score,
        decision,
    })
}

pub(super) fn coverage_complete(
    c: &Config,
    manifest: &BarManifest,
    snapshot: &BarSnapshot,
) -> Result<bool> {
    let months: std::collections::BTreeSet<_> = snapshot
        .dates
        .iter()
        .map(|day| (day.year(), day.month()))
        .collect();
    let calendar_from = manifest.window.from.max(
        c.calendar_coverage_at(manifest.window.to)?
            .covered_from
            .parse::<NaiveDate>()?,
    );
    let expected =
        super::super::qualification::exchange_dates(c, calendar_from, manifest.window.to)?;
    let observed = snapshot
        .dates
        .iter()
        .copied()
        .filter(|day| *day >= calendar_from);
    Ok(months.len() == 11 && observed.eq(expected))
}

pub(in crate::runtime::autoresearch) fn read_snapshot(
    c: &Config,
    window: &BarWindow,
) -> Result<BarSnapshot> {
    let archive = c.market_archive.as_ref().ok_or("archive_config_missing")?;
    let mut db = crate::runtime::market_archive::connect(archive)
        .map_err(|_| "archive_connection_failed")?;
    market_archive::bar_snapshot(&mut db, window, Duration::from_secs(20))
        .map_err(|_| "archive_bars_snapshot_unavailable".into())
}

/// Admit an experiment's frozen bar job to the loop queue. Admitting the same
/// experiment again returns its existing job.
pub(in crate::runtime::autoresearch) fn admit_bar(
    c: &Config,
    cfg: &PlannerConfig,
    store: &Store,
    experiment: &Experiment,
    goal: &str,
    now: i64,
) -> Result<std::result::Result<(String, u32), Value>> {
    let code = store.object(&experiment.source_blob)?;
    let params: Value = serde_json::from_slice(&store.object(&experiment.params_blob)?)?;
    let manifest: BarManifest = serde_json::from_slice(&store.object(&experiment.manifest_blob)?)?;
    if manifest.runner_sha256.as_deref() != Some(harness::runtime_sha256()?.as_str()) {
        return Err("archive_bars_runner_changed".into());
    }
    let archive = c.market_archive.as_ref().ok_or("archive_config_missing")?;
    let (research, mut candidate) = loop_settings(c, cfg)?;
    candidate.sandbox = manifest.sandbox.clone();
    let mut request = harness::bar_request(
        archive,
        manifest.window,
        manifest.splits,
        manifest.capital,
        manifest.costs,
        params,
        &candidate,
    )
    .ok_or("candidate_archive_bars_invalid")?;
    request.qualification = Some(nivra_research::archive_bars::Qualification {
        snapshot: manifest
            .snapshot
            .ok_or("archive_bars_snapshot_unavailable")?,
        fold_boundaries_ms: Some(manifest.fold_boundaries_ms),
        embargo_sessions: manifest.embargo_sessions,
    });
    let input = serde_json::to_vec(&request)?;
    let root = research_root(c)?;
    let mut db = harness::open_queue(&research, &root, true)?;
    let admitted = harness::admit(&mut Admit {
        settings: &research,
        candidate: &candidate,
        db: &mut db,
        authority: &root,
        binding: loop_binding(),
        session: loop_session(store, experiment)?,
        task_key: &task_key(experiment),
        parent_turn_id: "autoresearch",
        model_id: "autoresearch",
        goal,
        dataset_id: harness::ARCHIVE_BARS_DATASET_ID,
        policy_sha256: experiment.manifest_blob.clone(),
        code: &code,
        input: &input,
        limits: None,
        now,
    })?;
    Ok(match admitted {
        Admitted::Queued(record) => Ok((record.spec.task_id, record.spec.revision)),
        Admitted::Refused(value) => Err(value),
    })
}
