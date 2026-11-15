use sadr_core::controller::{Book, ControlError, DayId, DayKind, Phase, previous_trading_day};
use sadr_core::time::{ExchangeDate, LocalTime};

fn hash(fill: &str) -> String {
    fill.repeat(32)
}

fn date(text: &str) -> ExchangeDate {
    ExchangeDate::parse(text).unwrap()
}

fn open_book() -> Book {
    Book::open(
        DayId {
            broker: "upstox".to_string(),
            account: "beta".to_string(),
            date: date("2026-10-15"),
        },
        &hash("ab"),
    )
    .unwrap()
}

fn ready(book: &mut Book) {
    book.boot().unwrap();
    book.reconcile().unwrap();
    book.wake().unwrap();
    book.acquire(true).unwrap();
    book.prepare(true).unwrap();
}

#[test]
fn one_primary_day_skips_late_entry_and_preserves_halt() {
    let mut book = open_book();
    ready(&mut book);
    book.begin_session(DayKind::Trading, LocalTime::parse("09:00").unwrap())
        .unwrap();
    assert_eq!(book.phase(), Phase::Running);
    assert!(book.entry_permitted());
    assert!(!book.trading_authorized());
    assert!(book.reconciled());
    assert!(!book.replayed());

    let mut late = open_book();
    ready(&mut late);
    late.begin_session(DayKind::Trading, LocalTime::parse("09:15").unwrap())
        .unwrap();
    assert_eq!(late.phase(), Phase::Skipped);
    assert!(!late.entry_permitted());

    let mut closed = open_book();
    ready(&mut closed);
    closed
        .begin_session(DayKind::Closed, LocalTime::parse("09:00").unwrap())
        .unwrap();
    assert_eq!(closed.phase(), Phase::Skipped);

    let duplicate = book
        .open_another(
            DayId {
                broker: "upstox".to_string(),
                account: "beta".to_string(),
                date: date("2026-10-15"),
            },
            &hash("cd"),
        )
        .unwrap_err();
    assert_eq!(duplicate, ControlError::DuplicateDay);
    assert_eq!(book.bind_policy(&hash("cd")), Err(ControlError::Phase));
    assert_eq!(book.id().date, date("2026-10-15"));
    assert_eq!(book.policy_hash(), &hash("ab"));

    book.halt();
    assert_eq!(book.phase(), Phase::Halted);
    assert!(!book.entry_permitted());
    assert_eq!(book.commit_effect(), Err(ControlError::Halted));
}

#[test]
fn crash_replay_fence_and_previous_trading_day_keep_one_effect() {
    let mut book = open_book();
    assert_eq!(book.note_intent(), Err(ControlError::Phase));
    ready(&mut book);
    book.begin_session(DayKind::Trading, LocalTime::parse("09:00").unwrap())
        .unwrap();
    book.note_intent().unwrap();
    book.crash_before_commit();
    assert!(!book.pending());
    assert!(!book.effect_committed());
    assert_eq!(book.commit_effect(), Err(ControlError::Phase));
    book.note_intent().unwrap();
    book.commit_effect().unwrap();
    assert!(book.effect_committed());
    assert_eq!(book.commit_effect(), Err(ControlError::ExistingEffect));
    book.retain_unknown_model();
    assert!(book.model_attempt_retained());
    assert!(!book.refunded());

    let generation = book.generation();
    book.fence();
    assert!(book.accept_child(generation).is_err());
    book.accept_child(book.generation()).unwrap();
    book.reject_clock_jump();
    assert_eq!(book.phase(), Phase::Blocked);
    assert!(!book.replayed());

    let monday = date("2026-10-12");
    assert_eq!(previous_trading_day(monday, &[]), Some(date("2026-10-09")));
    assert_eq!(
        previous_trading_day(monday, &[date("2026-10-09")]),
        Some(date("2026-10-08"))
    );
}

#[test]
fn completed_day_cannot_rearm_and_new_days_keep_original_identity() {
    let mut book = open_book();
    ready(&mut book);
    book.begin_session(DayKind::Trading, LocalTime::parse("09:00").unwrap())
        .unwrap();
    let mut next = book.id().clone();
    next.date = date("2026-10-16");
    assert_eq!(
        book.open_another(next.clone(), &hash("ab")),
        Err(ControlError::Phase)
    );
    book.close().unwrap();
    book.report().unwrap();
    book.queue_review().unwrap();
    book.sleep().unwrap();
    assert_eq!(book.phase(), Phase::Completed);
    assert!(!book.entry_permitted());
    assert_eq!(book.wake(), Err(ControlError::Phase));
    book.reject_clock_jump();
    book.sleep().unwrap();
    assert_eq!(book.phase(), Phase::Completed);
    let fresh = book.open_another(next.clone(), &hash("ab")).unwrap();
    assert_eq!(fresh.phase(), Phase::Boot);
    assert!(!fresh.trading_authorized());
    let mut wrong = next.clone();
    wrong.account = "other".into();
    assert_eq!(
        book.open_another(wrong, &hash("ab")),
        Err(ControlError::Identity)
    );
    let mut old = next;
    old.date = date("2026-10-14");
    assert_eq!(
        book.open_another(old, &hash("ab")),
        Err(ControlError::Identity)
    );
}
#[test]
fn pending_effect_cannot_be_reported_as_completed_or_carried_into_a_new_day() {
    let mut book = open_book();
    ready(&mut book);
    book.begin_session(DayKind::Trading, LocalTime::parse("09:00").unwrap())
        .unwrap();
    book.note_intent().unwrap();
    book.retain_unknown_model();
    book.close().unwrap();
    assert_eq!(book.report(), Err(ControlError::PendingEffect));
    assert_eq!(book.phase(), Phase::Closing);
    assert!(book.pending());
    assert!(book.model_attempt_retained());
    assert!(!book.refunded());
    let mut next = book.id().clone();
    next.date = date("2026-10-16");
    assert_eq!(
        book.open_another(next, &hash("ab")),
        Err(ControlError::Phase)
    );
    book.halt();
    assert_eq!(book.sleep(), Ok(()));
    assert_eq!(book.phase(), Phase::Halted);
}

#[test]
fn exhausted_fencing_counter_halts_instead_of_accepting_an_old_child_generation() {
    let mut value = serde_json::to_value(open_book()).unwrap();
    value["generation"] = serde_json::json!(u64::MAX);
    let mut book: Book = serde_json::from_value(value).unwrap();
    book.validate().unwrap();
    book.fence();
    assert_eq!(book.phase(), Phase::Halted);
    assert_eq!(book.accept_child(u64::MAX), Err(ControlError::Halted));
    assert!(!book.entry_permitted());
}
