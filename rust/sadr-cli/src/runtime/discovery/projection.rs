use super::*;

/// Scanner prospects with their sampled move, the only input to a scanner trigger.
pub(crate) fn candidates(projection: &Value) -> Vec<(String, Option<f64>)> {
    if let Some(champion) = projection["strategy_champion"].as_str() {
        return projection["strategy_snapshots"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| s["spec_id"].as_str() == Some(champion))
            .and_then(|s| s["rows"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|r| {
                Some((
                    r["key"].as_str()?.to_owned(),
                    if r["selected"] == true && r["trigger_eligible"] != false {
                        Some(1.0)
                    } else if r["trigger_eligible"] == true {
                        // Leaving the shortlist while still above the wake
                        // threshold is not evidence that a signal reset.
                        None
                    } else if (r["score"].is_number() && r["reason"] != "missing_trigger_feature")
                        || r["reason"]
                            .as_str()
                            .is_some_and(|s| s.starts_with("filtered:"))
                    {
                        Some(0.0)
                    } else {
                        None
                    },
                ))
            })
            .collect();
    }
    projection["prospects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some((
                p["instrument_key"].as_str()?.to_owned(),
                if p.get("strategy_score").is_some() {
                    Some(1.0)
                } else {
                    p["sample_change_bps"].as_f64()
                },
            ))
        })
        .collect()
}
/// Exact champion or live-ranking provenance carried by a planner decision;
/// historic plans stay absent. Live ranking is attribution-only and never
/// authorizes activation.
pub(crate) fn attribution(projection: &Value) -> Value {
    if let Some(live) = projection
        .get("live_ranking")
        .filter(|live| live.is_object())
    {
        let sha = live["sha256"].as_str().unwrap_or_default();
        let name = live["name"].as_str().unwrap_or_default();
        if sha.len() == 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            let reason = projection["selection"]
                .as_str()
                .filter(|reason| !reason.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| "live_ranking_consumed".into());
            return json!({
                "spec_id": name,
                "spec_version": 0,
                "spec_hash": sha,
                "live_ranking": true,
                "selection_reason": reason,
            });
        }
        return Value::Null;
    }
    if let Some(live) = projection.get("live_ranking") {
        if !live.is_object() {
            return Value::Null;
        }
        let sha = live["sha256"].as_str().unwrap_or_default();
        let name = live["name"].as_str().unwrap_or_default();
        if sha.len() == 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            let reason = projection["selection"]
                .as_str()
                .filter(|reason| !reason.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| "live_ranking_consumed".into());
            return json!({
                "spec_id": name,
                "spec_version": 0,
                "spec_hash": sha,
                "live_ranking": true,
                "selection_reason": reason,
            });
        }
        return Value::Null;
    }
    let Some(champion) = projection["strategy_champion"].as_str() else {
        return Value::Null;
    };
    let champion = champion.to_owned();
    projection["strategy_snapshots"].as_array().into_iter().flatten()
        .find(|s| s["spec_id"].as_str() == Some(champion.as_str()))
        .map(|s| json!({"scan_id":s["scan_id"],"spec_id":s["spec_id"],"spec_version":s["spec_version"],"spec_hash":s["spec_hash"],"live_ranking":false}))
        .unwrap_or(Value::Null)
}
pub(crate) fn preloaded_metadata(
    scan: &Scanner,
    instruments: &[Instrument],
    now: i64,
) -> (usize, Option<i64>) {
    let mut count = 0;
    let mut oldest_age = None;
    for instrument in instruments {
        let Some(sample) = scan.samples.get(&instrument.key) else {
            continue;
        };
        if sample.metadata_preloaded {
            count += 1;
            if let Some(age) = sample
                .market_fields_receipt
                .and_then(|at| now.checked_sub(at))
            {
                oldest_age = Some(oldest_age.map_or(age, |oldest: i64| oldest.max(age)));
            }
        }
    }
    (count, oldest_age)
}

/// Exact selected strategy identity and current per-instrument scores. Paper only.
#[derive(Clone, Debug)]
pub(crate) struct LiveRanking {
    pub sha256: String,
    pub name: String,
    pub scores: std::collections::BTreeMap<String, f64>,
}
pub(crate) fn apply_live_ranking(prospects: Vec<Value>, live: &LiveRanking) -> Vec<Value> {
    let mut prospects = prospects
        .into_iter()
        .filter(|p| {
            p["instrument_key"]
                .as_str()
                .and_then(|key| live.scores.get(key))
                .is_some_and(|score| score.is_finite())
        })
        .map(|mut p| {
            let key = p["instrument_key"].as_str().unwrap_or_default();
            p["live_score"] = json!(live.scores.get(key).copied().unwrap_or(f64::NAN));
            p
        })
        .collect::<Vec<_>>();
    prospects.sort_by(|a, b| {
        b["live_score"]
            .as_f64()
            .unwrap_or(f64::NEG_INFINITY)
            .total_cmp(&a["live_score"].as_f64().unwrap_or(f64::NEG_INFINITY))
            .then(
                a["instrument_key"]
                    .as_str()
                    .cmp(&b["instrument_key"].as_str()),
            )
    });
    prospects
}
pub(crate) fn project(
    scan: &Scanner,
    instruments: &[Instrument],
    settings: &Settings,
    now: i64,
) -> Value {
    project_with_live(scan, instruments, settings, now, None)
}
pub(crate) fn project_with_live(
    scan: &Scanner,
    instruments: &[Instrument],
    settings: &Settings,
    now: i64,
    live: Option<&LiveRanking>,
) -> Value {
    // The cash scanner projection is cash-only: NSE_FO option rows ride a
    // separate bounded shortlist and must never enter cash screens, breadth or
    // ranking. Filtering here keeps callers free to pass the combined
    // tradable allowlist.
    let cash = instruments
        .iter()
        .filter(|instrument| {
            sadr_core::market::Segment::from_key(&instrument.key)
                != Some(sadr_core::market::Segment::NseFo)
        })
        .cloned()
        .collect::<Vec<_>>();
    let instruments = &cash[..];
    // Invalid configured strategies never become runtime selection authority.
    if settings
        .strategy
        .as_ref()
        .is_some_and(|s| s.validate().is_err())
    {
        let mut fallback = settings.clone();
        fallback.strategy = None;
        return project(scan, instruments, &fallback, now);
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
    // The built-in composition owns default ranking. Keep presentation and
    // diagnostics separate so existing journals and model evidence stay identical.
    let universe: Vec<_> = instruments.iter().map(|i| i.key.clone()).collect();
    let default_ranking = sadr_core::strategy::evaluate_default_scanner_with_policy(
        &sadr_core::strategy::StrategySpec::default(),
        scan,
        &universe,
        now,
        settings.max_scan_age_ms,
        policy,
    );
    if let Ok(ranking) = default_ranking {
        let ranks: std::collections::BTreeMap<_, _> = ranking
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.score.is_some())
            .map(|(rank, row)| (row.key.as_str(), rank))
            .collect();
        prospects.sort_by_key(|row| {
            ranks
                .get(row["instrument_key"].as_str().unwrap_or_default())
                .copied()
                .unwrap_or(usize::MAX)
        });
    } else {
        // Invalid universe/cache admission supplies no selectable evidence.
        prospects.clear();
    }
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
    if let Some(live) = live {
        // The selected scorer owns this scanner shortlist. Unscored prospects are
        // omitted, matching scorer rejection semantics; validation still decides.
        prospects = apply_live_ranking(prospects, live);
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
    let mut projection = json!({"strategy_champion":settings.strategy.as_ref().filter(|s|s.enabled).map(|s|s.champion.as_str()),"strategy_snapshots":strategy_snapshots,"as_of_ms":now,"fresh_count":fresh,"eligible_count":eligible,"stale_count":stale,"unseen_count":unseen,
        "metadata_preloaded_count":metadata_preloaded_count,"metadata_preloaded_oldest_age_ms":metadata_preloaded_oldest_age_ms,"oldest_fresh_age_ms":oldest,"screen_reasons":reasons,"unavailable":unavailable,"unavailable_count":unavailable_count,"unavailable_truncated":unavailable_count>100,"symbol_mismatch_count":symbol_mismatches,
        "market_median_bps":baseline,"market_sample_count":changes.len(),"presented_count":prospects.len(),
        "max_scan_age_ms":settings.max_scan_age_ms,"prospects":prospects,
        "selection":"eligible fresh samples by absolute retained price change; market median uses all fresh samples before screens/truncation",
        "limitations":"Irregular price samples are screening evidence, never executable depth. Missing metadata is unknown."});
    if let Some(live) = live {
        projection["live_ranking"] = json!({
            "sha256": live.sha256,
            "name": live.name,
            "scored_count": live.scores.len(),
            "paper_only": true
        });
        projection["selection"] = json!(
            "selected strategy scorer scores, descending; unscored fresh samples are omitted; validation still decides"
        );
    }
    projection
}
