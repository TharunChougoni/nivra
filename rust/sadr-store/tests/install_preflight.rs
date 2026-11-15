use rusqlite::{Connection, OpenFlags};
use sadr_store::install_preflight::{self as preflight, Error};
use serde::Deserialize;
use serde_json::Value;
#[derive(Deserialize)]
struct Vectors {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    name: String,
    version: String,
    body: Option<Value>,
    expected: String,
}
#[test]
fn all_python_pristine_install_verdicts_match_on_read_only_synthetic_books() {
    let cases: Vectors = serde_json::from_str(include_str!(
        "../../../fixtures/rust/pristine_install_vectors.json"
    ))
    .unwrap();
    assert_eq!(cases.cases.len(), 16);
    for case in cases.cases {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("paper.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE state(id INTEGER PRIMARY KEY,body TEXT)")
            .unwrap();
        if let Some(body) = case.body {
            db.execute("INSERT INTO state VALUES(1,?)", [body.to_string()])
                .unwrap();
        }
        drop(db);
        let original = std::fs::read(&path).unwrap();
        let read = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let verdict = match preflight::inspect(read, &case.version) {
            Ok(()) => "allowed".into(),
            Err(error) => error.to_string(),
        };
        assert_eq!(verdict, case.expected, "{}", case.name);
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}
#[test]
fn malformed_false_values_duplicate_keys_and_unknown_versions_fail_closed() {
    for field in [
        r#""halted":0"#,
        r#""units":false"#,
        r#""positions":[]"#,
        r#""fills":{}"#,
    ] {
        let raw = format!("{{\"cash\":\"50000\",{field}}}");
        assert_eq!(
            preflight::validate_body("v0.4.1", raw.as_bytes()),
            Err(Error::Invalid)
        );
    }
    assert_eq!(
        preflight::validate_body("v0.4.1", br#"{"cash":"50000","cash":"0"}"#),
        Err(Error::Invalid)
    );
    assert_eq!(
        preflight::validate_body("v0.4.3", br#"{"cash":"50000"}"#),
        Err(Error::Version)
    );
    assert_eq!(
        preflight::inspect(Connection::open_in_memory().unwrap(), "v0.4.1"),
        Err(Error::Unavailable)
    );
}
