//! Durable controller checkpoints in a separately initialized database.
//! No path discovery, journal migration, model calls or trading effects.
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use sadr_core::controller::{Book, DayId, Phase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Storage,
    InvalidState,
    DuplicateDay,
    Conflict,
    Missing,
    Transition,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub revision: i64,
    pub book: Book,
}

/// Only an empty, explicitly supplied database can become a controller store.
pub fn initialize(db: &mut Connection) -> Result<(), Error> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| Error::Storage)?;
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
    tx.execute_batch("CREATE TABLE controller_days(broker TEXT NOT NULL, account TEXT NOT NULL, day TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0), body TEXT NOT NULL, PRIMARY KEY(broker,account,day));").map_err(|_| Error::Storage)?;
    tx.commit().map_err(|_| Error::Storage)
}

pub fn create(db: &mut Connection, book: Book) -> Result<Checkpoint, Error> {
    book.validate().map_err(|_| Error::InvalidState)?;
    if book.phase() != Phase::Boot {
        return Err(Error::InvalidState);
    }
    let body = serde_json::to_string(&book).map_err(|_| Error::InvalidState)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| Error::Storage)?;
    let id = book.id();
    if read(&tx, id)?.is_some() {
        return Err(Error::DuplicateDay);
    }
    tx.execute(
        "INSERT INTO controller_days VALUES(?,?,?,1,?)",
        params![id.broker, id.account, id.date.to_string(), body],
    )
    .map_err(|_| Error::Storage)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(Checkpoint { revision: 1, book })
}

pub fn load(db: &mut Connection, id: &DayId) -> Result<Checkpoint, Error> {
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    let result = read(&tx, id)?.ok_or(Error::Missing);
    // End the read snapshot before returning state to a caller that may act.
    tx.rollback().map_err(|_| Error::Storage)?;
    result
}

/// Optimistic revision plus SQLite's write lock rejects duplicate controllers.
/// The closure may only transform state; it must never perform external effects.
pub fn transition(
    db: &mut Connection,
    id: &DayId,
    expected: i64,
    apply: impl FnOnce(&mut Book) -> Result<(), sadr_core::controller::ControlError>,
) -> Result<Checkpoint, Error> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| Error::Storage)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.revision != expected {
        return Err(Error::Conflict);
    }
    let mut book = old.book.clone();
    apply(&mut book).map_err(|_| Error::Transition)?;
    book.validate().map_err(|_| Error::InvalidState)?;
    if book.id() != id || book.generation() < old.book.generation() {
        return Err(Error::InvalidState);
    }
    let revision = expected.checked_add(1).ok_or(Error::InvalidState)?;
    tx.execute("UPDATE controller_days SET revision=?,body=? WHERE broker=? AND account=? AND day=? AND revision=?",params![revision,serde_json::to_string(&book).map_err(|_| Error::InvalidState)?,id.broker,id.account,id.date.to_string(),expected]).map_err(|_| Error::Storage)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(Checkpoint { revision, book })
}
fn read(tx: &Transaction<'_>, id: &DayId) -> Result<Option<Checkpoint>, Error> {
    let mut stmt = tx
        .prepare("SELECT revision,body FROM controller_days WHERE broker=? AND account=? AND day=?")
        .map_err(|_| Error::Storage)?;
    let mut rows = stmt
        .query(params![id.broker, id.account, id.date.to_string()])
        .map_err(|_| Error::Storage)?;
    let Some(row) = rows.next().map_err(|_| Error::Storage)? else {
        return Ok(None);
    };
    let revision: i64 = row.get(0).map_err(|_| Error::InvalidState)?;
    let body: String = row.get(1).map_err(|_| Error::InvalidState)?;
    if revision < 1 || body.len() > 16384 {
        return Err(Error::InvalidState);
    }
    let book: Book = serde_json::from_str(&body).map_err(|_| Error::InvalidState)?;
    book.validate().map_err(|_| Error::InvalidState)?;
    if book.id() != id {
        return Err(Error::InvalidState);
    }
    Ok(Some(Checkpoint { revision, book }))
}
