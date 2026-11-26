//! The single-slot job queue: enqueue, claim, lease expiry, cancel, completion.
use super::*;

pub fn initialize(db: &mut Connection) -> Result<()> {
    let tx = immediate(db)?;
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if count != 0 {
        return Err(Error::Storage);
    }
    tx.execute_batch("CREATE TABLE scripts(code TEXT NOT NULL,runtime TEXT NOT NULL,PRIMARY KEY(code,runtime));
        CREATE TABLE jobs(id TEXT PRIMARY KEY,spec TEXT NOT NULL,state TEXT NOT NULL,attempts INTEGER NOT NULL,lease TEXT,until_ms INTEGER,result TEXT);
        CREATE TABLE leases(token TEXT PRIMARY KEY,job TEXT NOT NULL,attempt INTEGER NOT NULL);
        CREATE TABLE job_events(seq INTEGER PRIMARY KEY,job TEXT NOT NULL,event TEXT NOT NULL,at_ms INTEGER NOT NULL,attempt INTEGER NOT NULL);").map_err(|_|Error::Storage)?;
    tx.commit().map_err(|_| Error::Storage)
}
/// Caller must be the reviewed trusted registry owner. Models never receive this API.
pub fn register_reviewed(db: &mut Connection, code: &str, runtime: &str) -> Result<()> {
    if !valid_hash(code) || !valid_hash(runtime) {
        return Err(Error::Invalid);
    }
    db.execute(
        "INSERT OR IGNORE INTO scripts VALUES(?,?)",
        params![code, runtime],
    )
    .map_err(|_| Error::Storage)?;
    Ok(())
}
pub(super) fn immediate(db: &mut Connection) -> Result<Transaction<'_>> {
    let sync: i64 = db
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .map_err(|_| Error::Storage)?;
    if sync < 2 {
        return Err(Error::Storage);
    }
    db.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| Error::Storage)
}
pub(super) fn event(
    tx: &Transaction<'_>,
    id: &str,
    name: &str,
    now: i64,
    attempt: u32,
) -> Result<()> {
    let last: Option<i64> = tx
        .query_row("SELECT max(at_ms) FROM job_events WHERE job=?", [id], |r| {
            r.get(0)
        })
        .map_err(|_| Error::Storage)?;
    if last.is_some_and(|last| now < last) {
        return Err(Error::Invalid);
    }
    tx.execute(
        "INSERT INTO job_events(job,event,at_ms,attempt) VALUES(?,?,?,?)",
        params![id, name, now, attempt],
    )
    .map_err(|_| Error::Storage)?;
    Ok(())
}
pub fn enqueue(db: &mut Connection, spec: &Spec, now: i64) -> Result<Job> {
    spec.validate()?;
    if now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let job = enqueue_tx(&tx, spec, now)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}

pub(super) fn enqueue_tx(tx: &Transaction<'_>, spec: &Spec, now: i64) -> Result<Job> {
    spec.validate()?;
    if now < 0 {
        return Err(Error::Invalid);
    }
    if let Some(old) = read(tx, &spec.id)? {
        if &old.spec == spec {
            return Ok(old);
        }
        return Err(Error::Collision);
    }
    let trials: i64 = tx
        .query_row(
            "SELECT count(*) FROM jobs WHERE json_extract(spec,'$.review_id')=?",
            [&spec.review_id],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if trials >= 4 {
        return Err(Error::Capacity);
    }
    if spec.capability == SpecCapability::RegisteredAnalysis {
        let registered: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM scripts WHERE code=? AND runtime=?)",
                params![spec.script_hash, spec.runtime_hash],
                |r| r.get(0),
            )
            .map_err(|_| Error::Storage)?;
        if !registered {
            return Err(Error::Unregistered);
        }
    }
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM jobs WHERE state IN ('Pending','Running','Unknown','CancelRequested')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if count >= 500 {
        return Err(Error::Capacity);
    }
    tx.execute(
        "INSERT INTO jobs VALUES(?,?,'Pending',0,NULL,NULL,NULL)",
        params![
            spec.id,
            serde_json::to_string(spec).map_err(|_| Error::Invalid)?
        ],
    )
    .map_err(|_| Error::Storage)?;
    event(tx, &spec.id, "enqueued", now, 0)?;
    let job = read(tx, &spec.id)?.ok_or(Error::Missing)?;
    Ok(job)
}
pub fn load(db: &mut Connection, id: &str) -> Result<Job> {
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    let job = read(&tx, id)?.ok_or(Error::Missing);
    tx.rollback().map_err(|_| Error::Storage)?;
    job
}
pub fn claim(db: &mut Connection, id: &str, lease: &str, now: i64) -> Result<Job> {
    if !valid_hash(lease) || now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.state != State::Pending {
        return Err(Error::State);
    }
    if old.attempts >= old.spec.max_attempts {
        return Err(Error::Attempts);
    }
    tx.execute(
        "INSERT INTO leases VALUES(?,?,?)",
        params![lease, id, old.attempts + 1],
    )
    .map_err(|_| Error::Lease)?;
    let running: i64 = tx
        .query_row(
            "SELECT count(*) FROM jobs WHERE state IN ('Running','Unknown','CancelRequested')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if running > 0 {
        return Err(Error::Capacity);
    }
    let until = now
        .checked_add(i64::from(old.spec.wall_millis))
        .ok_or(Error::Invalid)?;
    tx.execute(
        "UPDATE jobs SET state='Running',attempts=attempts+1,lease=?,until_ms=? WHERE id=?",
        params![lease, until, id],
    )
    .map_err(|_| Error::Storage)?;
    event(&tx, id, "claimed", now, old.attempts + 1)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
pub fn expire(db: &mut Connection, id: &str, now: i64) -> Result<Job> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.state != State::Running || old.until.is_none_or(|until| now < until) {
        return Err(Error::State);
    }
    tx.execute("UPDATE jobs SET state='Unknown' WHERE id=?", [id])
        .map_err(|_| Error::Storage)?;
    event(&tx, id, "lease_expired_unknown", now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
/// Record cancellation intent only. Running/unknown workers retain their slot.
pub fn request_cancel(db: &mut Connection, id: &str, now: i64) -> Result<Job> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if matches!(old.state, State::Cancelled | State::CancelRequested) {
        return Ok(old);
    }
    let next = match old.state {
        State::Pending | State::Terminated => "Cancelled",
        State::Running | State::Unknown => "CancelRequested",
        _ => return Err(Error::State),
    };
    tx.execute("UPDATE jobs SET state=? WHERE id=?", params![next, id])
        .map_err(|_| Error::Storage)?;
    event(&tx, id, "cancel_requested", now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
/// The trusted qualified worker must stop the WHOLE sandbox before calling this.
/// A digest records its receipt; fixtures/caller declarations do not qualify containment.
pub fn record_termination(
    db: &mut Connection,
    id: &str,
    lease: &str,
    receipt: &str,
    now: i64,
) -> Result<Job> {
    if !valid_hash(lease) || !valid_hash(receipt) || now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.lease.as_deref() != Some(lease) {
        return Err(Error::Lease);
    }
    let label = format!("terminated:{receipt}");
    if matches!(old.state, State::Terminated | State::Cancelled) {
        let prior:Option<String>=tx.query_row("SELECT event FROM job_events WHERE job=? AND attempt=? AND event LIKE 'terminated:%' ORDER BY seq DESC LIMIT 1",params![id,old.attempts],|r|r.get(0)).optional().map_err(|_|Error::Storage)?;
        return if prior.as_deref() == Some(&label) {
            Ok(old)
        } else {
            Err(Error::Collision)
        };
    }
    let next = match old.state {
        State::Unknown => "Terminated",
        State::CancelRequested => "Cancelled",
        _ => return Err(Error::State),
    };
    tx.execute("UPDATE jobs SET state=? WHERE id=?", params![next, id])
        .map_err(|_| Error::Storage)?;
    event(&tx, id, &label, now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
pub fn complete(db: &mut Connection, id: &str, lease: &str, result: &str, now: i64) -> Result<Job> {
    complete_with_clock(db, id, lease, result, || now)
}
/// Production workers sample the acceptance clock after acquiring the writer
/// lock. Waiting for SQLite must not extend an expired compute lease.
pub fn complete_with_clock(
    db: &mut Connection,
    id: &str,
    lease: &str,
    result: &str,
    clock: impl FnOnce() -> i64,
) -> Result<Job> {
    if !valid_hash(lease) || !valid_hash(result) {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let now = clock();
    if now < 0 {
        return Err(Error::Invalid);
    }
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.lease.as_deref() != Some(lease) {
        return Err(Error::Lease);
    }
    if old.state == State::Succeeded {
        return if old.result_hash.as_deref() == Some(result) {
            Ok(old)
        } else {
            Err(Error::Collision)
        };
    }
    if old.state != State::Running
        || old
            .until
            .is_none_or(|until| now >= until || now < until - i64::from(old.spec.wall_millis))
    {
        return Err(Error::Lease);
    }
    tx.execute(
        "UPDATE jobs SET state='Succeeded',result=? WHERE id=?",
        params![result, id],
    )
    .map_err(|_| Error::Storage)?;
    event(&tx, id, "completed", now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
pub(super) fn read(tx: &Transaction<'_>, id: &str) -> Result<Option<Job>> {
    if !valid_hash(id) {
        return Err(Error::Invalid);
    }
    type StoredRow = (
        String,
        String,
        u32,
        Option<String>,
        Option<i64>,
        Option<String>,
    );
    let row: Option<StoredRow>=tx.query_row("SELECT spec,state,attempts,lease,until_ms,result FROM jobs WHERE id=? AND length(spec)<=16384",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().map_err(|_|Error::Storage)?;
    let Some((spec, state, attempts, lease, until, result_hash)) = row else {
        return Ok(None);
    };
    let spec: Spec = serde_json::from_str(&spec).map_err(|_| Error::Invalid)?;
    spec.validate()?;
    let state = match state.as_str() {
        "Pending" => State::Pending,
        "Running" => State::Running,
        "Unknown" => State::Unknown,
        "CancelRequested" => State::CancelRequested,
        "Terminated" => State::Terminated,
        "Succeeded" => State::Succeeded,
        "Cancelled" => State::Cancelled,
        "Refused" => State::Refused,
        _ => return Err(Error::Invalid),
    };
    if spec.id != id
        || attempts > spec.max_attempts
        || lease.as_ref().is_some_and(|s| !valid_hash(s))
        || result_hash.as_ref().is_some_and(|s| !valid_hash(s))
    {
        return Err(Error::Invalid);
    }
    let lease_valid = lease.is_some()
        && until.is_some_and(|at| at >= i64::from(spec.wall_millis))
        && attempts > 0;
    if match state {
        State::Pending => lease.is_some() || until.is_some() || result_hash.is_some(),
        State::Running | State::Unknown | State::CancelRequested | State::Terminated => {
            !lease_valid || result_hash.is_some()
        }
        State::Succeeded => !lease_valid || result_hash.is_none(),
        State::Cancelled => {
            result_hash.is_some()
                || if lease.is_none() {
                    until.is_some()
                } else {
                    !lease_valid
                }
        }
        State::Refused => {
            lease.is_some() || until.is_some() || result_hash.is_some() || attempts != 0
        }
    } {
        return Err(Error::Invalid);
    }
    if let Some(token) = lease.as_ref() {
        let owner: Option<(String, u32)> = tx
            .query_row(
                "SELECT job,attempt FROM leases WHERE token=?",
                [token],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| Error::Storage)?;
        if owner != Some((id.to_owned(), attempts)) {
            return Err(Error::Invalid);
        }
    }
    if state == State::Terminated || (state == State::Cancelled && lease.is_some()) {
        let receipt:Option<String>=tx.query_row("SELECT event FROM job_events WHERE job=? AND attempt=? AND event LIKE 'terminated:%' ORDER BY seq DESC LIMIT 1",params![id,attempts],|r|r.get(0)).optional().map_err(|_|Error::Storage)?;
        if !receipt
            .as_deref()
            .and_then(|value| value.strip_prefix("terminated:"))
            .is_some_and(valid_hash)
        {
            return Err(Error::Invalid);
        }
    }
    Ok(Some(Job {
        spec,
        state,
        attempts,
        lease,
        until,
        result_hash,
    }))
}
