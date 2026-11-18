//! Limit fallback (#245, #21) contract: which failures switch, the exact chat
//! request, the reply envelope and readiness. No sockets are opened.
use super::*;

#[test]
fn fallback_eligibility_reason_codes_are_static_and_secret_free() {
    // The reviewed set is exactly these static operator-visible codes; it is
    // what evidence, logs, alerts and readiness may publish.
    assert_eq!(
        FALLBACK_REASONS,
        [
            "usage_limit_reached",
            "rate_limited",
            "quota_exceeded",
            "entitlement_auth_failed",
            "response_validation_rejected",
        ]
    );
    assert!(FALLBACK_REASONS.iter().all(|code| {
        code.bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    }));
    // A primary error carrying a synthetic credential yields only a static
    // code: provider text never becomes the reason in evidence or the alert.
    let secret = "sk-secret-0123456789abcdef";
    for (error, stage) in [
        (
            format!("model_http_401 invalid_api_key {secret}"),
            "catalog",
        ),
        (
            format!("model_http_200 rejected {secret}"),
            "response_validation",
        ),
        (
            format!("model_http_429 usage_limit_reached {secret}"),
            "http_response",
        ),
    ] {
        let reason = fallback_reason(&error, &json!({"stage":stage})).unwrap();
        assert!(FALLBACK_REASONS.contains(&reason));
        assert!(!reason.contains(secret) && !reason.contains("sk-"));
    }
}
