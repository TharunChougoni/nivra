use super::super::super::workspace::{WorkspaceOp, WorkspaceSettings, execute};
use super::*;

fn hash(label: &str) -> String {
    sadr_core::replay::digest(label.as_bytes())
}

fn spec(hypothesis: &str) -> ExperimentSpec {
    ExperimentSpec {
        version: 1,
        parent_candidate_id: None,
        session: "2026-10-16".into(),
        source_sha256: hash("source"),
        params_sha256: hash("params"),
        dataset_hash: hash("dataset"),
        feature_abi: "cash_quote_action_tape_v1".into(),
        hypothesis: hypothesis.into(),
        training_from_ms: 1_000,
        training_to_ms: 1_999,
        selection_from_ms: 2_000,
        selection_to_ms: 3_000,
        holding_ms: 1_000,
        max_exit_delay_ms: 2_000,
    }
}

fn report_for(spec: &ExperimentSpec) -> Value {
    json!({
        "schema": 1,
        "source_sha256": spec.source_sha256,
        "params_sha256": spec.params_sha256,
        "feature_abi": spec.feature_abi,
        "promotion_authorized": false,
        "scientific_state": "insufficient_evidence"
    })
}

fn advance(dir: &Path, spec: ExperimentSpec, now: i64) -> Value {
    let admitted = admit_spec(dir, spec.clone(), now).unwrap();
    let id = admitted["candidate_id"].as_str().unwrap().to_owned();
    record_abi(dir, &id, now + 1).unwrap();
    record_replay(dir, &id, &report_for(&spec), now + 2).unwrap();
    open_shadow(
        dir,
        &id,
        &json!([{"key":"SYNTHETIC|A","entry_ms":2000,"due_ms":3000}]),
        now + 3,
    )
    .unwrap();
    mature(
            dir,
            &id,
            &json!({"quotes":[{"key":"SYNTHETIC|A","source_ms":3000,"receipt_ms":3000,"interpolated":false}]}),
            now + 4,
        )
        .unwrap()
}

#[test]
fn lifecycle_admits_ingests_and_enforces_scientific_gates() {
    let dir = tempfile::tempdir().unwrap();
    let book = dir.path();
    let leaked = ExperimentSpec {
        training_to_ms: 2_000,
        ..spec("overlap")
    };
    assert_eq!(
        admit_spec(book, leaked, 10).unwrap_err().to_string(),
        "lifecycle_spec_invalid"
    );
    let first = advance(book, spec("first cost-aware tape"), 20);
    assert_eq!(first["state"], "eligible");
    assert_eq!(first["scientific_state"], "eligible");
    assert_eq!(first["activation_authorized"], false);
    assert_eq!(first["edge_qualified"], false);
    assert_eq!(first["checks"]["multiple_testing"], "pass");
    let first_id = first["candidate_id"].as_str().unwrap().to_owned();
    // Terminal maturity is eligible research evidence with the original
    // candidate id and result hash; no queue or provider call is created.
    let (c, mut research_cfg) = super::super::super::decision_owner::owner_fixture_config(book);
    research_cfg.owner_tasks = Some(super::super::super::owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: super::super::super::owner_tasks::ReviewSettings::default(),
        research: Some(super::super::super::owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: Some(Default::default()),
        }),
    });
    research_cfg.workspace = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
    let due = super::super::super::supervisor::research_due_evidence(&c, &research_cfg).unwrap();
    assert!(due.iter().any(|evidence| {
        evidence.kind == super::super::super::owner_tasks::ResearchEvidenceKind::Maturity
            && evidence.task_id == first_id
            && evidence.maturity_state.as_deref() == Some("eligible")
    }));
    assert_eq!(
        record_replay(
            book,
            &first_id,
            &report_for(&spec("first cost-aware tape")),
            30
        )
        .unwrap_err()
        .to_string(),
        "lifecycle_transition_invalid"
    );
    let selected = select_paper(book, &first_id, 40).unwrap();
    assert_eq!(selected["state"], "paper_selected");
    assert_eq!(selected["activation_authorized"], false);
    let second = advance(book, spec("second look at the same bytes"), 50);
    assert_eq!(second["state"], "insufficient");
    assert_eq!(second["checks"]["multiple_testing"], "unadjusted");
    assert_eq!(second["scientific_state"], "insufficient_evidence");
    assert_eq!(second["activation_authorized"], false);

    let settings: WorkspaceSettings = serde_json::from_value(json!({"enabled": true})).unwrap();
    super::super::super::workspace::ensure(book, &settings).unwrap();
    execute(
        book,
        &settings,
        WorkspaceOp::Write {
            path: "reports/scores.json".into(),
            text: json!({
                "script":"scanner/cost_aware_momentum.py",
                "status":"ok",
                "scores":{"SYNTHETIC|A":1.25},
                "score_count":1,
                "params_source":"scanner/cost_aware_momentum.params.json",
                "execution_authorized":false,
                "evidence_only":true,
                "authority":"advisory_scores_not_orders"
            })
            .to_string(),
        },
    )
    .unwrap();
    let ingested = dispatch(
        book,
        &json!({
            "action":"ingest_workspace",
            "report":"reports/scores.json",
            "session":"2026-10-16",
            "hypothesis":"seeded scanner scores stay advisory",
            "training_from_ms":1000,
            "training_to_ms":1999,
            "selection_from_ms":2000,
            "selection_to_ms":3000,
            "holding_ms":1000,
            "max_exit_delay_ms":2000,
            "at_ms":60
        }),
        60,
    )
    .unwrap();
    assert_eq!(ingested["state"], "sandbox_abi");
    assert_eq!(ingested["feature_abi"], "cost_aware_momentum_v1");
    assert_eq!(ingested["activation_authorized"], false);
    assert_eq!(ingested["scientific_state"], "insufficient_evidence");
    execute(
        book,
        &settings,
        WorkspaceOp::Write {
            path: "reports/orders.json".into(),
            text: json!({
                "script":"scanner/cost_aware_momentum.py",
                "status":"ok",
                "scores":{"SYNTHETIC|A":1.25},
                "authority":"advisory_scores_not_orders",
                "execution_authorized":true
            })
            .to_string(),
        },
    )
    .unwrap();
    assert_eq!(
        dispatch(
            book,
            &json!({
                "action":"ingest_workspace",
                "report":"reports/orders.json",
                "session":"2026-10-16",
                "hypothesis":"must refuse execution authority",
                "training_from_ms":1000,
                "training_to_ms":1999,
                "selection_from_ms":2000,
                "selection_to_ms":3000,
                "holding_ms":1000,
                "max_exit_delay_ms":2000,
                "at_ms":70
            }),
            70,
        )
        .unwrap_err()
        .to_string(),
        "lifecycle_ingest_execution_refused"
    );
    let listed = library(book, "2026-10-16").unwrap();
    assert_eq!(listed["activation_authorized"], false);
    assert!(
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["candidate_id"] == ingested["candidate_id"])
    );
    let merged = merge_into_library(
        Some(json!({"status":"disabled","items":[],"activation_authorized":false})),
        &listed,
    )
    .unwrap();
    assert_eq!(merged["status"], "ready");
    assert_eq!(merged["activation_authorized"], false);
    assert_eq!(
        retire(book, &first_id, "operator_withdrawn", 80).unwrap()["state"],
        "retired"
    );
    // #232: only an observation-purpose spec may take Replayed -> Evaluated,
    // and a parked measured observation is not reported as failed checks.
    let replayed = |label: &str, abi: &str, now: i64| {
        let spec = ExperimentSpec {
            feature_abi: abi.into(),
            ..spec(label)
        };
        let id = admit_spec(book, spec.clone(), now).unwrap()["candidate_id"]
            .as_str()
            .unwrap()
            .to_owned();
        record_abi(book, &id, now + 1).unwrap();
        record_replay(book, &id, &report_for(&spec), now + 2).unwrap();
        id
    };
    let tape = replayed("replay only", "cash_quote_action_tape_v1", 90);
    let refused = apply_transition(
        book,
        &tape,
        "evaluated",
        "observation_matured",
        &json!({"measurement_sha256": hash("m")}),
        95,
        |_, _, _| Ok(()),
    );
    assert_eq!(
        refused.unwrap_err().to_string(),
        "lifecycle_transition_invalid"
    );
    let observed = replayed(
        "prospective observation",
        sadr_cli::candidate_execution::OBSERVATION_ABI,
        100,
    );
    let measured = apply_transition(
        book,
        &observed,
        "evaluated",
        "observation_matured",
        &json!({"measurement_sha256": hash("m")}),
        105,
        |_, _, projection| {
            projection.observation_measurement = Some(json!({"status":"verified"}));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(measured["state"], "evaluated");
    assert_eq!(measured["scientific_state"], "observation_measured");
    assert_eq!(measured["checks"], Value::Null);
    assert_eq!(measured["activation_authorized"], false);
    assert_eq!(
        conclude_if_ready(book, &observed, 106).unwrap().unwrap()["scientific_state"],
        "observation_measured"
    );
    assert_eq!(
        scientific_state("evaluated", false),
        "insufficient_evidence"
    );
}
