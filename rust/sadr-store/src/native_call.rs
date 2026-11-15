//! Offline preparation composition, not an HTTP client or session/invocation mandate.
use crate::{
    journal::{self, ModelAttempt, ModelJournal},
    native_lease::{self, Lease},
};
use rusqlite::Connection;
use sadr_core::{
    native_grant::GrantRecord,
    native_request::Envelope,
    native_spec::{self, NativeSpec},
};
use serde_json::Value;
use std::fmt;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Spec(native_spec::Error),
    Lease(native_lease::Error),
    Journal(journal::JournalError),
    InvalidTime,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spec(e) => e.fmt(f),
            Self::Lease(e) => e.fmt(f),
            Self::Journal(e) => e.fmt(f),
            Self::InvalidTime => f.write_str("native_call_time_invalid"),
        }
    }
}
impl std::error::Error for Error {}
/// Inputs are trusted caller snapshots/declarations, not proof of actual eligibility,
/// financial freshness, original-file identity, current grant file or session authority.
pub struct Input<'a> {
    pub eligible_count: usize,
    pub spec: &'a NativeSpec,
    pub original: &'a ModelJournal,
    pub grant: &'a GrantRecord,
    pub grant_id: &'a str,
    pub lease: &'a Lease,
    pub now_rfc3339: &'a str,
    pub catalog: &'a [u8],
    pub prompt: &'a str,
    pub schema: &'a Value,
}
pub struct Prepared {
    envelope: Envelope,
    attempt: ModelAttempt,
    spec: NativeSpec,
    grant_id: String,
    namespace: std::path::PathBuf,
    grant_file: Option<crate::native_grant::Fingerprint>,
}
impl fmt::Debug for Prepared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PreparedNativeRequest(redacted, no invocation authority)")
    }
}
impl Prepared {
    pub(crate) fn bind_grant_file(&mut self, fingerprint: crate::native_grant::Fingerprint) {
        self.grant_file = Some(fingerprint);
    }
    pub(crate) fn grant_file(&self) -> Option<crate::native_grant::Fingerprint> {
        self.grant_file
    }
    pub(crate) fn reserved_attempt(&self) -> &ModelAttempt {
        &self.attempt
    }
    pub fn invocation_authorized(&self) -> bool {
        false
    }
    /// Consume preparation once. An HTTP adapter still needs all original binding,
    /// grant, lease, freshness and deadline gates and must never retry ambiguous sends.
    pub fn into_parts(self) -> (Envelope, ModelAttempt) {
        (self.envelope, self.attempt)
    }
}
/// No eligible candidates: return before lease/catalog/time checks or any DB effect.
/// Otherwise reject all supplied policy/request evidence before reserving one row.
pub fn prepare(db: &mut Connection, input: Input<'_>) -> Result<Option<Prepared>, Error> {
    if input.eligible_count == 0 {
        return Ok(None);
    }
    input.lease.verify().map_err(Error::Lease)?;
    let now = input.now_rfc3339.parse().map_err(|_| Error::InvalidTime)?;
    input
        .spec
        .check_grant(input.grant, input.grant_id, now)
        .map_err(Error::Spec)?;
    input
        .spec
        .check_catalog(input.catalog)
        .map_err(Error::Spec)?;
    let billing_day = now.naive_utc().date().to_string();
    let envelope = input
        .spec
        .request(input.prompt, input.schema)
        .map_err(Error::Spec)?;
    input.lease.verify().map_err(Error::Lease)?;
    let attempt = journal::reserve_native_attempt(
        db,
        input.original,
        input.spec.account_hash(),
        input.spec.exact_model(),
        &billing_day,
    )
    .map_err(Error::Journal)?;
    // A failure after commit preserves the reserved row for reconciliation.
    input.lease.verify().map_err(Error::Lease)?;
    Ok(Some(Prepared {
        envelope,
        attempt,
        spec: input.spec.clone(),
        grant_id: input.grant_id.into(),
        namespace: input.lease.namespace().into(),
        grant_file: None,
    }))
}

/// File-backed admission inputs; owner obtains its own original/grant snapshots.
pub struct FileInput<'a> {
    pub eligible_count: usize,
    pub spec: &'a NativeSpec,
    pub grant_id: &'a str,
    pub lease: &'a Lease,
    pub now_rfc3339: &'a str,
    pub catalog: &'a [u8],
    pub prompt: &'a str,
    pub schema: &'a Value,
}
impl FileInput<'_> {
    pub(crate) fn bind<'a>(
        &'a self,
        original: &'a ModelJournal,
        grant: &'a GrantRecord,
    ) -> Input<'a> {
        Input {
            eligible_count: self.eligible_count,
            spec: self.spec,
            grant_id: self.grant_id,
            lease: self.lease,
            now_rfc3339: self.now_rfc3339,
            catalog: self.catalog,
            prompt: self.prompt,
            schema: self.schema,
            original,
            grant,
        }
    }
}

/// Owns immutable prepared policy and one bounded monotonic deadline. No HTTP client.
pub struct InFlight {
    pub(crate) prepared: Prepared,
    deadline: sadr_core::native_stream::Deadline,
}
impl std::fmt::Debug for InFlight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeInFlight(redacted, no authority)")
    }
}
impl Prepared {
    pub fn begin_inflight(
        self,
        start_ms: u64,
    ) -> Result<InFlight, sadr_core::native_stream::Error> {
        let deadline = sadr_core::native_stream::Deadline::new(
            start_ms,
            u64::from(self.spec.timeout_seconds()) * 1000,
        )?;
        Ok(InFlight {
            prepared: self,
            deadline,
        })
    }
    pub(crate) fn spec(&self) -> &NativeSpec {
        &self.spec
    }
    pub(crate) fn grant_id(&self) -> &str {
        &self.grant_id
    }
    pub(crate) fn namespace(&self) -> &std::path::Path {
        &self.namespace
    }
}
impl InFlight {
    pub(crate) fn check_deadline(
        &mut self,
        now_ms: u64,
    ) -> Result<(), sadr_core::native_stream::Error> {
        self.deadline.check(now_ms)
    }
    pub fn into_parts(self) -> (Envelope, ModelAttempt) {
        self.prepared.into_parts()
    }
    pub fn invocation_authorized(&self) -> bool {
        false
    }
}
