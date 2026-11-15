//! Durable controller metadata; never a replacement usage journal or invocation authority.
//! Caller must separately verify original journal file identity and active owner lease.
use crate::journal::{ModelCaps, ModelJournal};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use sadr_core::native_grant::GrantRecord;
use serde::{Deserialize, Serialize};
use std::fmt;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("native_binding_invalid")
    }
}
impl std::error::Error for Error {}
/// journal_identity is a trusted caller's original-file identity digest, not proof thereof.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub account_hash: String,
    pub grant_id: String,
    pub journal_identity: String,
    pub caps: ModelCaps,
}
impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeBindingIdentity(redacted)")
    }
}
fn hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn candidate(
    identity: &Identity,
    grant: &GrantRecord,
    journal: &ModelJournal,
) -> Result<i64, Error> {
    if !hex(&identity.account_hash, 64)
        || !hex(&identity.grant_id, 32)
        || !hex(&identity.journal_identity, 64)
        || !grant.matches_binding(&identity.account_hash, &identity.grant_id)
        || journal.account_hash() != identity.account_hash
        || journal.caps() != identity.caps
        || identity.caps.daily_micro_usd != 0
        || identity.caps.monthly_micro_usd != 0
    {
        return Err(Error);
    }
    Ok(journal.attempts().last().map(|a| a.id()).unwrap_or(0))
}
pub fn initialize(db: &Connection) -> Result<(), Error> {
    if !db.is_autocommit() {
        return Err(Error);
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS native_binding(id INTEGER PRIMARY KEY CHECK(id=1),identity TEXT NOT NULL,high_water INTEGER NOT NULL CHECK(high_water>=0));").map_err(|_|Error)
}
/// Explicit first enrollment only; duplicate enrollment refuses instead of replacing metadata.
pub fn enroll(
    db: &mut Connection,
    identity: &Identity,
    grant: &GrantRecord,
    journal: &ModelJournal,
) -> Result<(), Error> {
    let high = candidate(identity, grant, journal)?;
    if !db.is_autocommit() {
        return Err(Error);
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| Error)?;
    let count: i64 = tx
        .query_row("SELECT COUNT(*) FROM native_binding", [], |r| r.get(0))
        .map_err(|_| Error)?;
    if count != 0 {
        return Err(Error);
    }
    tx.execute(
        "INSERT INTO native_binding(id,identity,high_water) VALUES(1,?1,?2)",
        rusqlite::params![serde_json::to_string(identity).map_err(|_| Error)?, high],
    )
    .map_err(|_| Error)?;
    tx.commit().map_err(|_| Error)
}
/// Verify exact binding and retain a monotone high-water mark atomically.
/// Never creates missing metadata, rotates grants, resets caps, or writes usage rows.
pub fn checkpoint(
    db: &mut Connection,
    identity: &Identity,
    grant: &GrantRecord,
    journal: &ModelJournal,
) -> Result<(), Error> {
    let high = candidate(identity, grant, journal)?;
    if !db.is_autocommit() {
        return Err(Error);
    }
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| Error)?;
    let (raw, prior): (String, i64) = tx
        .query_row(
            "SELECT identity,high_water FROM native_binding WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error)?
        .ok_or(Error)?;
    if raw.len() > 4096 || prior < 0 || high < prior {
        return Err(Error);
    }
    let saved: Identity =
        serde_json::from_value(sadr_core::native::decode(raw.as_bytes()).map_err(|_| Error)?)
            .map_err(|_| Error)?;
    if &saved != identity {
        return Err(Error);
    }
    let count: i64 = tx
        .query_row("SELECT COUNT(*) FROM native_binding", [], |r| r.get(0))
        .map_err(|_| Error)?;
    if count != 1 {
        return Err(Error);
    }
    tx.execute("UPDATE native_binding SET high_water=?1 WHERE id=1", [high])
        .map_err(|_| Error)?;
    tx.commit().map_err(|_| Error)
}
