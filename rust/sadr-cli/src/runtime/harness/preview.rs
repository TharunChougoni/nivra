//! Read-only review of a financially bound close snapshot. No carry/rollover authority.
use super::*;
use sadr_core::postclose::{self as contract, EvidenceInput, EvidenceItem, EvidenceKind};

fn run(config: &Path, planner: &Path, output: &Path) -> Result<Value> {
    let c: Config = serde_json::from_slice(
        &crate::read_bounded(config, 16384).map_err(|_| "preview_config_invalid")?,
    )?;
    c.validate()?;
    let c = rollover::current(&c)?;
    let (_, body) = rollover::bound_state(&c)?;
    let state: sadr_core::execution::Portfolio = serde_json::from_str(&body)?;
    state.reconcile()?;
    if !state.closed || state.halted || !state.positions.is_empty() {
        return Err("review_preview_requires_closed_flat_book".into());
    }
    let bytes = crate::read_bounded(&c.directory.join("report.json"), 16 * 1024 * 1024)
        .map_err(|_| "preview_report_unavailable")?;
    let report: Value = serde_json::from_slice(&bytes)?;
    if report["paper_only"] != true
        || report["account"] != c.account
        || report["session"] != c.session.to_string()
    {
        return Err("preview_report_identity".into());
    }
    let reported = report.get("portfolio").ok_or("preview_portfolio_missing")?;
    let financial: sadr_core::execution::Portfolio = serde_json::from_value(reported.clone())?;
    financial.reconcile()?;
    if financial != state {
        return Err("preview_financial_state_changed".into());
    }
    let mut state_hash = digest(body.as_bytes());
    let current: Value = serde_json::from_str(&body)?;
    let source = if &current != reported {
        // This exception is diagnostic only: prove the exact originally sealed
        // bytes, without repairing the state, acknowledgment or rollover gate.
        if reported.get("model_led").is_some() || current["model_led"] != false {
            return Err("preview_schema_drift_unknown".into());
        }
        let prior = body.replacen(",\"model_led\":false", "", 1);
        let prior_value: Value = serde_json::from_str(&prior)?;
        let ack: Value = serde_json::from_slice(
            &crate::read_bounded(&c.directory.join("review.json"), 1024 * 1024)
                .map_err(|_| "preview_seal_unavailable")?,
        )?;
        if &prior_value != reported
            || ack["reason"] != "operator_acknowledged_closed_stopped_book"
            || ack["session"] != report["session"]
            || ack["state_hash"] != digest(prior.as_bytes())
            || ack["report_hash"] != digest(&bytes)
        {
            return Err("preview_original_seal_mismatch".into());
        }
        state_hash = digest(prior.as_bytes());
        "original_sealed_snapshot_default_field_drift"
    } else {
        "current_closed_snapshot"
    };
    let summary=json!({"session":c.session,"cash":financial.cash,"fills":financial.fills.len(),"open_holdings":financial.positions.len(),"stopped":financial.stopped,"halted":financial.halted,"source":source,"limitations":"Historical close review only; no rollover, resume, current quote or execution authority"}).to_string();
    let sealed = contract::seal(EvidenceInput {
        account: digest(format!("upstox\0{}", c.account).as_bytes()),
        session: c.session,
        session_binding: book_binding(&c)?,
        report_sha256: digest(&bytes),
        state_sha256: state_hash,
        evidence: vec![EvidenceItem {
            id: "report".into(),
            kind: EvidenceKind::Report,
            summary,
            artifact_sha256: digest(&bytes),
        }],
    })?;
    let settings = PlannerConfig::load_for_session(planner, c.session)?;
    let role = &settings
        .post_close
        .as_ref()
        .ok_or("review_preview_settings_missing")?
        .strategy;
    if !settings.enabled || !role.enabled {
        return Err("review_preview_disabled".into());
    }
    if !output.is_absolute() || output.exists() || output.starts_with(&c.directory) {
        return Err("preview_output_invalid".into());
    }
    sadr_store::native_grant::file_path_ok(output).map_err(|_| "preview_path_invalid")?;
    fs::DirBuilder::new().mode(0o700).create(output)?;
    rollover::atomic(
        &output.join("sealed-evidence.json"),
        &serde_json::to_value(&sealed)?,
    )?;
    let base = role
        .model_config
        .as_deref()
        .or(settings.model_config.as_deref())
        .ok_or("review_model_config_missing")?;
    let mut backend = planner::Subscription::new(base);
    let bundle = json!({"session_evidence":sealed});
    let raw = super::run(
        &c,
        &settings,
        "strategy",
        role,
        &format!(
            "Diagnostic historical close review. First call read_evidence for session_evidence. If a registered analysis is available, run it once and examine its result before producing the review. Label current-model historical analysis as exploratory. Source: {source}. Investigate only supplied evidence and registered development data. {}",
            serde_json::to_string(&sealed)?
        ),
        &bundle,
        &mut backend,
        &planner::preview_guard(),
    )?;
    rollover::atomic(
        &output.join("model-output.json"),
        &json!({"raw_output":raw,"transport_evidence":backend.provenance()}),
    )?;
    let validated = contract::validate_output(&raw, &sealed)?;
    super::publish_validated(&c, "strategy", role, &raw);
    let result = json!({"paper_only":true,"traded":false,"advisory_only":true,"source":source,"review":validated});
    rollover::atomic(&output.join("result.json"), &result)?;
    Ok(
        json!({"paper_only":true,"traded":false,"status":"validated_advisory_review","observations":validated.observations.len(),"hypotheses":validated.hypotheses.len(),"memory_proposals":validated.memory.len(),"source":source}),
    )
}
pub(crate) fn entry(config: &Path, planner: &Path, output: &Path) -> ExitCode {
    match run(config, planner, output) {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("review_preview_failed_preserve_artifacts_no_retry");
            ExitCode::FAILURE
        }
    }
}
