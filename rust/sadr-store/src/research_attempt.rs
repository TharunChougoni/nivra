//! Durable one-shot publication only. Caller preflight, router/original-journal guards,
//! model completion, sandbox admission and execution authority remain separate gates.
//! Initial mkdir/open requires a cooperative exclusive writer in the private parent;
//! subsequent guards pin the original namespace. This is not hostile same-user isolation.
use sadr_core::{
    native,
    research_evidence::{EvidenceRequest, EvidenceTools},
    research_prompt::HistoricalPlan,
    research_proposal::{Action, Proposal},
    research_script::ScriptRequest,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Component, Path, PathBuf},
};
const MAX_ARTIFACT: usize = 1_000_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("research_attempt_unavailable_preserve_no_retry")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Failed,
    Cancelled,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Claimed,
    Selected,
    LookupAbstained,
    PromptRecorded,
    Terminal,
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn owner() -> Result<u32, Error> {
    Ok(fs::metadata("/proc/self").map_err(|_| Error)?.uid())
}
fn id(m: &fs::Metadata) -> (u64, u64) {
    (m.dev(), m.ino())
}
fn private_dir(m: &fs::Metadata) -> Result<bool, Error> {
    Ok(m.is_dir() && m.uid() == owner()? && m.mode() & 0o7777 == 0o700)
}
fn directory(path: &Path) -> Result<fs::File, Error> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Error);
    }
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut file =
        fs::File::from(rustix::fs::open("/", flags, rustix::fs::Mode::empty()).map_err(|_| Error)?);
    for c in path.components() {
        if let Component::Normal(name) = c {
            file = fs::File::from(
                rustix::fs::openat(&file, name, flags, rustix::fs::Mode::empty())
                    .map_err(|_| Error)?,
            );
        }
    }
    if !private_dir(&file.metadata().map_err(|_| Error)?)? {
        return Err(Error);
    }
    Ok(file)
}
struct Published {
    file: fs::File,
    identity: (u64, u64),
    bytes: Vec<u8>,
}
struct Artifacts {
    path: PathBuf,
    parent: fs::File,
    parent_id: (u64, u64),
    directory: fs::File,
    directory_id: (u64, u64),
    published: BTreeMap<&'static str, Published>,
    failed: bool,
}
impl Artifacts {
    fn create(path: &Path) -> Result<Self, Error> {
        let parent = directory(path.parent().ok_or(Error)?)?;
        let parent_id = id(&parent.metadata().map_err(|_| Error)?);
        let name = path.file_name().ok_or(Error)?;
        rustix::fs::mkdirat(
            &parent,
            name,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR | rustix::fs::Mode::XUSR,
        )
        .map_err(|_| Error)?;
        let directory = fs::File::from(
            rustix::fs::openat(
                &parent,
                name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|_| Error)?,
        );
        parent.sync_all().map_err(|_| Error)?;
        let directory_id = id(&directory.metadata().map_err(|_| Error)?);
        let a = Self {
            path: path.into(),
            parent,
            parent_id,
            directory,
            directory_id,
            published: BTreeMap::new(),
            failed: false,
        };
        a.verify()?;
        Ok(a)
    }
    fn verify(&self) -> Result<(), Error> {
        if self.failed {
            return Err(Error);
        }
        let named_parent = directory(self.path.parent().ok_or(Error)?)?;
        let named = directory(&self.path)?;
        for m in [
            named_parent.metadata().map_err(|_| Error)?,
            self.parent.metadata().map_err(|_| Error)?,
        ] {
            if !private_dir(&m)? || id(&m) != self.parent_id {
                return Err(Error);
            }
        }
        for m in [
            named.metadata().map_err(|_| Error)?,
            self.directory.metadata().map_err(|_| Error)?,
        ] {
            if !private_dir(&m)? || id(&m) != self.directory_id {
                return Err(Error);
            }
        }
        let actual_names = fs::read_dir(format!("/proc/self/fd/{}", self.directory.as_raw_fd()))
            .map_err(|_| Error)?
            .map(|entry| {
                entry
                    .map_err(|_| Error)?
                    .file_name()
                    .into_string()
                    .map_err(|_| Error)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let expected_names = self
            .published
            .keys()
            .map(|name| (*name).to_owned())
            .collect::<BTreeSet<_>>();
        if actual_names != expected_names {
            return Err(Error);
        }
        for (name, published) in &self.published {
            let file = self.open_read(name)?;
            for m in [
                file.metadata().map_err(|_| Error)?,
                published.file.metadata().map_err(|_| Error)?,
            ] {
                if !m.is_file()
                    || m.uid() != owner()?
                    || m.mode() & 0o7777 != 0o600
                    || m.nlink() != 1
                    || id(&m) != published.identity
                    || m.len() != published.bytes.len() as u64
                {
                    return Err(Error);
                }
            }
            let mut bytes = Vec::new();
            (&file)
                .take(MAX_ARTIFACT as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| Error)?;
            if bytes != published.bytes {
                return Err(Error);
            }
        }
        Ok(())
    }
    fn open_read(&self, name: &str) -> Result<fs::File, Error> {
        Ok(fs::File::from(
            rustix::fs::openat(
                &self.directory,
                name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|_| Error)?,
        ))
    }
    fn write(&mut self, name: &'static str, payload: &[u8]) -> Result<(), Error> {
        let result = (|| {
            self.verify()?;
            if payload.len() > MAX_ARTIFACT {
                return Err(Error);
            }
            let flags = rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC;
            let mut file = fs::File::from(
                rustix::fs::openat(
                    &self.directory,
                    name,
                    flags,
                    rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
                )
                .map_err(|_| Error)?,
            );
            file.write_all(payload).map_err(|_| Error)?;
            file.sync_all().map_err(|_| Error)?;
            self.directory.sync_all().map_err(|_| Error)?;
            let identity = id(&file.metadata().map_err(|_| Error)?);
            self.published.insert(
                name,
                Published {
                    file,
                    identity,
                    bytes: payload.into(),
                },
            );
            self.verify()
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
pub struct Attempt {
    artifacts: Artifacts,
    plan: Value,
    plan_hash: String,
    configuration_hash: Option<String>,
    tools: Option<EvidenceTools>,
    selected: Vec<String>,
    stage: Stage,
}
impl fmt::Debug for Attempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResearchAttempt(redacted, one-shot, no authority)")
    }
}
impl Attempt {
    /// Host must complete conservative preflight and bind this prompt hash before
    /// claiming. A declaration/hash is neither model admission nor provenance proof.
    pub fn claim(
        destination: &Path,
        plan: &HistoricalPlan,
        confirmed_hash: &str,
        prompt_hash: &str,
        configuration_hash: Option<&str>,
    ) -> Result<Self, Error> {
        if confirmed_hash != plan.hash()
            || !hash(prompt_hash)
            || configuration_hash.is_some_and(|s| !hash(s))
        {
            return Err(Error);
        }
        let fields = native::decode(plan.canonical_json()).map_err(|_| Error)?;
        let tools = fields
            .get("tools")
            .map(|v| {
                EvidenceTools::parse(&serde_json::to_vec(v).map_err(|_| Error)?).map_err(|_| Error)
            })
            .transpose()?;
        let claim = json!({"state":"CLAIMED","plan_hash":plan.hash(),"configuration_hash":configuration_hash,"capture_id":fields["capture_id"],"runtime_hash":fields["runtime_hash"],"frontier":fields["frontier"],"prompt_hash":prompt_hash,"activation":"NOT_AUTHORIZED"});
        let artifacts = Artifacts::create(destination)?;
        let mut a = Self {
            artifacts,
            plan: fields,
            plan_hash: plan.hash().into(),
            configuration_hash: configuration_hash.map(str::to_owned),
            tools,
            selected: Vec::new(),
            stage: Stage::Claimed,
        };
        a.artifacts.write("plan.json", plan.canonical_json())?;
        a.artifacts
            .write("attempt.json", python_json(&claim)?.as_bytes())?;
        Ok(a)
    }
    pub fn verify(&mut self) -> Result<(), Error> {
        let result = self.artifacts.verify();
        if result.is_err() {
            self.artifacts.failed = true;
        }
        result
    }
    /// Archived lookup publication; reads only the exact catalog retained in the plan.
    pub fn record_selection(&mut self, request: &EvidenceRequest) -> Result<(), Error> {
        let result = self.record_selection_inner(request);
        if result.is_err() {
            let _ = self.mark_failure(Failure::Failed);
        }
        result
    }
    fn record_selection_inner(&mut self, request: &EvidenceRequest) -> Result<(), Error> {
        if self.stage != Stage::Claimed {
            return Err(Error);
        }
        let tools = self.tools.as_ref().ok_or(Error)?;
        let ids: Vec<_> = request.source_ids().iter().map(String::as_str).collect();
        let context = tools.read(&ids).map_err(|_| Error)?;
        #[derive(Serialize)]
        struct Lookup<'a> {
            action: &'static str,
            source_ids: &'a [String],
        }
        let action = if ids.is_empty() { "abstain" } else { "read" };
        let raw = serde_json::to_vec(&Lookup {
            action,
            source_ids: request.source_ids(),
        })
        .map_err(|_| Error)?;
        self.artifacts.write("tool-request.json", &raw)?;
        self.artifacts
            .write("tool-context.json", python_json(&context)?.as_bytes())?;
        self.selected = request.source_ids().to_vec();
        self.stage = if ids.is_empty() {
            Stage::LookupAbstained
        } else {
            Stage::Selected
        };
        Ok(())
    }
    pub fn record_proposal_prompt(&mut self, prompt_hash: &str) -> Result<(), Error> {
        let result = self.record_proposal_prompt_inner(prompt_hash);
        if result.is_err() {
            let _ = self.mark_failure(Failure::Failed);
        }
        result
    }
    fn record_proposal_prompt_inner(&mut self, prompt_hash: &str) -> Result<(), Error> {
        if self.stage != Stage::Selected || !hash(prompt_hash) {
            return Err(Error);
        }
        self.artifacts.write(
            "proposal-prompt.json",
            python_json(&json!({"prompt_hash":prompt_hash}))?.as_bytes(),
        )?;
        self.stage = Stage::PromptRecorded;
        Ok(())
    }
    pub fn finish(
        mut self,
        proposal: Proposal,
        request: Option<ScriptRequest>,
    ) -> Result<Value, Error> {
        let result = self.finish_inner(proposal, request);
        if result.is_err() {
            let _ = self.mark_failure(Failure::Failed);
        }
        result
    }
    fn finish_inner(
        &mut self,
        proposal: Proposal,
        request: Option<ScriptRequest>,
    ) -> Result<Value, Error> {
        if self.stage == Stage::Terminal
            || (self.tools.is_some()
                && !matches!(self.stage, Stage::LookupAbstained | Stage::PromptRecorded))
        {
            return Err(Error);
        }
        let selected: Vec<_> = self.selected.iter().map(String::as_str).collect();
        let proposal = Proposal::parse(proposal.original_json(), &selected).map_err(|_| Error)?;
        if (proposal.action() == Action::Propose) != request.is_some()
            || (self.stage == Stage::LookupAbstained && proposal.action() != Action::Abstain)
        {
            return Err(Error);
        }
        if let Some(request) = &request {
            let raw = native::decode(request.canonical_json()).map_err(|_| Error)?;
            if raw["code"].as_str() != proposal.code()
                || raw["capture_id"] != self.plan["capture_id"]
                || raw["runtime_hash"] != self.plan["runtime_hash"]
                || raw["trial_ref"] != self.plan["trial_ref"]
                || raw["limits"] != self.plan["limits"]
            {
                return Err(Error);
            }
        }
        let fields = native::decode(proposal.original_json()).map_err(|_| Error)?;
        #[derive(Serialize)]
        struct CanonicalProposal<'a> {
            action: &'static str,
            code: Option<&'a str>,
            rationale: &'a str,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            evidence_ids: Vec<&'a str>,
        }
        let raw = serde_json::to_vec(&CanonicalProposal {
            action: if proposal.action() == Action::Propose {
                "propose"
            } else {
                "abstain"
            },
            code: proposal.code(),
            rationale: fields["rationale"].as_str().ok_or(Error)?,
            evidence_ids: proposal.evidence_ids().iter().map(String::as_str).collect(),
        })
        .map_err(|_| Error)?;
        self.artifacts.write("proposal.json", &raw)?;
        if let Some(request) = &request {
            self.artifacts
                .write("request.json", request.canonical_json())?;
        }
        let result = json!({"state":if request.is_some(){"PROPOSED"}else{"ABSTAIN"},"plan_hash":self.plan_hash,"configuration_hash":self.configuration_hash,"request_hash":request.as_ref().map(ScriptRequest::hash),"proposal_hash":digest(&raw),"runtime_validation":"DEFERRED_TO_SEPARATE_RUNNER","registration":"CALLER_REFERENCE_ONLY","execution":"NOT_AUTHORIZED","activation":"NOT_AUTHORIZED","promotion":"NOT_READY","holdout":"NOT_OPENED"});
        self.artifacts
            .write("result.json", python_json(&result)?.as_bytes())?;
        self.stage = Stage::Terminal;
        Ok(result)
    }
    pub fn fail(mut self, kind: Failure) -> Result<(), Error> {
        self.mark_failure(kind)
    }
    fn mark_failure(&mut self, kind: Failure) -> Result<(), Error> {
        if self.stage == Stage::Terminal {
            return Err(Error);
        }
        self.stage = Stage::Terminal;
        #[derive(Serialize)]
        struct Marker {
            state: &'static str,
            activation: &'static str,
            retry: &'static str,
        }
        let raw = serde_json::to_vec(&Marker {
            state: match kind {
                Failure::Failed => "FAILED",
                Failure::Cancelled => "CANCELLED",
            },
            activation: "NOT_AUTHORIZED",
            retry: "NOT_AUTHORIZED",
        })
        .map_err(|_| Error)?;
        // Historical failure marker uses Python's unsorted default separators.
        let text = String::from_utf8(raw)
            .map_err(|_| Error)?
            .replace("\":\"", "\": \"")
            .replace("\",\"", "\", \"");
        self.artifacts.write("failure.json", text.as_bytes())
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if self.stage != Stage::Terminal {
            let _ = self.mark_failure(Failure::Failed);
        }
    }
}
fn ascii_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 32 || (c as u32) >= 127 => {
                let mut units = [0; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn python_json(v: &Value) -> Result<String, Error> {
    match v {
        Value::Object(m) => {
            let sorted: BTreeMap<_, _> = m.iter().collect();
            Ok(format!(
                "{{{}}}",
                sorted
                    .iter()
                    .map(|(k, v)| Ok(format!("{}: {}", ascii_string(k), python_json(v)?)))
                    .collect::<Result<Vec<_>, Error>>()?
                    .join(", ")
            ))
        }
        Value::Array(a) => Ok(format!(
            "[{}]",
            a.iter()
                .map(python_json)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        )),
        Value::String(s) => Ok(ascii_string(s)),
        _ => serde_json::to_string(v).map_err(|_| Error),
    }
}
