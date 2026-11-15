use sadr_store::analytics_token::{analytics_token, environment_token};
use std::{fs, os::unix::fs::PermissionsExt};

fn write(path: &std::path::Path, body: &[u8]) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
#[test]
fn original_python_goldens() {
    let vectors: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/rust/analytics_token_vectors.json"
    ))
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("synthetic-token");
    for case in vectors["cases"].as_array().unwrap() {
        let result = if let Some(body) = case["body"].as_str() {
            write(&path, body.as_bytes());
            analytics_token(Some(&path), || {
                panic!("explicit file must not inspect environment")
            })
        } else {
            let entries = case["environment"].as_object().unwrap();
            analytics_token(None, || {
                Ok(environment_token(entries.iter().map(|(key, value)| {
                    (key.as_str(), value.as_str().unwrap())
                })))
            })
        };
        assert_eq!(
            result.is_ok(),
            case["python_accepted"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        if let Ok(token) = result {
            token.with_secret(|value| {
                assert_eq!(value, case["token"].as_str().unwrap(), "{}", case["name"])
            });
            assert_eq!(format!("{token:?}"), "AnalyticsToken([REDACTED])");
            assert_eq!(format!("{token}"), "[REDACTED]");
        }
    }
}
