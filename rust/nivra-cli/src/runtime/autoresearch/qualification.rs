use super::jobs::{self, BarManifest};
use crate::runtime::harness::research as harness;
use crate::runtime::planner::PlannerConfig;
use crate::runtime::{Config, Result};
use chrono::{Datelike, Months, NaiveDate};
use nivra_core::autoresearch::{Stage, ist_day, qualification};
use nivra_store::market_archive::ist_midnight;
use nivra_store::research_memory::{Campaign, Experiment, Holdout, Store};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// A future-only confirmation window. No date in the freeze month is eligible.
fn future_months(now: i64) -> Result<(NaiveDate, NaiveDate)> {
    let from = ist_day(now)
        .with_day(1)
        .and_then(|d| d.checked_add_months(Months::new(1)))
        .ok_or("research_holdout_window_invalid")?;
    let to = from
        .checked_add_months(Months::new(2))
        .and_then(|d| d.pred_opt())
        .ok_or("research_holdout_window_invalid")?;
    Ok((from, to))
}

/// Run at most one confirmation per campaign. Remember a crash after the first
/// data read as inconclusive, since reopening could expose revised inputs.
pub(super) fn advance(
    c: &Config,
    cfg: &PlannerConfig,
    store: &mut Store,
    campaign: &Campaign,
    last_sealed: NaiveDate,
    now: i64,
    stop: &dyn Fn() -> bool,
) -> Result<&'static str> {
    let Some(holdout) = store.holdout(campaign.id)? else {
        let finalist = store
            .cells(campaign.id)?
            .into_iter()
            .map(|cell| store.experiment(cell.incumbent_experiment_id))
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .filter(|e| e.stage == Stage::Bars && e.status == "scored")
            .filter_map(|e| e.score.as_ref().map(|score| (score.net_pnl, e.id)))
            .max_by_key(|&(net, id)| (net, std::cmp::Reverse(id)));
        let Some((_, finalist)) = finalist else {
            return Ok("qualification_no_finalist");
        };
        let (from, to) = future_months(now)?;
        return Ok(if store.freeze_finalist(campaign.id, finalist, from, to)? {
            "waiting_for_future_holdout"
        } else {
            "research_holdout_window_reserved"
        });
    };
    match holdout.state.as_str() {
        "retired" if holdout.verdict.as_deref() == Some("keep") => return Ok("holdout_passed"),
        "retired" => return Ok("holdout_finished"),
        "opened" => {
            store.finish_holdout(campaign.id, "inconclusive", None)?;
            return Ok("holdout_interrupted");
        }
        _ => {}
    }
    if ist_day(now) <= holdout.to {
        return Ok("waiting_for_future_holdout");
    }
    let dates = exchange_dates(c, holdout.from, holdout.to)?;
    let Some(last) = dates.last().filter(|&&day| day <= last_sealed) else {
        return Ok("waiting_for_future_holdout");
    };
    if !super::cli::archive_covers(c, *last)? {
        return Ok("waiting_for_holdout_tape");
    }
    let Some(_slot) = harness::compute_slot(c)? else {
        return Ok("research_compute_busy");
    };
    if !store.open_holdout_once(campaign.id)? {
        return Ok("holdout_already_opened");
    }
    let result = confirm(c, cfg, store, &holdout, dates, stop);
    let (verdict, evidence) = match result {
        Ok((verdict, report)) => (verdict, store.put_object(&serde_json::to_vec(&report)?)?),
        Err(error) => (
            "inconclusive",
            store.put_object(&serde_json::to_vec(&json!({
                "reason":crate::runtime::harness::failure_reason(error.as_ref())
            }))?)?,
        ),
    };
    store.finish_holdout(campaign.id, verdict, Some(&evidence))?;
    Ok(if verdict == "keep" {
        "holdout_passed"
    } else {
        "holdout_finished"
    })
}

pub(super) fn exchange_dates(c: &Config, from: NaiveDate, to: NaiveDate) -> Result<Vec<NaiveDate>> {
    let mut expected = Vec::new();
    for date in from.iter_days().take_while(|day| *day <= to) {
        if c.is_trading_day(date)
            .map_err(|_| "qualification_calendar_unavailable")?
        {
            expected.push(date);
        }
    }
    Ok(expected)
}

fn confirm(
    c: &Config,
    cfg: &PlannerConfig,
    store: &Store,
    holdout: &Holdout,
    expected: Vec<NaiveDate>,
    stop: &dyn Fn() -> bool,
) -> Result<(&'static str, Value)> {
    let finalist = store
        .experiment(holdout.finalist)?
        .filter(|e| e.status == "scored")
        .ok_or("research_finalist_unavailable")?;
    let mut manifest: BarManifest =
        serde_json::from_slice(&store.object(&finalist.manifest_blob)?)?;
    let discovery_to = manifest.window.to;
    manifest.window.to = holdout.to;
    let archive = c.market_archive.as_ref().ok_or("archive_config_missing")?;
    let mut db = crate::runtime::market_archive::connect(archive)
        .map_err(|_| "archive_connection_failed")?;
    let (snapshot, prefix) = nivra_store::market_archive::bar_snapshot_with_prefix(
        &mut db,
        &manifest.window,
        Some(discovery_to),
        Duration::from_secs(20),
    )
    .map_err(|_| "archive_bars_snapshot_unavailable")?;
    if prefix != manifest.snapshot {
        return Err("archive_bars_snapshot_changed".into());
    }
    let observed: Vec<_> = snapshot
        .dates
        .iter()
        .copied()
        .filter(|day| *day >= holdout.from)
        .collect();
    if observed != expected {
        return Err("research_holdout_coverage_incomplete".into());
    }
    manifest.splits.training_to_ms = ist_midnight(holdout.from) - 1;
    manifest.splits.selection_from_ms = ist_midnight(holdout.from);
    manifest.splits.selection_to_ms = ist_midnight(
        holdout
            .to
            .succ_opt()
            .ok_or("research_holdout_window_invalid")?,
    ) - 1;
    let (_, candidate) = jobs::loop_settings(c, cfg)?;
    let deadline = Instant::now()
        + Duration::from_millis(candidate.wall_millis.into()).min(Duration::from_secs(300));
    let mut run = |experiment: &Experiment| -> Result<qualification::Fold> {
        let params = serde_json::from_slice(&store.object(&experiment.params_blob)?)?;
        let mut request = harness::bar_request(
            archive,
            manifest.window.clone(),
            manifest.splits.clone(),
            manifest.capital,
            manifest.costs.clone(),
            params,
            &candidate,
        )
        .ok_or("candidate_archive_bars_invalid")?;
        request.sandbox = manifest.sandbox.clone();
        request.qualification = Some(nivra_research::archive_bars::Qualification {
            snapshot: snapshot.clone(),
            fold_boundaries_ms: None,
            embargo_sessions: manifest.embargo_sessions,
        });
        let source = String::from_utf8(store.object(&experiment.source_blob)?)?;
        let report = nivra_research::archive_bars::run_stoppable(
            &request,
            &source,
            &mut db,
            deadline.saturating_duration_since(Instant::now()),
            stop,
        )?;
        let mut body: Value = serde_json::from_slice(&report)?;
        if body["status"] == "unreproducible" {
            return Err("archive_bars_snapshot_changed".into());
        }
        Ok(serde_json::from_value(body["test"].take())
            .map_err(|_| "archive_bars_report_invalid")?)
    };
    let candidate = [run(&finalist)?];
    let baseline = if let Some(id) = finalist.parent_id {
        let parent = store
            .experiment(id)?
            .filter(|e| e.manifest_blob == finalist.manifest_blob && e.status == "scored")
            .ok_or("research_parent_unavailable")?;
        Some([run(&parent)?])
    } else {
        None
    };
    let score = qualification::pooled(&candidate)?;
    let comparison = qualification::paired(
        &candidate,
        baseline.as_ref().map(|folds| &folds[..]),
        manifest.capital,
    )?;
    let decision = qualification::verdict(&score, comparison.as_ref(), manifest.max_drawdown_bps);
    Ok((
        decision.verdict.as_str(),
        json!({"score":score,"comparison":comparison,"reason":decision.reason,
        "bar_snapshot_sha256":snapshot.ordered_sha256, "scored_dates":observed.len()}),
    ))
}
