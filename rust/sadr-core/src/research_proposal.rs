//! Untrusted proposal text only. Validation never registers, executes or promotes code.
use serde::Deserialize;
use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("research_proposal_invalid")
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Propose,
    Abstain,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fields {
    action: Action,
    #[serde(default)]
    code: Option<String>,
    rationale: String,
    #[serde(default)]
    evidence_ids: Vec<String>,
}
pub struct Proposal {
    fields: Fields,
    original: Vec<u8>,
}
impl fmt::Debug for Proposal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResearchProposal(untrusted text, no authority)")
    }
}
impl Proposal {
    /// Citation admission is part of parsing, not an optional later check.
    /// `selected` is the trusted host's exact prior tool selection, never model input.
    pub fn parse(raw: &[u8], selected: &[&str]) -> Result<Self, Error> {
        // Unicode code/rationale limits are scalar counts, with a separate raw JSON cap.
        if raw.len() > 256 * 1024
            || selected.len() > 3
            || selected.iter().collect::<BTreeSet<_>>().len() != selected.len()
        {
            return Err(Error);
        }
        let value = crate::native::decode(raw).map_err(|_| Error)?;
        let f: Fields = serde_json::from_value(value).map_err(|_| Error)?;
        if !(1..=2000).contains(&f.rationale.chars().count())
            || (f.action == Action::Propose) != f.code.is_some()
            || f.code
                .as_ref()
                .is_some_and(|s| !(1..=32000).contains(&s.chars().count()))
            || f.evidence_ids.len() > 3
            || f.evidence_ids.iter().collect::<BTreeSet<_>>().len() != f.evidence_ids.len()
            || f.evidence_ids
                .iter()
                .any(|id| !selected.contains(&id.as_str()))
        {
            return Err(Error);
        }
        Ok(Self {
            fields: f,
            original: raw.to_vec(),
        })
    }
    pub fn action(&self) -> Action {
        self.fields.action
    }
    pub fn code(&self) -> Option<&str> {
        self.fields.code.as_deref()
    }
    pub fn evidence_ids(&self) -> &[String] {
        &self.fields.evidence_ids
    }
    pub fn original_json(&self) -> &[u8] {
        &self.original
    }
    pub fn execution_authorized(&self) -> bool {
        false
    }
    pub fn promotion_authorized(&self) -> bool {
        false
    }
}
