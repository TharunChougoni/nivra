use sadr_core::time::Timestamp;
use sadr_store::protection::{
    ProtectionJob, ProtectionJobState, ProtectionProfile, dashboard_projection,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
fn hash(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn stamp(s: &str) -> Timestamp {
    Timestamp::parse(s).unwrap()
}
fn job(report: Value) -> ProtectionJob {
    let capture = "a".repeat(64);
    let raw = canonical(&json!({"capture":capture,"watchdog":report}));
    ProtectionJob::new(
        1,
        ProtectionJobState::Done,
        Some(capture),
        Some(raw.clone()),
        Some(hash(&raw)),
    )
}
fn report() -> Value {
    serde_json::from_str::<Value>(include_str!(
        "../../../fixtures/rust/protection_dashboard_vectors.json"
    ))
    .unwrap()["report"]
        .clone()
}
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .map(|(k, v)| format!("{}: {}", serde_json::to_string(k).unwrap(), canonical(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical).collect::<Vec<_>>().join(", ")
        ),
        _ => v.to_string(),
    }
}

fn historical_projection_retains_completed_evidence_without_authority() {
    let config = r#"{"max_observations":5}"#;
    let h = hash(config);
    let t = stamp("2026-10-01T07:00:00Z");
    let p = ProtectionProfile::new(h.clone(), 1, Some(t));
    let jobs = vec![job(report())];
    for (now, age, recent) in [
        ("2026-10-01T06:59:59Z", -1.0, false),
        ("2026-10-01T07:00:02Z", 2.0, true),
        ("2026-10-01T12:30:02+05:30", 2.0, true),
        ("2026-10-01T07:00:02.001Z", 2.001, false),
        ("2026-10-02T07:00:00Z", 86400.0, false),
    ] {
        let result =
            dashboard_projection(Some(&h), Some((&h, config)), Some((&p, &jobs)), stamp(now));
        assert_eq!(result["state"], "RECORDED");
        assert_eq!(result["age_seconds"], age);
        assert_eq!(result["observation_recent"], recent);
        assert_eq!(result["process_health"], "UNVERIFIED");
        assert_eq!(result["source_verified"], false);
        assert_eq!(result["entry_authorized"], false);
    }
    for state in [ProtectionJobState::Claimed, ProtectionJobState::Captured] {
        let mut pending = jobs.clone();
        pending.push(ProtectionJob::new(
            2,
            state,
            (state == ProtectionJobState::Captured).then(|| "b".repeat(64)),
            None,
            None,
        ));
        let p = ProtectionProfile::new(h.clone(), 2, Some(t));
        let result = dashboard_projection(Some(&h), Some((&h, config)), Some((&p, &pending)), t);
        assert_eq!(result["state"], "INCOMPLETE");
        assert_eq!(result["pending"], 1);
        assert_eq!(result["observed_at"], report()["observed_at"]);
        assert_eq!(result["observation_recent"], false);
    }
}
#[test]
fn projection_preserves_history_and_rejects_untrusted_evidence() {
    historical_projection_retains_completed_evidence_without_authority();
    let config = r#"{"max_observations":5}"#;
    let h = hash(config);
    let t = stamp("2026-10-01T07:00:00Z");
    assert_eq!(
        dashboard_projection(None, None, None, t)["state"],
        "UNCONFIGURED"
    );
    assert_eq!(
        dashboard_projection(Some(&h), None, None, t)["state"],
        "UNAVAILABLE"
    );
    assert_eq!(
        dashboard_projection(Some(&h), Some((&h, config)), None, t)["state"],
        "WAITING"
    );
    let p = ProtectionProfile::new(h.clone(), 1, Some(t));
    for (key, value) in [
        ("observed_at", json!("2026-10-01T07:00:00")),
        ("status", json!("private-secret")),
        ("reasons", json!(["private-secret"])),
        ("broker_reconciled", json!(true)),
        ("token", json!("private-secret")),
    ] {
        let mut r = report();
        r[key] = value;
        let out = dashboard_projection(Some(&h), Some((&h, config)), Some((&p, &[job(r)])), t);
        assert_eq!(out["state"], "UNAVAILABLE");
        assert!(!out.to_string().contains("private-secret"));
    }
    let mut r = report();
    r["status"] = "HALTED".into();
    r["alerts"] = json!([{"credential":"private-alert"}]);
    let out = dashboard_projection(Some(&h), Some((&h, config)), Some((&p, &[job(r)])), t);
    assert_eq!(out["watchdog_status"], "HALTED");
    assert_eq!(out["exit_alert_count"], 1);
    assert_eq!(out["observation_recent"], true);
    assert!(!out.to_string().contains("private-alert"));
}
