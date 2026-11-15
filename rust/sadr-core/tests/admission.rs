use sadr_core::admission::{GuardReason, InstallGuard, admit};
use sadr_core::time::LocalTime;

fn guard(now: &str) -> InstallGuard {
    InstallGuard {
        now: LocalTime::parse(now).unwrap(),
        exchange_hours: false,
        hold: false,
        work_active: false,
        clock_trusted: true,
        artifact_hash_ok: true,
        resource_evidence_fresh: true,
        interrupted: false,
        downgrade: false,
        prerelease: true,
    }
}

#[test]
fn blackout_hold_and_bad_artifacts_do_not_start() {
    let noon = admit(&guard("10:00"));
    assert!(!noon.admitted());
    assert!(noon.reasons().contains(&GuardReason::Blackout));
    assert!(!noon.would_start());
    assert!(!noon.stable_python_wheel());
    assert!(!noon.installation_authorized());

    let mut held = guard("08:59");
    held.hold = true;
    held.downgrade = true;
    let decision = admit(&held);
    assert!(decision.reasons().contains(&GuardReason::Hold));
    assert!(decision.reasons().contains(&GuardReason::Downgrade));
    assert!(!decision.admitted());
    assert!(!decision.would_start());
    a_clear_window_can_be_staged_and_still_does_not_start();
}

fn a_clear_window_can_be_staged_and_still_does_not_start() {
    let decision = admit(&guard("08:59"));
    assert!(decision.admitted());
    assert!(decision.prerelease());
    assert!(!decision.would_start());
    assert!(!decision.stable_python_wheel());
    assert!(!decision.trading_authorized());
}
