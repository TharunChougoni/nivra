//! Offline adapters. All source books and journals remain read-only.
use rusqlite::{Connection, OpenFlags, params};
use sadr_core::{
    execution::Scanner,
    historical::{Event, EventKind, Manifest},
    strategy::{FeatureSnapshot, extract_features_with_lookback},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

fn rejected() -> String {
    "strategy_source_rejected".into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaperManifest {
    #[serde(default)]
    paper_costs: Option<sadr_core::finance::FeeArithmetic>,
    schema: u32,
    session: chrono::NaiveDate,
    previous_session: chrono::NaiveDate,
    capital: sadr_core::finance::Amount,
    universe: Vec<sadr_core::execution::Instrument>,
    events_sha256: String,
    synthetic: bool,
    max_observation_gap_ms: i64,
    history_directory: Option<std::path::PathBuf>,
    provenance: Value,
}

/// Accept each producer's actual contract; paper captures have no historical
/// engine/calendar bindings and must never acquire invented ones during import.
type ArchiveIdentity = (
    chrono::NaiveDate,
    chrono::NaiveDate,
    Vec<sadr_core::execution::Instrument>,
);
pub(crate) fn archive_identity(manifest: &[u8], events: &[u8]) -> Result<ArchiveIdentity, String> {
    let value: Value = serde_json::from_slice(manifest).map_err(|_| rejected())?;
    if value.get("schema").is_some() {
        let paper: PaperManifest = serde_json::from_value(value).map_err(|_| rejected())?;
        if paper.schema != 1
            || paper
                .paper_costs
                .as_ref()
                .is_some_and(|costs| costs.validate().is_err())
            || paper.previous_session >= paper.session
            || paper.capital <= sadr_core::finance::Amount::ZERO
            || paper.max_observation_gap_ms <= 0
            || paper.universe.is_empty()
            || paper.universe.len() > 10_000
            || !paper.provenance.is_object()
        {
            return Err(rejected());
        }
        if paper.events_sha256 != sadr_core::replay::digest(events) {
            return Err("strategy_archive_binding_mismatch".into());
        }
        if paper
            .provenance
            .get("legacy_quote_purpose_unknown")
            .is_some_and(|count| count.as_u64() != Some(0))
        {
            return Err("strategy_archive_screening_purpose_unknown".into());
        }
        // Source labels are parsed, never used to read files or claim genuine
        // market data. The caller retains the original manifest alongside output.
        let _source_labels = (paper.synthetic, paper.history_directory);
        let mut keys = BTreeSet::new();
        for instrument in &paper.universe {
            if instrument.key.is_empty()
                || instrument.key.len() > 128
                || instrument.symbol.is_empty()
                || instrument.symbol.len() > 64
                || instrument.series != "EQ"
                || !keys.insert(&instrument.key)
                || sadr_core::finance::CashOrderGrid::new(1, instrument.tick).is_err()
            {
                return Err(rejected());
            }
        }
        Ok((paper.previous_session, paper.session, paper.universe))
    } else {
        let historical: Manifest = serde_json::from_value(value).map_err(|_| rejected())?;
        if historical.bindings.data != sadr_core::replay::digest(events)
            || historical.bindings.engine != sadr_core::replay::engine_fingerprint()
        {
            return Err("strategy_archive_binding_mismatch".into());
        }
        sadr_core::historical::Runner::new(historical.clone()).map_err(|_| rejected())?;
        Ok((
            historical.warmup_start,
            historical.session,
            historical.universe,
        ))
    }
}

/// Validate the exact manifest/events binding without exporting features.
/// This is the same `archive_identity` check used by every archive consumer.
pub fn validate_archive(manifest: &[u8], events: &[u8]) -> Result<(), String> {
    archive_identity(manifest, events).map(|_| ())
}

/// Rebuild the feature cache from the existing paper-replay export format.
/// Execution/protection observations cannot masquerade as screening observations.
/// This evaluates rankings, never simulates orders or claims executable returns.
pub fn export_features(
    manifest: &[u8],
    events: &[u8],
    max_age_ms: i64,
) -> Result<Vec<FeatureSnapshot>, String> {
    export_features_with_lookback(manifest, events, max_age_ms, 20)
}
pub fn export_features_with_lookback(
    manifest: &[u8],
    events: &[u8],
    max_age_ms: i64,
    lookback_samples: usize,
) -> Result<Vec<FeatureSnapshot>, String> {
    export_features_configured(
        manifest,
        events,
        max_age_ms,
        lookback_samples,
        &ArchiveConfig::default(),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArchiveConfig {
    pub max_archive_bytes: u64,
    pub max_observations: usize,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
}
impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            max_archive_bytes: 32 * 1024 * 1024,
            max_observations: 100_000,
            from_ms: None,
            to_ms: None,
        }
    }
}
impl ArchiveConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=128 * 1024 * 1024).contains(&self.max_archive_bytes)
            || !(1..=1_000_000).contains(&self.max_observations)
            || self
                .from_ms
                .zip(self.to_ms)
                .is_some_and(|(from, to)| from > to)
        {
            return Err("strategy_archive_config_invalid".into());
        }
        Ok(())
    }
}
pub fn export_features_configured(
    manifest: &[u8],
    events: &[u8],
    max_age_ms: i64,
    lookback_samples: usize,
    config: &ArchiveConfig,
) -> Result<Vec<FeatureSnapshot>, String> {
    config.validate()?;
    if manifest.len() > sadr_core::replay::MAX_MANIFEST
        || events.len() as u64 > config.max_archive_bytes
        || !(1..=300_000).contains(&max_age_ms)
        || !(1..=20).contains(&lookback_samples)
    {
        return Err(rejected());
    }
    let (first_date, last_date, universe) = archive_identity(manifest, events)?;
    let instruments: std::collections::BTreeMap<_, _> = universe
        .iter()
        .map(|instrument| (instrument.key.as_str(), instrument))
        .collect();
    let mut scanner = Scanner::default();
    let mut last = None;
    let mut snapshots = Vec::new();
    let mut count = 0usize;
    for raw in events.split(|b| *b == b'\n').filter(|row| !row.is_empty()) {
        if raw.len() > 128 * 1024 {
            return Err(rejected());
        }
        let event: Event = serde_json::from_slice(raw).map_err(|_| rejected())?;
        let date = chrono::DateTime::from_timestamp_millis(event.at)
            .ok_or_else(rejected)?
            .with_timezone(&chrono::FixedOffset::east_opt(19_800).ok_or_else(rejected)?)
            .date_naive();
        if last.is_some_and(|at| event.at <= at)
            || event.available_at > event.at
            || event.observations.len() > 200
            || date < first_date
            || date > last_date
        {
            return Err("strategy_archive_chronology".into());
        }
        last = Some(event.at);
        let mut keys = BTreeSet::new();
        for observation in &event.observations {
            count += 1;
            if count > config.max_observations
                || !keys.insert(&observation.key)
                || observation.quote.receipt > event.available_at
                || observation.quote.source > observation.quote.receipt
            {
                return Err(rejected());
            }
            let instrument = instruments
                .get(observation.key.as_str())
                .ok_or_else(rejected)?;
            let grid = sadr_core::finance::CashOrderGrid::new(1, instrument.tick)
                .map_err(|_| rejected())?;
            if observation.quote.bid > observation.quote.ask
                || grid.validate(1, observation.quote.bid).is_err()
                || grid.validate(1, observation.quote.ask).is_err()
            {
                return Err("strategy_archive_quote_invalid".into());
            }
            if matches!(event.kind, EventKind::Screen) {
                scanner
                    .observe_fields(
                        event.at,
                        instrument,
                        &observation.quote,
                        &observation.fields(),
                    )
                    .map_err(|_| rejected())?;
            }
        }
        // Validate the entire source and warm the cache before the requested
        // window. Windowing never bypasses late corruption or hash checks.
        if matches!(event.kind, EventKind::Screen)
            && config.from_ms.is_none_or(|from| event.at >= from)
            && config.to_ms.is_none_or(|to| event.at <= to)
        {
            if snapshots.len() >= 10_000 {
                return Err(rejected());
            }
            snapshots.push(extract_features_with_lookback(
                &scanner,
                event.at,
                max_age_ms,
                max_age_ms,
                lookback_samples,
            )?);
        }
    }
    if snapshots.is_empty() {
        return Err("strategy_no_screening_evidence".into());
    }
    Ok(snapshots)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Quotes,
    Scanner,
    Portfolio,
    Events,
    Attempts,
    Plans,
    Actions,
    ModelAudit,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceQuery {
    pub kind: SourceKind,
    pub from_ms: i64,
    pub to_ms: i64,
    #[serde(default)]
    pub after_rowid: i64,
    pub limit: usize,
    #[serde(default)]
    pub instrument_key: Option<String>,
    #[serde(default)]
    pub audit_id: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct SourcePage {
    pub rows: Vec<Value>,
    pub next_rowid: i64,
    pub truncated: bool,
    pub limitations: &'static str,
}

/// Fixed SQL only: the caller chooses a category/window/key, never SQL or a filename.
/// `session` is operator-selected, outside the model's tool arguments.
pub fn read_source(
    session: &Path,
    q: &SourceQuery,
    budget_bytes: usize,
) -> Result<SourcePage, String> {
    if q.from_ms > q.to_ms
        || q.after_rowid < 0
        || !(1..=100).contains(&q.limit)
        || !(1024..=1_000_000).contains(&budget_bytes)
        || q.instrument_key
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 256)
    {
        return Err(rejected());
    }
    if matches!(q.kind, SourceKind::ModelAudit) {
        return read_model_audit(session, q, budget_bytes);
    }
    let path = session.join("paper.sqlite");
    if std::fs::symlink_metadata(&path)
        .map_err(|_| rejected())?
        .file_type()
        .is_symlink()
    {
        return Err(rejected());
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| rejected())?;
    db.pragma_update(None, "query_only", true)
        .map_err(|_| rejected())?;
    let mut steps = 0usize;
    db.progress_handler(
        1000,
        Some(move || {
            steps += 1;
            steps > 1000
        }),
    )
    .map_err(|_| rejected())?;
    db.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 2_000_000)
        .map_err(|_| rejected())?;
    let mut rows = Vec::new();
    let mut used = 0usize;
    let mut cursor = q.after_rowid;
    let mut truncated = false;
    let sql = match q.kind {
        SourceKind::Quotes => {
            "SELECT rowid,json_object('at',at,'receipt',receipt,'archive',json(body)) FROM ticks WHERE rowid>?1 AND at BETWEEN ?2 AND ?3 AND event='wide_quotes' ORDER BY rowid LIMIT ?4"
        }
        SourceKind::Scanner => {
            "SELECT rowid,body FROM scanner WHERE rowid>?1 AND ?2<=?3 ORDER BY rowid LIMIT ?4"
        }
        SourceKind::Portfolio => {
            "SELECT rowid,body FROM state WHERE rowid>?1 AND ?2<=?3 ORDER BY rowid LIMIT ?4"
        }
        SourceKind::Events => {
            "SELECT rowid,json_object('id',id,'at',at,'state',state,'body',json(body)) FROM planner_event_log WHERE rowid>?1 AND at BETWEEN ?2 AND ?3 ORDER BY rowid LIMIT ?4"
        }
        SourceKind::Attempts => {
            "SELECT a.rowid,json_object('id',a.id,'event_id',a.event_id,'state',a.state,'body',json(a.body),'result',a.result) FROM planner_attempt_log a JOIN planner_event_log e ON e.id=a.event_id WHERE a.rowid>?1 AND e.at BETWEEN ?2 AND ?3 ORDER BY a.rowid LIMIT ?4"
        }
        SourceKind::Plans => {
            "SELECT rowid,body FROM planner_plan WHERE rowid>?1 AND ?2<=?3 ORDER BY rowid LIMIT ?4"
        }
        SourceKind::Actions => {
            "SELECT rowid,json_object('decide_id',decision,'index',idx,'verdict',verdict) FROM planner_action WHERE rowid>?1 AND ?2<=?3 ORDER BY rowid LIMIT ?4"
        }
        SourceKind::ModelAudit => return Err(rejected()),
    };
    let mut statement = db
        .prepare(sql)
        .map_err(|_| "strategy_source_category_unavailable".to_string())?;
    let source = statement
        .query_map(
            params![q.after_rowid, q.from_ms, q.to_ms, (q.limit + 1) as i64],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .map_err(|_| rejected())?;
    for row in source {
        let (id, raw) = row.map_err(|_| rejected())?;
        if rows.len() == q.limit {
            truncated = true;
            break;
        }
        let mut value: Value = serde_json::from_str(&raw).map_err(|_| rejected())?;
        if matches!(q.kind, SourceKind::Quotes) {
            let archive = &value["archive"];
            let body: Value = serde_json::from_str(archive["body"].as_str().ok_or_else(rejected)?)
                .map_err(|_| rejected())?;
            value = json!({"at":value["at"],"receipt":value["receipt"],"purpose":archive["purpose"],"quotes":body["data"]});
            if let Some(key) = &q.instrument_key
                && let Some(data) = value["quotes"].as_object_mut()
            {
                data.retain(|k, v| k == key || v["instrument_token"].as_str() == Some(key));
            }
        } else if matches!(q.kind, SourceKind::Portfolio) {
            let p: sadr_core::execution::Portfolio =
                serde_json::from_value(value).map_err(|_| rejected())?;
            value = json!({"cash":p.cash,"positions":p.positions,"fills":p.fills,"halted":p.halted,"stopped":p.stopped,"closed":p.closed});
            if let Some(key) = &q.instrument_key {
                value["positions"]
                    .as_object_mut()
                    .ok_or_else(rejected)?
                    .retain(|k, _| k == key);
                value["fills"]
                    .as_array_mut()
                    .ok_or_else(rejected)?
                    .retain(|v| v["instrument"]["key"].as_str() == Some(key));
            }
        } else if matches!(q.kind, SourceKind::Scanner)
            && let Some(key) = &q.instrument_key
        {
            value["samples"]
                .as_object_mut()
                .ok_or_else(rejected)?
                .retain(|k, _| k == key);
        }
        // Never expose account/provider envelopes from model inputs.
        redact(&mut value);
        let cost = serde_json::to_vec(&value).map_err(|_| rejected())?.len() + 64;
        if used + cost > budget_bytes {
            truncated = true;
            break;
        }
        used += cost;
        cursor = id;
        rows.push(json!({"rowid":id,"value":value}));
    }
    if truncated && rows.is_empty() {
        return Err("strategy_source_page_too_large".into());
    }
    Ok(SourcePage {
        rows,
        next_rowid: cursor,
        truncated,
        limitations: "Original stored evidence only. Scanner/portfolio/plans/actions are latest or untimed records; their time window cannot be inferred. Missing attribution is unknown. No provider authenticity, executable backtest or P&L attribution is inferred.",
    })
}
fn redact(v: &mut Value) {
    redact_depth(v, 0);
}
fn redact_depth(v: &mut Value, depth: usize) {
    if depth > 64 {
        *v = Value::Null;
        return;
    }
    if let Value::String(text) = v
        && (text.starts_with('{') || text.starts_with('['))
        && let Ok(parsed) = serde_json::from_str::<Value>(text)
    {
        *v = parsed;
    }
    match v {
        Value::Object(m) => {
            m.retain(|k, _| {
                !matches!(
                    k.to_ascii_lowercase().as_str(),
                    "account"
                        | "binding"
                        | "credentials"
                        | "authorization"
                        | "access_token"
                        | "refresh_token"
                        | "token"
                        | "account_id"
                        | "client_secret"
                        | "api_key"
                        | "password"
                        | "secret"
                        | "session_binding"
                        | "account_binding"
                        | "account_hash"
                        | "account_payload"
                        | "api_secret"
                        | "api_token"
                        | "auth"
                        | "headers"
                        | "config"
                        | "configuration"
                        | "broker_config"
                        | "provider_config"
                )
            });
            for value in m.values_mut() {
                redact_depth(value, depth + 1);
            }
        }
        Value::Array(a) => {
            for value in a {
                redact_depth(value, depth + 1)
            }
        }
        _ => {}
    }
}

fn read_model_audit(session: &Path, q: &SourceQuery, budget: usize) -> Result<SourcePage, String> {
    use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt};
    let id = q.audit_id.as_deref().ok_or_else(rejected)?;
    if id.is_empty()
        || id.len() > 256
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(rejected());
    }
    let model = session.join("model");
    let audit = model.join(id);
    for directory in [&model, &audit] {
        if !std::fs::symlink_metadata(directory)
            .map_err(|_| rejected())?
            .file_type()
            .is_dir()
        {
            return Err(rejected());
        }
    }
    let mut rows = Vec::new();
    let mut used = 0usize;
    let mut cursor = q.after_rowid;
    let mut truncated = false;
    for (index, filename) in [
        "input.json",
        "luna-review.json",
        "output.json",
        "plan.json",
        "failure.json",
    ]
    .iter()
    .enumerate()
    {
        let rowid = index as i64 + 1;
        if rowid <= q.after_rowid {
            continue;
        }
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(audit.join(filename))
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                cursor = rowid;
                continue;
            }
            Err(_) => return Err(rejected()),
        };
        let metadata = file.metadata().map_err(|_| rejected())?;
        if !metadata.is_file() || metadata.len() > 2_000_000 {
            return Err(rejected());
        }
        let mut bytes = Vec::new();
        file.take(2_000_001)
            .read_to_end(&mut bytes)
            .map_err(|_| rejected())?;
        if bytes.len() > 2_000_000 {
            return Err(rejected());
        }
        let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| rejected())?;
        // Original model inputs can contain account/config envelopes. Only their
        // frozen discovery evidence is available to this tool.
        if *filename == "input.json" {
            value =
                json!({"input_hash":value["input_hash"],"discovery":value["input"]["discovery"]});
        } else if *filename == "plan.json"
            && let Some(input) = value.get_mut("input")
        {
            *input = json!({"discovery":input["discovery"]});
        }
        let at = value["at_ms"]
            .as_i64()
            .or_else(|| value["discovery"]["as_of_ms"].as_i64())
            .or_else(|| value["input"]["discovery"]["as_of_ms"].as_i64());
        if at.is_some_and(|at| at < q.from_ms || at > q.to_ms) {
            cursor = rowid;
            continue;
        }
        redact(&mut value);
        let row = json!({"rowid":rowid,"audit_id":id,"artifact":filename,"value":value});
        let cost = serde_json::to_vec(&row).map_err(|_| rejected())?.len();
        if rows.len() == q.limit || used + cost > budget {
            truncated = true;
            break;
        }
        used += cost;
        cursor = rowid;
        rows.push(row);
    }
    if rows.is_empty() && truncated {
        return Err("strategy_source_page_too_large".into());
    }
    Ok(SourcePage {
        rows,
        next_rowid: cursor,
        truncated,
        limitations: "Original model audit only. Missing files and untimed artifacts remain unknown. No secrets/configuration envelopes or write authority are exposed.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_preserves_receipts_and_sources_are_read_only() {
        let example = sadr_core::replay::example();
        let mut manifest = example["manifest"].clone();
        let events = example["events"].as_str().unwrap().as_bytes();
        let snapshots =
            export_features(&serde_json::to_vec(&manifest).unwrap(), events, 2000).unwrap();
        assert_eq!(snapshots.len(), 16);
        assert!(
            snapshots.last().unwrap().rows[0]
                .features
                .contains_key(&sadr_core::strategy::Feature::Price)
        );
        let session: chrono::NaiveDate =
            serde_json::from_value(manifest["session"].clone()).unwrap();
        let mut paper = json!({
            "schema":1,"session":session,"previous_session":session.pred_opt().unwrap(),
            "capital":"5000","universe":manifest["universe"],
            "events_sha256":sadr_core::replay::digest(events),"synthetic":true,
            "max_observation_gap_ms":60000,"history_directory":null,
            "provenance":{"legacy_quote_purpose_unknown":0}
        });
        let paper_snapshots =
            export_features(&serde_json::to_vec(&paper).unwrap(), events, 2000).unwrap();
        assert_eq!(
            serde_json::to_value(&snapshots).unwrap(),
            serde_json::to_value(&paper_snapshots).unwrap()
        );
        let mut window = ArchiveConfig {
            max_archive_bytes: 128 * 1024 * 1024,
            max_observations: 1_000_000,
            from_ms: Some(snapshots[5].observed_ms),
            to_ms: Some(snapshots[7].observed_ms),
        };
        let windowed = export_features_configured(
            &serde_json::to_vec(&paper).unwrap(),
            events,
            2000,
            20,
            &window,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&windowed).unwrap(),
            serde_json::to_value(&snapshots[5..8]).unwrap()
        );
        // Even a three-tick output validates/counts all source observations.
        window.max_observations = 8;
        assert!(
            export_features_configured(
                &serde_json::to_vec(&paper).unwrap(),
                events,
                2000,
                20,
                &window
            )
            .is_err()
        );
        window.max_archive_bytes = 129 * 1024 * 1024;
        assert!(window.validate().is_err());
        paper["events_sha256"] = json!("0".repeat(64));
        assert_eq!(
            export_features(&serde_json::to_vec(&paper).unwrap(), events, 2000).unwrap_err(),
            "strategy_archive_binding_mismatch"
        );
        paper["events_sha256"] = json!(sadr_core::replay::digest(events));
        paper["provenance"]["legacy_quote_purpose_unknown"] = json!(1);
        assert_eq!(
            export_features(&serde_json::to_vec(&paper).unwrap(), events, 2000).unwrap_err(),
            "strategy_archive_screening_purpose_unknown"
        );
        let mut rows: Vec<Value> = std::str::from_utf8(events)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        let at = rows[0]["at"].as_i64().unwrap();
        rows[0]["observations"][0]["quote"]["receipt"] = json!(at + 1);
        let future = rows.iter().map(|r| format!("{r}\n")).collect::<String>();
        manifest["bindings"]["data"] = json!(sadr_core::replay::digest(future.as_bytes()));
        assert!(
            export_features(
                &serde_json::to_vec(&manifest).unwrap(),
                future.as_bytes(),
                2000
            )
            .is_err()
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("paper.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE scanner(id INTEGER PRIMARY KEY,body TEXT NOT NULL); INSERT INTO scanner VALUES(1,'{\"samples\":{},\"cursor\":0,\"ranking\":[],\"account\":\"redact\"}');").unwrap();
        drop(db);
        let before = std::fs::read(&path).unwrap();
        let q = SourceQuery {
            kind: SourceKind::Scanner,
            from_ms: 0,
            to_ms: i64::MAX,
            after_rowid: 0,
            limit: 10,
            instrument_key: None,
            audit_id: None,
        };
        let page = read_source(directory.path(), &q, 4096).unwrap();
        assert_eq!(page.rows.len(), 1);
        assert!(page.rows[0]["value"].get("account").is_none());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            read_source(
                directory.path(),
                &SourceQuery {
                    kind: SourceKind::Quotes,
                    ..q
                },
                4096
            )
            .is_err()
        );
        let audit = directory.path().join("model/fixture");
        std::fs::create_dir_all(&audit).unwrap();
        std::fs::write(audit.join("input.json"), br#"{"input_hash":"fixture","input":{"account":"private","discovery":{"as_of_ms":1000,"prospects":[]}}}"#).unwrap();
        std::fs::write(
            audit.join("luna-review.json"),
            br#"{"at_ms":1000,"digest":{"summary":"fixture"},"client_secret":"private"}"#,
        )
        .unwrap();
        let query = SourceQuery {
            kind: SourceKind::ModelAudit,
            from_ms: 0,
            to_ms: 2000,
            after_rowid: 0,
            limit: 10,
            instrument_key: None,
            audit_id: Some("fixture".into()),
        };
        let page = read_source(directory.path(), &query, 4096).unwrap();
        assert_eq!(page.rows.len(), 2);
        assert!(!serde_json::to_string(&page).unwrap().contains("private"));
        assert!(
            read_source(
                directory.path(),
                &SourceQuery {
                    audit_id: Some("../fixture".into()),
                    ..query
                },
                4096
            )
            .is_err()
        );
    }
}
