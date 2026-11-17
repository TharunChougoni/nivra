//! Trusted, explicit laboratory lifecycle commands; never exposed as model tools.
use nivra_store::strategy_results::{LabConfig, Qualification, ResultsDb};
use serde::Deserialize;
use std::{error::Error, fs::File, io::Read, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QualificationInput {
    proposal_id: String,
    replay_passed: bool,
    shadow_days: u32,
    qualification: Qualification,
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Box<dyn Error>> {
    let file = File::open(path).map_err(|_| "strategy_approval_input_rejected")?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > 1_048_576 {
        return Err("strategy_approval_input_rejected".into());
    }
    let mut bytes = Vec::new();
    file.take(1_048_577).read_to_end(&mut bytes)?;
    if bytes.len() > 1_048_576 {
        return Err("strategy_approval_input_rejected".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "strategy_approval_input_rejected".into())
}
pub fn qualify(database: &Path, input: &Path) -> Result<(), Box<dyn Error>> {
    let input: QualificationInput = read(input)?;
    let db = ResultsDb::open(database)?;
    // One transaction prevents partially registered qualification attestations.
    db.record_measured_qualification(
        &input.proposal_id,
        input.replay_passed,
        input.shadow_days,
        &input.qualification,
    )?;
    println!("strategy_qualification_recorded_no_activation");
    Ok(())
}
pub fn approve(
    database: &Path,
    proposal: &str,
    approval_ref: &str,
    between_sessions: bool,
    config: &Path,
) -> Result<(), Box<dyn Error>> {
    let config: LabConfig = read(config)?;
    config.validate()?;
    if !config.enabled {
        return Err("strategy_orchestrator_disabled".into());
    }
    ResultsDb::open(database)?.approve_with_policy(
        proposal,
        "Tharun",
        approval_ref,
        between_sessions,
        config.min_shadow_days,
        &config.promotion,
    )?;
    println!("strategy_owner_approval_recorded_no_activation");
    Ok(())
}
