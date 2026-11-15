//! Deterministic append-only producer for every enabled champion/shadow scan.
//! Original paper books and journals retain their independent authority.
use super::{Result, discovery};
use sadr_core::{execution::Scanner, strategy::ScanSnapshot};
use sadr_store::strategy_results::{Evidence, EvidenceKind, ResultsDb, proposal_hash};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub limits: Limits,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Limits {
    pub max_scan_rows: usize,
    pub chunk_bytes: usize,
    pub max_batch_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_scan_rows: 20_000,
            chunk_bytes: 196_608,
            max_batch_bytes: 67_108_864,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if !(1..=20_000).contains(&self.limits.max_scan_rows)
            || !(4096..=262_144).contains(&self.limits.chunk_bytes)
            || !(262_144..=134_217_728).contains(&self.limits.max_batch_bytes)
        {
            return Err("strategy_results_limits_invalid".into());
        }
        Ok(())
    }
}
pub(super) fn record_results(
    settings: &discovery::Settings,
    projection: &Value,
    scanner: &Scanner,
    session_binding: &str,
) -> Result<()> {
    let Some(strategy) = settings.strategy.as_ref().filter(|s| s.enabled) else {
        return Ok(());
    };
    strategy.validate()?;
    let producer = settings.strategy_results.clone().unwrap_or_default();
    producer.validate()?;
    if session_binding.len() != 64 || !session_binding.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("strategy_results_session_identity_invalid".into());
    }
    let values = projection["strategy_snapshots"]
        .as_array()
        .ok_or("strategy_results_snapshots_missing")?;
    if values.len() != strategy.specs.len() || !projection["strategy_error"].is_null() {
        return Err("strategy_results_evaluation_incomplete".into());
    }
    let mut records = Vec::new();
    let mut batch_bytes = 0usize;
    let now = projection["as_of_ms"]
        .as_i64()
        .ok_or("strategy_results_snapshot_invalid")?;
    let champion = strategy
        .specs
        .iter()
        .find(|s| s.id == strategy.champion)
        .ok_or("strategy_champion_missing")?;
    let keys: std::collections::BTreeSet<_> = values
        .iter()
        .flat_map(|v| v["rows"].as_array().into_iter().flatten())
        .filter_map(|row| row["key"].as_str())
        .collect();
    if keys.len() > producer.limits.max_scan_rows {
        return Err("strategy_results_universe_too_large".into());
    }
    let hashes: Vec<_> = strategy
        .specs
        .iter()
        .map(proposal_hash)
        .collect::<std::result::Result<_, _>>()?;
    let mut quote_chunks = Vec::new();
    let mut current = Vec::new();
    let mut bytes = 2048usize;
    for key in keys {
        let sample = scanner.samples.get(key);
        if sample.is_some_and(|s| s.history.len() > scanner.settings.history_max_samples) {
            return Err("strategy_results_sample_invalid".into());
        }
        let row = json!({"key":key,"sample":sample});
        let cost = serde_json::to_vec(&row)?.len() + 1;
        if cost + 2048 > producer.limits.chunk_bytes {
            return Err("strategy_results_row_too_large".into());
        }
        if bytes + cost > producer.limits.chunk_bytes {
            quote_chunks.push(std::mem::take(&mut current));
            bytes = 2048;
        }
        bytes += cost;
        current.push(row);
    }
    if !current.is_empty() {
        quote_chunks.push(current);
    }
    let mut quote_chunk_ids = Vec::new();
    for (index, rows) in quote_chunks.into_iter().enumerate() {
        let id = format!("{session_binding}:quotes:{now}:{index}");
        let rows_hash = sadr_core::replay::digest(&serde_json::to_vec(&rows)?);
        let body = json!({"format":"strategy_cache_chunk_v1","session_binding":session_binding,"observed_ms":now,"chunk_index":index,"rows_hash":rows_hash,"spec_hashes":hashes,"label":"Rank-tick scanner cache snapshots. Original sample seen/source/metadata receipts are retained. LTPC is not executable depth; intervening WS messages are not reconstructed.","rows":rows});
        let size = serde_json::to_vec(&body)?.len();
        if size > producer.limits.chunk_bytes {
            return Err("strategy_results_chunk_too_large".into());
        }
        batch_bytes = batch_bytes
            .checked_add(size)
            .ok_or("strategy_results_batch_too_large")?;
        quote_chunk_ids.push(id.clone());
        records.push(Evidence {
            id,
            kind: EvidenceKind::Quote,
            observed_ms: now,
            spec_id: format!("{}:{}", champion.id, champion.version),
            scan_id: None,
            digest_id: None,
            decide_id: None,
            body,
        });
    }
    for spec in &strategy.specs {
        let matching: Vec<_> = values
            .iter()
            .filter(|v| v["spec_id"].as_str() == Some(spec.id.as_str()))
            .collect();
        if matching.len() != 1 {
            return Err("strategy_results_spec_snapshot_missing".into());
        }
        let mut scan: ScanSnapshot = serde_json::from_value(matching[0].clone())
            .map_err(|_| "strategy_results_snapshot_invalid")?;
        let original_id = scan.scan_id.clone();
        if scan.spec_hash != proposal_hash(spec)?
            || scan.spec_version != spec.version
            || scan.rows.len() > producer.limits.max_scan_rows
            || projection["as_of_ms"].as_i64() != Some(scan.observed_ms)
        {
            return Err("strategy_results_snapshot_binding_mismatch".into());
        }
        scan.seal()?;
        if scan.scan_id != original_id {
            return Err("strategy_results_snapshot_hash_mismatch".into());
        }
        let prefix = format!("{session_binding}:scan:{}", scan.scan_id);
        let body_hash = sadr_core::replay::digest(&serde_json::to_vec(&scan)?);
        let mut header = serde_json::to_value(&scan)?;
        header
            .as_object_mut()
            .ok_or("strategy_results_snapshot_invalid")?
            .remove("rows");
        let mut chunks: Vec<Vec<Value>> = Vec::new();
        let mut current = Vec::new();
        let mut bytes = 2048usize;
        for row in &scan.rows {
            let value = serde_json::to_value(row)?;
            let cost = serde_json::to_vec(&value)?.len() + 1;
            if cost + 2048 > producer.limits.chunk_bytes {
                return Err("strategy_results_row_too_large".into());
            }
            if bytes + cost > producer.limits.chunk_bytes {
                chunks.push(std::mem::take(&mut current));
                bytes = 2048;
            }
            bytes += cost;
            current.push(value);
        }
        if !current.is_empty() {
            chunks.push(current);
        }
        let mut chunk_ids = Vec::new();
        let mut row_start = 0usize;
        for (index, rows) in chunks.into_iter().enumerate() {
            let id = format!("{prefix}:chunk:{index}");
            let chunk_hash = sadr_core::replay::digest(&serde_json::to_vec(&rows)?);
            let count = rows.len();
            let body = json!({"format":"strategy_scan_chunk_v1","session_binding":session_binding,"scan_id":scan.scan_id,"spec_hash":scan.spec_hash,"chunk_index":index,"row_start":row_start,"rows_hash":chunk_hash,"rows":rows});
            row_start += count;
            let size = serde_json::to_vec(&body)?.len();
            if size > producer.limits.chunk_bytes {
                return Err("strategy_results_chunk_too_large".into());
            }
            batch_bytes = batch_bytes
                .checked_add(size)
                .ok_or("strategy_results_batch_too_large")?;
            chunk_ids.push(id.clone());
            records.push(Evidence {
                id,
                kind: EvidenceKind::ScanChunk,
                observed_ms: scan.observed_ms,
                spec_id: format!("{}:{}", spec.id, spec.version),
                scan_id: Some(scan.scan_id.clone()),
                digest_id: None,
                decide_id: None,
                body,
            });
        }
        let body = json!({"format":"strategy_scan_manifest_v1","session_binding":session_binding,"scan_id":scan.scan_id,"spec_hash":scan.spec_hash,"snapshot_header":header,"snapshot_body_hash":body_hash,"row_count":scan.rows.len(),"selected_count":scan.rows.iter().filter(|r|r.selected).count(),"chunk_ids":chunk_ids,"quote_chunk_ids":quote_chunk_ids});
        batch_bytes = batch_bytes
            .checked_add(serde_json::to_vec(&body)?.len())
            .ok_or("strategy_results_batch_too_large")?;
        records.push(Evidence {
            id: prefix,
            kind: EvidenceKind::Scan,
            observed_ms: scan.observed_ms,
            spec_id: format!("{}:{}", spec.id, spec.version),
            scan_id: Some(scan.scan_id.clone()),
            digest_id: None,
            decide_id: None,
            body,
        });
        if batch_bytes > producer.limits.max_batch_bytes {
            return Err("strategy_results_batch_too_large".into());
        }
    }
    let db = ResultsDb::open(
        settings
            .strategy_results_database
            .as_deref()
            .ok_or("strategy_results_database_required")?,
    )?;
    for spec in &strategy.specs {
        db.register_spec(spec)?;
    }
    db.record_batch(&records)?;
    Ok(())
}

/// Mirror immutable plan/fill facts after execution and before the next model event.
/// Carried/legacy decisions without attribution remain explicitly unknown.
pub(super) fn record_pipeline(
    settings: &discovery::Settings,
    c: &super::Config,
    portfolio: &sadr_core::execution::Portfolio,
    plan: Option<&Value>,
) -> Result<()> {
    if !settings.strategy.as_ref().is_some_and(|s| s.enabled) {
        return Ok(());
    }
    let producer = settings.strategy_results.clone().unwrap_or_default();
    producer.validate()?;
    let binding = super::book_binding(c)?;
    let db = ResultsDb::open(
        settings
            .strategy_results_database
            .as_deref()
            .ok_or("strategy_results_database_required")?,
    )?;
    for spec in &settings
        .strategy
        .as_ref()
        .ok_or("strategy_results_spec_missing")?
        .specs
    {
        db.register_spec(spec)?;
    }
    let mut records = Vec::new();
    if let Some(plan) = plan {
        super::planner::validate_saved(c, plan)?;
        let decision = plan["decision_id"]
            .as_str()
            .ok_or("strategy_results_decision_invalid")?;
        let attr = &plan["strategy_attribution"];
        let digest_id = plan["digest_attribution"]["digest_id"].as_str();
        let observed = plan["input"]["discovery"]["as_of_ms"].as_i64().unwrap_or(0);
        if attr.is_null() || digest_id.is_none() {
            records.push(Evidence{id:format!("{binding}:unknown-plan:{decision}"),kind:EvidenceKind::Failure,observed_ms:observed,spec_id:"unknown:legacy".into(),scan_id:None,digest_id:None,decide_id:Some(decision.into()),body:json!({"attribution_status":"unknown_legacy_plan","decision_id":decision,"reference":format!("model/{decision}/plan.json")})});
        } else {
            if super::discovery::attribution(&plan["input"]["discovery"]) != *attr
                || plan["digest_attribution"]["strategy"] != *attr
            {
                return Err("strategy_results_plan_attribution_mismatch".into());
            }
            let scan_id = attr["scan_id"]
                .as_str()
                .ok_or("strategy_results_plan_attribution_missing")?;
            let spec_hash = attr["spec_hash"]
                .as_str()
                .ok_or("strategy_results_plan_attribution_missing")?;
            let spec_id = format!(
                "{}:{}",
                attr["spec_id"]
                    .as_str()
                    .ok_or("strategy_results_plan_attribution_missing")?,
                attr["spec_version"]
                    .as_u64()
                    .ok_or("strategy_results_plan_attribution_missing")?
            );
            let digest_id = digest_id.ok_or("strategy_results_digest_missing")?;
            let frozen: Vec<_> = plan["input"]["discovery"]["strategy_snapshots"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|s| {
                    s["scan_id"].as_str() == Some(scan_id)
                        && s["spec_hash"].as_str() == Some(spec_hash)
                })
                .collect();
            if frozen.len() != 1 {
                return Err("strategy_results_frozen_scan_missing".into());
            }
            records.extend(frozen_scan_records(frozen[0], &binding, &producer)?);
            let actual_decide = plan["luna_handoff_requested"].as_bool() != Some(false);
            let make = |id: String, kind, body| Evidence {
                id,
                kind,
                observed_ms: observed,
                spec_id: spec_id.clone(),
                scan_id: Some(scan_id.into()),
                digest_id: Some(digest_id.into()),
                decide_id: actual_decide.then(|| decision.into()),
                body,
            };
            let verdicts: Vec<super::planner::Verdict> =
                serde_json::from_value(plan["verdicts"].clone())
                    .map_err(|_| "strategy_results_verdict_invalid")?;
            let verdict_value = serde_json::to_value(verdicts)?;
            let mut body = json!({"spec_hash":spec_hash,"decision_id":decision,"strategy_attribution":attr,"digest_attribution":plan["digest_attribution"],"reference":format!("model/{decision}/plan.json"),"raw_output_hash":sadr_core::replay::digest(&serde_json::to_vec(&plan["raw_output"])?),"parsed_plan_hash":sadr_core::replay::digest(&serde_json::to_vec(&plan["parsed_plan"])?),"verdicts_hash":sadr_core::replay::digest(&serde_json::to_vec(&verdict_value)?)});
            if serde_json::to_vec(&verdict_value)?.len() + 2048 <= producer.limits.chunk_bytes {
                body["verdicts"] = verdict_value;
            }
            records.push(make(
                if actual_decide {
                    format!("{binding}:decide:{decision}")
                } else {
                    format!("{binding}:wait:{decision}")
                },
                if actual_decide {
                    EvidenceKind::Decide
                } else {
                    EvidenceKind::Handoff
                },
                body,
            ));
            // Bounded allowlisted audit metadata; raw inputs/provider envelopes are excluded.
            let audit_path = c
                .directory
                .join("model")
                .join(decision)
                .join("luna-review.json");
            use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt};
            let audit = match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(audit_path)
            {
                Ok(file) => {
                    if !file.metadata()?.is_file() || file.metadata()?.len() > 2_000_000 {
                        return Err("strategy_results_digest_audit_invalid".into());
                    }
                    let mut bytes = Vec::new();
                    file.take(2_000_001).read_to_end(&mut bytes)?;
                    if bytes.len() > 2_000_000 {
                        return Err("strategy_results_digest_audit_invalid".into());
                    }
                    let value: Value = serde_json::from_slice(&bytes)
                        .map_err(|_| "strategy_results_digest_audit_invalid")?;
                    if value["session_binding"] != binding
                        || value["digest_id"] != digest_id
                        || value["strategy_attribution"] != *attr
                    {
                        return Err("strategy_results_digest_binding_mismatch".into());
                    }
                    Some((value, sadr_core::replay::digest(&bytes)))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err("strategy_results_digest_audit_invalid".into()),
            };
            let digest_body = json!({"spec_hash":spec_hash,"digest_id":digest_id,"reference":format!("model/{decision}/luna-review.json"),"availability":if audit.is_some(){"recorded"}else{"missing"},"artifact_hash":audit.as_ref().map(|(_,hash)|hash)});
            records.push(make(
                format!("{binding}:digest:{decision}"),
                EvidenceKind::Digest,
                digest_body,
            ));
            if let Some((audit, _)) = audit {
                records.push(make(format!("{binding}:gate:{decision}"),EvidenceKind::Gate,json!({"spec_hash":spec_hash,"digest_id":digest_id,"luna_decision":audit["luna_decision"].as_str(),"handoff_requested":audit["handoff_requested"].as_bool(),"gate":audit["gate"].as_str(),"reference":format!("model/{decision}/luna-review.json")})));
            }
        }
    }
    // Persist the plan first in the same append-only batch used for its matching fills.
    // Older exact decisions are looked up from this dedicated ledger, never inferred.
    for (index, fill) in portfolio.fills.iter().enumerate() {
        let prior = if let Some(id) = &fill.decision_id {
            let key = format!("{binding}:decide:{id}");
            records
                .iter()
                .find(|r| r.id == key)
                .cloned()
                .or(db.record_by_id(&key, &EvidenceKind::Decide)?)
        } else {
            None
        };
        let row_id = format!("{binding}:fill:{index}");
        if let Some(attribution) = prior {
            records.push(Evidence{id:row_id,kind:EvidenceKind::Fill,observed_ms:fill.at,spec_id:attribution.spec_id,scan_id:attribution.scan_id,digest_id:attribution.digest_id,decide_id:attribution.decide_id,body:json!({"spec_hash":attribution.body["spec_hash"],"fill_index":index,"fill":fill})});
        } else {
            records.push(Evidence{id:format!("{binding}:unknown-fill:{index}"),kind:EvidenceKind::Failure,observed_ms:fill.at,spec_id:"unknown:legacy".into(),scan_id:None,digest_id:None,decide_id:fill.decision_id.clone(),body:json!({"attribution_status":"unknown_legacy_or_carried_fill","fill_index":index,"fill":fill})});
        }
    }
    // Position marks evolve; content hashes identify snapshots without replacing prior marks.
    for position in portfolio.positions.values() {
        let Some(decision) = &position.decision_id else {
            let value = serde_json::to_value(position)?;
            let hash = sadr_core::replay::digest(&serde_json::to_vec(&value)?);
            records.push(Evidence{id:format!("{binding}:unknown-position:{hash}"),kind:EvidenceKind::Failure,observed_ms:position.marked_at,spec_id:"unknown:legacy".into(),scan_id:None,digest_id:None,decide_id:None,body:json!({"attribution_status":"unknown_legacy_or_carried_position","position":value})});
            continue;
        };
        let key = format!("{binding}:decide:{decision}");
        let prior = records
            .iter()
            .find(|r| r.id == key)
            .cloned()
            .or(db.record_by_id(&key, &EvidenceKind::Decide)?);
        if let Some(attribution) = prior {
            let value = serde_json::to_value(position)?;
            let hash = sadr_core::replay::digest(&serde_json::to_vec(&value)?);
            records.push(Evidence {
                id: format!("{binding}:position:{hash}"),
                kind: EvidenceKind::Position,
                observed_ms: position.marked_at,
                spec_id: attribution.spec_id,
                scan_id: attribution.scan_id,
                digest_id: attribution.digest_id,
                decide_id: attribution.decide_id,
                body: json!({"spec_hash":attribution.body["spec_hash"],"position":value}),
            });
        } else {
            let value = serde_json::to_value(position)?;
            let hash = sadr_core::replay::digest(&serde_json::to_vec(&value)?);
            records.push(Evidence{id:format!("{binding}:unknown-position:{hash}"),kind:EvidenceKind::Failure,observed_ms:position.marked_at,spec_id:"unknown:legacy".into(),scan_id:None,digest_id:None,decide_id:Some(decision.clone()),body:json!({"attribution_status":"unknown_legacy_or_carried_position","position":value})});
        }
    }
    let size: usize = records
        .iter()
        .map(|r| serde_json::to_vec(&r.body).map(|v| v.len()))
        .collect::<std::result::Result<Vec<_>, _>>()?
        .iter()
        .sum();
    if size > producer.limits.max_batch_bytes {
        return Err("strategy_results_batch_too_large".into());
    }
    if !records.is_empty() {
        db.record_batch_idempotent(&records)?;
    }
    Ok(())
}

/// Preserve the exact model input snapshot, independently of nearby rank ticks.
fn frozen_scan_records(value: &Value, binding: &str, settings: &Settings) -> Result<Vec<Evidence>> {
    let mut scan: ScanSnapshot = serde_json::from_value(value.clone())
        .map_err(|_| "strategy_results_frozen_scan_invalid")?;
    let original = scan.scan_id.clone();
    scan.seal()?;
    if scan.scan_id != original || scan.rows.len() > settings.limits.max_scan_rows {
        return Err("strategy_results_frozen_scan_invalid".into());
    }
    let hash = sadr_core::replay::digest(&serde_json::to_vec(&scan)?);
    let mut header = serde_json::to_value(&scan)?;
    header
        .as_object_mut()
        .ok_or("strategy_results_frozen_scan_invalid")?
        .remove("rows");
    let prefix = format!("{binding}:input-scan:{}", scan.scan_id);
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut size = 2048usize;
    for row in &scan.rows {
        let value = serde_json::to_value(row)?;
        let cost = serde_json::to_vec(&value)?.len() + 1;
        if cost + 2048 > settings.limits.chunk_bytes {
            return Err("strategy_results_row_too_large".into());
        }
        if size + cost > settings.limits.chunk_bytes {
            chunks.push(std::mem::take(&mut current));
            size = 2048;
        }
        size += cost;
        current.push(value);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    let mut records = Vec::new();
    let mut ids = Vec::new();
    let mut row_start = 0usize;
    let make = |id, kind, body| Evidence {
        id,
        kind,
        observed_ms: scan.observed_ms,
        spec_id: format!("{}:{}", scan.spec_id, scan.spec_version),
        scan_id: Some(scan.scan_id.clone()),
        digest_id: None,
        decide_id: None,
        body,
    };
    for (index, rows) in chunks.into_iter().enumerate() {
        let id = format!("{prefix}:chunk:{index}");
        let rows_hash = sadr_core::replay::digest(&serde_json::to_vec(&rows)?);
        let body = json!({"format":"strategy_scan_chunk_v1","session_binding":binding,"scan_id":scan.scan_id,"spec_hash":scan.spec_hash,"chunk_index":index,"row_start":row_start,"rows_hash":rows_hash,"rows":rows});
        row_start += body["rows"]
            .as_array()
            .ok_or("strategy_results_frozen_scan_invalid")?
            .len();
        if serde_json::to_vec(&body)?.len() > settings.limits.chunk_bytes {
            return Err("strategy_results_chunk_too_large".into());
        }
        ids.push(id.clone());
        records.push(make(id, EvidenceKind::ScanChunk, body));
    }
    records.push(make(prefix,EvidenceKind::Scan,json!({"format":"strategy_scan_manifest_v1","source":"frozen_model_input","session_binding":binding,"scan_id":scan.scan_id,"spec_hash":scan.spec_hash,"snapshot_header":header,"snapshot_body_hash":hash,"row_count":scan.rows.len(),"selected_count":scan.rows.iter().filter(|r|r.selected).count(),"chunk_ids":ids})));
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sadr_core::strategy::{ScanRow, StrategySpec};
    use sadr_store::strategy_results::Query;
    #[test]
    fn quiet_shadow_chunks_reconstruct_and_keep_original_cache_receipts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("results.sqlite");
        let spec = StrategySpec::default();
        let mut shadow = spec.clone();
        shadow.id = "quiet_shadow".into();
        let mut settings:discovery::Settings=serde_json::from_value(json!({"max_scan_age_ms":2000,"max_prospects":10,"strategy":{"enabled":true,"champion":spec.id,"specs":[spec,shadow]},"strategy_results_database":path,"strategy_results":{"limits":{"chunk_bytes":4096}}})).unwrap();
        let mut scanner = Scanner::default();
        let rows: Vec<_> = (0..100)
            .map(|i| {
                let key = format!("fixture-{i}");
                scanner.samples.insert(
                    key.clone(),
                    sadr_core::execution::Sample {
                        seen: 123,
                        source: Some(120),
                        market_fields_receipt: Some(110),
                        price_kind: Some("ltpc".into()),
                        fresh_for_ms: Some(2000),
                        ..Default::default()
                    },
                );
                ScanRow {
                    key,
                    features: Default::default(),
                    score: None,
                    reason: "unavailable_or_stale".into(),
                    selected: false,
                    trigger_eligible: Some(false),
                }
            })
            .collect();
        let scans: Vec<_> = settings
            .strategy
            .as_ref()
            .unwrap()
            .specs
            .iter()
            .map(|spec| {
                let mut scan = ScanSnapshot {
                    scan_id: String::new(),
                    scorer_id: None,
                    scorer_source_hash: None,
                    scorer_failure: None,
                    spec_hash: proposal_hash(spec).unwrap(),
                    spec_id: spec.id.clone(),
                    spec_version: 1,
                    observed_ms: 1000,
                    rows: rows.clone(),
                };
                scan.seal().unwrap();
                scan
            })
            .collect();
        let projection = json!({"as_of_ms":1000,"strategy_snapshots":scans});
        record_results(&settings, &projection, &scanner, &"a".repeat(64)).unwrap();
        let db = ResultsDb::open_read_only(&path).unwrap();
        let query = Query {
            kind: EvidenceKind::Scan,
            from_ms: 0,
            to_ms: 2000,
            after_id: None,
            limit: 100,
            spec_id: None,
            instrument_key: None,
        };
        let manifests = db.query(&query).unwrap();
        assert_eq!(manifests.len(), 2);
        let chunks = db
            .query(&Query {
                kind: EvidenceKind::ScanChunk,
                ..query.clone()
            })
            .unwrap();
        for manifest in &manifests {
            assert_eq!(manifest.body["row_count"], 100);
            assert_eq!(manifest.body["selected_count"], 0);
            let mut restored = manifest.body["snapshot_header"].clone();
            let mut restored_rows = Vec::new();
            for id in manifest.body["chunk_ids"].as_array().unwrap() {
                let chunk = chunks
                    .iter()
                    .find(|c| c.id == id.as_str().unwrap())
                    .unwrap();
                assert_eq!(
                    sadr_core::replay::digest(&serde_json::to_vec(&chunk.body["rows"]).unwrap()),
                    chunk.body["rows_hash"].as_str().unwrap()
                );
                restored_rows.extend(chunk.body["rows"].as_array().unwrap().clone());
            }
            restored["rows"] = json!(restored_rows);
            let restored: ScanSnapshot = serde_json::from_value(restored).unwrap();
            assert_eq!(
                sadr_core::replay::digest(&serde_json::to_vec(&restored).unwrap()),
                manifest.body["snapshot_body_hash"].as_str().unwrap()
            );
        }
        let quotes = db
            .query(&Query {
                kind: EvidenceKind::Quote,
                ..query.clone()
            })
            .unwrap();
        assert!(quotes.len() > 1);
        for row in quotes
            .iter()
            .flat_map(|c| c.body["rows"].as_array().unwrap())
        {
            assert_eq!(row["sample"]["seen"], 123);
            assert_eq!(row["sample"]["source"], 120);
            assert_eq!(row["sample"]["market_fields_receipt"], 110);
            assert_eq!(row["sample"]["price_kind"], "ltpc");
        }
        assert!(record_results(&settings, &projection, &scanner, &"a".repeat(64)).is_err());
        assert_eq!(db.query(&query).unwrap().len(), 2);
        let frozen = frozen_scan_records(
            &projection["strategy_snapshots"][0],
            &"a".repeat(64),
            settings.strategy_results.as_ref().unwrap(),
        )
        .unwrap();
        let manifest = frozen
            .iter()
            .find(|r| matches!(r.kind, EvidenceKind::Scan))
            .unwrap();
        assert_eq!(manifest.body["source"], "frozen_model_input");
        assert_eq!(
            manifest.scan_id.as_deref(),
            projection["strategy_snapshots"][0]["scan_id"].as_str()
        );
        let restored_rows: Vec<_> = frozen
            .iter()
            .filter(|r| matches!(r.kind, EvidenceKind::ScanChunk))
            .flat_map(|r| r.body["rows"].as_array().unwrap().clone())
            .collect();
        let mut restored = manifest.body["snapshot_header"].clone();
        restored["rows"] = json!(restored_rows);
        let restored: ScanSnapshot = serde_json::from_value(restored).unwrap();
        assert_eq!(
            sadr_core::replay::digest(&serde_json::to_vec(&restored).unwrap()),
            manifest.body["snapshot_body_hash"].as_str().unwrap()
        );
        let writer = ResultsDb::open(&path).unwrap();
        writer.record_batch_idempotent(&frozen).unwrap();
        writer.record_batch_idempotent(&frozen).unwrap();
        settings.strategy.as_mut().unwrap().enabled = false;
        settings.strategy_results_database = None;
        record_results(&settings, &Value::Null, &Scanner::default(), "").unwrap();
    }
}
