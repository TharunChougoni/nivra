use super::*;

#[cfg(test)]
pub(crate) fn legacy_project(
    scan: &Scanner,
    instruments: &[Instrument],
    settings: &Settings,
    now: i64,
) -> Value {
    if settings
        .strategy
        .as_ref()
        .is_some_and(|s| s.enabled && s.validate().is_err())
    {
        return json!({"as_of_ms":now,"prospects":[],"strategy_snapshots":[],"strategy_error":"invalid_strategy_settings","financial_quote":false});
    }
    let recommended = sadr_core::execution::ScreenPolicy::recommended();
    let policy = settings.screens.as_ref().unwrap_or(&recommended);
    let mut prospects = vec![];
    let mut unavailable = vec![];
    let mut reasons = std::collections::BTreeMap::<String, usize>::new();
    let mut changes = Vec::new();
    let mut fresh = 0;
    let mut stale = 0;
    let mut unseen = 0;
    let mut oldest = None::<i64>;
    for instrument in instruments {
        let sample = scan.samples.get(&instrument.key);
        let max_age = sample
            .and_then(|s| s.fresh_for_ms)
            .unwrap_or(settings.max_scan_age_ms)
            .min(if sample.is_some_and(|s| s.scan_activity.is_some()) {
                settings
                    .market_feed
                    .as_ref()
                    .map(|f| f.scan_activity_timeout_ms)
                    .unwrap_or(settings.max_scan_age_ms)
            } else {
                settings.max_scan_age_ms
            });
        let age = sample
            .filter(|s| s.seen > 0)
            .and_then(|s| now.checked_sub(s.scan_activity.unwrap_or(s.seen)));
        let reason = if sample.is_none_or(|s| s.seen == 0) {
            unseen += 1;
            sample
                .map(|s| s.reason.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("unseen")
        } else if sample.is_some_and(|s| {
            matches!(
                s.reason.as_str(),
                "no_quote_returned" | "invalid_quote" | "stale"
            )
        }) {
            stale += 1;
            sample.unwrap().reason.as_str()
        } else if age.is_none_or(|age| age < 0 || age > max_age)
            || sample
                .filter(|s| s.scan_activity.is_none())
                .and_then(|s| s.source)
                .is_some_and(|at| {
                    now.checked_sub(at)
                        .is_none_or(|age| age < 0 || age > max_age)
                })
        {
            stale += 1;
            "stale"
        } else {
            fresh += 1;
            oldest = Some(oldest.unwrap_or(0).max(age.unwrap_or(0)));
            let sample = sample.unwrap();
            let change = sample
                .history
                .first()
                .zip(sample.current.as_ref().or_else(|| sample.history.last()))
                .and_then(|(first, last)| {
                    let first = first.amount().to_string().parse::<f64>().ok()?;
                    let last = last.amount().to_string().parse::<f64>().ok()?;
                    (first > 0.0).then_some((last / first - 1.0) * 10000.0)
                });
            if let Some(change) = change {
                changes.push(change);
            }
            let reason = if let Some(fields) = sample.market_fields.as_ref() {
                sample
                    .current
                    .as_ref()
                    .or_else(|| sample.history.last())
                    .and_then(|price| policy.reason(price.amount(), fields).ok())
                    .unwrap_or("missing_market_fields")
            } else if sample.reason == "eligible_no_signal" {
                "missing_market_fields"
            } else {
                sample.reason.as_str()
            };
            let reason = if sample.reason == "missing_market_fields" {
                "missing_market_fields"
            } else {
                reason
            };
            if reason == "eligible_no_signal"
                || settings.strategy.as_ref().is_some_and(|s| s.enabled)
            {
                prospects.push(json!({"instrument_key":instrument.key,"symbol":instrument.symbol,"observed_ms":sample.seen,"screening_age_ms":age,"source_ms":sample.source,"sampled_price_ms":sample.last,"latest_screening_price":sample.current.as_ref().or_else(||sample.history.last()),"price_kind":sample.price_kind.as_deref().unwrap_or("legacy_bid"),"sampled_bid_ms":if sample.price_kind.as_deref().is_none_or(|k|k=="bid"){Some(sample.last)}else{None},"latest_sampled_bid":if sample.price_kind.as_deref().is_none_or(|k|k=="bid"){sample.current.as_ref().or_else(||sample.history.last())}else{None},"sample_count":sample.history.len(),"sample_change_bps":change,"screening_reason":reason,"diagnostic":sample.diagnostic,"financial_quote":false,
                    "bid":sample.executable.as_ref().map(|q|q.bid),"ask":sample.executable.as_ref().map(|q|q.ask),
                    "bid_units":sample.executable.as_ref().map(|q|q.bid_units),"ask_units":sample.executable.as_ref().map(|q|q.ask_units),
                    "quote_source_ms":sample.executable.as_ref().map(|q|q.source),"quote_age_ms":sample.executable.as_ref().and_then(|q|now.checked_sub(q.receipt))}));
            }
            reason
        };
        *reasons.entry(reason.to_owned()).or_default() += 1;
        if matches!(
            reason,
            "unseen" | "stale" | "no_quote_returned" | "invalid_quote" | "missing_market_fields"
        ) || sample.is_some_and(|s| s.diagnostic.is_some())
        {
            unavailable.push(json!({"instrument_key":instrument.key,"symbol":instrument.symbol,"reason":reason,
                "diagnostic":sample.and_then(|s|s.diagnostic.as_deref()),"last_observed_ms":sample.filter(|s|s.seen>0).map(|s|s.seen),"age_ms":age}));
        }
    }
    prospects.sort_by(|a, b| {
        b["sample_change_bps"]
            .as_f64()
            .unwrap_or(0.0)
            .abs()
            .total_cmp(&a["sample_change_bps"].as_f64().unwrap_or(0.0).abs())
            .then_with(|| {
                a["instrument_key"]
                    .as_str()
                    .cmp(&b["instrument_key"].as_str())
            })
    });
    let mut strategy_snapshots = Vec::new();
    if let Some(strategy) = settings.strategy.as_ref().filter(|s| s.enabled) {
        // Cache entries from an earlier universe must never consume shortlist
        // slots or enter the cross-sectional baseline for this universe.
        let mut universe_scan = scan.clone();
        universe_scan
            .samples
            .retain(|key, _| instruments.iter().any(|i| i.key == *key));
        let metadata_age = settings
            .market_feed
            .as_ref()
            .map(|feed| feed.metadata_stale_ms)
            .unwrap_or(settings.max_scan_age_ms);
        for spec in &strategy.specs {
            let result = sadr_core::strategy::extract_features_with_settings(
                &universe_scan,
                now,
                settings.max_scan_age_ms,
                metadata_age,
                spec.lookback_samples,
                &strategy.feature_settings,
            )
            .and_then(|features| sadr_core::strategy::evaluate(spec, &features));
            if let Ok(mut snapshot) = result {
                for instrument in instruments {
                    if !snapshot.rows.iter().any(|r| r.key == instrument.key) {
                        snapshot.rows.push(sadr_core::strategy::ScanRow {
                            key: instrument.key.clone(),
                            features: Default::default(),
                            score: None,
                            reason: "unavailable_or_stale".into(),
                            selected: false,
                            trigger_eligible: None,
                        });
                    }
                }
                if snapshot.seal().is_err() {
                    prospects.clear();
                    continue;
                }
                if spec.id == strategy.champion {
                    prospects.retain_mut(|p| {
                        let row = snapshot
                            .rows
                            .iter()
                            .find(|r| Some(r.key.as_str()) == p["instrument_key"].as_str());
                        if let Some(row) = row.filter(|r| r.selected) {
                            p["strategy_scan_id"] = json!(snapshot.scan_id);
                            p["strategy_spec_hash"] = json!(snapshot.spec_hash);
                            p["strategy_score"] = json!(row.score);
                            p["strategy_spec_id"] = json!(spec.id);
                            p["strategy_spec_version"] = json!(spec.version);
                            p["strategy_features"] = json!(row.features);
                            true
                        } else {
                            false
                        }
                    });
                    prospects.sort_by(|a, b| {
                        b["strategy_score"]
                            .as_f64()
                            .unwrap_or(0.0)
                            .total_cmp(&a["strategy_score"].as_f64().unwrap_or(0.0))
                            .then(
                                a["instrument_key"]
                                    .as_str()
                                    .cmp(&b["instrument_key"].as_str()),
                            )
                    });
                }
                strategy_snapshots.push(snapshot);
            } else if spec.id == strategy.champion {
                prospects.clear();
            }
        }
    }
    let eligible = prospects.len();
    let baseline = (!changes.is_empty()).then(|| median(changes.clone()));
    // An enabled spec owns its shortlist size. Applying the legacy presentation
    // cap here would wake Luna for selected keys omitted from its evidence.
    if !settings.strategy.as_ref().is_some_and(|s| s.enabled) {
        prospects.truncate(settings.max_prospects);
    }
    let unavailable_count = unavailable.len();
    unavailable.truncate(100);
    let symbol_mismatches = scan
        .samples
        .values()
        .filter(|s| s.diagnostic.as_deref() == Some("symbol_mismatch"))
        .count();
    let (metadata_preloaded_count, metadata_preloaded_oldest_age_ms) =
        preloaded_metadata(scan, instruments, now);
    json!({"strategy_champion":settings.strategy.as_ref().filter(|s|s.enabled).map(|s|s.champion.as_str()),"strategy_snapshots":strategy_snapshots,"as_of_ms":now,"fresh_count":fresh,"eligible_count":eligible,"stale_count":stale,"unseen_count":unseen,
        "metadata_preloaded_count":metadata_preloaded_count,"metadata_preloaded_oldest_age_ms":metadata_preloaded_oldest_age_ms,"oldest_fresh_age_ms":oldest,"screen_reasons":reasons,"unavailable":unavailable,"unavailable_count":unavailable_count,"unavailable_truncated":unavailable_count>100,"symbol_mismatch_count":symbol_mismatches,
        "market_median_bps":baseline,"market_sample_count":changes.len(),"presented_count":prospects.len(),
        "max_scan_age_ms":settings.max_scan_age_ms,"prospects":prospects,
        "selection":"eligible fresh samples by absolute retained price change; market median uses all fresh samples before screens/truncation",
        "limitations":"Irregular price samples are screening evidence, never executable depth. Missing metadata is unknown."})
}
