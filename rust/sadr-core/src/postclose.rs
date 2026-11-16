//! Bounded post-close evidence and review output. Digests establish consistency,
//! not authenticity; callers supply sanitized projections of verified local artifacts.
//! No model output here can authorize execution, configuration or strategy changes.
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const MAX_EVIDENCE: usize = 256;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Report,
    Fill,
    Event,
    Attempt,
    Health,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceItem {
    pub id: String,
    pub kind: EvidenceKind,
    pub summary: String,
    pub artifact_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceInput {
    /// Opaque broker/account binding; never a raw account payload.
    pub account: String,
    pub session: NaiveDate,
    pub session_binding: String,
    pub report_sha256: String,
    pub state_sha256: String,
    pub evidence: Vec<EvidenceItem>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedEvidence {
    pub input: EvidenceInput,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CitedClaim {
    pub text: String,
    pub evidence_ids: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Observation,
    Hypothesis,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryProposal {
    pub kind: MemoryKind,
    pub text: String,
    pub evidence_ids: Vec<String>,
    pub uncertainty: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOutput {
    pub observations: Vec<CitedClaim>,
    pub hypotheses: Vec<CitedClaim>,
    pub uncertainties: Vec<String>,
    pub memory: Vec<MemoryProposal>,
}
fn digest(v: &impl Serialize) -> Result<String> {
    Ok(Sha256::digest(serde_json::to_vec(v)?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:|".contains(&b))
}
/// This rejects common credential/log envelopes, not arbitrary secrets. Runtime
/// must construct summaries from approved typed fields, never redact raw logs here.
fn text(s: &str, max: usize) -> bool {
    if s.trim().is_empty() || s.len() > max || s.chars().any(|c| c.is_control()) {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    ![
        "authorization",
        "bearer ",
        "access_token",
        "refresh_token",
        "api_key",
        "api-key",
        "password",
        "client_secret",
        "private key",
        "<think",
        "chain_of_thought",
        "raw_account",
        "set-cookie",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}
fn item(e: &EvidenceItem) -> bool {
    identifier(&e.id) && text(&e.summary, 1024) && hash(&e.artifact_sha256)
}
pub fn seal(input: EvidenceInput) -> Result<SealedEvidence> {
    if !identifier(&input.account)
        || !hash(&input.session_binding)
        || !hash(&input.report_sha256)
        || !hash(&input.state_sha256)
        || input.evidence.is_empty()
        || input.evidence.len() > MAX_EVIDENCE
        || input.evidence.iter().any(|e| !item(e))
        || input
            .evidence
            .iter()
            .map(|e| &e.id)
            .collect::<BTreeSet<_>>()
            .len()
            != input.evidence.len()
        || serde_json::to_vec(&input)?.len() > 400_000
    {
        return Err("postclose_evidence_invalid".into());
    }
    let sha256 = digest(&input)?;
    Ok(SealedEvidence { input, sha256 })
}
impl SealedEvidence {
    pub fn validate(&self) -> Result<()> {
        if seal(self.input.clone())?.sha256 != self.sha256 {
            return Err("postclose_seal_changed".into());
        }
        Ok(())
    }
}
fn citations(ids: &[String], allowed: &BTreeSet<&str>) -> bool {
    !ids.is_empty()
        && ids.len() <= 8
        && ids.iter().all(|id| allowed.contains(id.as_str()))
        && ids.iter().collect::<BTreeSet<_>>().len() == ids.len()
}
fn validate_review(output: &ReviewOutput, sealed: &SealedEvidence) -> Result<()> {
    sealed.validate()?;
    let allowed = sealed
        .input
        .evidence
        .iter()
        .map(|e| e.id.as_str())
        .collect::<BTreeSet<_>>();
    if output.observations.len() > 16
        || output.hypotheses.len() > 16
        || output.uncertainties.is_empty()
        || output.uncertainties.len() > 16
        || output.memory.len() > 8
        || output
            .memory
            .iter()
            .enumerate()
            .any(|(i, m)| output.memory[..i].contains(m))
        || output
            .observations
            .iter()
            .chain(&output.hypotheses)
            .any(|c| !text(&c.text, 1024) || !citations(&c.evidence_ids, &allowed))
        || output.uncertainties.iter().any(|u| !text(u, 1024))
        || output.memory.iter().any(|m| {
            !text(&m.text, 512)
                || !text(&m.uncertainty, 512)
                || !citations(&m.evidence_ids, &allowed)
        })
    {
        return Err("postclose_review_invalid".into());
    }
    // Memory is a bounded carry-forward of a cited review claim, never a new claim.
    for m in &output.memory {
        let claims = match m.kind {
            MemoryKind::Observation => &output.observations,
            MemoryKind::Hypothesis => &output.hypotheses,
        };
        if !claims
            .iter()
            .any(|c| c.text == m.text && c.evidence_ids == m.evidence_ids)
        {
            return Err("postclose_memory_not_reviewed".into());
        }
    }
    Ok(())
}
pub fn validate_output(raw: &str, sealed: &SealedEvidence) -> Result<ReviewOutput> {
    if raw.len() > 65_536 {
        return Err("postclose_output_bound".into());
    }
    let value = crate::native::decode(raw.as_bytes()).map_err(|_| "postclose_output_invalid")?;
    let output: ReviewOutput =
        serde_json::from_value(value).map_err(|_| "postclose_output_invalid")?;
    validate_review(&output, sealed)?;
    Ok(output)
}
#[cfg(test)]
pub(crate) fn behavioral_assertions() {
    use serde_json::json;
    let day = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
    let input = EvidenceInput {
        account: "account-hash".into(),
        session: day,
        session_binding: "a".repeat(64),
        report_sha256: "b".repeat(64),
        state_sha256: "c".repeat(64),
        evidence: vec![EvidenceItem {
            id: "fill:0".into(),
            kind: EvidenceKind::Fill,
            summary: "One paper fill observed".into(),
            artifact_sha256: "d".repeat(64),
        }],
    };
    let sealed = seal(input.clone()).unwrap();
    assert_eq!(sealed, seal(input.clone()).unwrap());
    let raw = json!({"observations":[{"text":"One paper fill observed","evidence_ids":["fill:0"]}],"hypotheses":[],"uncertainties":["No execution quality conclusion from one fill"],"memory":[{"kind":"observation","text":"One paper fill observed","evidence_ids":["fill:0"],"uncertainty":"Synthetic evidence only"}]}).to_string();
    let output = validate_output(&raw, &sealed).unwrap();
    assert!(validate_output(&raw.replace("fill:0", "invented"), &sealed).is_err());
    assert!(
        validate_output(
            &raw.replace("\"hypotheses\":[]", "\"hypotheses\":[],\"hypotheses\":[]"),
            &sealed
        )
        .is_err()
    );
    assert!(
        validate_output(
            &raw.replace("\"observations\":", "\"orders\":[],\"observations\":"),
            &sealed
        )
        .is_err()
    );
    let mut bad = input.clone();
    bad.evidence[0].summary = "Authorization: Bearer secret".into();
    assert!(seal(bad).is_err());
    let mut bad = input;
    bad.evidence.push(bad.evidence[0].clone());
    assert!(seal(bad).is_err());
    let mut bad = sealed.clone();
    bad.input.state_sha256 = "e".repeat(64);
    assert!(bad.validate().is_err());
    let mut bad = output;
    bad.memory[0].kind = MemoryKind::Hypothesis;
    assert!(validate_review(&bad, &sealed).is_err());
}
