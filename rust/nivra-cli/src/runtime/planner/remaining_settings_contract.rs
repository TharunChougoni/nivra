use super::*;
#[test]
fn model_role_analysis_defaults_and_bounds() {
    let mut role: ModelRole = serde_json::from_str("{}").unwrap();
    assert_eq!(role.max_analysis_tools, 4);
    assert!(!role_invalid(&role, 120_000));
    assert!(
        serde_json::to_value(&role)
            .unwrap()
            .get("max_analysis_tools")
            .is_none()
    );
    role.max_analysis_tools = 65;
    assert!(role_invalid(&role, 120_000));
    role.max_analysis_tools = 0;
    assert!(!role_invalid(&role, 120_000));
    assert_eq!(
        serde_json::to_value(&role).unwrap()["max_analysis_tools"],
        0
    );
}
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
