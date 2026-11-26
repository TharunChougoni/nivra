//! Separate research database. Never opens a trading book or mutates source journals.
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

const APPLICATION_ID: i64 = 0x53414453;
const SCHEMA: &str = "PRAGMA application_id=1396786259;
CREATE TABLE schema_version(version INTEGER NOT NULL); INSERT INTO schema_version VALUES(2);
CREATE TABLE evidence(id TEXT PRIMARY KEY, kind TEXT NOT NULL, observed_ms INTEGER NOT NULL, spec_id TEXT NOT NULL, scan_id TEXT, digest_id TEXT, decide_id TEXT, body TEXT NOT NULL);
CREATE INDEX evidence_window ON evidence(kind,observed_ms,id);
CREATE INDEX evidence_spec_hash ON evidence(kind,json_extract(body,'$.spec_hash'),observed_ms,id);
CREATE TABLE proposals(id TEXT PRIMARY KEY, parent_id TEXT, source_hash TEXT NOT NULL, rationale TEXT NOT NULL, body TEXT NOT NULL, replay_passed INTEGER NOT NULL DEFAULT 0, shadow_days INTEGER NOT NULL DEFAULT 0);
CREATE TABLE notes(id INTEGER PRIMARY KEY, run_id TEXT NOT NULL, body TEXT NOT NULL);
CREATE TABLE specs(hash TEXT PRIMARY KEY, spec_id TEXT NOT NULL, body TEXT NOT NULL);
CREATE TABLE qualifications(id TEXT PRIMARY KEY, proposal_id TEXT NOT NULL REFERENCES proposals(id), body TEXT NOT NULL);
CREATE TABLE approvals(id INTEGER PRIMARY KEY, proposal_id TEXT NOT NULL REFERENCES proposals(id), approver TEXT NOT NULL, approval_ref TEXT NOT NULL, between_sessions INTEGER NOT NULL, policy TEXT);
";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Quote,
    History,
    Scan,
    ScanChunk,
    Digest,
    Handoff,
    Gate,
    Decide,
    Failure,
    Order,
    Fill,
    Position,
    Trade,
    ForwardReturn,
    Scorecard,
    Replay,
}
impl EvidenceKind {
    fn key(&self) -> &'static str {
        match self {
            Self::Quote => "quote",
            Self::History => "history",
            Self::Scan => "scan",
            Self::ScanChunk => "scan_chunk",
            Self::Digest => "digest",
            Self::Handoff => "handoff",
            Self::Gate => "gate",
            Self::Decide => "decide",
            Self::Failure => "failure",
            Self::Order => "order",
            Self::Fill => "fill",
            Self::Position => "position",
            Self::Trade => "trade",
            Self::ForwardReturn => "forward_return",
            Self::Scorecard => "scorecard",
            Self::Replay => "replay",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub id: String,
    pub kind: EvidenceKind,
    pub observed_ms: i64,
    pub spec_id: String,
    pub scan_id: Option<String>,
    pub digest_id: Option<String>,
    pub decide_id: Option<String>,
    pub body: serde_json::Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub id: String,
    pub parent_id: Option<String>,
    pub source_hash: String,
    pub rationale: String,
    pub spec: nivra_core::strategy::StrategySpec,
    #[serde(default)]
    pub scorer: Option<ScorerSource>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScorerSource {
    pub language: ScorerLanguage,
    pub source: String,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScorerLanguage {
    Python,
    Wasm,
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn proposal_hash(spec: &nivra_core::strategy::StrategySpec) -> Result<String, String> {
    Ok(hash(&serde_json::to_vec(spec).map_err(|_| err())?))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub kind: EvidenceKind,
    pub from_ms: i64,
    pub to_ms: i64,
    pub after_id: Option<String>,
    pub limit: usize,
    #[serde(default)]
    pub spec_id: Option<String>,
    #[serde(default)]
    pub instrument_key: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Page {
    pub rows: Vec<Evidence>,
    pub next_after_id: Option<String>,
    pub estimated_tokens: usize,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryQuery {
    pub kind: RegistryKind,
    pub after_id: Option<String>,
    pub limit: usize,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryKind {
    Specs,
    Proposals,
    Notes,
}
fn err() -> String {
    "strategy_results_operation_rejected".into()
}
fn bounded(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.contains('\0')
}

fn validate_scorer(s: &ScorerSource) -> Result<(), String> {
    let invalid = || "strategy_scorer_source_invalid".to_string();
    if !bounded(&s.source, 524288) {
        return Err(invalid());
    }
    let bytes = match s.language {
        ScorerLanguage::Python => {
            if s.source.len() > 262144 {
                return Err(invalid());
            }
            s.source.as_bytes().to_vec()
        }
        ScorerLanguage::Wasm => {
            // Canonical lowercase hex encodes the exact binary supplied
            // to the sandbox; hashes identify bytes, not their encoding.
            if !s.source.len().is_multiple_of(2)
                || !s
                    .source
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(invalid());
            }
            let bytes: Vec<u8> = s
                .source
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| {
                    let nibble = |b: u8| {
                        if b.is_ascii_digit() {
                            b - b'0'
                        } else {
                            b - b'a' + 10
                        }
                    };
                    (nibble(pair[0]) << 4) | nibble(pair[1])
                })
                .collect();
            if !bytes.starts_with(b"\0asm\x01\0\0\0") {
                return Err(invalid());
            }
            bytes
        }
    };
    if hash(&bytes) != s.sha256 {
        return Err(invalid());
    }
    Ok(())
}

/// Trusted evaluator attestation. Immutable and deliberately absent from model tools.
/// Metrics refer to a declared, disjoint holdout on the same recorded dataset.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Qualification {
    pub id: String,
    pub replay_id: String,
    pub champion: nivra_core::strategy::StrategySpec,
    pub sample_count: u32,
    pub out_of_sample_count: u32,
    pub training_end_ms: i64,
    pub holdout_start_ms: i64,
    pub holdout_end_ms: i64,
    pub candidate_return_bps: f64,
    pub champion_return_bps: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PromotionPolicy {
    pub min_samples: u32,
    pub min_out_of_sample: u32,
    pub min_improvement_bps: f64,
    /// Largest relative numeric change; structural changes count as 1.
    pub max_parameter_change: f64,
}
impl Default for PromotionPolicy {
    fn default() -> Self {
        Self {
            min_samples: 100,
            min_out_of_sample: 30,
            min_improvement_bps: 5.0,
            max_parameter_change: 0.25,
        }
    }
}
impl PromotionPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=1_000_000).contains(&self.min_samples)
            || !(1..=self.min_samples).contains(&self.min_out_of_sample)
            || !self.min_improvement_bps.is_finite()
            || self.min_improvement_bps < 0.0
            || self.min_improvement_bps > 1_000_000.0
            || !self.max_parameter_change.is_finite()
            || !(0.0..=1.0).contains(&self.max_parameter_change)
        {
            return Err(err());
        }
        Ok(())
    }
}
fn parameter_change(a: &serde_json::Value, b: &serde_json::Value) -> f64 {
    use serde_json::Value;
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => {
            let a = a.as_f64().unwrap_or(f64::INFINITY);
            let b = b.as_f64().unwrap_or(f64::INFINITY);
            (a - b).abs() / a.abs().max(b.abs()).max(1.0)
        }
        (Value::Object(a), Value::Object(b)) if a.len() == b.len() => a
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "id" | "version"))
            .map(|(k, v)| b.get(k).map_or(1.0, |w| parameter_change(v, w)))
            .fold(0.0, f64::max),
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => a
            .iter()
            .zip(b)
            .map(|(v, w)| parameter_change(v, w))
            .fold(0.0, f64::max),
        _ if a == b => 0.0,
        _ => 1.0,
    }
}

/// Bound fixed read queries too: row limits alone do not bound JSON/filter scans.
struct ReadBudget<'a>(&'a Connection);
impl<'a> ReadBudget<'a> {
    fn new(connection: &'a Connection) -> Result<Self, String> {
        let mut steps = 0usize;
        connection
            .progress_handler(
                1000,
                Some(move || {
                    steps += 1;
                    steps > 1000
                }),
            )
            .map_err(|_| err())?;
        Ok(Self(connection))
    }
}
impl Drop for ReadBudget<'_> {
    fn drop(&mut self) {
        let _ = self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

pub struct ResultsDb {
    connection: Connection,
}
impl ResultsDb {
    /// Opens only a dedicated database; foreign databases fail before any migration.
    pub fn open(path: &Path) -> Result<Self, String> {
        let exists = path.exists();
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(|_| err())?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(|_| err())?;
        let id: i64 = connection
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .map_err(|_| err())?;
        if exists && id != APPLICATION_ID {
            return Err(err());
        }
        if !exists {
            connection
                .execute_batch(&format!("BEGIN IMMEDIATE;{SCHEMA}COMMIT;"))
                .map_err(|_| err())?;
        }
        let version: i64 = connection
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .map_err(|_| err())?;
        if version == 1 {
            connection.execute_batch("BEGIN IMMEDIATE; CREATE INDEX evidence_spec_hash ON evidence(kind,json_extract(body,'$.spec_hash'),observed_ms,id); CREATE TABLE specs(hash TEXT PRIMARY KEY, spec_id TEXT NOT NULL, body TEXT NOT NULL); CREATE TABLE qualifications(id TEXT PRIMARY KEY, proposal_id TEXT NOT NULL REFERENCES proposals(id), body TEXT NOT NULL); ALTER TABLE approvals RENAME TO approvals_v1; CREATE TABLE approvals(id INTEGER PRIMARY KEY, proposal_id TEXT NOT NULL REFERENCES proposals(id), approver TEXT NOT NULL, approval_ref TEXT NOT NULL, between_sessions INTEGER NOT NULL, policy TEXT); INSERT INTO approvals(proposal_id,approver,approval_ref,between_sessions,policy) SELECT proposal_id,approver,approval_ref,between_sessions,NULL FROM approvals_v1; UPDATE schema_version SET version=2; COMMIT;").map_err(|_| err())?;
        } else if version != 2 {
            return Err(err());
        }
        Ok(Self { connection })
    }
    pub fn open_read_only(path: &Path) -> Result<Self, String> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| err())?;
        let id: i64 = connection
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .map_err(|_| err())?;
        if id != APPLICATION_ID {
            return Err(err());
        }
        let version: i64 = connection
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .map_err(|_| err())?;
        if version != 2 {
            return Err(err());
        }
        Ok(Self { connection })
    }
    /// Bounded, fixed-query presentation data. This never infers that approval is activation.
    pub fn report_summary(&self) -> Result<serde_json::Value, String> {
        let _budget = ReadBudget::new(&self.connection)?;
        let mut statement = self.connection.prepare("SELECT hash,body,(SELECT body FROM evidence WHERE kind='scorecard' AND json_extract(body,'$.spec_hash')=specs.hash ORDER BY observed_ms DESC,id DESC LIMIT 1),(SELECT body FROM evidence WHERE kind='scan' AND json_extract(body,'$.spec_hash')=specs.hash ORDER BY observed_ms DESC,id DESC LIMIT 1) FROM specs ORDER BY hash LIMIT 65").map_err(|_|err())?;
        let values = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|_| err())?;
        let mut specs = Vec::new();
        let mut truncated = false;
        for value in values {
            if specs.len() == 64 {
                truncated = true;
                break;
            }
            let (hash, body, card, scan) = value.map_err(|_| err())?;
            if body.len() > 262144
                || card.as_ref().is_some_and(|v| v.len() > 262144)
                || scan.as_ref().is_some_and(|v| v.len() > 262144)
            {
                return Err(err());
            }
            let spec: nivra_core::strategy::StrategySpec =
                serde_json::from_str(&body).map_err(|_| err())?;
            spec.validate()?;
            if proposal_hash(&spec)? != hash {
                return Err(err());
            }
            let card: serde_json::Value = card
                .map(|v| serde_json::from_str(&v))
                .transpose()
                .map_err(|_| err())?
                .unwrap_or(serde_json::Value::Null);
            let scan: serde_json::Value = scan
                .map(|v| serde_json::from_str(&v))
                .transpose()
                .map_err(|_| err())?
                .unwrap_or(serde_json::Value::Null);
            let universe = scan["rows"].as_array();
            specs.push(serde_json::json!({"hash":hash,"id":spec.id,"version":spec.version,"lookback_samples":spec.lookback_samples,
                "scorecard":{"replay_config":card["replay_config"],"samples":card["measured_marks"].as_u64(),"gross_bps":card["mean_forward_return_bps"].as_f64(),"net_bps":card["cost_adjusted"]["net_expectancy_bps"].as_f64(),"missing_marks":card["missing_forward_marks"].as_u64(),"missing_mark_reasons":card["missing_mark_reasons"],"failed_ticks":card["failed_ticks"].as_u64(),"hit_rate":card["cost_adjusted"]["hit_rate"].as_f64(),"drawdown_bps":card["cost_adjusted"]["cumulative_drawdown_bps"].as_f64()},
                "scan":{"at_ms":scan["observed_ms"].as_i64().or_else(||scan["snapshot_header"]["observed_ms"].as_i64()),"universe":universe.map(Vec::len).or_else(||scan["row_count"].as_u64().map(|n|n as usize)),"selected":universe.map(|rows|rows.iter().filter(|r|r["selected"].as_bool()==Some(true)).count()).or_else(||scan["selected_count"].as_u64().map(|n|n as usize))}}));
        }
        let mut statement = self.connection.prepare("SELECT p.source_hash,a.proposal_id,a.approver,a.approval_ref,a.between_sessions FROM approvals a JOIN proposals p ON p.id=a.proposal_id ORDER BY a.id DESC LIMIT 101").map_err(|_|err())?;
        let values = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, bool>(4)?,
                ))
            })
            .map_err(|_| err())?;
        let mut approvals = Vec::new();
        for value in values {
            if approvals.len() == 100 {
                truncated = true;
                break;
            }
            let (hash, proposal, approver, reference, between_sessions) =
                value.map_err(|_| err())?;
            if !bounded(&hash, 256)
                || !bounded(&proposal, 256)
                || !bounded(&approver, 256)
                || !bounded(&reference, 1024)
            {
                return Err(err());
            }
            approvals.push(serde_json::json!({"hash":hash,"proposal":proposal,"approver":approver,"reference":reference,"between_sessions":between_sessions}));
        }
        Ok(serde_json::json!({"specs":specs,"approvals":approvals,"truncated":truncated}))
    }
    /// Preserve every exact versioned specification independently of run retention.
    pub fn register_spec(
        &self,
        spec: &nivra_core::strategy::StrategySpec,
    ) -> Result<String, String> {
        spec.validate()?;
        let spec_hash = proposal_hash(spec)?;
        self.connection
            .execute(
                "INSERT OR IGNORE INTO specs(hash,spec_id,body) VALUES(?1,?2,?3)",
                params![
                    spec_hash,
                    format!("{}:{}", spec.id, spec.version),
                    serde_json::to_string(spec).map_err(|_| err())?
                ],
            )
            .map_err(|_| err())?;
        Ok(spec_hash)
    }
    /// Trusted deterministic producer only; not exposed by Orchestrator.
    pub fn record(&self, row: &Evidence) -> Result<(), String> {
        if !bounded(&row.id, 256)
            || !bounded(&row.spec_id, 256)
            || [&row.scan_id, &row.digest_id, &row.decide_id]
                .iter()
                .any(|v| v.as_ref().is_some_and(|id| !bounded(id, 256)))
        {
            return Err(err());
        }
        if matches!(
            row.kind,
            EvidenceKind::Trade | EvidenceKind::Fill | EvidenceKind::Order
        ) && [
            row.scan_id.as_ref(),
            row.digest_id.as_ref(),
            row.decide_id.as_ref(),
        ]
        .iter()
        .any(|x| x.is_none())
        {
            return Err("strategy_attribution_incomplete".into());
        }
        let body = serde_json::to_string(&row.body).map_err(|_| err())?;
        if body.len() > 262144 {
            return Err(err());
        }
        self.connection
            .execute(
                "INSERT INTO evidence VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    row.id,
                    row.kind.key(),
                    row.observed_ms,
                    row.spec_id,
                    row.scan_id,
                    row.digest_id,
                    row.decide_id,
                    body
                ],
            )
            .map_err(|_| err())?;
        Ok(())
    }
    /// Re-observing identical research facts is safe; conflicting IDs fail closed.
    /// This never submits or retries a paper/broker action.
    pub fn record_batch_idempotent(&self, rows: &[Evidence]) -> Result<(), String> {
        if rows.is_empty() || rows.len() > 100_000 {
            return Err(err());
        }
        let transaction = self.connection.unchecked_transaction().map_err(|_| err())?;
        for row in rows {
            if let Some(existing) = self.record_by_id(&row.id, &row.kind)? {
                if serde_json::to_value(existing).map_err(|_| err())?
                    != serde_json::to_value(row).map_err(|_| err())?
                {
                    return Err("strategy_results_evidence_conflict".into());
                }
            } else {
                self.record(row)?;
            }
        }
        transaction.commit().map_err(|_| err())
    }
    pub fn record_by_id(&self, id: &str, kind: &EvidenceKind) -> Result<Option<Evidence>, String> {
        if !bounded(id, 256) {
            return Err(err());
        }
        let raw=self.connection.query_row("SELECT observed_ms,spec_id,scan_id,digest_id,decide_id,body FROM evidence WHERE id=?1 AND kind=?2",params![id,kind.key()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,String>(5)?))).optional().map_err(|_|err())?;
        raw.map(
            |(observed_ms, spec_id, scan_id, digest_id, decide_id, body)| {
                if body.len() > 262144 {
                    return Err(err());
                }
                Ok(Evidence {
                    id: id.into(),
                    kind: kind.clone(),
                    observed_ms,
                    spec_id,
                    scan_id,
                    digest_id,
                    decide_id,
                    body: serde_json::from_str(&body).map_err(|_| err())?,
                })
            },
        )
        .transpose()
    }
    /// Atomically preserves a replay and its scorecard; duplicate runs leave no partial rows.
    pub fn record_batch(&self, rows: &[Evidence]) -> Result<(), String> {
        if rows.is_empty() || rows.len() > 100_000 {
            return Err(err());
        }
        let transaction = self.connection.unchecked_transaction().map_err(|_| err())?;
        for row in rows {
            self.record(row)?;
        }
        transaction.commit().map_err(|_| err())
    }
    pub fn query(&self, q: &Query) -> Result<Vec<Evidence>, String> {
        let _budget = ReadBudget::new(&self.connection)?;
        if q.limit == 0
            || q.limit > 100
            || q.from_ms > q.to_ms
            || q.after_id.as_ref().is_some_and(|v| !bounded(v, 256))
            || q.spec_id.as_ref().is_some_and(|v| !bounded(v, 256))
            || q.instrument_key.as_ref().is_some_and(|v| !bounded(v, 256))
        {
            return Err(err());
        }
        let mut stmt=self.connection.prepare("SELECT id,observed_ms,spec_id,scan_id,digest_id,decide_id,body FROM evidence WHERE kind=?1 AND observed_ms BETWEEN ?2 AND ?3 AND id>?4 AND (?6 IS NULL OR spec_id=?6) AND (?7 IS NULL OR json_extract(body, '$.key')=?7 OR json_extract(body, '$.instrument_key')=?7 OR EXISTS (SELECT 1 FROM json_each(body, '$.rows') WHERE json_extract(value, '$.key')=?7)) ORDER BY id LIMIT ?5").map_err(|_|err())?;
        let rows = stmt
            .query_map(
                params![
                    q.kind.key(),
                    q.from_ms,
                    q.to_ms,
                    q.after_id.as_deref().unwrap_or(""),
                    q.limit as i64,
                    q.spec_id,
                    q.instrument_key
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get::<_, String>(6)?,
                    ))
                },
            )
            .map_err(|_| err())?;
        rows.map(|r| {
            let (id, observed_ms, spec_id, scan_id, digest_id, decide_id, body) =
                r.map_err(|_| err())?;
            Ok(Evidence {
                id,
                kind: q.kind.clone(),
                observed_ms,
                spec_id,
                scan_id,
                digest_id,
                decide_id,
                body: serde_json::from_str(&body).map_err(|_| err())?,
            })
        })
        .collect()
    }
    /// Fixed typed registry reads; notes are restricted to the caller's own run.
    pub fn read_registry(
        &self,
        run: &str,
        query: &RegistryQuery,
    ) -> Result<serde_json::Value, String> {
        let _budget = ReadBudget::new(&self.connection)?;
        if !bounded(run, 256)
            || !(1..=100).contains(&query.limit)
            || query.after_id.as_ref().is_some_and(|id| !bounded(id, 256))
        {
            return Err(err());
        }
        let cursor = query.after_id.as_deref().unwrap_or("");
        let mut rows = Vec::new();
        let mut next = None;
        match query.kind {
            RegistryKind::Specs => {
                let mut statement = self
                    .connection
                    .prepare("SELECT hash,body FROM specs WHERE hash>?1 ORDER BY hash LIMIT ?2")
                    .map_err(|_| err())?;
                let values = statement
                    .query_map(params![cursor, query.limit as i64], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })
                    .map_err(|_| err())?;
                for value in values {
                    let (id, body) = value.map_err(|_| err())?;
                    next = Some(id.clone());
                    rows.push(serde_json::json!({"hash":id,"spec":serde_json::from_str::<serde_json::Value>(&body).map_err(|_|err())?}));
                }
            }
            RegistryKind::Proposals => {
                let mut statement = self.connection.prepare("SELECT id,body,replay_passed,shadow_days FROM proposals WHERE id>?1 ORDER BY id LIMIT ?2").map_err(|_| err())?;
                let values = statement
                    .query_map(params![cursor, query.limit as i64], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, bool>(2)?,
                            r.get::<_, u32>(3)?,
                        ))
                    })
                    .map_err(|_| err())?;
                for value in values {
                    let (id, body, replay_passed, shadow_days) = value.map_err(|_| err())?;
                    next = Some(id.clone());
                    rows.push(serde_json::json!({"id":id,"proposal":serde_json::from_str::<serde_json::Value>(&body).map_err(|_| err())?,"replay_passed":replay_passed,"shadow_days":shadow_days}));
                }
            }
            RegistryKind::Notes => {
                let after = if cursor.is_empty() {
                    0
                } else {
                    cursor.parse::<i64>().map_err(|_| err())?
                };
                if after < 0 {
                    return Err(err());
                }
                let mut statement = self
                    .connection
                    .prepare(
                        "SELECT id,body FROM notes WHERE run_id=?1 AND id>?2 ORDER BY id LIMIT ?3",
                    )
                    .map_err(|_| err())?;
                let values = statement
                    .query_map(params![run, after, query.limit as i64], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                    })
                    .map_err(|_| err())?;
                for value in values {
                    let (id, body) = value.map_err(|_| err())?;
                    next = Some(id.to_string());
                    rows.push(serde_json::json!({"id":id,"body":body}));
                }
            }
        }
        Ok(serde_json::json!({"rows":rows,"next_after_id":next}))
    }
    pub fn propose(&self, p: &Proposal) -> Result<(), String> {
        p.spec.validate()?;
        if proposal_hash(&p.spec)? != p.source_hash {
            return Err("strategy_source_hash_mismatch".into());
        }
        if let Some(scorer) = &p.scorer {
            validate_scorer(scorer)?;
        }
        if let Some(parent) = &p.parent_id {
            if !bounded(parent, 256) || parent == &p.id {
                return Err(err());
            }
            let exists: bool = self
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM proposals WHERE id=?1)",
                    [parent],
                    |r| r.get(0),
                )
                .map_err(|_| err())?;
            if !exists {
                return Err("strategy_lineage_parent_missing".into());
            }
        }
        if !bounded(&p.id, 256)
            || !bounded(&p.rationale, 8192)
            || p.source_hash.len() != 64
            || !p.source_hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(err());
        }
        self.register_spec(&p.spec)?;
        self.connection.execute("INSERT INTO proposals(id,parent_id,source_hash,rationale,body) VALUES(?1,?2,?3,?4,?5)",params![p.id,p.parent_id,p.source_hash,p.rationale,serde_json::to_string(&p).map_err(|_|err())?]).map_err(|_|err())?;
        Ok(())
    }
    pub fn note(&self, run: &str, body: &str) -> Result<(), String> {
        if !bounded(run, 256) || !bounded(body, 8192) {
            return Err(err());
        }
        self.connection
            .execute(
                "INSERT INTO notes(run_id,body) VALUES(?1,?2)",
                params![run, body],
            )
            .map_err(|_| err())?;
        Ok(())
    }
    /// Only trusted validation/shadow producers may register measured lifecycle evidence.
    pub fn record_qualification(
        &self,
        id: &str,
        replay_passed: bool,
        shadow_days: u32,
    ) -> Result<(), String> {
        if !bounded(id, 256) || shadow_days > 365 {
            return Err(err());
        }
        let transaction = self.connection.unchecked_transaction().map_err(|_| err())?;
        self.connection
            .execute(
                "INSERT INTO qualifications(id,proposal_id,body) VALUES(?1,?2,?3)",
                params![
                    format!("legacy:{id}:{replay_passed}:{shadow_days}"),
                    id,
                    serde_json::json!({"replay_passed":replay_passed,"shadow_days":shadow_days})
                        .to_string()
                ],
            )
            .map_err(|_| err())?;
        let count = self
            .connection
            .execute(
                "UPDATE proposals SET replay_passed=?2,shadow_days=?3 WHERE id=?1",
                params![id, replay_passed, shadow_days],
            )
            .map_err(|_| err())?;
        if count != 1 {
            return Err(err());
        }
        transaction.commit().map_err(|_| err())
    }
    pub fn record_measured_qualification(
        &self,
        proposal_id: &str,
        replay_passed: bool,
        shadow_days: u32,
        q: &Qualification,
    ) -> Result<(), String> {
        let transaction = self.connection.unchecked_transaction().map_err(|_| err())?;
        self.record_qualification_evidence(proposal_id, q)?;
        // The historical registration method uses its own transaction; perform
        // the same append and derived-status update inside this atomic boundary.
        if shadow_days > 365 {
            return Err(err());
        }
        self.connection
            .execute(
                "INSERT INTO qualifications(id,proposal_id,body) VALUES(?1,?2,?3)",
                params![
                    format!("status:{}", q.id),
                    proposal_id,
                    serde_json::json!({"replay_passed":replay_passed,"shadow_days":shadow_days})
                        .to_string()
                ],
            )
            .map_err(|_| err())?;
        self.connection
            .execute(
                "UPDATE proposals SET replay_passed=?2,shadow_days=?3 WHERE id=?1",
                params![proposal_id, replay_passed, shadow_days],
            )
            .map_err(|_| err())?;
        transaction.commit().map_err(|_| err())
    }
    pub fn record_qualification_evidence(
        &self,
        proposal_id: &str,
        q: &Qualification,
    ) -> Result<(), String> {
        let proposal = self.load_proposal(proposal_id)?;
        q.champion.validate()?;
        if !bounded(&q.id, 256)
            || !bounded(&q.replay_id, 256)
            || q.id.starts_with("legacy:")
            || q.out_of_sample_count > q.sample_count
            || q.training_end_ms >= q.holdout_start_ms
            || q.holdout_start_ms > q.holdout_end_ms
            || !q.candidate_return_bps.is_finite()
            || !q.champion_return_bps.is_finite()
        {
            return Err(err());
        }
        let replay_matches: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM evidence WHERE id=?1 AND kind='replay' AND json_extract(body,'$.spec_hash')=?2 AND json_extract(body,'$.failed_ticks')=0)",params![q.replay_id,proposal.source_hash],|r|r.get(0)).map_err(|_|err())?;
        if !replay_matches {
            return Err("strategy_replay_evidence_required".into());
        }
        self.connection
            .execute(
                "INSERT INTO qualifications(id,proposal_id,body) VALUES(?1,?2,?3)",
                params![
                    q.id,
                    proposal_id,
                    serde_json::to_string(q).map_err(|_| err())?
                ],
            )
            .map_err(|_| err())?;
        Ok(())
    }
    fn require_promotion_policy(&self, id: &str, policy: &PromotionPolicy) -> Result<(), String> {
        policy.validate()?;
        let proposal = self.load_proposal(id)?;
        let candidate = serde_json::to_value(&proposal.spec).map_err(|_| err())?;
        let mut statement = self
            .connection
            .prepare("SELECT body FROM qualifications WHERE proposal_id=?1 ORDER BY id")
            .map_err(|_| err())?;
        let rows = statement
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(|_| err())?;
        for row in rows {
            let body = row.map_err(|_| err())?;
            let Ok(q) = serde_json::from_str::<Qualification>(&body) else {
                continue;
            };
            if q.sample_count >= policy.min_samples
                && q.out_of_sample_count >= policy.min_out_of_sample
                && q.candidate_return_bps - q.champion_return_bps >= policy.min_improvement_bps
                && parameter_change(
                    &serde_json::to_value(&q.champion).map_err(|_| err())?,
                    &candidate,
                ) <= policy.max_parameter_change
            {
                return Ok(());
            }
        }
        Err("strategy_promotion_guardrails_required".into())
    }
    /// Immutable proposal lookup. Revalidate source, schema and lineage before use.
    pub fn load_proposal(&self, id: &str) -> Result<Proposal, String> {
        if !bounded(id, 256) {
            return Err(err());
        }
        let body: String = self
            .connection
            .query_row("SELECT body FROM proposals WHERE id=?1", [id], |r| r.get(0))
            .map_err(|_| err())?;
        let p: Proposal = serde_json::from_str(&body).map_err(|_| err())?;
        p.spec.validate()?;
        if p.id != id || proposal_hash(&p.spec)? != p.source_hash || !bounded(&p.rationale, 8192) {
            return Err(err());
        }
        if let Some(scorer) = &p.scorer {
            validate_scorer(scorer)?;
        }
        if let Some(parent) = &p.parent_id {
            let exists: bool = self
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM proposals WHERE id=?1)",
                    [parent],
                    |r| r.get(0),
                )
                .map_err(|_| err())?;
            if !exists || parent == id {
                return Err("strategy_lineage_parent_missing".into());
            }
        }
        Ok(p)
    }
    /// Runtime admission is tied to the exact immutable champion specification.
    pub fn require_approved(&self, spec_hash: &str, min_shadow_days: u32) -> Result<(), String> {
        if spec_hash.len() != 64 || !(1..=365).contains(&min_shadow_days) {
            return Err("strategy_qualification_required".into());
        }
        let mut statement = self.connection.prepare("SELECT p.id,a.policy FROM proposals p JOIN approvals a ON a.proposal_id=p.id WHERE p.source_hash=?1 AND json_extract(p.body,'$.scorer') IS NULL AND p.replay_passed=1 AND p.shadow_days>=?2 AND a.approver='Tharun' AND a.between_sessions=1 AND length(a.approval_ref)>0").map_err(|_|err())?;
        let ids = statement
            .query_map(params![spec_hash, min_shadow_days], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(|_| err())?;
        for row in ids {
            let (id, Some(policy)) = row.map_err(|_| err())? else {
                continue;
            };
            let policy: PromotionPolicy = serde_json::from_str(&policy).map_err(|_| err())?;
            if self.require_promotion_policy(&id, &policy).is_ok() {
                return Ok(());
            }
        }
        Err("strategy_champion_approval_required".into())
    }
    /// Records approval evidence; does not modify runtime configuration or activate a session.
    pub fn approve(
        &self,
        id: &str,
        approver: &str,
        reference: &str,
        between_sessions: bool,
        min_shadow_days: u32,
    ) -> Result<(), String> {
        self.approve_with_policy(
            id,
            approver,
            reference,
            between_sessions,
            min_shadow_days,
            &PromotionPolicy::default(),
        )
    }
    pub fn approve_with_policy(
        &self,
        id: &str,
        approver: &str,
        reference: &str,
        between_sessions: bool,
        min_shadow_days: u32,
        policy: &PromotionPolicy,
    ) -> Result<(), String> {
        if approver != "Tharun"
            || !between_sessions
            || !bounded(reference, 1024)
            || min_shadow_days == 0
        {
            return Err("strategy_explicit_approval_required".into());
        }
        let eligible: bool = self
            .connection
            .query_row(
                "SELECT replay_passed=1 AND shadow_days>=?2 FROM proposals WHERE id=?1",
                params![id, min_shadow_days],
                |r| r.get(0),
            )
            .map_err(|_| err())?;
        if !eligible {
            return Err("strategy_qualification_required".into());
        }
        self.require_promotion_policy(id, policy)?;
        self.connection
            .execute(
                "INSERT INTO approvals(proposal_id,approver,approval_ref,between_sessions,policy) VALUES(?1,?2,?3,1,?4)",
                params![
                    id,
                    approver,
                    reference,
                    serde_json::to_string(policy).map_err(|_| err())?
                ],
            )
            .map_err(|_| err())?;
        Ok(())
    }
}
/// Explicit laboratory policy. A model gateway must be enabled by its operator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LabConfig {
    pub enabled: bool,
    pub token_budget: usize,
    pub min_shadow_days: u32,
    pub read_sources: bool,
    pub promotion: PromotionPolicy,
}
impl Default for LabConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token_budget: 4096,
            min_shadow_days: 5,
            read_sources: false,
            promotion: PromotionPolicy::default(),
        }
    }
}
impl LabConfig {
    pub fn validate(&self) -> Result<(), String> {
        self.promotion.validate()?;
        if !(1..=1_000_000).contains(&self.token_budget)
            || !(1..=365).contains(&self.min_shadow_days)
        {
            return Err(err());
        }
        Ok(())
    }
}
/// Typed model tools have no SQL, lifecycle qualification, approval or runtime-write variant.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "tool",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ModelTool {
    Read(Query),
    Propose(Proposal),
    Note(String),
}

/// Capability surface deliberately excludes SQL, producer writes and approvals.
pub struct Orchestrator<'a> {
    db: &'a ResultsDb,
    run: String,
    remaining_tokens: usize,
}
impl<'a> Orchestrator<'a> {
    pub fn new(db: &'a ResultsDb, run: String, token_budget: usize) -> Result<Self, String> {
        if !bounded(&run, 256) || token_budget == 0 || token_budget > 1_000_000 {
            return Err(err());
        }
        Ok(Self {
            db,
            run,
            remaining_tokens: token_budget,
        })
    }
    pub fn configured(db: &'a ResultsDb, run: String, config: &LabConfig) -> Result<Self, String> {
        config.validate()?;
        if !config.enabled {
            return Err("strategy_orchestrator_disabled".into());
        }
        Self::new(db, run, config.token_budget)
    }
    pub fn read_registry(&mut self, query: &RegistryQuery) -> Result<serde_json::Value, String> {
        let value = self.db.read_registry(&self.run, query)?;
        self.charge(&value)?;
        Ok(value)
    }
    pub fn remaining_tokens(&self) -> usize {
        self.remaining_tokens
    }
    /// Conservative byte accounting shared by source/replay tools and DB pages.
    pub fn charge<T: Serialize>(&mut self, value: &T) -> Result<(), String> {
        let cost = serde_json::to_vec(value)
            .map_err(|_| err())?
            .len()
            .saturating_add(32);
        if cost > self.remaining_tokens {
            return Err("strategy_tool_budget_exhausted".into());
        }
        self.remaining_tokens -= cost;
        Ok(())
    }
    pub fn call(&mut self, tool: ModelTool) -> Result<serde_json::Value, String> {
        match tool {
            ModelTool::Read(query) => serde_json::to_value(self.read(&query)?).map_err(|_| err()),
            ModelTool::Propose(proposal) => {
                let response = serde_json::json!({"status":"pending_qualification_and_approval"});
                self.charge(&response)?;
                self.propose(&proposal)?;
                Ok(response)
            }
            ModelTool::Note(note) => {
                let response = serde_json::json!({"status":"recorded"});
                self.charge(&response)?;
                self.note(&note)?;
                Ok(response)
            }
        }
    }
    pub fn read(&mut self, q: &Query) -> Result<Page, String> {
        let mut rows = self.db.query(q)?;
        let had_evidence = !rows.is_empty();
        // Reserve the complete response envelope, including a conservative width
        // for its own accounting field. Empty pages must also consume budget.
        let envelope = serde_json::to_vec(&Page {
            rows: Vec::new(),
            next_after_id: None,
            estimated_tokens: usize::MAX,
        })
        .map_err(|_| err())?
        .len()
        .saturating_add(32);
        let mut cost = envelope;
        let mut row_bytes = 0usize;
        let mut accepted = 0;
        for row in &rows {
            let bytes = serde_json::to_vec(row).map_err(|_| err())?.len();
            let cursor_bytes = serde_json::to_vec(&row.id).map_err(|_| err())?.len();
            let candidate_rows = row_bytes
                .saturating_add(bytes)
                .saturating_add(usize::from(accepted > 0));
            let candidate = envelope
                .saturating_add(candidate_rows)
                .saturating_add(cursor_bytes.saturating_sub(4));
            if candidate > self.remaining_tokens {
                break;
            }
            cost = candidate;
            row_bytes = candidate_rows;
            accepted += 1;
        }
        rows.truncate(accepted);
        if (rows.is_empty() && had_evidence) || cost > self.remaining_tokens {
            return Err("strategy_tool_budget_exhausted".into());
        }
        self.remaining_tokens -= cost;
        let next_after_id = rows.last().map(|r| r.id.clone());
        Ok(Page {
            rows,
            next_after_id,
            estimated_tokens: cost,
        })
    }
    pub fn propose(&self, p: &Proposal) -> Result<(), String> {
        self.db.propose(p)
    }
    pub fn note(&self, body: &str) -> Result<(), String> {
        self.db.note(&self.run, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row() -> Evidence {
        Evidence {
            id: "scan-1".into(),
            kind: EvidenceKind::Scan,
            observed_ms: 100,
            spec_id: "test:1".into(),
            scan_id: None,
            digest_id: None,
            decide_id: None,
            body: serde_json::json!({"ranked":2}),
        }
    }
    #[test]
    fn proposals_require_real_hash_and_promotion_requires_measured_qualification_and_owner() {
        use nivra_core::strategy::{Feature, Normalization, StrategySpec, Weight};
        let spec = StrategySpec {
            id: "test".into(),
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
            cooldown_ms: 0,
            trigger: None,
        };
        let mut proposal = Proposal {
            id: "proposal-1".into(),
            parent_id: None,
            source_hash: "0".repeat(64),
            rationale: "recorded hypothesis".into(),
            spec,
            scorer: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let db = ResultsDb::open(&dir.path().join("results.sqlite")).unwrap();
        assert!(db.propose(&proposal).is_err());
        proposal.source_hash = proposal_hash(&proposal.spec).unwrap();
        db.propose(&proposal).unwrap();
        let mut wasm = proposal.clone();
        wasm.id = "wasm-proposal".into();
        wasm.scorer = Some(ScorerSource {
            language: ScorerLanguage::Wasm,
            source: "0061736d01000000".into(),
            sha256: hash(b"\0asm\x01\0\0\0"),
        });
        db.propose(&wasm).unwrap();
        wasm.id = "wrong-wasm-hash".into();
        wasm.scorer.as_mut().unwrap().sha256 = hash(b"0061736d01000000");
        assert!(db.propose(&wasm).is_err());
        wasm.scorer.as_mut().unwrap().source = "not wasm".into();
        assert!(db.propose(&wasm).is_err());
        assert!(
            db.approve("proposal-1", "Tharun", "manual-1", true, 2)
                .is_err()
        );
        db.record_qualification("proposal-1", true, 2).unwrap();
        assert!(
            db.approve("proposal-1", "model", "manual-1", true, 2)
                .is_err()
        );
        assert!(
            db.approve("proposal-1", "Tharun", "manual-1", false, 2)
                .is_err()
        );
        assert!(
            db.approve("proposal-1", "Tharun", "manual-1", true, 0)
                .is_err()
        );
        assert!(
            db.approve("proposal-1", "Tharun", "manual-1", true, 2)
                .is_err()
        );
        let mut replay = row();
        replay.id = "replay-qualified".into();
        replay.kind = EvidenceKind::Replay;
        replay.spec_id = "test:1".into();
        replay.body = serde_json::json!({"spec_hash":proposal.source_hash,"failed_ticks":0});
        db.record(&replay).unwrap();
        let mut qualification = Qualification {
            id: "qualification-1".into(),
            replay_id: replay.id,
            champion: proposal.spec.clone(),
            sample_count: 100,
            out_of_sample_count: 30,
            training_end_ms: 1,
            holdout_start_ms: 2,
            holdout_end_ms: 3,
            candidate_return_bps: 10.0,
            champion_return_bps: 0.0,
        };
        qualification.out_of_sample_count = 1;
        db.record_qualification_evidence("proposal-1", &qualification)
            .unwrap();
        assert!(
            db.approve("proposal-1", "Tharun", "manual-1", true, 2)
                .is_err()
        );
        qualification.id = "qualification-2".into();
        qualification.out_of_sample_count = 30;
        db.record_qualification_evidence("proposal-1", &qualification)
            .unwrap();
        assert!(
            db.record_qualification_evidence("proposal-1", &qualification)
                .is_err()
        );
        assert!(db.load_proposal("wasm-proposal").is_ok());
        db.approve("proposal-1", "Tharun", "manual-1", true, 2)
            .unwrap();
    }
    #[test]
    fn foreign_databases_and_unattributed_trades_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let foreign = dir.path().join("foreign.sqlite");
        Connection::open(&foreign)
            .unwrap()
            .execute_batch("CREATE TABLE sealed(value TEXT);")
            .unwrap();
        assert!(ResultsDb::open(&foreign).is_err());
        let legacy = dir.path().join("legacy-results.sqlite");
        let legacy_schema = SCHEMA.replace("VALUES(2)","VALUES(1)").replace("CREATE INDEX evidence_spec_hash ON evidence(kind,json_extract(body,'$.spec_hash'),observed_ms,id);", "")
            .replace("CREATE TABLE specs(hash TEXT PRIMARY KEY, spec_id TEXT NOT NULL, body TEXT NOT NULL);", "")
            .replace("CREATE TABLE qualifications(id TEXT PRIMARY KEY, proposal_id TEXT NOT NULL REFERENCES proposals(id), body TEXT NOT NULL);", "")
            .replace("CREATE TABLE approvals(id INTEGER PRIMARY KEY, proposal_id TEXT NOT NULL REFERENCES proposals(id), approver TEXT NOT NULL, approval_ref TEXT NOT NULL, between_sessions INTEGER NOT NULL, policy TEXT);", "CREATE TABLE approvals(proposal_id TEXT PRIMARY KEY REFERENCES proposals(id), approver TEXT NOT NULL, approval_ref TEXT NOT NULL, between_sessions INTEGER NOT NULL);");
        let old = Connection::open(&legacy).unwrap();
        old.execute_batch(&legacy_schema).unwrap();
        old.execute_batch("INSERT INTO notes(run_id,body) VALUES('original','preserve'); INSERT INTO proposals(id,source_hash,rationale,body,replay_passed,shadow_days) VALUES('old','hash','original','{}',1,5); INSERT INTO approvals VALUES('old','Tharun','original-owner-record',1);").unwrap();
        drop(old);
        assert!(ResultsDb::open_read_only(&legacy).is_err());
        let migrated = ResultsDb::open(&legacy).unwrap();
        assert_eq!(
            migrated
                .connection
                .query_row("SELECT body FROM notes", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "preserve"
        );
        assert_eq!(
            migrated
                .connection
                .query_row("SELECT approval_ref FROM approvals_v1", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "original-owner-record"
        );
        assert!(migrated.require_approved("hash", 5).is_err());
        assert!(ResultsDb::open_read_only(&legacy).is_ok());
        let db = ResultsDb::open(&dir.path().join("results.sqlite")).unwrap();
        let mut trade = row();
        trade.kind = EvidenceKind::Trade;
        assert!(db.record(&trade).is_err());
    }
    #[test]
    fn tool_pagination_budget_and_readonly_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results.sqlite");
        let db = ResultsDb::open(&path).unwrap();
        db.record(&row()).unwrap();
        let mut second = row();
        second.id = "scan-2".into();
        db.record(&second).unwrap();
        let query = Query {
            kind: EvidenceKind::Scan,
            from_ms: 0,
            to_ms: 200,
            after_id: None,
            limit: 1,
            spec_id: None,
            instrument_key: None,
        };
        assert!(
            db.query(&Query {
                spec_id: Some("different:1".into()),
                ..query.clone()
            })
            .unwrap()
            .is_empty()
        );
        assert!(
            db.query(&Query {
                instrument_key: Some("missing".into()),
                ..query.clone()
            })
            .unwrap()
            .is_empty()
        );
        let mut tool = Orchestrator::new(&db, "run".into(), 2048).unwrap();
        let first = tool.read(&query).unwrap();
        assert_eq!(first.rows[0].id, "scan-1");
        let next = Query {
            after_id: first.next_after_id,
            ..query.clone()
        };
        assert_eq!(tool.read(&next).unwrap().rows[0].id, "scan-2");
        assert!(
            Orchestrator::new(&db, "run".into(), 1)
                .unwrap()
                .read(&query)
                .is_err()
        );
        let readonly = ResultsDb::open_read_only(&path).unwrap();
        assert!(readonly.note("run", "write forbidden").is_err());
        assert!(readonly.record(&row()).is_err());
    }
    #[test]
    fn batch_is_atomic_when_a_late_row_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let db = ResultsDb::open(&dir.path().join("results.sqlite")).unwrap();
        let valid = row();
        let mut invalid = row();
        invalid.id.clear();
        assert!(db.record_batch(&[valid.clone(), invalid]).is_err());
        // First insert rolled back, so its immutable ID remains available.
        db.record(&valid).unwrap();
        db.record_batch_idempotent(std::slice::from_ref(&valid))
            .unwrap();
        let mut conflict = valid.clone();
        conflict.body = serde_json::json!({"ranked":999});
        let mut new = valid.clone();
        new.id = "new-evidence".into();
        assert!(
            db.record_batch_idempotent(&[new.clone(), conflict])
                .is_err()
        );
        assert!(db.record_by_id(&new.id, &new.kind).unwrap().is_none());
        db.record(&new).unwrap();
    }
    #[test]
    fn model_gateway_requires_enablement_and_rejects_privileged_tools() {
        let dir = tempfile::tempdir().unwrap();
        let db = ResultsDb::open(&dir.path().join("results.sqlite")).unwrap();
        assert!(Orchestrator::configured(&db, "run".into(), &LabConfig::default()).is_err());
        let config = LabConfig {
            enabled: true,
            ..LabConfig::default()
        };
        let mut gateway = Orchestrator::configured(&db, "run".into(), &config).unwrap();
        gateway
            .call(ModelTool::Note("experiment negative".into()))
            .unwrap();
        db.note("other-run", "private research").unwrap();
        let registry = gateway
            .read_registry(&RegistryQuery {
                kind: RegistryKind::Notes,
                after_id: None,
                limit: 10,
            })
            .unwrap();
        assert_eq!(registry["rows"].as_array().unwrap().len(), 1);
        assert_eq!(registry["rows"][0]["body"], "experiment negative");
        for forbidden in ["approve", "sql", "order", "record_qualification"] {
            assert!(
                serde_json::from_value::<ModelTool>(
                    serde_json::json!({"tool":forbidden,"input":{}})
                )
                .is_err()
            );
        }
    }
    #[test]
    fn empty_evidence_is_not_a_model_failure() {
        let dir = tempfile::tempdir().unwrap();
        let db = ResultsDb::open(&dir.path().join("results.sqlite")).unwrap();
        let query = Query {
            kind: EvidenceKind::Scan,
            from_ms: 0,
            to_ms: 200,
            after_id: None,
            limit: 10,
            spec_id: None,
            instrument_key: None,
        };
        assert!(
            Orchestrator::new(&db, "run".into(), 256)
                .unwrap()
                .read(&query)
                .unwrap()
                .rows
                .is_empty()
        );
        let mut bounded = Orchestrator::new(&db, "bounded".into(), 256).unwrap();
        bounded.read(&query).unwrap();
        bounded.read(&query).unwrap();
        assert!(bounded.read(&query).is_err());
        let mut notes = Orchestrator::new(&db, "notes".into(), 128).unwrap();
        notes.call(ModelTool::Note("first".into())).unwrap();
        notes.call(ModelTool::Note("second".into())).unwrap();
        assert!(
            notes
                .call(ModelTool::Note("must not write".into()))
                .is_err()
        );
        let count: i64 = db
            .connection
            .query_row("SELECT count(*) FROM notes WHERE run_id='notes'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
    }
}
