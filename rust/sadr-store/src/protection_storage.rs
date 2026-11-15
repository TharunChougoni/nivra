//! Read-only persisted protection rows; no plan/file/owner or entry authority.
use crate::{
    StoreError,
    protection::{ProtectionJob, ProtectionProfile},
};
use rusqlite::{Connection, limits::Limit, types::ValueRef};
use serde_json::json;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ProtectionRows {
    profile: ProtectionProfile,
    jobs: Vec<ProtectionJob>,
    halted: bool,
}
impl ProtectionRows {
    pub fn profile(&self) -> &ProtectionProfile {
        &self.profile
    }
    pub fn jobs(&self) -> &[ProtectionJob] {
        &self.jobs
    }
    pub fn halted(&self) -> bool {
        self.halted
    }
}

/// Validate the relevant three ordinary table profiles and read one snapshot.
/// Other ordinary tables/indexes are not certified or read. No path is opened;
/// supplied file handles must be read-only and original identity remains external.
/// Read snapshots close before return. A returned projection is never a writer
/// lock, prepared-plan acceptance, owner lease or permission to resume an entry.
pub fn inspect_protection_connection(
    mut db: Connection,
    max_jobs: u64,
) -> Result<ProtectionRows, StoreError> {
    db.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .map_err(|_| StoreError::Protection)?;
    if !db.is_autocommit() || !(1..=100_000).contains(&max_jobs) {
        return Err(StoreError::Protection);
    }
    db.busy_timeout(Duration::from_millis(250))
        .map_err(|_| StoreError::Protection)?;
    db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 1_000_000)
        .map_err(|_| StoreError::Protection)?;
    db.execute_batch(
        "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA read_uncommitted=OFF;",
    )
    .map_err(|_| StoreError::Protection)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    db.progress_handler(1000, Some(move || Instant::now() >= deadline))
        .map_err(|_| StoreError::Protection)?;
    let transaction = db.transaction().map_err(|_| StoreError::Protection)?;
    let result = read_rows(&transaction, max_jobs, deadline, true);
    transaction.rollback().map_err(|_| StoreError::Protection)?;
    result
}

type Column = (&'static str, &'static str, i64, Option<&'static str>, i64);
const ACCOUNT: &[Column] = &[
    ("id", "INTEGER", 0, None, 1),
    ("cash", "TEXT", 0, None, 0),
    ("initial", "TEXT", 0, None, 0),
    ("halted", "INTEGER", 1, Some("0"), 0),
];
const PROFILE: &[Column] = &[
    ("id", "INTEGER", 0, None, 1),
    ("value", "TEXT", 1, None, 0),
    ("observations", "INTEGER", 1, None, 0),
    ("last_at", "TEXT", 0, None, 0),
];
const JOBS: &[Column] = &[
    ("seq", "INTEGER", 0, None, 1),
    ("state", "TEXT", 1, None, 0),
    ("capture", "TEXT", 0, None, 0),
    ("result", "TEXT", 0, None, 0),
    ("result_hash", "TEXT", 0, None, 0),
];

fn schema(db: &Connection, require_readonly_file: bool) -> Result<(), StoreError> {
    let mut stmt = db
        .prepare("PRAGMA database_list")
        .map_err(|_| StoreError::Schema)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Schema)?;
    let row = rows
        .next()
        .map_err(|_| StoreError::Schema)?
        .ok_or(StoreError::Schema)?;
    let name: String = row.get(1).map_err(|_| StoreError::Schema)?;
    let file: String = row.get(2).map_err(|_| StoreError::Schema)?;
    let writable_file = require_readonly_file
        && !file.is_empty()
        && !db.is_readonly("main").map_err(|_| StoreError::Schema)?;
    if name != "main" || writable_file || rows.next().map_err(|_| StoreError::Schema)?.is_some() {
        return Err(StoreError::Schema);
    }
    let mut stmt = db
        .prepare("SELECT type,name,sql FROM main.sqlite_schema LIMIT 129")
        .map_err(|_| StoreError::Schema)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Schema)?;
    let mut objects = 0;
    let mut relevant = 0;
    while let Some(row) = rows.next().map_err(|_| StoreError::Schema)? {
        objects += 1;
        if objects > 128 {
            return Err(StoreError::Bound);
        }
        let kind: String = row.get(0).map_err(|_| StoreError::Schema)?;
        let name: String = row.get(1).map_err(|_| StoreError::Schema)?;
        let sql: Option<String> = row.get(2).map_err(|_| StoreError::Schema)?;
        if kind == "table"
            && sql.as_deref().is_some_and(|s| {
                s.trim_start()
                    .to_ascii_uppercase()
                    .starts_with("CREATE TABLE ")
            })
        {
            if matches!(
                name.as_str(),
                "account" | "paper_protection_profile" | "paper_protection_jobs"
            ) {
                relevant += 1;
            }
        } else if kind != "index" {
            return Err(StoreError::Schema);
        }
    }
    if relevant != 3 {
        return Err(StoreError::Schema);
    }
    for (table, columns) in [
        ("account", ACCOUNT),
        ("paper_protection_profile", PROFILE),
        ("paper_protection_jobs", JOBS),
    ] {
        let mut stmt = db
            .prepare(&format!("PRAGMA main.table_xinfo({table})"))
            .map_err(|_| StoreError::Schema)?;
        let mut rows = stmt.query([]).map_err(|_| StoreError::Schema)?;
        for (index, (name, kind, not_null, default, pk)) in columns.iter().enumerate() {
            let row = rows
                .next()
                .map_err(|_| StoreError::Schema)?
                .ok_or(StoreError::Schema)?;
            let got: (i64, String, String, i64, Option<String>, i64, i64) = (
                row.get(0).map_err(|_| StoreError::Schema)?,
                row.get(1).map_err(|_| StoreError::Schema)?,
                row.get(2).map_err(|_| StoreError::Schema)?,
                row.get(3).map_err(|_| StoreError::Schema)?,
                row.get(4).map_err(|_| StoreError::Schema)?,
                row.get(5).map_err(|_| StoreError::Schema)?,
                row.get(6).map_err(|_| StoreError::Schema)?,
            );
            if got
                != (
                    index as i64,
                    (*name).into(),
                    (*kind).into(),
                    *not_null,
                    default.map(str::to_owned),
                    *pk,
                    0,
                )
            {
                return Err(StoreError::Schema);
            }
        }
        if rows.next().map_err(|_| StoreError::Schema)?.is_some() {
            return Err(StoreError::Schema);
        }
    }
    Ok(())
}

fn text(
    row: &rusqlite::Row<'_>,
    index: usize,
    total: &mut usize,
) -> Result<Option<String>, StoreError> {
    match row.get_ref(index).map_err(|_| StoreError::Rows)? {
        ValueRef::Null => Ok(None),
        ValueRef::Text(bytes) if bytes.len() <= 65_536 => {
            *total = total.checked_add(bytes.len()).ok_or(StoreError::Bound)?;
            if *total > 8 * 1024 * 1024 {
                return Err(StoreError::Bound);
            }
            Ok(Some(
                std::str::from_utf8(bytes)
                    .map_err(|_| StoreError::Rows)?
                    .to_owned(),
            ))
        }
        ValueRef::Text(_) => Err(StoreError::Bound),
        _ => Err(StoreError::Rows),
    }
}

/// Future writers may call this only after their own lock/clock checks. This
/// helper performs no effects and does not prove transaction kind or ownership.
pub(crate) fn read_rows(
    db: &Connection,
    max_jobs: u64,
    deadline: Instant,
    require_readonly_file: bool,
) -> Result<ProtectionRows, StoreError> {
    if db.is_autocommit() || !(1..=100_000).contains(&max_jobs) {
        return Err(StoreError::Protection);
    }
    schema(db, require_readonly_file)?;
    let mut stmt = db
        .prepare("SELECT id,halted FROM main.account LIMIT 2")
        .map_err(|_| StoreError::Rows)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Rows)?;
    let row = rows
        .next()
        .map_err(|_| StoreError::Rows)?
        .ok_or(StoreError::Rows)?;
    let id: i64 = row.get(0).map_err(|_| StoreError::Rows)?;
    let halted: i64 = row.get(1).map_err(|_| StoreError::Rows)?;
    if id != 1 || !matches!(halted, 0 | 1) || rows.next().map_err(|_| StoreError::Rows)?.is_some() {
        return Err(StoreError::Rows);
    }
    let mut total = 0;
    let mut stmt = db
        .prepare("SELECT id,value,observations,last_at FROM main.paper_protection_profile LIMIT 2")
        .map_err(|_| StoreError::Rows)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Rows)?;
    let row = rows
        .next()
        .map_err(|_| StoreError::Rows)?
        .ok_or(StoreError::Rows)?;
    let id: i64 = row.get(0).map_err(|_| StoreError::Rows)?;
    let observations: i64 = row.get(2).map_err(|_| StoreError::Rows)?;
    let profile=serde_json::from_value(json!({"value":text(row,1,&mut total)?,"observations":observations,"last_at":text(row,3,&mut total)?})).map_err(|_|StoreError::Rows)?;
    if id != 1 || rows.next().map_err(|_| StoreError::Rows)?.is_some() {
        return Err(StoreError::Rows);
    }
    let mut jobs = Vec::new();
    let mut stmt=db.prepare("SELECT seq,state,capture,result,result_hash FROM main.paper_protection_jobs ORDER BY seq LIMIT 100001").map_err(|_|StoreError::Rows)?;
    let mut rows = stmt.query([]).map_err(|_| StoreError::Rows)?;
    while let Some(row) = rows.next().map_err(|_| StoreError::Rows)? {
        if jobs.len() as u64 >= max_jobs || Instant::now() >= deadline {
            return Err(StoreError::Bound);
        }
        let seq: i64 = row.get(0).map_err(|_| StoreError::Rows)?;
        let job=serde_json::from_value(json!({"seq":seq,"state":text(row,1,&mut total)?,"capture":text(row,2,&mut total)?,"result":text(row,3,&mut total)?,"result_hash":text(row,4,&mut total)?})).map_err(|_|StoreError::Rows)?;
        jobs.push(job);
    }
    if Instant::now() >= deadline {
        return Err(StoreError::Bound);
    }
    Ok(ProtectionRows {
        profile,
        jobs,
        halted: halted == 1,
    })
}

/// Read historical dashboard evidence from one bounded query-only snapshot.
/// No config-named path is opened. The end clock is sampled after rollback, so
/// slow reads cannot make old evidence appear fresh. Errors are redacted.
pub fn inspect_protection_dashboard(
    mut db: Connection,
    end_clock: impl FnOnce() -> sadr_core::time::Timestamp,
) -> serde_json::Value {
    use crate::protection::dashboard_projection;
    use rusqlite::OptionalExtension;
    let mut read = || -> Result<_, StoreError> {
        if !db.is_autocommit() {
            return Err(StoreError::Protection);
        }
        db.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .map_err(|_| StoreError::Protection)?;
        if !db.is_readonly("main").map_err(|_| StoreError::Protection)? {
            let file: String = db
                .query_row(
                    "SELECT file FROM pragma_database_list WHERE name='main'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|_| StoreError::Protection)?;
            if !file.is_empty() {
                return Err(StoreError::Protection);
            }
        }
        db.busy_timeout(Duration::from_millis(250))
            .map_err(|_| StoreError::Protection)?;
        db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 1_000_000)
            .map_err(|_| StoreError::Protection)?;
        db.execute_batch(
            "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA read_uncommitted=OFF;",
        )
        .map_err(|_| StoreError::Protection)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        db.progress_handler(1000, Some(move || Instant::now() >= deadline))
            .map_err(|_| StoreError::Protection)?;
        let tx = db.transaction().map_err(|_| StoreError::Protection)?;
        let result = (|| {
            let required: Option<String> = tx
                .query_row(
                    "SELECT value FROM main.metadata WHERE key='paper_entry_protection_required'",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|_| StoreError::Protection)?;
            let present = |name: &str| -> Result<bool, StoreError> {
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name=? AND type='table')",
                    [name],
                    |r| r.get(0),
                )
                .map_err(|_| StoreError::Protection)
            };
            let has_guard = present("paper_entry_protection")?;
            let has_profile = present("paper_protection_profile")?;
            let has_jobs = present("paper_protection_jobs")?;
            if has_profile != has_jobs || (!has_guard && (has_profile || required.is_some())) {
                return Err(StoreError::Protection);
            }
            let guard: Option<(String, String)> = if has_guard {
                Some(
                    tx.query_row(
                        "SELECT value,config FROM main.paper_entry_protection WHERE id=1",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .map_err(|_| StoreError::Protection)?,
                )
            } else {
                None
            };
            let rows = if has_profile {
                Some(read_rows(&tx, 100_000, deadline, true)?)
            } else {
                None
            };
            Ok((required, guard, rows))
        })();
        tx.rollback().map_err(|_| StoreError::Protection)?;
        result
    };
    let result = read();
    let now = end_clock();
    match result {
        Ok((required, guard, rows)) => dashboard_projection(
            required.as_deref(),
            guard.as_ref().map(|(h, c)| (h.as_str(), c.as_str())),
            rows.as_ref().map(|r| (&r.profile, r.jobs.as_slice())),
            now,
        ),
        Err(_) => dashboard_projection(Some("invalid"), None, None, now),
    }
}
