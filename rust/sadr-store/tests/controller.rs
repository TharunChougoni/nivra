use rusqlite::Connection;
use sadr_core::{
    controller::{Book, DayId, DayKind, Phase},
    time::{ExchangeDate, LocalTime},
};
use sadr_store::controller::{self, Error};
fn book() -> Book {
    Book::open(
        DayId {
            broker: "upstox".into(),
            account: "fixture".into(),
            date: ExchangeDate::parse("2026-10-15").unwrap(),
        },
        &"ab".repeat(32),
    )
    .unwrap()
}
#[test]
fn reopened_checkpoint_keeps_halt_and_rejects_duplicate_and_stale_writers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("controller.sqlite");
    let mut db = Connection::open(&path).unwrap();
    controller::initialize(&mut db).unwrap();
    let b = book();
    let id = b.id().clone();
    let initial = controller::create(&mut db, b.clone()).unwrap();
    assert_eq!(controller::create(&mut db, b), Err(Error::DuplicateDay));
    let mut other = Connection::open(&path).unwrap();
    let stopped = controller::transition(&mut db, &id, initial.revision, |b| {
        b.halt();
        b.retain_unknown_model();
        Ok(())
    })
    .unwrap();
    assert_eq!(
        controller::transition(&mut other, &id, 1, |b| b.boot()),
        Err(Error::Conflict)
    );
    drop(db);
    let mut db = Connection::open(&path).unwrap();
    assert_eq!(controller::load(&mut db, &id).unwrap(), stopped);
    assert!(db.is_autocommit());
    assert_eq!(stopped.book.phase(), Phase::Halted);
    assert!(stopped.book.model_attempt_retained());
    assert!(!stopped.book.refunded());
    assert_eq!(
        controller::transition(&mut db, &id, 2, |b| b.boot()),
        Err(Error::Transition)
    );
    assert_eq!(controller::load(&mut db, &id).unwrap(), stopped);
}
#[test]
fn failed_commit_and_failed_transition_preserve_checkpoint() {
    let mut db = Connection::open_in_memory().unwrap();
    controller::initialize(&mut db).unwrap();
    let b = book();
    let id = b.id().clone();
    let original = controller::create(&mut db, b).unwrap();
    assert_eq!(
        controller::transition(&mut db, &id, 1, |b| b.wake()),
        Err(Error::Transition)
    );
    db.execute_batch("CREATE TRIGGER fail_checkpoint BEFORE UPDATE ON controller_days BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    assert_eq!(
        controller::transition(&mut db, &id, 1, |b| b.boot()),
        Err(Error::Storage)
    );
    assert!(db.is_autocommit());
    assert_eq!(controller::load(&mut db, &id).unwrap(), original);
}
#[test]
fn accelerated_day_retains_pending_effect_across_reopen_without_replaying_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("controller.sqlite");
    let mut db = Connection::open(&path).unwrap();
    controller::initialize(&mut db).unwrap();
    let b = book();
    let id = b.id().clone();
    controller::create(&mut db, b).unwrap();
    let running = controller::transition(&mut db, &id, 1, |b| {
        b.boot()?;
        b.reconcile()?;
        b.wake()?;
        b.acquire(true)?;
        b.prepare(true)?;
        b.begin_session(DayKind::Trading, LocalTime::parse("09:00").unwrap())?;
        b.note_intent()
    })
    .unwrap();
    drop(db);
    let mut db = Connection::open(&path).unwrap();
    let recovered = controller::load(&mut db, &id).unwrap();
    assert_eq!(recovered, running);
    assert!(recovered.book.pending());
    assert!(!recovered.book.effect_committed());
    // A pending effect is evidence requiring reconciliation, not permission to retry.
    let halted = controller::transition(&mut db, &id, recovered.revision, |b| {
        b.halt();
        Ok(())
    })
    .unwrap();
    assert_eq!(halted.book.phase(), Phase::Halted);
    assert!(halted.book.pending());
}
#[test]
fn initialization_never_adopts_existing_books_and_corruption_is_denied() {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE original_journal(value TEXT)")
        .unwrap();
    assert_eq!(controller::initialize(&mut db), Err(Error::Storage));
    let mut db = Connection::open_in_memory().unwrap();
    controller::initialize(&mut db).unwrap();
    let b = book();
    let id = b.id().clone();
    controller::create(&mut db, b).unwrap();
    db.execute(
        "UPDATE controller_days SET body=json_set(body,'$.entry_permitted',json('true'))",
        [],
    )
    .unwrap();
    assert_eq!(controller::load(&mut db, &id), Err(Error::InvalidState));
    assert!(db.is_autocommit());
}

#[test]
fn terminal_completion_survives_reopen_and_cannot_wake_as_the_same_primary_day() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("controller.sqlite");
    let mut db = Connection::open(&path).unwrap();
    controller::initialize(&mut db).unwrap();
    let b = book();
    let id = b.id().clone();
    controller::create(&mut db, b).unwrap();
    let completed = controller::transition(&mut db, &id, 1, |b| {
        b.boot()?;
        b.reconcile()?;
        b.wake()?;
        b.acquire(true)?;
        b.prepare(true)?;
        b.begin_session(DayKind::Trading, LocalTime::parse("09:00").unwrap())?;
        b.close()?;
        b.report()?;
        b.queue_review()?;
        b.sleep()
    })
    .unwrap();
    assert_eq!(completed.book.phase(), Phase::Completed);
    drop(db);
    let mut db = Connection::open(&path).unwrap();
    assert_eq!(controller::load(&mut db, &id).unwrap(), completed);
    assert_eq!(
        controller::transition(&mut db, &id, completed.revision, |b| b.wake()),
        Err(Error::Transition)
    );
    assert_eq!(controller::load(&mut db, &id).unwrap(), completed);
    assert!(db.is_autocommit());
}
