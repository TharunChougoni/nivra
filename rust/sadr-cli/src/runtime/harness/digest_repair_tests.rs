use super::*;
#[test]
fn decide_survives_failed_tools_up_to_the_configured_count() {
    // 26 Oct 14:22 IST: one snapshot returned empty_depth and the grok decide
    // aborted with harness_decision_tool_failed_no_entries (#43).
    let mut role = crate::runtime::planner::ModelRole::default();
    assert!(!aborts_on_failed_tool("decide", 1, &role));
    assert!(!aborts_on_failed_tool("decide", 3, &role));
    assert!(aborts_on_failed_tool("decide", 4, &role));
    role.max_failed_tools = Some(0);
    assert!(aborts_on_failed_tool("decide", 1, &role));
    role.max_failed_tools = Some(10);
    assert!(!aborts_on_failed_tool("decide", 10, &role));
    assert!(!aborts_on_failed_tool("digest", 99, &role));
}
#[test]
fn digest_directive_acknowledgements_and_bounds_preserve_rejection() {
    let now = 1_000;
    let directive = super::directives::Directive {
        id: "dir-0123456789abcdef".into(),
        text: "review A".into(),
        keys: vec![],
        target: "both".into(),
        issued_at_ms: now,
        expires_at_ms: now + 60_000,
        supersedes: None,
        request_id: None,
        session: None,
        authorization_scope: None,
        expected_revision: None,
        idempotency_key: None,
    };
    let store = tempfile::tempdir().unwrap();
    super::directives::issue(store.path(), &directive).unwrap();
    let base = json!({
        "summary":"ok",
        "evidence_ids":[],
        "missing_data":[],
        "uncertainties":[]
    });
    let mut acknowledged = base.clone();
    acknowledged["directive_acknowledgements"] = json!([directive.id.clone()]);
    let normalized: Value = serde_json::from_str(
        &normalize_digest(&acknowledged.to_string(), &Value::Null, &BTreeSet::new()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        normalized["directive_acknowledgements"],
        json!([directive.id.clone()])
    );
    assert!(super::directives::unacknowledged(store.path(), &normalized, now).is_empty());
    assert!(
        super::directives::unacknowledged_for(store.path(), &normalized, now, "decide").is_empty()
    );
    for candidate in [
        base.clone(),
        json!({
            "summary":"ok","evidence_ids":[],"missing_data":[],
            "uncertainties":[],"directive_acknowledgements":[]
        }),
        json!({
            "summary":"ok","evidence_ids":[],"missing_data":[],
            "uncertainties":[],"directive_acknowledgements":["dir-unknown"]
        }),
    ] {
        let normalized: Value = serde_json::from_str(
            &normalize_digest(&candidate.to_string(), &Value::Null, &BTreeSet::new()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            super::directives::unacknowledged(store.path(), &normalized, now),
            vec![directive.id.clone()]
        );
    }
    // Decide accepts the same optional field without changing its plan.
    let plan = json!({
        "actions":[],"stance":"abstain","notes":"wait",
        "directive_acknowledgements":[directive.id.clone()]
    });
    let parsed = planner::plan_document(&plan.to_string()).unwrap();
    assert_eq!(
        parsed["directive_acknowledgements"],
        json!([directive.id.clone()])
    );
    serde_json::from_value::<planner::TradePlan>(plan).unwrap();

    let base = json!({"summary":"ok","evidence_ids":[],"missing_data":[],"uncertainties":[]});
    let cases = [
        ("summary_empty", "summary", json!(" ")),
        ("summary_length", "summary", json!("x".repeat(8193))),
        ("missing_data_count", "missing_data", json!(vec!["x"; 65])),
        ("uncertainties_count", "uncertainties", json!(vec!["x"; 65])),
        ("thesis_bounds", "thesis", json!("not an object")),
        (
            "missing_data_item_length",
            "missing_data",
            json!(["x".repeat(513)]),
        ),
        (
            "uncertainties_item_length",
            "uncertainties",
            json!(["x".repeat(513)]),
        ),
    ];
    assert_eq!(
        digest_bounds_violation(&serde_json::from_value(base.clone()).unwrap(), &json!({})),
        None
    );
    let held = json!({"positions":{"NSE_EQ|A":{"units":10}}});
    let missing = json!({"summary":"ok","evidence_ids":[],"missing_data":[],
            "uncertainties":[],"positions_review":[]});
    assert_eq!(
        normalize_digest(&missing.to_string(), &held, &BTreeSet::new())
            .unwrap_err()
            .to_string(),
        "harness_digest_positions_review_invalid"
    );
    let reviewed = json!({"summary":"ok","evidence_ids":[],"missing_data":[],
            "uncertainties":[],"positions_review":[{"instrument_key":"NSE_EQ|A",
            "opinion":"exit","thesis_status":"weakened","worth_keeping_reason":"thesis broke",
            "evidence_ids":["portfolio"],"confidence":"med"}]});
    assert!(normalize_digest(&reviewed.to_string(), &held, &BTreeSet::new()).is_ok());
    for (expected, field, value) in cases {
        let mut candidate = base.clone();
        candidate[field] = value;
        let digest = serde_json::from_value(candidate.clone()).unwrap();
        assert_eq!(digest_bounds_violation(&digest, &json!({})), Some(expected));
        assert_eq!(
            normalize_digest(&candidate.to_string(), &Value::Null, &BTreeSet::new())
                .unwrap_err()
                .to_string(),
            "harness_digest_bounds_invalid"
        );
    }
    // 27 Oct 13:19 IST: a valid digest cited 13 known IDs and was rejected
    // as harness_digest_bounds_invalid (#69). Keep the first 12.
    let ids: Vec<String> = (0..13).map(|i| format!("citation-{i}")).collect();
    let raw = json!({
        "summary":"ok",
        "evidence_ids": ids,
        "missing_data":[],
        "uncertainties":[]
    })
    .to_string();
    let tool_ids: BTreeSet<String> = ids.iter().cloned().collect();
    let normalized = normalize_digest(&raw, &Value::Null, &tool_ids).unwrap();
    let value: Value = serde_json::from_str(&normalized).unwrap();
    assert_eq!(value["evidence_ids"].as_array().unwrap().len(), 12);
    assert_eq!(value["evidence_ids"][0], "citation-0");
    assert_eq!(value["evidence_ids"][11], "citation-11");

    // Unknown citations are still dropped before the cap, so an oversized
    // list of unknown IDs cannot fail the digest or survive normalization.
    let unknown = json!({
        "summary":"ok",
        "evidence_ids": vec!["unknown"; 13],
        "missing_data":[],
        "uncertainties":[]
    })
    .to_string();
    let normalized = normalize_digest(&unknown, &Value::Null, &BTreeSet::new()).unwrap();
    let value: Value = serde_json::from_str(&normalized).unwrap();
    assert_eq!(value["evidence_ids"], json!([]));

    // Owner facts are a separate, bounded schema: no handoff/thesis prose,
    // one holding per held key, allowlisted instruments and sanitized
    // nested citations. Ineligible or incomplete facts never become work.
    let owner = json!({
        "complete":true,
        "as_of_ms":now,
        "coverage":{"universe":1,"scanned":1,"quoted":1,"featured":1,"experiments":0},
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
        normalized["changed_prospects"][0]["evidence_ids"],
        json!(["bundle"])
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
        decision_owner: Some(planner::DecisionOwnerPolicy {
            version: 1,
            owner: planner::DecisionOwner::Orchestrator,
        }),
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
    // 26 Oct 12:09 IST: the object closed after `uncertainties`, then continued.
    let raw = "{\"summary\":\"s\",\"evidence_ids\":[\"bundle\"],\"missing_data\":[],\"uncertainties\":[\"u\"]}\n,\"handoff\":{\"decision\":\"decide\",\"instrument_keys\":[\"A\"]}}";
    let v: Value =
        serde_json::from_str(&normalize_digest(raw, &Value::Null, &BTreeSet::new()).unwrap())
            .unwrap();
    assert_eq!(v["handoff"]["decision"], "decide");
    assert_eq!(v["uncertainties"], json!(["u"]));
    // Repair never changes the model's no-trade choice, including CRLF.
    for separator in ["}\n,", "}\r\n,"] {
        let no_trade = raw.replace("}\n,", separator).replace(
            r#"{"decision":"decide","instrument_keys":["A"]}"#,
            r#"{"decision":"wait","reason":"no edge"}"#,
        );
        let normalized = normalize_digest(&no_trade, &Value::Null, &BTreeSet::new()).unwrap();
        let value: Value = serde_json::from_str(&normalized).unwrap();
        assert_eq!(value["handoff"]["decision"], "wait");
    }
    // Both strict and repaired inputs retain schema and bound enforcement;
    // errors contain no model-controlled field names or values.
    for (input, reason) in [
        (
            raw.replace(r#""summary":"s""#, r#""summary":7"#),
            "harness_digest_schema_invalid",
        ),
        (
            raw.replace(
                r#""summary":"s""#,
                r#""private_model_field":"secret","summary":"s""#,
            ),
            "harness_digest_schema_invalid",
        ),
        (
            raw.replace(r#""summary":"s""#, r#""summary":" ""#),
            "harness_digest_bounds_invalid",
        ),
        (
            raw.replace(r#""summary":"s""#, r#""summary":"s","summary":"duplicate""#),
            "harness_digest_json_invalid",
        ),
    ] {
        for input in [input.clone(), input.replace("}\n,", ",")] {
            assert_eq!(
                normalize_digest(&input, &Value::Null, &BTreeSet::new())
                    .unwrap_err()
                    .to_string(),
                reason
            );
        }
    }

    for bad in [
        "{\"summary\":\"s\"}\n,{",
        "not json",
        "{\"summary\":\"s\"}{\"x\":1}",
    ] {
        assert_eq!(
            normalize_digest(bad, &Value::Null, &BTreeSet::new())
                .unwrap_err()
                .to_string(),
            "harness_digest_json_invalid"
        );
    }
}
