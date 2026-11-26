//! Append-only, offline strategy source library. Never open a trading book here.
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

const APPLICATION_ID: i64 = 0x534c4942;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LibraryLimits {
    pub max_body_bytes: usize,
    pub max_metrics_bytes: usize,
}
impl Default for LibraryLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: 262_144,
            max_metrics_bytes: 65_536,
        }
    }
}

/// Errors deliberately exclude SQL text and stored source/input payloads.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Storage,
    InvalidInput,
    SizeLimit,
    WrongDatabase,
    HashNameConflict,
    NotFound,
    Ambiguous,
    InvalidUtf8,
    HashMismatch,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "strategy library: {self:?}")
    }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}
type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub strategy_id: i64,
    pub version: i64,
    /// Strategy name bound to this exact source hash.
    pub name: String,
    pub sha256: String,
    pub file_body: String,
    pub author_model: String,
    pub intent: String,
    pub inputs: Value,
    pub created_at_ms: i64,
}
pub struct NewVersion<'a> {
    pub name: &'a str,
    pub file_body: &'a str,
    pub author_model: &'a str,
    pub intent: &'a str,
    pub inputs: &'a Value,
    pub created_at_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    pub sha256: String,
    pub run_kind: String,
    /// ISO YYYY-MM-DD session date.
    pub session_date: String,
    pub regime: Option<String>,
    /// Object containing trades, net_pnl_after_costs, expectancy, max_drawdown.
    pub metrics: Value,
    pub recorded_at_ms: i64,
}
pub struct Library {
    connection: Connection,
    limits: LibraryLimits,
}
impl Library {
    /// Open an existing library without creating or migrating any schema.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let connection =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let id: i64 = connection.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if id != APPLICATION_ID || !matches!(version, 1 | 2) {
            return Err(Error::WrongDatabase);
        }
        Ok(Self {
            connection,
            limits: LibraryLimits::default(),
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_limits(path, LibraryLimits::default())
    }
    pub fn open_with_limits(path: impl AsRef<Path>, limits: LibraryLimits) -> Result<Self> {
        if limits.max_body_bytes == 0 || limits.max_metrics_bytes == 0 {
            return Err(Error::InvalidInput);
        }
        let mut connection = Connection::open(path)?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if id == 0 && version == 0 {
            let tables: i64 = tx.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |r| r.get(0),
            )?;
            if tables != 0 {
                return Err(Error::WrongDatabase);
            }
            tx.execute_batch("CREATE TABLE strategies(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
                CREATE TABLE versions(strategy_id INTEGER NOT NULL REFERENCES strategies(id),
                    version INTEGER NOT NULL CHECK(version > 0), sha256 TEXT NOT NULL UNIQUE,
                    file_body TEXT NOT NULL, author_model TEXT NOT NULL, intent TEXT NOT NULL,
                    inputs TEXT NOT NULL CHECK(json_valid(inputs)), created_at_ms INTEGER NOT NULL,
                    PRIMARY KEY(strategy_id, version));
                CREATE TABLE results(id INTEGER PRIMARY KEY, sha256 TEXT NOT NULL REFERENCES versions(sha256),
                    run_kind TEXT NOT NULL CHECK(run_kind IN ('replay','shadow','paper')),
                    session_date TEXT NOT NULL, regime TEXT,
                    metrics TEXT NOT NULL CHECK(json_valid(metrics)), recorded_at_ms INTEGER NOT NULL);
                CREATE INDEX results_lookup ON results(sha256, run_kind, regime);")?;
            for table in ["strategies", "versions", "results"] {
                for operation in ["UPDATE", "DELETE"] {
                    tx.execute_batch(&format!(
                        "CREATE TRIGGER {table}_no_{operation} BEFORE {operation} ON {table}
                        BEGIN SELECT RAISE(ABORT, 'append only'); END;"
                    ))?;
                }
            }
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
            tx.pragma_update(None, "user_version", 1)?;
        } else if id != APPLICATION_ID || !matches!(version, 1 | 2) {
            return Err(Error::WrongDatabase);
        }
        tx.commit()?;
        connection.pragma_update(None, "foreign_keys", true)?;
        Ok(Self { connection, limits })
    }
    /// Hashes exact UTF-8 source bytes. Repeated content retains original metadata.
    pub fn add_version(&mut self, input: NewVersion<'_>) -> Result<Version> {
        if input.file_body.len() > self.limits.max_body_bytes {
            return Err(Error::SizeLimit);
        }
        if input.name.trim().is_empty()
            || input.file_body.is_empty()
            || input.author_model.trim().is_empty()
            || input.intent.trim().is_empty()
            || input.created_at_ms < 0
        {
            return Err(Error::InvalidInput);
        }
        let sha = Sha256::digest(input.file_body.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx.query_row(
            "SELECT s.name FROM versions v JOIN strategies s ON s.id=v.strategy_id WHERE sha256=?1",
            [&sha], |r| r.get(0)).optional()?;
        if let Some(name) = existing {
            if name != input.name {
                return Err(Error::HashNameConflict);
            }
        } else {
            tx.execute(
                "INSERT INTO strategies(name) VALUES(?1) ON CONFLICT(name) DO NOTHING",
                [input.name],
            )?;
            let id: i64 = tx.query_row(
                "SELECT id FROM strategies WHERE name=?1",
                [input.name],
                |r| r.get(0),
            )?;
            tx.execute("INSERT INTO versions SELECT ?1, COALESCE(MAX(version),0)+1, ?2, ?3, ?4, ?5, ?6, ?7 FROM versions WHERE strategy_id=?1",
                params![id, sha, input.file_body, input.author_model, input.intent, input.inputs.to_string(), input.created_at_ms])?;
        }
        tx.commit()?;
        self.versions(input.name)?
            .into_iter()
            .find(|v| v.sha256 == sha)
            .ok_or(Error::Storage)
    }
    pub fn record_result(&self, result: &RunResult) -> Result<()> {
        if result.metrics.to_string().len() > self.limits.max_metrics_bytes {
            return Err(Error::SizeLimit);
        }
        if !valid_kind(&result.run_kind)
            || result.recorded_at_ms < 0
            || result.regime.as_ref().is_some_and(|r| r.trim().is_empty())
            || chrono::NaiveDate::parse_from_str(&result.session_date, "%Y-%m-%d")
                .map_or(true, |d| {
                    d.format("%Y-%m-%d").to_string() != result.session_date
                })
            || !valid_metrics(&result.metrics)
        {
            return Err(Error::InvalidInput);
        }
        self.connection.execute("INSERT INTO results(sha256,run_kind,session_date,regime,metrics,recorded_at_ms) VALUES(?1,?2,?3,?4,?5,?6)",
            params![result.sha256, result.run_kind, result.session_date, result.regime, result.metrics.to_string(), result.recorded_at_ms])?;
        Ok(())
    }
    /// Read all candidate identities and their immutable evaluation history.
    pub fn all_versions_with_results(&self) -> Result<Vec<(String, String, Vec<RunResult>)>> {
        let mut stmt = self.connection.prepare(
            "SELECT sha256,name FROM versions JOIN strategies ON strategies.id=strategy_id ORDER BY sha256"
        )?;
        let identities = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        identities
            .into_iter()
            .map(|(sha, name)| {
                let results = self.results(&sha, None, None)?;
                Ok((sha, name, results))
            })
            .collect()
    }
    pub fn versions(&self, name: &str) -> Result<Vec<Version>> {
        let mut stmt = self.connection.prepare("SELECT strategy_id,version,name,sha256,file_body,author_model,intent,inputs,created_at_ms FROM versions JOIN strategies ON strategies.id=strategy_id WHERE name=?1 ORDER BY version")?;
        let rows = stmt.query_map([name], |r| {
            Ok(Version {
                strategy_id: r.get(0)?,
                version: r.get(1)?,
                name: r.get(2)?,
                sha256: r.get(3)?,
                file_body: r.get(4)?,
                author_model: r.get(5)?,
                intent: r.get(6)?,
                inputs: json_column(r, 7)?,
                created_at_ms: r.get(8)?,
            })
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Into::into)
    }
    /// Read exactly one version by its lowercase/uppercase SHA-256 identity.
    /// Missing, duplicate/tampered, non-UTF-8 and oversize rows fail closed.
    pub fn version_by_sha(&self, sha: &str) -> Result<Version> {
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::InvalidInput);
        }
        let sha = sha.to_ascii_lowercase();
        let count: i64 = self.connection.query_row(
            "SELECT count(*) FROM versions WHERE sha256=?1",
            [&sha],
            |r| r.get(0),
        )?;
        if count == 0 {
            return Err(Error::NotFound);
        }
        if count > 1 {
            return Err(Error::Ambiguous);
        }
        // Lengths are checked before the payload columns are materialized so an
        // oversized source/metadata row cannot force an unbounded allocation.
        let mut stmt = self.connection.prepare(
            "SELECT s.name,v.strategy_id,v.version,v.sha256,v.file_body,v.author_model,
                    v.intent,v.inputs,v.created_at_ms,
                    length(CAST(v.file_body AS BLOB)),
                    length(CAST(s.name AS BLOB)) + length(CAST(v.author_model AS BLOB))
                        + length(CAST(v.intent AS BLOB)) + length(CAST(v.inputs AS BLOB))
             FROM versions v JOIN strategies s ON s.id=v.strategy_id
             WHERE v.sha256=?1",
        )?;
        let mut rows = stmt.query([&sha])?;
        let Some(row) = rows.next()? else {
            return Err(Error::NotFound);
        };
        let body_bytes: i64 = row.get(9)?;
        let metadata_bytes: i64 = row.get(10)?;
        if !(0..=self.limits.max_body_bytes as i64).contains(&body_bytes)
            || !(0..=self.limits.max_metrics_bytes as i64).contains(&metadata_bytes)
        {
            return Err(Error::SizeLimit);
        }
        let name = lookup_text(row, 0)?.to_owned();
        let strategy_id: i64 = row.get(1)?;
        let version: i64 = row.get(2)?;
        let stored_sha = lookup_text(row, 3)?.to_owned();
        let body = lookup_text(row, 4)?.to_owned();
        let author_model = lookup_text(row, 5)?.to_owned();
        let intent = lookup_text(row, 6)?.to_owned();
        let inputs = serde_json::from_str(lookup_text(row, 7)?).map_err(|_| Error::Storage)?;
        let created_at_ms: i64 = row.get(8)?;
        let actual_metadata_bytes = name.len()
            + author_model.len()
            + intent.len()
            + serde_json::to_string(&inputs)
                .map_err(|_| Error::Storage)?
                .len();
        if body.len() > self.limits.max_body_bytes
            || actual_metadata_bytes > self.limits.max_metrics_bytes
        {
            return Err(Error::SizeLimit);
        }
        let actual = Sha256::digest(body.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        if actual != sha || stored_sha != sha {
            return Err(Error::HashMismatch);
        }
        Ok(Version {
            strategy_id,
            version,
            name,
            sha256: stored_sha,
            file_body: body,
            author_model,
            intent,
            inputs,
            created_at_ms,
        })
    }
    /// None means no filter; a regime string selects exact matching non-null rows.
    pub fn results(
        &self,
        sha: &str,
        run_kind: Option<&str>,
        regime: Option<&str>,
    ) -> Result<Vec<RunResult>> {
        if run_kind.is_some_and(|k| !valid_kind(k)) {
            return Err(Error::InvalidInput);
        }
        let mut stmt = self.connection.prepare("SELECT sha256,run_kind,session_date,regime,metrics,recorded_at_ms FROM results WHERE sha256=?1 AND (?2 IS NULL OR run_kind=?2) AND (?3 IS NULL OR regime=?3) ORDER BY id")?;
        let rows = stmt.query_map(params![sha, run_kind, regime], |r| {
            Ok(RunResult {
                sha256: r.get(0)?,
                run_kind: r.get(1)?,
                session_date: r.get(2)?,
                regime: r.get(3)?,
                metrics: json_column(r, 4)?,
                recorded_at_ms: r.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(Into::into)
    }
}
fn valid_metrics(metrics: &Value) -> bool {
    metrics.get("trades").and_then(Value::as_u64).is_some()
        && ["net_pnl_after_costs", "expectancy", "max_drawdown"]
            .iter()
            .all(|key| {
                metrics
                    .get(key)
                    .and_then(Value::as_f64)
                    .is_some_and(f64::is_finite)
            })
}
fn valid_kind(kind: &str) -> bool {
    matches!(kind, "replay" | "shadow" | "paper")
}
fn json_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Value> {
    let text = text_column(row, index)?;
    serde_json::from_str(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}
fn text_column<'a>(row: &'a rusqlite::Row<'_>, index: usize) -> rusqlite::Result<&'a str> {
    match row.get_ref(index)? {
        rusqlite::types::ValueRef::Text(bytes) => std::str::from_utf8(bytes).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        }),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "non-text column",
            )),
        )),
    }
}
fn lookup_text<'a>(row: &'a rusqlite::Row<'_>, index: usize) -> Result<&'a str> {
    match row.get_ref(index)? {
        rusqlite::types::ValueRef::Text(bytes) => {
            std::str::from_utf8(bytes).map_err(|_| Error::InvalidUtf8)
        }
        _ => Err(Error::InvalidUtf8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn add(db: &mut Library, name: &str, body: &str) -> Result<Version> {
        db.add_version(NewVersion {
            name,
            file_body: body,
            author_model: "fixture-model",
            intent: "fixture intent",
            inputs: &json!({"window": 20}),
            created_at_ms: 1,
        })
    }
    #[test]
    fn version_history_is_idempotent_append_only_and_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut db = Library::open(&path).unwrap();
        let first = add(&mut db, "a", "first").unwrap();
        assert_eq!(first.version, 1);
        assert_eq!(
            first.sha256,
            "a7937b64b8caa58f03721bb6bacf5c78cb235febe0e70b1b84cd99541461a08e"
        );
        assert_eq!(add(&mut db, "a", "first").unwrap(), first);
        assert_eq!(add(&mut db, "a", "second").unwrap().version, 2);
        assert_eq!(add(&mut db, "b", "first"), Err(Error::HashNameConflict));
        assert!(db.versions("b").unwrap().is_empty());
        for sql in [
            "UPDATE versions SET intent='changed'",
            "DELETE FROM versions",
            "DELETE FROM strategies",
        ] {
            assert!(db.connection.execute(sql, []).is_err());
        }
        drop(db);
        let db = Library::open(&path).unwrap();
        assert_eq!(db.versions("a").unwrap()[0], first);
        assert_eq!(db.versions("a").unwrap().len(), 2);
    }
    #[test]
    fn exact_version_by_sha_reads_source_and_rejects_bad_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let mut db = Library::open(&path).unwrap();
        let version = add(&mut db, "a", "body").unwrap();
        assert_eq!(db.version_by_sha(&version.sha256).unwrap(), version);
        assert_eq!(
            db.version_by_sha(&version.sha256.to_ascii_uppercase())
                .unwrap(),
            version
        );
        assert_eq!(db.version_by_sha("bad"), Err(Error::InvalidInput));
        assert_eq!(db.version_by_sha(&"0".repeat(64)), Err(Error::NotFound));
        drop(db);
        let read_only = Library::open_read_only(&path).unwrap();
        assert_eq!(read_only.version_by_sha(&version.sha256).unwrap(), version);
        drop(read_only);
        // Non-UTF-8 source is rejected without exposing bytes.
        let db = Library::open(&path).unwrap();
        db.connection
            .execute("INSERT INTO strategies(name) VALUES('bad')", [])
            .unwrap();
        let id: i64 = db
            .connection
            .query_row("SELECT id FROM strategies WHERE name='bad'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let bad_sha = "b".repeat(64);
        db.connection
            .execute(
                "INSERT INTO versions(strategy_id,version,sha256,file_body,author_model,intent,inputs,created_at_ms)
                 VALUES(?1,1,?2,CAST(x'ff' AS TEXT),'m','i','{}',1)",
                params![id, bad_sha],
            )
            .unwrap();
        assert_eq!(db.version_by_sha(&bad_sha), Err(Error::InvalidUtf8));
        // Tampered stored source no longer matches its immutable identity.
        db.connection
            .execute_batch("DROP TRIGGER versions_no_UPDATE")
            .unwrap();
        db.connection
            .execute(
                "UPDATE versions SET file_body='tampered' WHERE sha256=?1",
                [&version.sha256],
            )
            .unwrap();
        assert_eq!(db.version_by_sha(&version.sha256), Err(Error::HashMismatch));
    }
    #[test]
    fn version_by_sha_enforces_source_metadata_and_ambiguity_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        {
            let mut db = Library::open_with_limits(
                &path,
                LibraryLimits {
                    max_body_bytes: 4,
                    ..Default::default()
                },
            )
            .unwrap();
            add(&mut db, "a", "four").unwrap();
        }
        let sha = {
            let db = Library::open_read_only(&path).unwrap();
            db.versions("a").unwrap()[0].sha256.clone()
        };
        let db = Library::open_with_limits(
            &path,
            LibraryLimits {
                max_body_bytes: 3,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(db.version_by_sha(&sha), Err(Error::SizeLimit));
        let metadata_path = dir.path().join("metadata.sqlite");
        let mut db = Library::open_with_limits(
            &metadata_path,
            LibraryLimits {
                max_metrics_bytes: 8,
                ..Default::default()
            },
        )
        .unwrap();
        let version = db
            .add_version(NewVersion {
                name: "a",
                file_body: "four",
                author_model: "model",
                intent: "intent",
                inputs: &json!({"window": 20}),
                created_at_ms: 1,
            })
            .unwrap();
        assert_eq!(db.version_by_sha(&version.sha256), Err(Error::SizeLimit));
        // A database that bypasses the unique index fails closed as ambiguous.
        let ambiguous_path = dir.path().join("ambiguous.sqlite");
        {
            let raw = Connection::open(&ambiguous_path).unwrap();
            raw.execute_batch(
                "CREATE TABLE strategies(id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                 CREATE TABLE versions(strategy_id INTEGER NOT NULL, version INTEGER NOT NULL,
                    sha256 TEXT NOT NULL, file_body TEXT NOT NULL, author_model TEXT NOT NULL,
                    intent TEXT NOT NULL, inputs TEXT NOT NULL, created_at_ms INTEGER NOT NULL);",
            )
            .unwrap();
            raw.execute("INSERT INTO strategies(id,name) VALUES(1,'a'),(2,'b')", [])
                .unwrap();
            for id in [1, 2] {
                raw.execute(
                    "INSERT INTO versions VALUES(?1,1,?2,'four','m','i','{}',1)",
                    params![id, &version.sha256],
                )
                .unwrap();
            }
            raw.pragma_update(None, "application_id", APPLICATION_ID)
                .unwrap();
            raw.pragma_update(None, "user_version", 1).unwrap();
        }
        let db = Library::open_read_only(&ambiguous_path).unwrap();
        assert_eq!(db.version_by_sha(&version.sha256), Err(Error::Ambiguous));
    }
    #[test]
    fn results_validate_and_filter_without_changing_history() {
        let mut db = Library::open(":memory:").unwrap();
        let version = add(&mut db, "a", "body").unwrap();
        let mut result = RunResult {
            sha256: version.sha256.clone(),
            run_kind: "replay".into(),
            session_date: "2026-10-23".into(),
            regime: None,
            metrics: json!({"trades":2,"net_pnl_after_costs":-3.0,"expectancy":-1.5,"max_drawdown":4}),
            recorded_at_ms: 2,
        };
        db.record_result(&result).unwrap();
        result.run_kind = "shadow".into();
        result.regime = Some("trend".into());
        db.record_result(&result).unwrap();
        result.run_kind = "paper".into();
        result.regime = Some("range".into());
        db.record_result(&result).unwrap();
        assert_eq!(db.results(&version.sha256, None, None).unwrap().len(), 3);
        let all = db.all_versions_with_results().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(
            (&all[0].0, all[0].1.as_str(), all[0].2.len()),
            (&version.sha256, "a", 3)
        );
        assert_eq!(
            db.results(&version.sha256, Some("shadow"), Some("trend"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db.results(&version.sha256, None, Some("range")).unwrap(),
            vec![result.clone()]
        );
        assert!(
            db.results(&version.sha256, Some("replay"), Some("trend"))
                .unwrap()
                .is_empty()
        );
        assert!(db.results("missing", None, None).unwrap().is_empty());
        result.run_kind = "live".into();
        assert_eq!(db.record_result(&result), Err(Error::InvalidInput));
        assert_eq!(
            db.results(&version.sha256, Some("live"), None),
            Err(Error::InvalidInput)
        );
        result.run_kind = "paper".into();
        result.session_date = "2026-02-30".into();
        assert_eq!(db.record_result(&result), Err(Error::InvalidInput));
        result.session_date = "2026-10-23".into();
        result.metrics = json!({"trades":-1});
        assert_eq!(db.record_result(&result), Err(Error::InvalidInput));
        result.metrics =
            json!({"trades":0,"net_pnl_after_costs":0,"expectancy":0,"max_drawdown":0});
        result.sha256 = "unknown".into();
        assert_eq!(db.record_result(&result), Err(Error::Storage));
        assert_eq!(db.results(&version.sha256, None, None).unwrap().len(), 3);
        for sql in ["DELETE FROM results", "UPDATE results SET run_kind='paper'"] {
            assert!(db.connection.execute(sql, []).is_err());
        }
    }
    #[test]
    fn limits_and_database_identity_fail_closed() {
        let limits: LibraryLimits = serde_json::from_str("{\"max_body_bytes\":4}").unwrap();
        assert_eq!(limits.max_metrics_bytes, 65_536);
        let mut db = Library::open_with_limits(
            ":memory:",
            LibraryLimits {
                max_metrics_bytes: 2,
                ..limits
            },
        )
        .unwrap();
        add(&mut db, "a", "four").unwrap();
        assert_eq!(add(&mut db, "a", "fives"), Err(Error::SizeLimit));
        let result = RunResult {
            sha256: "unknown".into(),
            run_kind: "replay".into(),
            session_date: "2026-10-23".into(),
            regime: None,
            metrics: json!({"trades":0}),
            recorded_at_ms: 1,
        };
        assert_eq!(db.record_result(&result), Err(Error::SizeLimit));
        assert_eq!(db.versions("a").unwrap().len(), 1);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.sqlite");
        let other = Connection::open(&path).unwrap();
        other.execute("CREATE TABLE book (id INTEGER)", []).unwrap();
        assert!(matches!(Library::open(&path), Err(Error::WrongDatabase)));
        assert_eq!(
            other
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let path = dir.path().join("future.sqlite");
        let db = Library::open(&path).unwrap();
        db.connection
            .pragma_update(None, "user_version", 3)
            .unwrap();
        assert!(matches!(Library::open(&path), Err(Error::WrongDatabase)));
        assert!(matches!(
            Library::open_read_only(&path),
            Err(Error::WrongDatabase)
        ));
    }
}
