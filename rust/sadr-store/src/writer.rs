//! Atomic paper-ledger effects on a caller-supplied connection.
//!
//! Cash, intent state, reservation and the audit row commit in one
//! `BEGIN IMMEDIATE` transaction. This module does not open paths, create or
//! migrate a schema, authenticate a file or owner, or treat a SQLite commit as
//! exactly-once broker execution. Nested savepoints stay outside this slice.
//! New entries recheck caller-supplied protection evidence after the lock.
//! Missing or rejected evidence commits a halt and an operator alert and does
//! not insert the entry. There is no historical bypass.

use crate::{
    StoreError,
    archive::LegacyUpstoxCapture,
    legacy::LegacyCandidate,
    protection::{ProtectionBinding, validate_observation},
    protection_storage::read_rows,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use sadr_core::{
    candidate::Candidate,
    finance::{Amount, FeeArithmetic, Money, Price, Side},
    market::Snapshot,
    risk::Verdict,
    time::Timestamp,
};
use serde_json::{Map, Value};
use std::{
    error::Error,
    fmt,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Applied,
    Unchanged,
    ProtectionDenied,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WriteError {
    Collision,
    Rejected,
    Unavailable,
    Store(StoreError),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collision => f.write_str("ledger_effect_collision"),
            Self::Rejected => f.write_str("ledger_effect_rejected"),
            Self::Unavailable => f.write_str("ledger_writer_unavailable"),
            Self::Store(error) => write!(f, "{error}"),
        }
    }
}

impl Error for WriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}

/// Captures and binding already loaded by the caller. The financial
/// transaction does not open another database or wait on a provider.
#[derive(Clone, Copy)]
pub struct EntryEvidence<'a> {
    pub binding: &'a ProtectionBinding,
    pub protected: &'a LegacyUpstoxCapture,
    pub latest_entry: &'a LegacyUpstoxCapture,
    pub snapshot: Option<&'a Snapshot>,
}

pub struct Reservation<'a> {
    pub intent_id: &'a str,
    pub candidate: &'a LegacyCandidate,
    pub verdict: &'a Verdict,
    pub cap: Money,
    pub requested: Timestamp,
    pub evidence: Option<EntryEvidence<'a>>,
}

pub struct Fill<'a> {
    pub fill_id: &'a str,
    pub intent_id: &'a str,
    pub leg_index: usize,
    pub units: i64,
    pub price: Price,
    pub fees: &'a FeeArithmetic,
    pub requested: Timestamp,
    pub exiting: bool,
    pub evidence: Option<EntryEvidence<'a>>,
}

pub struct LedgerWriter {
    db: Connection,
    clock: Box<dyn FnMut() -> Option<Timestamp>>,
}

impl LedgerWriter {
    /// Refuse a connection that is already inside a transaction. The caller
    /// supplies the schema; this method only sets WAL and a five-second busy
    /// timeout on that connection.
    pub fn open(
        db: Connection,
        clock: impl FnMut() -> Option<Timestamp> + 'static,
    ) -> Result<Self, WriteError> {
        if !db.is_autocommit() {
            return Err(WriteError::Rejected);
        }
        db.busy_timeout(Duration::from_secs(5))
            .map_err(|_| WriteError::Unavailable)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA query_only=OFF;")
            .map_err(|_| WriteError::Unavailable)?;
        Ok(Self {
            db,
            clock: Box::new(clock),
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.db
    }

    pub fn reserve(&mut self, reservation: Reservation<'_>) -> Result<Effect, WriteError> {
        self.reserve_with_barrier(reservation, |_| Ok(()))
    }

    /// `barrier` runs after the reservation writes and before commit. A crash
    /// test can pause there. It must not call a provider or model.
    pub fn reserve_with_barrier(
        &mut self,
        reservation: Reservation<'_>,
        barrier: impl FnOnce(&Connection) -> Result<(), WriteError>,
    ) -> Result<Effect, WriteError> {
        if !reservation.verdict.accepted
            || reservation.verdict.lots == 0
            || reservation.verdict.reservation <= Money::ZERO
            || reservation.cap <= Money::ZERO
        {
            return Err(WriteError::Rejected);
        }
        let lots = i64::try_from(reservation.verdict.lots).map_err(|_| WriteError::Rejected)?;
        self.transact(|db, clock| {
            match existing_hash(db, reservation.intent_id)? {
                Some(hash) if hash == reservation.candidate.original_hash() => {
                    return Ok(Effect::Unchanged);
                }
                Some(_) => return Err(WriteError::Collision),
                None => {}
            }
            let now = entry_clock(clock, reservation.requested)?;
            if !protection_allows(db, reservation.evidence, now) {
                return deny_entry(db, clock);
            }
            let account = load_account(db)?;
            let used = reserved_total(db)?;
            let next = used
                .checked_add(reservation.verdict.reservation.amount())
                .map_err(|_| WriteError::Store(StoreError::Arithmetic))?;
            let ceiling = min_amount(reservation.cap.amount(), account.initial);
            if account.halted
                || reservation.candidate.candidate().created_at() > now
                || now >= reservation.candidate.candidate().valid_until()
                || next > ceiling
                || next > account.cash
            {
                return Err(WriteError::Rejected);
            }
            db.execute(
                "INSERT INTO intents(id,hash,candidate,lots,reserved,state,created) VALUES(?,?,?,?,?,?,?)",
                (
                    reservation.intent_id,
                    reservation.candidate.original_hash(),
                    reservation.candidate.original_json(),
                    lots,
                    reservation.verdict.reservation.amount().to_string(),
                    "RESERVED",
                    now.to_ist_isoformat(),
                ),
            )
            .map_err(|_| WriteError::Unavailable)?;
            event(
                db,
                clock,
                "reserved",
                &[
                    ("hash", Value::String(reservation.candidate.original_hash().to_owned())),
                    ("intent", Value::String(reservation.intent_id.to_owned())),
                ],
            )?;
            barrier(db)?;
            Ok(Effect::Applied)
        })
    }

    pub fn fill(&mut self, fill: Fill<'_>) -> Result<Effect, WriteError> {
        self.fill_with_barrier(fill, |_| Ok(()))
    }

    /// `barrier` runs after the fill writes and before commit.
    pub fn fill_with_barrier(
        &mut self,
        fill: Fill<'_>,
        barrier: impl FnOnce(&Connection) -> Result<(), WriteError>,
    ) -> Result<Effect, WriteError> {
        self.transact(|db, clock| apply_fill(db, clock, &fill, barrier))
    }

    pub fn request_cancel(&mut self, intent_id: &str) -> Result<Effect, WriteError> {
        self.transact(|db, clock| {
            let state = required_text(db, "SELECT state FROM intents WHERE id=?1", (intent_id,))?;
            if !matches!(state.as_str(), "RESERVED" | "PART_FILLED") {
                return Ok(Effect::Unchanged);
            }
            db.execute(
                "UPDATE intents SET state='CANCEL_PENDING' WHERE id=?1",
                (intent_id,),
            )
            .map_err(|_| WriteError::Unavailable)?;
            event(
                db,
                clock,
                "cancel_requested",
                &[("intent", Value::String(intent_id.to_owned()))],
            )?;
            Ok(Effect::Applied)
        })
    }

    pub fn cancel(&mut self, intent_id: &str) -> Result<Effect, WriteError> {
        self.transact(|db, clock| {
            let candidate = stored_candidate(db, intent_id)?;
            let open = (0..candidate.legs().len()).try_fold(false, |open, leg| {
                Ok::<bool, WriteError>(open || holdings(db, intent_id, leg)? != 0)
            })?;
            db.execute(
                "UPDATE intents SET state='CANCELLED' WHERE id=?1",
                (intent_id,),
            )
            .map_err(|_| WriteError::Unavailable)?;
            if !open {
                db.execute("UPDATE intents SET reserved='0' WHERE id=?1", (intent_id,))
                    .map_err(|_| WriteError::Unavailable)?;
            }
            event(
                db,
                clock,
                "cancelled",
                &[("intent", Value::String(intent_id.to_owned()))],
            )?;
            Ok(Effect::Applied)
        })
    }

    pub fn halt(&mut self, reason: &str) -> Result<Effect, WriteError> {
        self.transact(|db, clock| {
            set_halted(db)?;
            event(
                db,
                clock,
                "halt",
                &[("reason", Value::String(reason.to_owned()))],
            )?;
            Ok(Effect::Applied)
        })
    }

    fn transact<T>(
        &mut self,
        body: impl FnOnce(&Connection, &mut dyn FnMut() -> Option<Timestamp>) -> Result<T, WriteError>,
    ) -> Result<T, WriteError> {
        if !self.db.is_autocommit() {
            return Err(WriteError::Rejected);
        }
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| WriteError::Unavailable)?;
        match body(&transaction, &mut self.clock) {
            Ok(value) => {
                transaction.commit().map_err(|_| WriteError::Unavailable)?;
                Ok(value)
            }
            Err(error) => {
                transaction
                    .rollback()
                    .map_err(|_| WriteError::Unavailable)?;
                Err(error)
            }
        }
    }
}

fn apply_fill(
    db: &Connection,
    clock: &mut dyn FnMut() -> Option<Timestamp>,
    fill: &Fill<'_>,
    barrier: impl FnOnce(&Connection) -> Result<(), WriteError>,
) -> Result<Effect, WriteError> {
    let candidate = stored_candidate(db, fill.intent_id)?;
    let leg = candidate
        .legs()
        .get(fill.leg_index)
        .ok_or(WriteError::Rejected)?;
    let side = if fill.exiting {
        opposite(leg.side())
    } else {
        leg.side()
    };
    if let Some(prior) = existing_fill(db, fill.fill_id)? {
        let leg_index = i64::try_from(fill.leg_index).map_err(|_| WriteError::Rejected)?;
        let same = prior.intent == fill.intent_id
            && prior.leg == leg_index
            && prior.units == fill.units
            && prior.price == fill.price.amount()
            && prior.side == side_name(side);
        if same {
            return Ok(Effect::Unchanged);
        }
        return Err(WriteError::Collision);
    }
    let now = if fill.exiting {
        fill.requested
    } else {
        let now = entry_clock(clock, fill.requested)?;
        if !protection_allows(db, fill.evidence, now) {
            return deny_entry(db, clock);
        }
        now
    };
    leg.instrument()
        .validate_order(fill.units, fill.price)
        .map_err(|_| WriteError::Rejected)?;
    check_fill_limits(db, fill, &candidate, leg.limit_price(), side, now)?;
    let account = load_account(db)?;
    if account.halted && !fill.exiting {
        return Err(WriteError::Rejected);
    }
    let turnover = fill
        .price
        .amount()
        .checked_mul(Amount::from_integer(fill.units))
        .map_err(|_| WriteError::Store(StoreError::Arithmetic))?;
    let fee = fill
        .fees
        .charge(
            side,
            Money::try_from(turnover).map_err(|_| WriteError::Store(StoreError::Arithmetic))?,
        )
        .map_err(|_| WriteError::Store(StoreError::Arithmetic))?;
    let cash = cash_after(account.cash, turnover, fee.amount(), side)?;
    let leg_index = i64::try_from(fill.leg_index).map_err(|_| WriteError::Rejected)?;
    db.execute(
        "INSERT INTO fills(id,intent,leg,side,units,price,fee,stamp) VALUES(?,?,?,?,?,?,?,?)",
        (
            fill.fill_id,
            fill.intent_id,
            leg_index,
            side_name(side),
            fill.units,
            fill.price.amount().to_string(),
            fee.amount().to_string(),
            now.to_ist_isoformat(),
        ),
    )
    .map_err(|_| WriteError::Unavailable)?;
    db.execute("UPDATE account SET cash=?1 WHERE id=1", (cash.to_string(),))
        .map_err(|_| WriteError::Unavailable)?;
    let state = next_state(db, fill.intent_id, &candidate, fill.exiting)?;
    db.execute(
        "UPDATE intents SET state=?1 WHERE id=?2",
        (state, fill.intent_id),
    )
    .map_err(|_| WriteError::Unavailable)?;
    event(
        db,
        clock,
        "fill",
        &[
            ("fee", Value::String(fee.amount().to_string())),
            ("id", Value::String(fill.fill_id.to_owned())),
            ("intent", Value::String(fill.intent_id.to_owned())),
            ("price", Value::String(fill.price.amount().to_string())),
            ("side", Value::String(side_name(side).to_owned())),
            ("units", Value::from(fill.units)),
        ],
    )?;
    barrier(db)?;
    Ok(Effect::Applied)
}

fn check_fill_limits(
    db: &Connection,
    fill: &Fill<'_>,
    candidate: &Candidate,
    limit: Price,
    side: Side,
    now: Timestamp,
) -> Result<(), WriteError> {
    let current = holdings(db, fill.intent_id, fill.leg_index)?;
    if fill.exiting {
        if fill.units > current.checked_abs().ok_or(WriteError::Rejected)? {
            return Err(WriteError::Rejected);
        }
    } else {
        let state = required_text(
            db,
            "SELECT state FROM intents WHERE id=?1",
            (fill.intent_id,),
        )?;
        if !matches!(
            state.as_str(),
            "RESERVED" | "PART_FILLED" | "CANCEL_PENDING"
        ) || now < candidate.created_at()
            || now >= candidate.valid_until()
        {
            return Err(WriteError::Rejected);
        }
        let leg = &candidate.legs()[fill.leg_index];
        let capacity = capacity_units(
            required_lots(db, fill.intent_id)?,
            leg.instrument().lot_size(),
        )?;
        let filled = current
            .checked_abs()
            .and_then(|value| value.checked_add(fill.units))
            .ok_or(WriteError::Rejected)?;
        if filled > capacity
            || (side == Side::Buy && fill.price > limit)
            || (side == Side::Sell && fill.price < limit)
        {
            return Err(WriteError::Rejected);
        }
    }
    let signed = if side == Side::Buy {
        fill.units
    } else {
        -fill.units
    };
    let new_position = current.checked_add(signed).ok_or(WriteError::Rejected)?;
    if fill.leg_index == 1 {
        let hedge = holdings(db, fill.intent_id, 0)?;
        if new_position.checked_abs().ok_or(WriteError::Rejected)? > hedge {
            return Err(WriteError::Rejected);
        }
    }
    if fill.leg_index == 0 && candidate.legs().len() == 2 {
        let hedge = holdings(db, fill.intent_id, 1)?
            .checked_abs()
            .ok_or(WriteError::Rejected)?;
        if new_position < hedge {
            return Err(WriteError::Rejected);
        }
    }
    Ok(())
}

fn next_state(
    db: &Connection,
    intent_id: &str,
    candidate: &Candidate,
    exiting: bool,
) -> Result<&'static str, WriteError> {
    let previous = required_text(db, "SELECT state FROM intents WHERE id=?1", (intent_id,))?;
    let lots = required_lots(db, intent_id)?;
    let mut flat = true;
    let mut complete = true;
    for (index, leg) in candidate.legs().iter().enumerate() {
        let quantity = holdings(db, intent_id, index)?;
        if quantity != 0 {
            flat = false;
        }
        let capacity = capacity_units(lots, leg.instrument().lot_size())?;
        if quantity.checked_abs().ok_or(WriteError::Rejected)? != capacity {
            complete = false;
        }
    }
    if exiting && flat {
        db.execute("UPDATE intents SET reserved='0' WHERE id=?1", (intent_id,))
            .map_err(|_| WriteError::Unavailable)?;
        return Ok("CLOSED");
    }
    if !exiting && complete {
        return Ok("FILLED");
    }
    if previous == "CANCEL_PENDING" {
        Ok("CANCEL_PENDING")
    } else {
        Ok("PART_FILLED")
    }
}

fn protection_allows(db: &Connection, evidence: Option<EntryEvidence<'_>>, now: Timestamp) -> bool {
    let Some(evidence) = evidence else {
        return false;
    };
    let Ok(rows) = read_rows(db, 100_000, Instant::now() + Duration::from_secs(2), false) else {
        return false;
    };
    validate_observation(
        evidence.binding,
        rows.profile(),
        rows.jobs(),
        rows.halted(),
        now,
        evidence.protected,
        evidence.latest_entry,
        evidence.snapshot,
    )
    .is_ok()
}

fn deny_entry(
    db: &Connection,
    clock: &mut dyn FnMut() -> Option<Timestamp>,
) -> Result<Effect, WriteError> {
    set_halted(db)?;
    event(
        db,
        clock,
        "operator_alert",
        &[(
            "reason",
            Value::String("paper_entry_protection_unavailable".to_owned()),
        )],
    )?;
    Ok(Effect::ProtectionDenied)
}

fn set_halted(db: &Connection) -> Result<(), WriteError> {
    let changed = db
        .execute("UPDATE account SET halted=1 WHERE id=1", [])
        .map_err(|_| WriteError::Unavailable)?;
    if changed != 1 {
        return Err(WriteError::Rejected);
    }
    let halted: i64 = db
        .query_row("SELECT halted FROM account WHERE id=1", [], |row| {
            row.get(0)
        })
        .map_err(|_| WriteError::Unavailable)?;
    if halted != 1 {
        return Err(WriteError::Rejected);
    }
    Ok(())
}

fn entry_clock(
    clock: &mut dyn FnMut() -> Option<Timestamp>,
    requested: Timestamp,
) -> Result<Timestamp, WriteError> {
    let actual = clock().ok_or(WriteError::Rejected)?;
    if actual < requested {
        return Err(WriteError::Rejected);
    }
    Ok(actual)
}

struct Account {
    cash: Amount,
    initial: Amount,
    halted: bool,
}

fn load_account(db: &Connection) -> Result<Account, WriteError> {
    let (cash, initial, halted): (String, String, i64) = db
        .query_row(
            "SELECT cash,initial,halted FROM account WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|_| WriteError::Rejected)?;
    if !matches!(halted, 0 | 1) {
        return Err(WriteError::Rejected);
    }
    Ok(Account {
        cash: Amount::parse(&cash).map_err(|_| WriteError::Rejected)?,
        initial: Amount::parse(&initial).map_err(|_| WriteError::Rejected)?,
        halted: halted == 1,
    })
}

fn reserved_total(db: &Connection) -> Result<Amount, WriteError> {
    let mut statement = db
        .prepare("SELECT reserved FROM intents")
        .map_err(|_| WriteError::Unavailable)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| WriteError::Unavailable)?;
    let mut total = Amount::ZERO;
    for reserved in rows {
        let reserved = reserved.map_err(|_| WriteError::Unavailable)?;
        total = total
            .checked_add(Amount::parse(&reserved).map_err(|_| WriteError::Rejected)?)
            .map_err(|_| WriteError::Store(StoreError::Arithmetic))?;
    }
    Ok(total)
}

fn existing_hash(db: &Connection, intent_id: &str) -> Result<Option<String>, WriteError> {
    db.query_row(
        "SELECT hash FROM intents WHERE id=?1",
        (intent_id,),
        |row| row.get(0),
    )
    .optional()
    .map_err(|_| WriteError::Unavailable)
}

struct StoredFill {
    intent: String,
    leg: i64,
    units: i64,
    price: Amount,
    side: String,
}

fn existing_fill(db: &Connection, fill_id: &str) -> Result<Option<StoredFill>, WriteError> {
    let row = db
        .query_row(
            "SELECT intent,leg,units,price,side FROM fills WHERE id=?1",
            (fill_id,),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|_| WriteError::Unavailable)?;
    row.map(|(intent, leg, units, price, side)| {
        Ok(StoredFill {
            intent,
            leg,
            units,
            price: Amount::parse(&price).map_err(|_| WriteError::Rejected)?,
            side,
        })
    })
    .transpose()
}

fn stored_candidate(db: &Connection, intent_id: &str) -> Result<Candidate, WriteError> {
    let (json, hash) = db
        .query_row(
            "SELECT candidate,hash FROM intents WHERE id=?1",
            (intent_id,),
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => WriteError::Rejected,
            _ => WriteError::Unavailable,
        })?;
    LegacyCandidate::parse(json, hash)
        .map(|candidate| candidate.candidate().clone())
        .map_err(|_| WriteError::Rejected)
}

fn required_text(db: &Connection, sql: &str, params: (&str,)) -> Result<String, WriteError> {
    db.query_row(sql, params, |row| row.get(0))
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => WriteError::Rejected,
            _ => WriteError::Unavailable,
        })
}

fn required_lots(db: &Connection, intent_id: &str) -> Result<i64, WriteError> {
    db.query_row(
        "SELECT lots FROM intents WHERE id=?1",
        (intent_id,),
        |row| row.get(0),
    )
    .map_err(|_| WriteError::Rejected)
}

fn holdings(db: &Connection, intent_id: &str, leg: usize) -> Result<i64, WriteError> {
    let leg = i64::try_from(leg).map_err(|_| WriteError::Rejected)?;
    db.query_row(
        "SELECT COALESCE(SUM(CASE WHEN side='buy' THEN units ELSE -units END),0) \
         FROM fills WHERE intent=?1 AND leg=?2",
        (intent_id, leg),
        |row| row.get(0),
    )
    .map_err(|_| WriteError::Unavailable)
}

fn capacity_units(lots: i64, lot_size: u64) -> Result<i64, WriteError> {
    let lot_size = i64::try_from(lot_size).map_err(|_| WriteError::Rejected)?;
    lots.checked_mul(lot_size).ok_or(WriteError::Rejected)
}

fn cash_after(
    cash: Amount,
    turnover: Amount,
    fee: Amount,
    side: Side,
) -> Result<Amount, WriteError> {
    let signed = if side == Side::Sell {
        turnover
    } else {
        Amount::ZERO
            .checked_sub(turnover)
            .map_err(|_| WriteError::Store(StoreError::Arithmetic))?
    };
    let next = cash
        .checked_add(signed)
        .and_then(|value| value.checked_sub(fee))
        .map_err(|_| WriteError::Store(StoreError::Arithmetic))?;
    if next < Amount::ZERO {
        return Err(WriteError::Rejected);
    }
    Ok(next)
}

fn event(
    db: &Connection,
    clock: &mut dyn FnMut() -> Option<Timestamp>,
    kind: &str,
    fields: &[(&str, Value)],
) -> Result<(), WriteError> {
    let mut payload = Map::new();
    payload.insert(
        "recorded_at".to_owned(),
        Value::String(
            clock()
                .map(|stamp| stamp.to_string())
                .unwrap_or_else(|| "clock-unavailable".to_owned()),
        ),
    );
    for (key, value) in fields {
        payload.insert((*key).to_owned(), value.clone());
    }
    let payload = Value::Object(payload);
    let text = serde_json::to_string(&payload).map_err(|_| WriteError::Unavailable)?;
    db.execute(
        "INSERT INTO events(kind,payload) VALUES(?1,?2)",
        (kind, text),
    )
    .map_err(|_| WriteError::Unavailable)?;
    Ok(())
}

fn min_amount(left: Amount, right: Amount) -> Amount {
    if left < right { left } else { right }
}

fn opposite(side: Side) -> Side {
    match side {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}
