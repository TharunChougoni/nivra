//! Bounded post-close evidence and advisory memory. Digests establish consistency,
//! not authenticity; callers supply sanitized projections of verified local artifacts.
//! No model output here can authorize execution, configuration or strategy changes.
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const MAX_EVIDENCE: usize = 256;
pub const MAX_MEMORY: usize = 128;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRole {
    Operations,
    Strategy,
}
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewProvenance {
    pub role: ReviewRole,
    pub model_id: String,
    pub prompt_version: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryEntry {
    pub id: String,
    pub account: String,
    pub session: NaiveDate,
    pub session_binding: String,
    pub report_sha256: String,
    pub state_sha256: String,
    pub input_sha256: String,
    pub review_sha256: String,
    pub provenance: ReviewProvenance,
    pub proposal: MemoryProposal,
    /// Exact cited sanitized items, in proposal citation order.
    pub evidence: Vec<EvidenceItem>,
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
fn provenance(p: &ReviewProvenance) -> bool {
    identifier(&p.model_id) && identifier(&p.prompt_version)
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
fn memory_digest(entry: &MemoryEntry) -> Result<String> {
    let mut copy = entry.clone();
    copy.id.clear();
    digest(&copy)
}
pub fn make_memory(
    sealed: &SealedEvidence,
    output: &ReviewOutput,
    p: &ReviewProvenance,
) -> Result<Vec<MemoryEntry>> {
    validate_review(output, sealed)?;
    if !provenance(p) {
        return Err("postclose_provenance_invalid".into());
    }
    let review_sha256 = digest(output)?;
    output
        .memory
        .iter()
        .map(|proposal| {
            let mut entry = MemoryEntry {
                id: String::new(),
                account: sealed.input.account.clone(),
                session: sealed.input.session,
                session_binding: sealed.input.session_binding.clone(),
                report_sha256: sealed.input.report_sha256.clone(),
                state_sha256: sealed.input.state_sha256.clone(),
                input_sha256: sealed.sha256.clone(),
                review_sha256: review_sha256.clone(),
                provenance: p.clone(),
                proposal: proposal.clone(),
                evidence: proposal
                    .evidence_ids
                    .iter()
                    .map(|id| {
                        sealed
                            .input
                            .evidence
                            .iter()
                            .find(|e| e.id == *id)
                            .cloned()
                            .ok_or_else(|| "postclose_citation_missing".into())
                    })
                    .collect::<Result<Vec<_>>>()?,
            };
            entry.id = memory_digest(&entry)?;
            Ok(entry)
        })
        .collect()
}
/// Select recent prior-session advisory memory. Runtime must independently verify
/// each source journal/closed book and carry-over chain; hashes are no authenticity.
/// Inputs and original journals are never pruned or changed.
pub fn memory_for(
    entries: &[MemoryEntry],
    account: &str,
    next_session: NaiveDate,
    max_entries: usize,
) -> Result<Vec<MemoryEntry>> {
    if !identifier(account) || entries.len() > MAX_MEMORY || max_entries > 32 {
        return Err("postclose_memory_bound".into());
    }
    let mut ids = BTreeSet::new();
    for e in entries {
        let allowed = e
            .evidence
            .iter()
            .map(|i| i.id.as_str())
            .collect::<BTreeSet<_>>();
        if e.account != account
            || e.session >= next_session
            || !hash(&e.id)
            || !hash(&e.session_binding)
            || !hash(&e.report_sha256)
            || !hash(&e.state_sha256)
            || !hash(&e.input_sha256)
            || !hash(&e.review_sha256)
            || !provenance(&e.provenance)
            || !text(&e.proposal.text, 512)
            || !text(&e.proposal.uncertainty, 512)
            || !citations(&e.proposal.evidence_ids, &allowed)
            || e.evidence.len() != e.proposal.evidence_ids.len()
            || e.evidence
                .iter()
                .zip(&e.proposal.evidence_ids)
                .any(|(item, id)| !self::item(item) || item.id != *id)
            || memory_digest(e)? != e.id
            || !ids.insert(&e.id)
        {
            return Err("postclose_memory_provenance_invalid".into());
        }
    }
    let mut selected = entries.to_vec();
    selected.sort_by(|a, b| b.session.cmp(&a.session).then_with(|| a.id.cmp(&b.id)));
    selected.truncate(max_entries);
    Ok(selected)
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
    let p = ReviewProvenance {
        role: ReviewRole::Operations,
        model_id: "grok-4.7".into(),
        prompt_version: "postclose-v1".into(),
    };
    let memory = make_memory(&sealed, &output, &p).unwrap();
    assert_eq!(memory.len(), 1);
    assert_eq!(
        memory_for(&memory, "account-hash", day.succ_opt().unwrap(), 8).unwrap(),
        memory
    );
    assert!(memory_for(&memory, "other", day.succ_opt().unwrap(), 8).is_err());
    assert!(memory_for(&memory, "account-hash", day, 8).is_err());
    let mut bad = memory.clone();
    bad[0].proposal.text = "Altered".into();
    assert!(memory_for(&bad, "account-hash", day.succ_opt().unwrap(), 8).is_err());
    assert!(
        memory_for(
            &[memory[0].clone(), memory[0].clone()],
            "account-hash",
            day.succ_opt().unwrap(),
            8
        )
        .is_err()
    );
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
    assert!(make_memory(&sealed, &bad, &p).is_err());
}
