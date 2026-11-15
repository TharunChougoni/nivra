//! Exact bounded script declarations. No interpreter, sandbox launch or execution authority.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("research_script_invalid")
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LimitFields {
    wall_seconds: u32,
    cpu_seconds: u32,
    memory_bytes: u64,
    workspace_bytes: u64,
    file_bytes: u64,
    output_bytes: u64,
}
impl LimitFields {
    fn valid(&self) -> bool {
        (1..=15).contains(&self.wall_seconds)
            && (1..=5).contains(&self.cpu_seconds)
            && (67108864..=268435456).contains(&self.memory_bytes)
            && (1048576..=16777216).contains(&self.workspace_bytes)
            && (1024..=1048576).contains(&self.file_bytes)
            && (256..=65536).contains(&self.output_bytes)
            && self.cpu_seconds <= self.wall_seconds
            && self.file_bytes <= self.workspace_bytes
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fields {
    code: String,
    capture_id: String,
    runtime_hash: String,
    trial_ref: String,
    limits: LimitFields,
}
pub struct ScriptRequest {
    fields: Fields,
    original: Vec<u8>,
    canonical: Vec<u8>,
    hash: String,
}
impl fmt::Debug for ScriptRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResearchScript(untrusted text, no authority)")
    }
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl ScriptRequest {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > 256 * 1024 {
            return Err(Error);
        }
        let value = crate::native::decode(raw).map_err(|_| Error)?;
        let f: Fields = serde_json::from_value(value).map_err(|_| Error)?;
        if !(1..=32000).contains(&f.code.chars().count())
            || !hash(&f.capture_id)
            || !hash(&f.runtime_hash)
            || !(1..=256).contains(&f.trial_ref.chars().count())
            || !f.limits.valid()
        {
            return Err(Error);
        }
        // Schema order and integer-only limits preserve the historical Pydantic JSON identity.
        let canonical = serde_json::to_vec(&f).map_err(|_| Error)?;
        let hash = Sha256::digest(&canonical)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Self {
            fields: f,
            original: raw.to_vec(),
            canonical,
            hash,
        })
    }
    pub fn original_json(&self) -> &[u8] {
        &self.original
    }
    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical
    }
    pub fn hash(&self) -> &str {
        &self.hash
    }
    pub(crate) fn code(&self) -> &str {
        &self.fields.code
    }
    pub fn capture_id(&self) -> &str {
        &self.fields.capture_id
    }
    pub fn runtime_hash(&self) -> &str {
        &self.fields.runtime_hash
    }
    pub fn trial_ref(&self) -> &str {
        &self.fields.trial_ref
    }
    pub fn execution_authorized(&self) -> bool {
        false
    }
    pub fn promotion_authorized(&self) -> bool {
        false
    }
}
