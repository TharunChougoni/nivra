use crate::runtime::{Config, Result, digest, market_archive};
use chrono::NaiveDate;
use nivra_core::{autoresearch::Stage, market::Segment};
use nivra_research::option_replay::{self as replay, Policy, Ranking, TAPE_UNAVAILABLE};
use nivra_store::{
    market_archive::options::{option_coverage, option_watermark},
    research_memory::{OptionStage, Store},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompareArgs {
    pub ranking: Ranking,
    pub hypothesis: String,
}

fn policy(c: &Config) -> Result<Policy> {
    if c.sleeves.options.capital <= nivra_core::finance::Amount::ZERO {
        return Err("option_sleeve_capital_required".into());
    }
    Ok(Policy {
        fees: c
            .paper_costs_by_segment
            .get(&Segment::NseFo)
            .ok_or("option_replay_costs_unavailable")?
            .clone(),
        sizing: c.sleeves.options.policy(),
        capital: c.sleeves.options.capital,
        session_start_minute: c.execution.session_start_minute,
        session_end_minute: c.execution.session_end_minute,
    })
}

fn unavailable(session: NaiveDate) -> Value {
    json!({
        "stage":Stage::OptionReplay, "session":session, "status":"waiting",
        "reason":TAPE_UNAVAILABLE, "execution_authorized":false,
    })
}

/// The controller and timer publish this under the existing loop lease.
pub(super) fn refresh(
    c: &Config,
    store: &Store,
    session: NaiveDate,
    now: i64,
    stop: &dyn Fn() -> bool,
) -> Result<()> {
    if !c.nifty_options.enabled
        || store.option_stage(session)?.is_some_and(|stage| {
            stage.cohort_digest.is_some() || now.saturating_sub(stage.checked_ms) < 60_000
        })
    {
        return Ok(());
    }
    let mut stage = OptionStage {
        session,
        checked_ms: now,
        proposed_ms: None,
        cohort_digest: None,
        body: unavailable(session),
    };
    let result = (|| -> Result<()> {
        let mut archive = market_archive::open(c)?;
        let coverage = option_coverage(&mut archive, session)?;
        let has_rows = coverage.gates > 0 && coverage.quotes > 0 && coverage.masters > 0;
        stage.body["coverage"] = serde_json::to_value(coverage)?;
        if !has_rows {
            return Ok(());
        }
        let policy = policy(c)?;
        let watermark = option_watermark(&mut archive, session)?;
        let cohort = replay::read(&mut archive, session, watermark, stop)?;
        let baseline = replay::compare(&policy, &cohort, Ranking::Gate)?;
        stage.body["observations"] = json!(baseline.observations);
        if baseline.reason.is_none() {
            stage.cohort_digest = Some(digest(&serde_json::to_vec(&cohort)?));
            stage.body["status"] = json!("ready");
            stage.body["reason"] = Value::Null;
            stage.body["policy"] = serde_json::to_value(policy)?;
            stage.body["watermark"] = json!(watermark);
        }
        Ok(())
    })();
    if let Err(error) = result {
        stage.body["detail"] = json!(crate::runtime::harness::failure_reason(error.as_ref()));
    }
    Ok(store.record_option_stage(&stage)?)
}

pub(super) fn status(c: &Config, store: &Store) -> Result<Value> {
    if !c.nifty_options.enabled {
        return Ok(json!({"stage":Stage::OptionReplay,"status":"disabled"}));
    }
    let Some(mut stage) = store.latest_option_stage()? else {
        return Ok(unavailable(c.previous_session));
    };
    if let Some(obj) = stage.body.as_object_mut() {
        obj.remove("policy");
    }
    if let Some(report) = store.latest_option_comparison(stage.session)? {
        stage.body["latest_comparison"] = json!({
            "status":report["status"], "reason":report["reason"], "ranking":report["ranking"],
            "hypothetical_difference":report["hypothetical_difference"],
            "cohort_sha256":report["cohort_sha256"],
        });
    }
    Ok(stage.body)
}

pub(super) fn compare(
    c: &Config,
    store: &Store,
    args: &CompareArgs,
    now: i64,
    stop: &dyn Fn() -> bool,
) -> Result<Value> {
    if !c.nifty_options.enabled {
        return Ok(unavailable(c.previous_session));
    }
    let stage = match store.latest_option_stage()? {
        Some(stage) => stage,
        None => {
            let session = super::cli::calendar(c, now).2;
            let stage = OptionStage {
                session,
                checked_ms: 0,
                proposed_ms: None,
                cohort_digest: None,
                body: unavailable(session),
            };
            store.record_option_stage(&stage)?;
            stage
        }
    };
    let ranking = serde_json::to_value(args.ranking)?;
    let ranking = ranking.as_str().ok_or("option_ranking_invalid")?;
    if let Some(report) = store.option_hypothesis(stage.session, ranking, &args.hypothesis)? {
        return Ok(report);
    }
    let Some(cohort_digest) = stage.cohort_digest else {
        return status(c, store);
    };
    // The stage seals the cohort and the fee and sleeve policies before any
    // proposed ranking is evaluated. Verify the digest again on every run.
    let mut archive = market_archive::open(c)?;
    let watermark = stage.body["watermark"]
        .as_i64()
        .ok_or("option_replay_watermark_missing")?;
    let cohort = replay::read(&mut archive, stage.session, watermark, stop)?;
    if digest(&serde_json::to_vec(&cohort)?) != cohort_digest {
        return Ok(unavailable(stage.session));
    }
    let policy: Policy = serde_json::from_value(stage.body["policy"].clone())?;
    let mut output = serde_json::to_value(replay::compare(&policy, &cohort, args.ranking)?)?;
    output["stage"] = json!(Stage::OptionReplay);
    output["cohort_sha256"] = json!(cohort_digest);
    output["recorded_ms"] = json!(now);
    output["status"] = json!(if output["reason"].is_null() {
        "compared"
    } else {
        "waiting"
    });
    Ok(store.finish_option_comparison(stage.session, ranking, &output)?)
}

#[cfg(test)]
mod tests;
