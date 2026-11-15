//! Synthetic original-file binding. No real book, credential, socket, or provider.
use rusqlite::Connection;
use sadr_core::{finance::Money, market::Snapshot, risk::Verdict, time::Timestamp};
use sadr_store::{
    archive::LegacyUpstoxCapture,
    binding::{BindError, FileIdentity, GateError, OriginalIdentity, bind, reserve_if_bound},
    legacy::LegacyCandidate,
    protection::ProtectionBinding,
    writer::{Effect, EntryEvidence, LedgerWriter, Reservation},
};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

const PLAN: &str = "abababababababababababababababababababababababababababababababab";
const OTHER_PLAN: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
const ACCOUNT: &str = "beta";

fn ledger_vectors() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/rust/ledger_vectors.json")).unwrap()
}

fn protection_vectors() -> Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/rust/protection_vectors.json"
    ))
    .unwrap()
}

fn now_stamp() -> Timestamp {
    Timestamp::parse("2026-10-15T10:00:00+05:30").unwrap()
}

fn trace_reserve() -> Value {
    ledger_vectors()["traces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|trace| trace["id"] == "reserve")
        .unwrap()
        .clone()
}

fn candidate() -> LegacyCandidate {
    let trace = trace_reserve();
    LegacyCandidate::parse(
        trace["candidate"]["model_json"]
            .as_str()
            .unwrap()
            .to_owned(),
        trace["candidate"]["hash"].as_str().unwrap().to_owned(),
    )
    .unwrap()
}

fn verdict() -> Verdict {
    let parsed: Value =
        serde_json::from_str(trace_reserve()["verdict_json"].as_str().unwrap()).unwrap();
    Verdict {
        accepted: parsed["accepted"].as_bool().unwrap(),
        reasons: parsed["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|reason| reason.as_str().unwrap().to_owned())
            .collect(),
        lots: parsed["lots"].as_u64().unwrap(),
        reservation: Money::parse(parsed["reservation"].as_str().unwrap()).unwrap(),
    }
}

struct Evidence {
    binding: ProtectionBinding,
    protected: LegacyUpstoxCapture,
    latest: LegacyUpstoxCapture,
    snapshot: Snapshot,
}

fn evidence() -> Evidence {
    let case = protection_vectors()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "valid-fresh-observation")
        .unwrap()
        .clone();
    Evidence {
        binding: serde_json::from_value(case["binding"].clone()).unwrap(),
        protected: LegacyUpstoxCapture::parse(
            case["protected_capture"]["original_json"]
                .as_str()
                .unwrap()
                .to_owned(),
            case["protected_capture"]["original_hash"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
        .unwrap(),
        latest: LegacyUpstoxCapture::parse(
            case["latest_entry_capture"]["original_json"]
                .as_str()
                .unwrap()
                .to_owned(),
            case["latest_entry_capture"]["original_hash"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
        .unwrap(),
        snapshot: serde_json::from_value(case["entry_snapshot"].clone()).unwrap(),
    }
}

fn evidence_ref(evidence: &Evidence) -> EntryEvidence<'_> {
    EntryEvidence {
        binding: &evidence.binding,
        protected: &evidence.protected,
        latest_entry: &evidence.latest,
        snapshot: Some(&evidence.snapshot),
    }
}

fn apply_ledger_schema(db: &Connection) {
    for sql in ledger_vectors()["schema_sql"].as_array().unwrap() {
        db.execute_batch(sql.as_str().unwrap()).unwrap();
    }
}

fn seed_protection(db: &Connection) {
    for sql in protection_vectors()["schema_sql"]["ledger_and_protection"]
        .as_array()
        .unwrap()
    {
        let sql = sql.as_str().unwrap();
        if sql.contains("CREATE TABLE paper_protection_profile")
            || sql.contains("CREATE TABLE paper_protection_jobs")
        {
            db.execute_batch(sql).unwrap();
        }
    }
    let case = protection_vectors()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "valid-fresh-observation")
        .unwrap()
        .clone();
    let profile = &case["profile"];
    db.execute(
        "INSERT INTO paper_protection_profile(id,value,observations,last_at) VALUES(1,?,?,?)",
        (
            profile["value"].as_str().unwrap(),
            profile["observations"].as_i64().unwrap(),
            profile["last_at"].as_str().unwrap(),
        ),
    )
    .unwrap();
    for job in case["jobs"].as_array().unwrap() {
        db.execute(
            "INSERT INTO paper_protection_jobs(seq,state,capture,result,result_hash) VALUES(?,?,?,?,?)",
            (
                job["seq"].as_i64().unwrap(),
                job["state"].as_str().unwrap(),
                job["capture"].as_str().unwrap(),
                job["result"].as_str().unwrap(),
                job["result_hash"].as_str().unwrap(),
            ),
        )
        .unwrap();
    }
}

struct Book {
    _dir: tempfile::TempDir,
    ledger: PathBuf,
    lock: PathBuf,
}

fn private_file(path: &Path, bytes: &[u8]) {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(path).unwrap();
    file.write_all(bytes).unwrap();
}

fn book() -> Book {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let ledger = dir.path().join("ledger.db");
    let lock = dir.path().join("shadow.db.worker.lock");
    private_file(&ledger, b"");
    private_file(&lock, b"lock-marker");
    Book {
        _dir: dir,
        ledger,
        lock,
    }
}

fn file_identity(path: &Path) -> FileIdentity {
    let meta = fs::metadata(path).unwrap();
    FileIdentity {
        path: path.to_path_buf(),
        device: meta.dev(),
        inode: meta.ino(),
    }
}

fn identity(book: &Book, account: &str, extra: Vec<FileIdentity>) -> OriginalIdentity {
    let mut files = vec![file_identity(&book.ledger)];
    files.extend(extra);
    OriginalIdentity {
        files,
        lock: file_identity(&book.lock),
        ledger: book.ledger.clone(),
        account: account.to_owned(),
        plan_hash: PLAN.to_owned(),
    }
}

fn chmod_sidecars(path: &Path) {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        if sidecar.exists() {
            fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}

fn open_writer(book: &Book, protected: bool) -> LedgerWriter {
    let db = Connection::open(&book.ledger).unwrap();
    apply_ledger_schema(&db);
    db.execute(
        "INSERT INTO account(id,cash,initial,halted) VALUES(1,'5000','5000',0)",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO metadata(key,value) VALUES('account_alias',?1),('prepared_plan',?2)",
        (ACCOUNT, PLAN),
    )
    .unwrap();
    if protected {
        seed_protection(&db);
    }
    let writer = LedgerWriter::open(db, || Some(now_stamp())).unwrap();
    chmod_sidecars(&book.ledger);
    writer
}

fn reservation<'a>(
    candidate: &'a LegacyCandidate,
    verdict: &'a Verdict,
    evidence: Option<&'a Evidence>,
) -> Reservation<'a> {
    Reservation {
        intent_id: "reserve",
        candidate,
        verdict,
        cap: Money::parse("5000").unwrap(),
        requested: now_stamp(),
        evidence: evidence.map(evidence_ref),
    }
}

fn snap(path: &Path) -> (u64, u64, u32, u64, u64, Vec<u8>) {
    let meta = fs::symlink_metadata(path).unwrap();
    let bytes = if meta.file_type().is_symlink() {
        Vec::new()
    } else {
        fs::read(path).unwrap()
    };
    (
        meta.dev(),
        meta.ino(),
        meta.mode(),
        meta.nlink(),
        meta.len(),
        bytes,
    )
}

fn counts(db: &Connection) -> (i64, i64, i64, i64) {
    let one = |sql: &str| db.query_row(sql, [], |row| row.get(0)).unwrap();
    (
        one("SELECT halted FROM account WHERE id=1"),
        one("SELECT COUNT(*) FROM intents"),
        one("SELECT COUNT(*) FROM events"),
        one("SELECT COUNT(*) FROM fills"),
    )
}

fn assert_untouched(
    writer: &LedgerWriter,
    lock: &Path,
    before: &(u64, u64, u32, u64, u64, Vec<u8>),
) {
    assert_eq!(counts(writer.connection()), (0, 0, 0, 0));
    assert_eq!(&snap(lock), before);
    assert!(lock.exists());
}

fn bind_error(error: GateError) -> BindError {
    match error {
        GateError::Bind(error) => error,
        GateError::Write(error) => panic!("writer ran after a binding failure: {error}"),
    }
}

#[test]
fn prepared_plan_mismatch_denies_before_reserve() {
    let book = book();
    let mut writer = open_writer(&book, false);
    let before = snap(&book.lock);
    let candidate = candidate();
    let verdict = verdict();
    let error = reserve_if_bound(
        &identity(&book, ACCOUNT, vec![]),
        OTHER_PLAN,
        &mut writer,
        reservation(&candidate, &verdict, None),
    )
    .unwrap_err();
    assert_eq!(bind_error(error), BindError::PlanMismatch);
    assert_untouched(&writer, &book.lock, &before);
}

#[test]
fn second_contender_denies_while_lease_is_held() {
    let book = book();
    let writer = open_writer(&book, false);
    drop(writer);
    let db = Connection::open(&book.ledger).unwrap();
    let held = bind(&db, &identity(&book, ACCOUNT, vec![]), PLAN).unwrap();
    assert!(!held.trading_authorized());
    let (ready_tx, ready_rx) = mpsc::channel();
    let ledger = book.ledger.clone();
    let lock = book.lock.clone();
    let contender = thread::spawn(move || {
        ready_rx.recv().unwrap();
        let db = Connection::open(&ledger).unwrap();
        let mut writer = LedgerWriter::open(db, || Some(now_stamp())).unwrap();
        let candidate = candidate();
        let verdict = verdict();
        let identity = OriginalIdentity {
            files: vec![file_identity(&ledger)],
            lock: file_identity(&lock),
            ledger,
            account: ACCOUNT.to_owned(),
            plan_hash: PLAN.to_owned(),
        };
        reserve_if_bound(
            &identity,
            PLAN,
            &mut writer,
            reservation(&candidate, &verdict, None),
        )
    });
    ready_tx.send(()).unwrap();
    let error = contender.join().unwrap().unwrap_err();
    assert_eq!(bind_error(error), BindError::Contended);
    let before = snap(&book.lock);
    drop(held);
    assert_eq!(snap(&book.lock), before);
    let db = Connection::open(&book.ledger).unwrap();
    assert_eq!(counts(&db), (0, 0, 0, 0));
}

#[test]
fn bound_reservation_reaches_the_writer() {
    let book = book();
    let mut writer = open_writer(&book, true);
    let before = snap(&book.lock);
    let candidate = candidate();
    let verdict = verdict();
    let evidence = evidence();
    let (lease, effect) = reserve_if_bound(
        &identity(&book, ACCOUNT, vec![]),
        PLAN,
        &mut writer,
        reservation(&candidate, &verdict, Some(&evidence)),
    )
    .unwrap();
    assert_eq!(effect, Effect::Applied);
    assert_eq!(lease.account(), ACCOUNT);
    assert_eq!(lease.plan_hash(), PLAN);
    assert!(!lease.trading_authorized());
    assert!(!lease.installation_authorized());
    let (halted, intents, events, fills) = counts(writer.connection());
    assert_eq!((halted, intents, fills), (0, 1, 0));
    assert_eq!(events, 1);
    let release = lease.release();
    assert!(!release.reconciled());
    assert!(!release.restart_permitted());
    assert_eq!(snap(&book.lock), before);
}
