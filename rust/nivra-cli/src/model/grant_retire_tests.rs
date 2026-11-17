use super::*;
#[test]
fn login_moves_only_an_expired_grant_aside() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("xai-grant");
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-23T09:00:00+05:30").unwrap();
    assert!(retire_expired_grant(&path, at).unwrap().is_none());
    std::fs::write(&path, br#"{"expires_at":"2026-10-23T14:02:00+05:30"}"#).unwrap();
    assert_eq!(
        retire_expired_grant(&path, at).unwrap_err().to_string(),
        "grant_exists_preserve_and_review"
    );
    assert!(path.exists(), "a live grant is never moved");
    let later = chrono::DateTime::parse_from_rfc3339("2026-10-23T16:00:00+05:30").unwrap();
    let backup = retire_expired_grant(&path, later).unwrap().unwrap();
    assert!(!path.exists() && backup.exists());
    assert!(backup.to_string_lossy().contains("xai-grant.expired-"));
    std::fs::write(&path, b"not json").unwrap();
    assert!(
        retire_expired_grant(&path, later).is_err(),
        "unreadable grants are preserved"
    );
    assert!(path.exists());
}
