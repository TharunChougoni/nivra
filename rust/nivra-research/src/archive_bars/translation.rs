//! Translation probes only expose the bar fields available in an action
//! observation. Compare with the original executable on the same clock.
use super::*;
use nivra_store::market_archive::BarUnit;

#[derive(Clone, Debug)]
pub struct Probe {
    pub bar_closed_at_ms: i64,
    pub prices_paise: BTreeMap<String, i64>,
    pub decisions: BTreeMap<String, bool>,
}

/// Quote observations supply a prospect's last price. Open, high, low and
/// bar volume have no equivalent in this adapter, so the projected run
/// cannot read their values. Order sizes are evaluated by the action ABI.
pub fn translation_probes(
    request: &Request,
    source: &str,
    db: &mut ArchiveClient,
    budget: Duration,
    stop: &dyn Fn() -> bool,
) -> Result<Vec<Probe>> {
    if !request.valid()
        || request.window.unit != BarUnit::Minute
        || budget.is_zero()
        || budget > Duration::from_secs(300)
    {
        return Err("translation_parity_unavailable".into());
    }
    let qualification = &request
        .qualification
        .as_ref()
        .ok_or("translation_snapshot_missing")?;
    let init = serde_json::to_vec(&json!({"params":request.params,"source":source}))?;
    let deadline = Instant::now() + budget;
    let start = || {
        Stream::start(
            &request.sandbox,
            include_bytes!("../bar_strategy.py"),
            &init,
            budget.as_secs().max(1),
        )
        .map_err(|_| "translation_strategy_start_failed")
    };
    let mut original = start()?;
    let mut projected = start()?;
    let embargoed = qualification.embargoed_dates(&request.splits);
    let mut snapshot = BarSnapshotBuilder::default();
    let mut probes = Vec::new();
    let mut response = Vec::new();
    market_archive::read_bar_batches(db, &request.window, budget, |at, keys, bars| {
        if stop() {
            return Err("decision_preempted".into());
        }
        if Instant::now() >= deadline {
            return Err("translation_wall_budget".into());
        }
        let Some(split) = request.split(at) else {
            return Ok(());
        };
        snapshot.push(at, keys, bars);
        if embargoed.contains(&nivra_core::autoresearch::ist_day(at)) {
            return Ok(());
        }
        let rows = |projected: bool| {
            bars.iter()
                .map(|bar| {
                    let full = |value| (!projected).then_some(value);
                    (
                        keys[bar.instrument].as_str(),
                        full(bar.open),
                        full(bar.high),
                        full(bar.low),
                        bar.close,
                        full(bar.volume),
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut call = |worker: &mut Stream, projected| -> Result<Vec<(usize, u64)>> {
            let mut input = serde_json::to_vec(&(at, split, rows(projected)))?;
            input.push(b'\n');
            worker
                .exchange_within(
                    &input,
                    &mut response,
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(request.sandbox.wall_ms)),
                )
                .map_err(|_| "translation_features_unavailable")?;
            let (reported_at, mut orders): (i64, Vec<(usize, u64)>) =
                serde_json::from_slice(&response)?;
            orders.sort_unstable();
            if reported_at != at
                || orders
                    .iter()
                    .any(|&(row, units)| row >= bars.len() || units > 1_000_000)
                || orders.windows(2).any(|pair| pair[0].0 == pair[1].0)
            {
                return Err("translation_actions_invalid".into());
            }
            Ok(orders)
        };
        let actions = call(&mut original, false)?;
        if actions != call(&mut projected, true)? {
            return Err("translation_feature_parity_failed".into());
        }
        if split == Split::Test {
            if probes.len() >= 10_000 || bars.len() > 20 {
                return Err("translation_probe_limit".into());
            }
            probes.push(Probe {
                bar_closed_at_ms: at + 60_000,
                prices_paise: bars
                    .iter()
                    .map(|b| (keys[b.instrument].clone(), b.close))
                    .collect(),
                decisions: actions
                    .into_iter()
                    .map(|(row, units)| (keys[bars[row].instrument].clone(), units != 0))
                    .collect(),
            });
        }
        Ok(())
    })?;
    original
        .finish()
        .map_err(|_| "translation_strategy_exit_failed")?;
    projected
        .finish()
        .map_err(|_| "translation_strategy_exit_failed")?;
    if snapshot.finish() != qualification.snapshot {
        return Err("archive_bars_snapshot_changed".into());
    }
    Ok(probes)
}
