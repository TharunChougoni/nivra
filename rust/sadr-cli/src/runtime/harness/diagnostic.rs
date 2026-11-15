//! Explicit synthetic protocol diagnostic. Its identity cannot validate as a paper mandate.
use super::super::orchestrator_model::Trigger;
use super::*;
use sadr_core::{
    execution::{Instrument, Portfolio},
    finance::{Amount, Price},
    postclose as contract,
};

fn run(
    path: &Path,
    directory: &Path,
    selected: Option<&str>,
    continuation: bool,
    authority: Option<&Path>,
) -> Result<Value> {
    let cfg = PlannerConfig::load(path)?;
    let digest_role = cfg.role(Trigger::Digest);
    if !cfg.enabled || !digest_role.enabled {
        return Err("harness_diagnostic_disabled".into());
    }
    if !directory.is_absolute()
        || directory.exists() != continuation
        || (continuation && selected.is_none())
    {
        return Err("harness_diagnostic_new_directory_required".into());
    }
    sadr_store::native_grant::file_path_ok(directory)
        .map_err(|_| "harness_diagnostic_path_invalid")?;
    if !continuation {
        fs::DirBuilder::new().mode(0o700).create(directory)?;
    }
    // No production account, token, read journal, session registry or paper.sqlite.
    // A diagnostic has its own first-created authority, never a copied/reset journal.
    let mut c: Config = serde_json::from_value(json!({
        "account":"synthetic-harness-diagnostic", "session":"2000-01-04", "previous_session":"2000-01-03",
        "calendar_source":"synthetic", "directory":directory,
        "token_file":directory.join("NO_CREDENTIAL"), "dashboard_token_file":directory.join("NO_CREDENTIAL"),
        "read_journal":directory.join("NO_READ_JOURNAL"), "read_scope":"synthetic-no-network-data",
        "read_journal_device":0,"read_journal_inode":0,"strategy":"synthetic-protocol-check","capital":"5000"
    }))?;
    if let Some(path) = authority {
        let original: Config = serde_json::from_slice(
            &crate::read_bounded(path, 16384)
                .map_err(|_| "harness_diagnostic_authority_unreadable")?,
        )?;
        original.validate()?;
        c = rollover::current(&original)?;
        let (_, body) = rollover::bound_state(&c)?;
        let state: sadr_core::execution::Portfolio = serde_json::from_str(&body)?;
        if !state.closed
            || state.halted
            || !state.positions.is_empty()
            || directory.starts_with(&c.directory)
        {
            return Err("harness_diagnostic_authority_requires_closed_flat_book".into());
        }
    }
    let p = Portfolio::with_capital(Amount::from_integer(5000));
    let instruments = vec![Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05")?,
    }];
    let sealed = contract::seal(contract::EvidenceInput {
        account:digest(b"synthetic-harness-diagnostic"), session:c.session, session_binding:book_binding(&c)?,
        report_sha256:digest(b"synthetic-report-zero-fills"),state_sha256:digest(b"synthetic-state"),
        evidence:vec![contract::EvidenceItem {id:"report".into(),kind:contract::EvidenceKind::Report,
            summary:"Synthetic protocol fixture: zero fills, no positions, missing current quotes. Not real market evidence.".into(),artifact_sha256:digest(b"synthetic-report-zero-fills")}]
    })?;
    let mut bundle = json!({"synthetic":true,"session":"2000-01-04","cash":p.cash,"positions":{},"allowlist":instruments,
        "shortlist":[{"instrument_key":"NSE_EQ|SYNTHETIC","synthetic":true,"current_quote":null}],
        "fees":{"status":"unknown_do_not_trade"},"session_evidence":sealed});
    let identity = json!({"synthetic":true,"traded":false,"configuration_sha256":digest(&serde_json::to_vec(&cfg)?),"binding":book_binding(&c)?});
    let anchor = directory.join("diagnostic-identity.json");
    if continuation {
        let original: Value = serde_json::from_slice(
            &crate::read_bounded(&anchor, 4096)
                .map_err(|_| "harness_diagnostic_original_identity_missing")?,
        )?;
        if original != identity || !c.directory.join("model/usage-reservations.json").exists() {
            return Err("harness_diagnostic_original_authority_changed".into());
        }
    } else {
        rollover::atomic(&anchor, &identity)?;
    }
    if directory.join("digest-validated.json").exists() {
        let saved: Value = serde_json::from_slice(
            &crate::read_bounded(&directory.join("digest-validated.json"), 131072)
                .map_err(|_| "harness_diagnostic_digest_unreadable")?,
        )?;
        let raw = saved["raw_output"]
            .as_str()
            .ok_or("harness_diagnostic_digest_invalid")?;
        validate_digest(raw, &bundle)?;
        bundle["digest"] = serde_json::from_str(raw)?;
    }
    if cfg.post_close.is_none() {
        return Err("harness_diagnostic_review_missing".into());
    }
    let strategy_role = cfg.role(Trigger::Strategy);
    let operations_role = cfg.role(Trigger::Operations);
    let mut summaries = vec![];
    for (name, role) in [
        (Trigger::Digest.as_str(), &digest_role),
        (Trigger::Strategy.as_str(), &strategy_role),
        (Trigger::Operations.as_str(), &operations_role),
    ] {
        if selected.is_some_and(|s| s != name) {
            continue;
        }
        if directory.join(format!("{name}-validated.json")).exists() {
            return Err("harness_diagnostic_role_already_validated".into());
        }
        if !role.enabled || role.daily_call_budget > 8 {
            return Err("harness_diagnostic_eight_call_budget_required".into());
        }
        let base = role
            .model_config
            .as_deref()
            .or(cfg.model_config.as_deref())
            .ok_or("harness_diagnostic_model_config_required")?;
        let mut backend = planner::Subscription::new(base);
        let exercise = if matches!(name, "strategy" | "operations") {
            "Read session_evidence with read_evidence. Exercise one registered analysis if available and report its actual result. Exercise configured web_search and x_search once each for public source context about historical backtest limitations; never treat current search as dated fixture evidence."
        } else {
            "Read each of the five evidence sections with read_evidence to exercise continuation handling."
        };
        let prompt = format!(
            "Synthetic protocol diagnostic, not a market decision or trading session. Missing executable quotes and fees require abstention. {exercise} Produce the strict JSON for your assigned system role. Frozen evidence: {bundle}"
        );
        let raw = super::run(
            &c,
            &cfg,
            name,
            role,
            &prompt,
            &bundle,
            &mut backend,
            &planner::preview_guard(),
        )?;
        match name {
            "digest" => {
                validate_digest(&raw, &bundle)?;
                bundle["digest"] = serde_json::from_str(&raw)?;
            }
            _ => {
                contract::validate_output(&raw, &sealed)?;
            }
        }
        super::publish_validated(&c, name, role, &raw);
        rollover::atomic(
            &directory.join(format!("{name}-validated.json")),
            &json!({"synthetic":true,"raw_output":raw,"transport_evidence":backend.provenance()}),
        )?;
        summaries.push(json!({"role":name,"model":role.model_id,"status":"schema_validated"}));
    }
    let result =
        json!({"synthetic":true,"traded":false,"market_acceptance":false,"roles":summaries});
    rollover::atomic(
        &directory.join(format!("result-{}.json", millis())),
        &result,
    )?;
    Ok(result)
}
pub(crate) fn entry(
    path: &Path,
    directory: &Path,
    selected: Option<&str>,
    continuation: bool,
    authority: Option<&Path>,
) -> ExitCode {
    match run(path, directory, selected, continuation, authority) {
        Ok(v) => {
            println!("{v}");
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("harness_diagnostic_failed_preserve_attempts_no_retry");
            ExitCode::FAILURE
        }
    }
}
