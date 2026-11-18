use super::*;
#[test]
fn planner_handoff_defaults_and_bounds() {
    let mut cfg: PlannerConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(cfg.max_handoff_instruments, 20);
    assert!(cfg.validate_handoff_limit().is_ok());
    assert!(
        serde_json::to_value(&cfg)
            .unwrap()
            .get("max_handoff_instruments")
            .is_none()
    );
    for n in [0, super::super::harness::MAX_INSTRUMENT_KEYS + 1] {
        cfg.max_handoff_instruments = n;
        assert!(cfg.validate_handoff_limit().is_err());
    }
    cfg.max_handoff_instruments = 1;
    assert!(cfg.validate_handoff_limit().is_ok());
    assert_eq!(
        serde_json::to_value(&cfg).unwrap()["max_handoff_instruments"],
        1
    );
}
