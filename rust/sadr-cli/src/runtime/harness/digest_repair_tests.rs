use super::*;
#[test]
fn owner_digest_bounds_normalize_rows_and_retain_rejections() {
    let now = 1_000;
    // A paper plan accepts optional directive acknowledgements without
    // changing its actions.
    let plan = json!({
        "actions":[],"stance":"abstain","notes":"wait",
        "directive_acknowledgements":["dir-0123456789abcdef"]
    });
    let parsed = planner::plan_document(&plan.to_string()).unwrap();
    assert_eq!(
        parsed["directive_acknowledgements"],
        json!(["dir-0123456789abcdef"])
    );
    serde_json::from_value::<planner::TradePlan>(plan).unwrap();

    // Owner facts are a separate, bounded schema: no handoff/thesis prose,
    // one holding per held key, allowlisted instruments and sanitized
    // nested citations. Ineligible or incomplete facts never become work.
    let owner = json!({
        "complete":true,
        "as_of_ms":now,
        "coverage":{"universe":1,"scanned":1,"quoted":1,"featured":1,"experiments":0},
        "brief":"Focus on NSE_EQ|A: it moved and is the only cited prospect.",
        "gaps":[],
        "changed_prospects":[{"instrument_key":"NSE_EQ|A","symbol":"A",
            "change_bps":12.5,"observed_ms":now,"reason":"moved",
            "evidence_ids":["bundle","unknown"]}],
        "active_rankings":[{"instrument_key":"NSE_EQ|A","rank":1,"score":1.0,
            "source":"scanner","as_of_ms":now,"evidence_ids":["bundle"]}],
        "candidate_rankings":[],
        "bar_context":[],
        "spread_depth":[],
        "holdings":[{"instrument_key":"NSE_EQ|A","units":10,"entry":"100",
            "mark":"101","evidence_ids":["portfolio"]}],
        "cash":{"available":"1000"},
        "fees":{"per_order":"1","buy_bps":"0","sell_bps":"0"},
        "recent_actions":[],
        "experiments":[],
        "evidence_ids":["bundle","unknown"]
    });
    let held = json!({"allowlist":[{"key":"NSE_EQ|A"}],
            "positions":{"NSE_EQ|A":{"units":10}}});
    validate_owner_digest(&owner.to_string(), &held).unwrap();
    let normalized: Value = serde_json::from_str(
        &normalize_owner_digest(&owner.to_string(), &held, &BTreeSet::new()).unwrap(),
    )
    .unwrap();
    assert_eq!(normalized["evidence_ids"], json!(["bundle"]));
    assert_eq!(
        normalized["brief"],
        json!("Focus on NSE_EQ|A: it moved and is the only cited prospect.")
    );
    assert_eq!(
        normalized["changed_prospects"][0]["evidence_ids"],
        json!(["bundle"])
    );
    let mut long_brief = owner.clone();
    long_brief["brief"] = json!("x".repeat(2_001));
    assert_eq!(
        validate_owner_digest(&long_brief.to_string(), &held)
            .unwrap_err()
            .to_string(),
        "harness_digest_bounds_invalid"
    );
    // Advisory ranking rows are normalized, not fatal: one foreign or
    // duplicate row must not discard the whole factual digest.
    let mut invalid_ranking = owner.clone();
    invalid_ranking["active_rankings"][0]["instrument_key"] = json!("NSE_EQ|B");
    validate_owner_digest(&invalid_ranking.to_string(), &held).unwrap();
    let normalized: Value = serde_json::from_str(
        &normalize_owner_digest(&invalid_ranking.to_string(), &held, &BTreeSet::new()).unwrap(),
    )
    .unwrap();
    assert_eq!(normalized["active_rankings"], json!([]));
    let mut duplicate_ranking = owner.clone();
    duplicate_ranking["active_rankings"] = json!([
        owner["active_rankings"][0].clone(),
        owner["active_rankings"][0].clone()
    ]);
    validate_owner_digest(&duplicate_ranking.to_string(), &held).unwrap();
    let normalized: Value = serde_json::from_str(
        &normalize_owner_digest(&duplicate_ranking.to_string(), &held, &BTreeSet::new()).unwrap(),
    )
    .unwrap();
    assert_eq!(normalized["active_rankings"].as_array().unwrap().len(), 1);
    let mut incomplete = owner.clone();
    incomplete["complete"] = json!(false);
    validate_owner_digest(&incomplete.to_string(), &Value::Null).unwrap();
    assert_eq!(
        normalize_owner_digest(&incomplete.to_string(), &held, &BTreeSet::new())
            .unwrap_err()
            .to_string(),
        "harness_digest_incomplete"
    );
    assert!(owner_digest_eligible(&owner, &held));
    assert!(!owner_digest_eligible(&incomplete, &held));
    assert!(!owner_digest_eligible(&owner, &json!({"positions":{}})));

    let parsed = |value: &Value| -> OwnerDigest { serde_json::from_value(value.clone()).unwrap() };
    let mut duplicate = owner.clone();
    duplicate["holdings"] = json!([owner["holdings"][0].clone(), owner["holdings"][0].clone()]);
    assert_eq!(
        owner_digest_bounds_violation(&parsed(&duplicate), &held),
        Some("owner_holdings_invalid")
    );
    let mut wrong_units = owner.clone();
    wrong_units["holdings"][0]["units"] = json!(11);
    assert_eq!(
        owner_digest_bounds_violation(&parsed(&wrong_units), &held),
        Some("owner_holdings_units")
    );
    let mut foreign = owner.clone();
    foreign["changed_prospects"][0]["instrument_key"] = json!("NSE_EQ|B");
    assert_eq!(
        owner_digest_bounds_violation(&parsed(&foreign), &held),
        Some("owner_changed_prospects_invalid")
    );
    // 13 Nov 09:39 IST: the same instrument legitimately appeared both as a
    // scanner prospect and as a collapsed price-move trigger, and the duplicate
    // row discarded the whole digest (#220). Deduplicate by instrument key
    // before the hard bounds check and keep the first occurrence.
    let mut duplicate_prospect = owner.clone();
    duplicate_prospect["changed_prospects"] = json!([
        owner["changed_prospects"][0].clone(),
        {
            "instrument_key":"NSE_EQ|A","symbol":"A",
            "change_bps":12.5,"observed_ms":now,
            "reason":"collapsed price-move trigger; scanner prospect",
            "evidence_ids":["bundle"]
        }
    ]);
    validate_owner_digest(&duplicate_prospect.to_string(), &held).unwrap();
    let normalized: Value = serde_json::from_str(
        &normalize_owner_digest(&duplicate_prospect.to_string(), &held, &BTreeSet::new()).unwrap(),
    )
    .unwrap();
    assert_eq!(normalized["changed_prospects"].as_array().unwrap().len(), 1);
    assert_eq!(normalized["changed_prospects"][0]["reason"], "moved");
    // Every other row check stays strict for the retained rows.
    for (field, value) in [
        ("reason", json!(" ")),
        ("observed_ms", json!(-1)),
        ("evidence_ids", json!([""])),
        ("instrument_key", json!("NSE_EQ|FOREIGN")),
    ] {
        let mut invalid = owner.clone();
        invalid["changed_prospects"][0][field] = value;
        assert_eq!(
            owner_digest_bounds_violation(&parsed(&invalid), &held),
            Some("owner_changed_prospects_invalid"),
            "{field}"
        );
        assert_eq!(
            normalize_owner_digest(&invalid.to_string(), &held, &BTreeSet::new())
                .unwrap_err()
                .to_string(),
            "harness_digest_bounds_invalid",
            "{field}"
        );
    }

    let facts = json!({"digest":owner,"positions":held["positions"]});
    let baseline = owner_facts_sha256(&facts).unwrap();
    let mut shifted = facts.clone();
    shifted["digest"]["as_of_ms"] = json!(now + 60_000);
    shifted["digest"]["changed_prospects"][0]["observed_ms"] = json!(now + 60_000);
    assert_eq!(owner_facts_sha256(&shifted).unwrap(), baseline);
    shifted["digest"]["changed_prospects"][0]["change_bps"] = json!(99.0);
    assert_ne!(owner_facts_sha256(&shifted).unwrap(), baseline);

    // A rejected digest body is retained beside its harness run record (#220).
    fn digest_role() -> ModelRole {
        ModelRole {
            enabled: true,
            model_id: crate::model::LUNA.into(),
            transport: sadr_cli::native_transport::Transport::OpenaiCodex,
            daily_call_budget: 8,
            ..ModelRole::default()
        }
    }
    struct DigestBody(Value);
    impl Backend for DigestBody {
        fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
            Ok(self.0.to_string())
        }
    }
    let owner_policy = PlannerConfig {
        orchestrator: Some(Default::default()),
        ..Default::default()
    };
    let archive_of = |c: &Config| {
        fs::read_dir(c.directory.join("model"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("harness-digest-"))
            })
            .unwrap()
    };
    let accepted_dir = tempfile::tempdir().unwrap();
    let accepted = super::super::tests::config(accepted_dir.path());
    let mut backend = DigestBody(duplicate_prospect.clone());
    let normalized = run(
        &accepted,
        &owner_policy,
        "digest",
        &digest_role(),
        "Duplicate prospect fixture",
        &held,
        &mut backend,
        &planner::preview_guard(),
    )
    .unwrap();
    let normalized: Value = serde_json::from_str(&normalized).unwrap();
    assert_eq!(normalized["changed_prospects"].as_array().unwrap().len(), 1);
    assert!(!archive_of(&accepted).join("rejected-digest.json").exists());

    let rejected_dir = tempfile::tempdir().unwrap();
    let rejected = super::super::tests::config(rejected_dir.path());
    let mut invalid = duplicate_prospect.clone();
    invalid["changed_prospects"][1]["instrument_key"] = json!("NSE_EQ|FOREIGN");
    invalid["changed_prospects"][1]["reason"] = json!("Bearer private-fixture-secret");
    let mut backend = DigestBody(invalid);
    assert_eq!(
        run(
            &rejected,
            &owner_policy,
            "digest",
            &digest_role(),
            "Rejected digest fixture",
            &held,
            &mut backend,
            &planner::preview_guard(),
        )
        .unwrap_err()
        .to_string(),
        "harness_digest_bounds_invalid"
    );
    let archive = archive_of(&rejected);
    let failure: Value =
        serde_json::from_slice(&fs::read(archive.join("failure.json")).unwrap()).unwrap();
    assert_eq!(failure["reason"], "harness_digest_bounds_invalid");
    assert_eq!(failure["rejected_digest"], "rejected-digest.json");
    let retained: Value =
        serde_json::from_slice(&fs::read(archive.join("rejected-digest.json")).unwrap()).unwrap();
    assert_eq!(retained["truncated"], false);
    assert_eq!(retained["paper_only"], true);
    let body = retained["body"].as_str().unwrap();
    assert!(body.contains("NSE_EQ|FOREIGN"));
    assert!(body.contains("\"moved\""));
    assert!(!body.contains("private-fixture-secret"));
    assert!(body.contains("[redacted]"));
    // The retained copy is bounded; a larger body keeps only its prefix.
    let bounded_dir = tempfile::tempdir().unwrap();
    assert!(retain_rejected_digest(
        bounded_dir.path(),
        &"x".repeat(REJECTED_DIGEST_MAX_BYTES + 64)
    ));
    let bounded: Value =
        serde_json::from_slice(&fs::read(bounded_dir.path().join("rejected-digest.json")).unwrap())
            .unwrap();
    assert_eq!(bounded["truncated"], true);
    assert_eq!(bounded["original_bytes"], REJECTED_DIGEST_MAX_BYTES + 64);
    assert!(bounded["body"].as_str().unwrap().len() <= REJECTED_DIGEST_MAX_BYTES);
}
#[test]
fn premature_close_is_repaired_but_garbage_is_not() {
    // 26 Oct 12:09 IST: the object closed early, then continued on a new line.
    let head = r#"{"complete":true,"as_of_ms":0,"coverage":{"universe":1,"scanned":1,"quoted":1,"featured":1,"experiments":0},"gaps":[],"changed_prospects":[],"active_rankings":[],"candidate_rankings":[],"bar_context":[],"spread_depth":[],"holdings":[],"cash":{"available":"0"},"fees":{"per_order":"0","buy_bps":"0","sell_bps":"0"},"recent_actions":[],"experiments":[]}"#;
    let raw = format!("{head}\n,\"evidence_ids\":[\"bundle\"]}}");
    for separator in ["}\n,", "}\r\n,"] {
        let input = raw.replace("}\n,", separator);
        let v: Value = serde_json::from_str(
            &normalize_owner_digest(&input, &Value::Null, &BTreeSet::new()).unwrap(),
        )
        .unwrap();
        assert_eq!(v["evidence_ids"], json!(["bundle"]));
        assert_eq!(v["complete"], true);
    }
    // Both strict and repaired inputs retain schema enforcement; errors
    // contain no model-controlled field names or values.
    for (input, reason) in [
        (
            raw.replace(r#""complete":true"#, r#""complete":7"#),
            "harness_digest_schema_invalid",
        ),
        (
            raw.replace(
                r#""complete":true"#,
                r#""private_model_field":"secret","complete":true"#,
            ),
            "harness_digest_schema_invalid",
        ),
        (
            raw.replace(r#""complete":true"#, r#""complete":true,"complete":false"#),
            "harness_digest_json_invalid",
        ),
    ] {
        for input in [input.clone(), input.replace("}\n,", ",")] {
            assert_eq!(
                normalize_owner_digest(&input, &Value::Null, &BTreeSet::new())
                    .unwrap_err()
                    .to_string(),
                reason
            );
        }
    }
    for bad in [
        "{\"complete\":true}\n,{",
        "not json",
        "{\"complete\":true}{\"x\":1}",
    ] {
        assert_eq!(
            normalize_owner_digest(bad, &Value::Null, &BTreeSet::new())
                .unwrap_err()
                .to_string(),
            "harness_digest_json_invalid"
        );
    }
}
