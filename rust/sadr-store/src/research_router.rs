//! One-shot research proposal orchestration. The completion is injected by the caller.
//! This module does not open a provider, journal, socket, or interpreter.
//! Preflight runs before the attempt directory is created. Cancellation is recorded
//! before drop, which would otherwise preserve the claim as FAILED.
use chrono::DateTime;
use sadr_core::{
    native,
    research_evidence::{EvidenceRequest, EvidenceTools},
    research_prompt::{HistoricalPlan, HistoricalPrompt},
    research_proposal::Proposal,
    research_script::ScriptRequest,
    research_signal::DailyReview,
};
use serde::Serialize;
use serde_json::Value;
use std::{fmt, path::Path};

use crate::research_attempt::{Attempt, Failure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallFault {
    Failed,
    Cancelled,
}
impl fmt::Display for CallFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Failed => "research_proposal_completion_failed",
            Self::Cancelled => "research_proposal_completion_cancelled",
        })
    }
}
impl std::error::Error for CallFault {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    Cancelled,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "Research proposal unavailable; preserve attempt, no retry",
            Self::Cancelled => "research_proposal_cancelled",
        })
    }
}
impl std::error::Error for Error {}

pub struct ProposalInput<'a> {
    pub plan: &'a HistoricalPlan,
    pub original: &'a DailyReview,
    pub frontier: &'a Value,
    pub distinct_from_cheap: bool,
    pub destination: &'a Path,
    pub confirmed_hash: &'a str,
    pub now: &'a str,
    pub configuration_hash: Option<&'a str>,
}

struct Prepared {
    fields: Value,
    claim_prompt: HistoricalPrompt,
    tools: bool,
}
struct Built {
    proposal: Proposal,
    request: Option<ScriptRequest>,
}
#[derive(Serialize)]
struct ScriptBody<'a> {
    code: &'a str,
    capture_id: &'a str,
    runtime_hash: &'a str,
    trial_ref: &'a str,
    limits: &'a Value,
}

const LOOKUP_ABSTAIN: &[u8] = br#"{"action":"abstain","rationale":"Evidence lookup abstained"}"#;

fn hex_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
fn aware_and_current(now: &str, received: &str) -> bool {
    let Ok(now) = DateTime::parse_from_rfc3339(now) else {
        return false;
    };
    let Ok(received) = DateTime::parse_from_rfc3339(received) else {
        return false;
    };
    now >= received
}
fn prepare(input: &ProposalInput<'_>) -> Result<Prepared, Error> {
    let fields = native::decode(input.plan.canonical_json()).map_err(|_| Error::Unavailable)?;
    let received = input.original.report()["received_at"]
        .as_str()
        .ok_or(Error::Unavailable)?;
    if input.confirmed_hash != input.plan.hash()
        || fields["capture_id"].as_str() != Some(input.original.capture_hash())
        || &fields["frontier"] != input.frontier
        || !input.distinct_from_cheap
        || !aware_and_current(input.now, received)
        || input.configuration_hash.is_some_and(|s| !hex_hash(s))
    {
        return Err(Error::Unavailable);
    }
    let tools = matches!(fields.get("tools"), Some(Value::Object(_)));
    if tools {
        let raw = serde_json::to_vec(&fields["tools"]).map_err(|_| Error::Unavailable)?;
        EvidenceTools::parse(&raw)
            .and_then(|tools| tools.available(input.now))
            .map_err(|_| Error::Unavailable)?;
    }
    let claim_prompt = if tools {
        input.plan.historical_catalog_prompt(input.original)
    } else {
        input.plan.historical_proposal_prompt(input.original, None)
    }
    .map_err(|_| Error::Unavailable)?;
    Ok(Prepared {
        fields,
        claim_prompt,
        tools,
    })
}
fn invoke(
    attempt: &mut Attempt,
    guard: &mut dyn FnMut() -> bool,
    complete: &mut dyn FnMut(&str) -> Result<Vec<u8>, CallFault>,
    prompt: &str,
) -> Result<Vec<u8>, CallFault> {
    if !guard() {
        return Err(CallFault::Failed);
    }
    attempt.verify().map_err(|_| CallFault::Failed)?;
    let payload = complete(prompt)?;
    if !guard() {
        return Err(CallFault::Failed);
    }
    attempt.verify().map_err(|_| CallFault::Failed)?;
    Ok(payload)
}
fn script_request(fields: &Value, code: &str) -> Result<ScriptRequest, CallFault> {
    let raw = serde_json::to_vec(&ScriptBody {
        code,
        capture_id: fields["capture_id"].as_str().ok_or(CallFault::Failed)?,
        runtime_hash: fields["runtime_hash"].as_str().ok_or(CallFault::Failed)?,
        trial_ref: fields["trial_ref"].as_str().ok_or(CallFault::Failed)?,
        limits: &fields["limits"],
    })
    .map_err(|_| CallFault::Failed)?;
    ScriptRequest::parse(&raw).map_err(|_| CallFault::Failed)
}
fn stage(fields: &Value, payload: &[u8], selected: &[&str]) -> Result<Built, CallFault> {
    let proposal = Proposal::parse(payload, selected).map_err(|_| CallFault::Failed)?;
    let request = match proposal.code() {
        Some(code) => Some(script_request(fields, code)?),
        None => None,
    };
    Ok(Built { proposal, request })
}
fn assemble(
    attempt: &mut Attempt,
    input: &ProposalInput<'_>,
    prepared: &Prepared,
    guard: &mut dyn FnMut() -> bool,
    complete: &mut dyn FnMut(&str) -> Result<Vec<u8>, CallFault>,
) -> Result<Built, CallFault> {
    if !prepared.tools {
        let payload = invoke(attempt, guard, complete, prepared.claim_prompt.text())?;
        return stage(&prepared.fields, &payload, &[]);
    }
    let payload = invoke(attempt, guard, complete, prepared.claim_prompt.text())?;
    let lookup = EvidenceRequest::parse(&payload).map_err(|_| CallFault::Failed)?;
    let ids = lookup.source_ids().to_vec();
    attempt
        .record_selection(&lookup)
        .map_err(|_| CallFault::Failed)?;
    if ids.is_empty() {
        let proposal = Proposal::parse(LOOKUP_ABSTAIN, &[]).map_err(|_| CallFault::Failed)?;
        return Ok(Built {
            proposal,
            request: None,
        });
    }
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let selected = input
        .plan
        .historical_proposal_prompt(input.original, Some(&refs))
        .map_err(|_| CallFault::Failed)?;
    attempt
        .record_proposal_prompt(selected.hash())
        .map_err(|_| CallFault::Failed)?;
    let payload = invoke(attempt, guard, complete, selected.text())?;
    stage(&prepared.fields, &payload, &refs)
}

/// Stage one proposal for a separate review. The returned record does not authorize
/// execution, promotion, registration, or a retry.
pub fn propose_research_script(
    input: &ProposalInput<'_>,
    guard: &mut dyn FnMut() -> bool,
    complete: &mut dyn FnMut(&str) -> Result<Vec<u8>, CallFault>,
) -> Result<Value, Error> {
    let prepared = prepare(input)?;
    let mut attempt = Attempt::claim(
        input.destination,
        input.plan,
        input.confirmed_hash,
        prepared.claim_prompt.hash(),
        input.configuration_hash,
    )
    .map_err(|_| Error::Unavailable)?;
    match assemble(&mut attempt, input, &prepared, guard, complete) {
        Ok(built) => attempt
            .finish(built.proposal, built.request)
            .map_err(|_| Error::Unavailable),
        Err(fault) => {
            // Drop would preserve a non-terminal claim as FAILED. Write cancellation first.
            let _ = attempt.fail(match fault {
                CallFault::Cancelled => Failure::Cancelled,
                CallFault::Failed => Failure::Failed,
            });
            Err(match fault {
                CallFault::Cancelled => Error::Cancelled,
                CallFault::Failed => Error::Unavailable,
            })
        }
    }
}
