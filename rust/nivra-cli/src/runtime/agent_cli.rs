//! Durable local operator requests for the personal agent CLI.
//!
//! `ask` queues only the existing read-only aside request. `instruct` writes a
//! typed, expiring owner directive into the existing directives store. Neither
//! path writes a trading book, journal, order or financial state, and neither
//! wakes the controller. `result` polls a durable request id; `cancel` applies
//! only to that request, never to the trading instance.
use super::{Config, digest, directives, rollover, session};
use nivra_cli::lock::AdvisoryLock;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const VERSION: u32 = 1;
const STORE: &str = "model/agent-requests.json";
const LOCK_PATH: &str = "model/agent-requests.lock";
const MAX_STORE_BYTES: u64 = 1_048_576;
const MAX_REQUESTS: usize = 64;
const MAX_QUESTION_CHARS: usize = 4_000;
const MAX_INSTRUCTION_CHARS: usize = 240;
const MIN_TTL_MS: i64 = 1_000;
const MAX_TTL_MS: i64 = 3_600_000;
const DEFAULT_TTL_MS: i64 = 900_000;
const LOCK_WAIT_MS: u64 = 1_000;

type AgentResult<T> = std::result::Result<T, &'static str>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    /// Monotonic per-session instruction revision. Kept outside the bounded
    /// request history so pruning a receipt can never rewind a revision.
    #[serde(default)]
    instruction_revisions: std::collections::BTreeMap<String, u64>,
    requests: Vec<Request>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    /// `ask` or `instruct`.
    kind: String,
    session: String,
    idempotency_key: String,
    /// Hash of the exact request content. The plaintext remains in the aside
    /// request or directives store, not in this idempotency index.
    request_sha256: String,
    /// queued | running | answered | applied | refused | expired | failed | cancelled
    state: String,
    created_at_ms: i64,
    updated_at_ms: i64,
    expires_at_ms: i64,
    #[serde(default)]
    revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authorization_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    directive_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    answer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

fn fresh() -> Store {
    Store {
        version: VERSION,
        instruction_revisions: std::collections::BTreeMap::new(),
        requests: Vec::new(),
    }
}

fn store_path(c: &Config) -> PathBuf {
    c.directory.join(STORE)
}

/// Return references to real nonterminal operator requests without carrying
/// their question, instruction text, credentials, or answer payloads.
pub(crate) fn unresolved_request_refs(root: &Path, session: &str, now: i64) -> Value {
    let path = root.join(STORE);
    let Ok(bytes) = crate::read_bounded(&path, MAX_STORE_BYTES) else {
        return json!({"available":false,"reason":"request_store_unavailable","refs":[]});
    };
    let Ok(store) = serde_json::from_slice::<Store>(&bytes) else {
        return json!({"available":false,"reason":"request_store_invalid","refs":[]});
    };
    if store.version != VERSION {
        return json!({"available":false,"reason":"request_store_version_unsupported","refs":[]});
    }
    let refs = store.requests.iter().filter(|request| {
        request.session == session && request.expires_at_ms > now && matches!(request.state.as_str(), "queued" | "running")
    }).map(|request| json!({"request_id":request.id,"kind":request.kind,"state":request.state,"expires_at_ms":request.expires_at_ms})).collect::<Vec<_>>();
    json!({"available":true,"refs":refs})
}

fn lock_path(c: &Config) -> PathBuf {
    c.directory.join(LOCK_PATH)
}

fn session_dir(c: &Config, session: &str) -> PathBuf {
    c.directory.join(session::ROOT_PREFIX).join(session)
}

fn aside_request_path(c: &Config, session: &str) -> PathBuf {
    session_dir(c, session).join("aside-request.json")
}

fn aside_reply_path(c: &Config, session: &str) -> PathBuf {
    session_dir(c, session).join("aside-reply.json")
}

fn aside_start_path(c: &Config, session: &str) -> PathBuf {
    session_dir(c, session).join("aside-start.json")
}

fn lock(c: &Config) -> AgentResult<AdvisoryLock> {
    use std::time::{Duration, Instant};
    let path = lock_path(c);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| "agent_store_unwritable")?;
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|_| "agent_store_unwritable")?;
    let meta = file.metadata().map_err(|_| "agent_lock_invalid")?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.mode() & 0o777 != 0o600
        || meta.uid()
            != fs::metadata("/proc/self")
                .map_err(|_| "agent_lock_invalid")?
                .uid()
    {
        return Err("agent_lock_invalid");
    }
    let started = Instant::now();
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(AdvisoryLock::new(file)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if started.elapsed() >= Duration::from_millis(LOCK_WAIT_MS) {
                    return Err("agent_store_busy");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Err("agent_lock_invalid"),
        }
    }
}

fn load(c: &Config) -> AgentResult<Store> {
    let path = store_path(c);
    if !path.exists() {
        return Ok(fresh());
    }
    let bytes =
        crate::read_bounded(&path, MAX_STORE_BYTES).map_err(|_| "agent_store_unreadable")?;
    let store: Store = serde_json::from_slice(&bytes).map_err(|_| "agent_store_invalid")?;
    if store.version != VERSION || store.requests.len() > MAX_REQUESTS {
        return Err("agent_store_invalid");
    }
    Ok(store)
}

fn save(c: &Config, store: &Store) -> AgentResult<()> {
    let value = serde_json::to_value(store).map_err(|_| "agent_store_unwritable")?;
    rollover::atomic(&store_path(c), &value).map_err(|_| "agent_store_unwritable")
}

fn read_value(path: &Path) -> Option<Value> {
    crate::read_bounded(path, 65_536)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn safe_reason(reason: &str) -> String {
    if !reason.is_empty()
        && reason.len() <= 96
        && reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        reason.to_owned()
    } else {
        "agent_request_failed".into()
    }
}

fn request_hash(
    kind: &str,
    session: &str,
    text: &str,
    scope: Option<&str>,
    revision: Option<u64>,
    expect_session: Option<&str>,
) -> String {
    digest(
        format!(
            "{kind}|{session}|{text}|{}|{}|{}",
            scope.unwrap_or_default(),
            revision.map(|value| value.to_string()).unwrap_or_default(),
            expect_session.unwrap_or_default()
        )
        .as_bytes(),
    )
}

fn generated_key(kind: &str, session: &str, hash: &str, now: i64) -> String {
    format!(
        "{kind}-{}-{}",
        session,
        &digest(format!("{kind}|{session}|{hash}|{now}").as_bytes())[..32]
    )
}

fn generated_id(prefix: &str, session: &str, hash: &str, now: i64) -> String {
    format!(
        "{prefix}-{}-{}",
        session,
        &digest(format!("{prefix}|{session}|{hash}|{now}").as_bytes())[..16]
    )
}

fn valid_identifier(value: &str, limit: usize) -> bool {
    !value.is_empty()
        && value.len() <= limit
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
}

fn ttl(raw: Option<i64>) -> AgentResult<i64> {
    let ttl = raw.unwrap_or(DEFAULT_TTL_MS);
    if !(MIN_TTL_MS..=MAX_TTL_MS).contains(&ttl) {
        return Err("agent_ttl_invalid");
    }
    Ok(ttl)
}

fn active_store<'a>(store: &'a Store, session: &str, kind: &str) -> Option<&'a Request> {
    store.requests.iter().find(|request| {
        request.session == session
            && request.kind == kind
            && matches!(request.state.as_str(), "queued" | "running")
    })
}

fn instruction_revision(store: &Store, session: &str) -> u64 {
    let recorded = store
        .instruction_revisions
        .get(session)
        .copied()
        .unwrap_or(0);
    let observed = store
        .requests
        .iter()
        .filter(|request| {
            request.kind == "instruct" && request.session == session && request.revision > 0
        })
        .map(|request| request.revision)
        .max()
        .unwrap_or(0);
    recorded.max(observed)
}

fn prune(store: &mut Store) {
    while store.requests.len() > MAX_REQUESTS {
        let terminal = store.requests.iter().position(|request| {
            matches!(
                request.state.as_str(),
                "answered" | "applied" | "refused" | "expired" | "failed" | "cancelled"
            )
        });
        match terminal {
            Some(index) => {
                store.requests.remove(index);
            }
            None => break,
        }
    }
}

fn terminal(state: &str) -> bool {
    matches!(
        state,
        "answered" | "applied" | "refused" | "expired" | "failed" | "cancelled"
    )
}

/// Pure view of the durable request plus its external evidence. It performs no
/// writes and no model call.
fn evaluate(c: &Config, request: &Request, now: i64) -> (String, Option<String>, Option<String>) {
    match request.kind.as_str() {
        "ask" => {
            if terminal(&request.state) {
                return (
                    request.state.clone(),
                    request.answer.clone(),
                    request.reason.clone(),
                );
            }
            if let Some(reply) = read_value(&aside_reply_path(c, &request.session))
                && reply["id"] == json!(request.id)
            {
                if let Some(text) = reply["reply"].as_str() {
                    return ("answered".into(), Some(session::bound_reply(text)), None);
                }
                let reason = reply["error"]
                    .as_str()
                    .map(safe_reason)
                    .unwrap_or_else(|| "agent_reply_invalid".into());
                return ("failed".into(), None, Some(reason));
            }
            if now >= request.expires_at_ms {
                return ("expired".into(), None, Some("agent_request_expired".into()));
            }
            if let Some(start) = read_value(&aside_start_path(c, &request.session))
                && start["id"] == json!(request.id)
            {
                return ("running".into(), None, None);
            }
            if let Some(pending) = read_value(&aside_request_path(c, &request.session))
                && pending["id"] == json!(request.id)
            {
                return ("queued".into(), None, None);
            }
            ("failed".into(), None, Some("agent_request_missing".into()))
        }
        "instruct" => {
            if request.state == "cancelled" || request.state == "refused" {
                return (request.state.clone(), None, request.reason.clone());
            }
            if now >= request.expires_at_ms {
                return ("expired".into(), None, Some("agent_request_expired".into()));
            }
            (request.state.clone(), None, request.reason.clone())
        }
        _ => (
            "failed".into(),
            None,
            Some("agent_request_kind_invalid".into()),
        ),
    }
}

fn apply_evaluation(
    request: &mut Request,
    state: String,
    answer: Option<String>,
    reason: Option<String>,
    now: i64,
) {
    if request.state != state {
        request.state = state;
        request.updated_at_ms = now;
    }
    if answer.is_some() {
        request.answer = answer;
    }
    if reason.is_some() {
        request.reason = reason;
    }
}

fn request_json(request: &Request) -> Value {
    let mut value = json!({
        "status": request.state,
        "id": request.id,
        "kind": request.kind,
        "session": request.session,
        "idempotency_key": request.idempotency_key,
        "created_at_ms": request.created_at_ms,
        "updated_at_ms": request.updated_at_ms,
        "expires_at_ms": request.expires_at_ms,
        "revision": request.revision,
        "paper_only": true,
        "trading_effect": "none",
    });
    if let Some(expected) = &request.expected_session {
        value["expected_session"] = json!(expected);
    }
    if let Some(expected) = request.expected_revision {
        value["expected_revision"] = json!(expected);
    }
    if let Some(scope) = &request.authorization_scope {
        value["authorization_scope"] = json!(scope);
    }
    if let Some(directive) = &request.directive_id {
        value["directive_id"] = json!(directive);
        value["target"] = json!(directives::OWNER_TARGET);
        value["application"] = json!("owner_context_only");
    }
    if let Some(answer) = &request.answer {
        value["answer"] = json!(answer);
        value["application"] = json!("read_only_answer");
    }
    if let Some(reason) = &request.reason {
        value["reason"] = json!(reason);
    }
    value
}

/// Queue one read-only question through the controller's existing aside slot.
fn ask(
    c: &Config,
    question: &str,
    request_id: Option<&str>,
    idempotency_key: Option<&str>,
    ttl_ms: Option<i64>,
    now: i64,
) -> AgentResult<Value> {
    let question = question.trim();
    if question.is_empty() || question.chars().count() > MAX_QUESTION_CHARS {
        return Err("agent_question_invalid");
    }
    let ttl = ttl(ttl_ms)?;
    let session = c.session.to_string();
    let hash = request_hash("ask", &session, question, None, None, None);
    let _lock = lock(c)?;
    let mut store = load(c)?;
    let key = idempotency_key
        .map(str::to_owned)
        .unwrap_or_else(|| generated_key("ask", &session, &hash, now));
    if !valid_identifier(&key, 160) {
        return Err("agent_idempotency_key_invalid");
    }
    let id = request_id
        .map(str::to_owned)
        .unwrap_or_else(|| generated_id("ask", &session, &hash, now));
    if !valid_identifier(&id, 80) {
        return Err("agent_request_id_invalid");
    }
    if let Some(prior) = store
        .requests
        .iter()
        .find(|request| request.idempotency_key == key || request.id == id)
    {
        if prior.kind == "ask" && prior.request_sha256 == hash && prior.session == session {
            let mut value = request_json(prior);
            value["duplicate"] = json!(true);
            return Ok(value);
        }
        return Err("agent_idempotency_conflict");
    }
    if let Some(prior) = active_store(&store, &session, "ask").cloned() {
        let (state, answer, reason) = evaluate(c, &prior, now);
        if !terminal(&state) {
            return Err("agent_ask_busy");
        }
        // The only outstanding request is already terminal (usually expired).
        // Record that receipt and free the single aside slot.
        if let Some(saved) = store.requests.iter_mut().find(|saved| saved.id == prior.id) {
            apply_evaluation(saved, state, answer, reason, now);
        }
        remove_matching_request(c, &session, &prior.id);
        save(c, &store)?;
    }
    if let Some(pending) = read_value(&aside_request_path(c, &session)) {
        // A dashboard or older client may own the one read-only aside slot.
        let _ = pending;
        return Err("agent_ask_busy");
    }
    // The slot is free. Clear only stale progress/reply evidence that carries the
    // same id, never another request's answer.
    let _ = fs::remove_file(aside_start_path(c, &session));
    let reply_path = aside_reply_path(c, &session);
    if read_value(&reply_path).is_some_and(|value| value["id"] == json!(id)) {
        let _ = fs::remove_file(reply_path);
    }
    let expires_at_ms = now.saturating_add(ttl);
    let request = Request {
        id: id.clone(),
        kind: "ask".into(),
        session: session.clone(),
        idempotency_key: key,
        request_sha256: hash,
        state: "queued".into(),
        created_at_ms: now,
        updated_at_ms: now,
        expires_at_ms,
        revision: 0,
        expected_session: None,
        expected_revision: None,
        authorization_scope: None,
        directive_id: None,
        answer: None,
        reason: None,
    };
    store.requests.push(request.clone());
    prune(&mut store);
    save(c, &store)?;
    let directory = session_dir(c, &session);
    if fs::create_dir_all(&directory).is_err()
        || rollover::atomic(
            &aside_request_path(c, &session),
            &json!({
                "id": id,
                "session": session,
                "question": question,
                "at_ms": now,
                "request_id": id,
                "idempotency_key": request.idempotency_key,
            }),
        )
        .is_err()
    {
        if let Some(saved) = store
            .requests
            .iter_mut()
            .find(|saved| saved.id == request.id)
        {
            saved.state = "failed".into();
            saved.updated_at_ms = now;
            saved.reason = Some("agent_aside_unavailable".into());
            let _ = save(c, &store);
        }
        return Err("agent_aside_unavailable");
    }
    let mut value = request_json(&request);
    value["duplicate"] = json!(false);
    Ok(value)
}

/// Queue one explicitly requested, expiring operator instruction for the one
/// decision owner. The instruction is context, never an order or execution.
#[allow(clippy::too_many_arguments)]
fn instruct(
    c: &Config,
    text: &str,
    scope: &str,
    expect_session: &str,
    expect_revision: u64,
    request_id: Option<&str>,
    idempotency_key: Option<&str>,
    ttl_ms: Option<i64>,
    now: i64,
) -> AgentResult<Value> {
    let text = text.trim();
    let scope = scope.trim();
    if text.is_empty() || text.chars().count() > MAX_INSTRUCTION_CHARS {
        return Err("agent_instruction_invalid");
    }
    if !["review_only", "decision_context"].contains(&scope) {
        return Err("agent_scope_invalid");
    }
    let ttl = ttl(ttl_ms)?;
    let current_session = c.session.to_string();
    let hash = request_hash(
        "instruct",
        &current_session,
        text,
        Some(scope),
        Some(expect_revision),
        Some(expect_session),
    );
    let _lock = lock(c)?;
    let mut store = load(c)?;
    let key = idempotency_key
        .map(str::to_owned)
        .unwrap_or_else(|| generated_key("instruct", &current_session, &hash, now));
    if !valid_identifier(&key, 160) {
        return Err("agent_idempotency_key_invalid");
    }
    let id = request_id
        .map(str::to_owned)
        .unwrap_or_else(|| generated_id("instruct", &current_session, &hash, now));
    if !valid_identifier(&id, 80) {
        return Err("agent_request_id_invalid");
    }
    if let Some(prior) = store
        .requests
        .iter()
        .find(|request| request.idempotency_key == key || request.id == id)
    {
        if prior.kind == "instruct"
            && prior.request_sha256 == hash
            && prior.session == current_session
        {
            let mut value = request_json(prior);
            value["duplicate"] = json!(true);
            return Ok(value);
        }
        return Err("agent_idempotency_conflict");
    }
    let current_revision = instruction_revision(&store, &current_session);
    let refusal = if expect_session != current_session {
        Some("agent_session_changed")
    } else if expect_revision != current_revision {
        Some("agent_revision_changed")
    } else {
        None
    };
    if let Some(reason) = refusal {
        let request = Request {
            id: id.clone(),
            kind: "instruct".into(),
            session: current_session,
            idempotency_key: key,
            request_sha256: hash,
            state: "refused".into(),
            created_at_ms: now,
            updated_at_ms: now,
            expires_at_ms: now.saturating_add(ttl),
            revision: 0,
            expected_session: Some(expect_session.to_owned()),
            expected_revision: Some(expect_revision),
            authorization_scope: Some(scope.to_owned()),
            directive_id: None,
            answer: None,
            reason: Some(reason.into()),
        };
        store.requests.push(request.clone());
        prune(&mut store);
        save(c, &store)?;
        let mut value = request_json(&request);
        value["duplicate"] = json!(false);
        return Ok(value);
    }
    let revision = current_revision.saturating_add(1);
    let directive_id = format!(
        "dir-{}",
        &digest(format!("{id}|{text}|{scope}").as_bytes())[..16]
    );
    let directive = directives::Directive {
        id: directive_id.clone(),
        text: text.to_owned(),
        keys: Vec::new(),
        target: directives::OWNER_TARGET.into(),
        issued_at_ms: now,
        expires_at_ms: now.saturating_add(ttl),
        supersedes: None,
        request_id: Some(id.clone()),
        session: Some(current_session.clone()),
        authorization_scope: Some(scope.to_owned()),
        expected_revision: Some(expect_revision),
        idempotency_key: Some(key.clone()),
    };
    directives::issue_once(c.directory.as_path(), &directive)
        .map_err(|_| "agent_instruction_unwritten")?;
    store
        .instruction_revisions
        .insert(current_session.clone(), revision);
    let request = Request {
        id: id.clone(),
        kind: "instruct".into(),
        session: current_session,
        idempotency_key: key,
        request_sha256: hash,
        state: "applied".into(),
        created_at_ms: now,
        updated_at_ms: now,
        expires_at_ms: directive.expires_at_ms,
        revision,
        expected_session: Some(expect_session.to_owned()),
        expected_revision: Some(expect_revision),
        authorization_scope: Some(scope.to_owned()),
        directive_id: Some(directive_id),
        answer: None,
        reason: None,
    };
    store.requests.push(request.clone());
    prune(&mut store);
    save(c, &store)?;
    let mut value = request_json(&request);
    value["duplicate"] = json!(false);
    Ok(value)
}

/// Poll one durable request by id. This never writes a new aside request or a
/// new directive, so a reconnect cannot resubmit work.
fn result(c: &Config, request_id: &str, now: i64) -> AgentResult<Value> {
    if !valid_identifier(request_id, 80) {
        return Err("agent_request_id_invalid");
    }
    let _lock = lock(c)?;
    let mut store = load(c)?;
    let current = store
        .requests
        .iter()
        .find(|request| request.id == request_id)
        .cloned()
        .ok_or("agent_request_unknown")?;
    if current.kind == "instruct"
        && current.state == "applied"
        && now >= current.expires_at_ms
        && let Some(directive_id) = current.directive_id.as_deref()
    {
        let _ = directives::expire(c.directory.as_path(), directive_id, now);
    }
    let (state, answer, reason) = evaluate(c, &current, now);
    if current.kind == "ask"
        && matches!(
            state.as_str(),
            "answered" | "expired" | "failed" | "cancelled"
        )
    {
        remove_matching_request(c, &current.session, &current.id);
    }
    let saved = store
        .requests
        .iter_mut()
        .find(|request| request.id == request_id)
        .ok_or("agent_request_unknown")?;
    apply_evaluation(saved, state, answer, reason, now);
    let value = request_json(saved);
    save(c, &store)?;
    let mut value = value;
    value["resubmitted"] = json!(false);
    Ok(value)
}

fn remove_matching_request(c: &Config, session: &str, request_id: &str) {
    let path = aside_request_path(c, session);
    if read_value(&path).is_some_and(|value| value["id"] == json!(request_id)) {
        let _ = fs::remove_file(path);
    }
    let start = aside_start_path(c, session);
    if read_value(&start).is_some_and(|value| value["id"] == json!(request_id)) {
        let _ = fs::remove_file(start);
    }
}

/// Cancel only the named question or instruction. The trading session, book,
/// journal and controller are not touched.
fn cancel(c: &Config, request_id: &str, now: i64) -> AgentResult<Value> {
    if !valid_identifier(request_id, 80) {
        return Err("agent_request_id_invalid");
    }
    let _lock = lock(c)?;
    let mut store = load(c)?;
    let current = store
        .requests
        .iter()
        .find(|request| request.id == request_id)
        .cloned()
        .ok_or("agent_request_unknown")?;
    let (observed, _, _) = evaluate(c, &current, now);
    let saved = store
        .requests
        .iter_mut()
        .find(|request| request.id == request_id)
        .ok_or("agent_request_unknown")?;
    if observed != current.state {
        saved.state = observed.clone();
        saved.updated_at_ms = now;
    }
    let cancellable = match current.kind.as_str() {
        "ask" => matches!(observed.as_str(), "queued" | "running"),
        "instruct" => observed == "applied",
        _ => false,
    };
    if !cancellable {
        if current.kind == "ask" && terminal(&observed) {
            remove_matching_request(c, &current.session, &current.id);
        }
        if observed != current.state {
            save(c, &store)?;
        }
        return Err("agent_request_not_cancellable");
    }
    match current.kind.as_str() {
        "ask" => remove_matching_request(c, &current.session, &current.id),
        "instruct" => {
            if let Some(directive_id) = current.directive_id.as_deref() {
                directives::supersede(c.directory.as_path(), directive_id, now)
                    .map_err(|_| "agent_cancel_unrecorded")?;
            }
        }
        _ => return Err("agent_request_kind_invalid"),
    }
    saved.state = "cancelled".into();
    saved.updated_at_ms = now;
    saved.reason = Some("operator_cancelled".into());
    let value = request_json(saved);
    save(c, &store)?;
    let mut value = value;
    value["cancelled"] = json!(true);
    value["cancellation_scope"] = json!("request_only");
    Ok(value)
}

fn model_selection_summary(c: &Config) -> Value {
    let path = c.directory.join("model/model-selection.json");
    let Some(value) = read_value(&path) else {
        return if path.exists() {
            json!({"status":"unreadable"})
        } else {
            json!({"status":"default"})
        };
    };
    if value["session"] != json!(c.session.to_string()) {
        return json!({"status":"other_session"});
    }
    json!({
        "status":"recorded",
        "revision":value["revision"],
        "active_profile":value["active"]["profile"],
        "pending_profile":value["pending"]["profile"],
        "receipt_count":value["receipts"].as_array().map_or(0, Vec::len),
    })
}

fn book_summary(c: &Config) -> Value {
    let path = c.directory.join("paper.sqlite");
    if !path.is_file() {
        return json!({"status":"absent"});
    }
    let read = || -> std::result::Result<Value, ()> {
        let db = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| ())?;
        let body: String = db
            .query_row("SELECT body FROM state WHERE id=1", [], |row| row.get(0))
            .map_err(|_| ())?;
        let portfolio: nivra_core::execution::Portfolio =
            serde_json::from_str(&body).map_err(|_| ())?;
        Ok(json!({
            "status":"recorded",
            "stopped":portfolio.stopped,
            "halted":portfolio.halted,
            "closed":portfolio.closed,
            "model_led":portfolio.model_led,
            "health":portfolio.health,
            "positions":portfolio.positions.len(),
            "fills":portfolio.fills.len(),
        }))
    };
    read().unwrap_or_else(|()| json!({"status":"unreadable"}))
}

fn controller_summary(c: &Config) -> Value {
    let Some(value) = read_value(&c.directory.join("model/supervisor-state.json")) else {
        return json!({"status":"not_recorded"});
    };
    if value["session"] != json!(c.session.to_string()) {
        return json!({"status":"not_recorded_for_session"});
    }
    json!({
        "status":"recorded",
        "runs":value["runs"],
        "failures":value["failures"],
        "last_run_ms":value["last_run_ms"],
        "parked":value["parked"],
        "compaction_failed":value["compaction_failed"],
        "last_skip":value["skips"].as_array().and_then(|skips| skips.last()).cloned().unwrap_or(Value::Null),
    })
}

fn status(c: &Config, now: i64) -> AgentResult<Value> {
    let store = load(c)?;
    let session = c.session.to_string();
    let mut counts = std::collections::BTreeMap::<String, u64>::new();
    for request in store.requests.iter().filter(|request| {
        request.session == session && matches!(request.kind.as_str(), "ask" | "instruct")
    }) {
        let (state, _, _) = evaluate(c, request, now);
        *counts
            .entry(format!("{}:{}", request.kind, state))
            .or_default() += 1;
    }
    let pending_aside = read_value(&aside_request_path(c, &session)).map(|value| {
        json!({
            "id":value["id"],
            "running":read_value(&aside_start_path(c, &session)).is_some_and(|start| start["id"] == value["id"]),
        })
    });
    let owner_active = directives::active(c.directory.as_path(), now, directives::OWNER_TARGET)
        .into_iter()
        .map(|directive| directive.id)
        .collect::<Vec<_>>();
    Ok(json!({
        "status":"ok",
        "now_ms":now,
        "paper_only":true,
        "live_enabled":false,
        "model_call":false,
        "writes":false,
        "session":{
            "id":session,
            "account":c.account,
            "previous":c.previous_session.to_string(),
            "directory_present":c.directory.is_dir(),
            "journal_present":c.read_journal.is_file(),
            "controller_lock_present":c.directory.join("controller.lock").is_file(),
        },
        "book":book_summary(c),
        "controller":controller_summary(c),
        "model_selection":model_selection_summary(c),
        "agent":{
            "instruction_revision":instruction_revision(&store, &c.session.to_string()),
            "counts":counts,
            "pending_aside":pending_aside.unwrap_or(Value::Null),
        },
        "directives":{"owner_active":owner_active},
    }))
}

fn load_current(path: &Path) -> AgentResult<Config> {
    let bytes = crate::read_bounded(path, 65_536).map_err(|_| "config_unreadable")?;
    let paper: Config = serde_json::from_slice(&bytes).map_err(|_| "config_invalid")?;
    rollover::current(&paper).map_err(|_| "agent_session_unavailable")
}

fn print_value(
    value: &Value,
    json_out: bool,
    plain: impl FnOnce(&Value) -> String,
) -> AgentResult<()> {
    if json_out {
        println!(
            "{}",
            serde_json::to_string(value).map_err(|_| "agent_output_unavailable")?
        );
    } else {
        println!("{}", plain(value));
    }
    Ok(())
}

pub(crate) fn status_entry(path: &Path, json_out: bool) -> AgentResult<()> {
    let c = load_current(path)?;
    let value = status(&c, super::millis())?;
    print_value(&value, json_out, |value| {
        format!(
            "status={} session={} revision={} pending_aside={} paper_only=true model_call=false",
            value["status"].as_str().unwrap_or_default(),
            value["session"]["id"].as_str().unwrap_or_default(),
            value["agent"]["instruction_revision"]
                .as_u64()
                .unwrap_or_default(),
            value["agent"]["pending_aside"]["id"]
                .as_str()
                .unwrap_or("-"),
        )
    })
}

#[cfg(test)]
pub(crate) fn queue_question(c: &Config, question: &str, id: &str, now: i64) -> AgentResult<Value> {
    ask(c, question, Some(id), Some(id), None, now)
}

pub(crate) fn ask_entry(
    path: &Path,
    question: &str,
    request_id: Option<&str>,
    idempotency_key: Option<&str>,
    ttl_ms: Option<i64>,
    json_out: bool,
) -> AgentResult<()> {
    let c = load_current(path)?;
    let value = ask(
        &c,
        question,
        request_id,
        idempotency_key,
        ttl_ms,
        super::millis(),
    )?;
    print_value(&value, json_out, |value| {
        format!(
            "status={} id={} session={} duplicate={}",
            value["status"].as_str().unwrap_or_default(),
            value["id"].as_str().unwrap_or_default(),
            value["session"].as_str().unwrap_or_default(),
            value["duplicate"].as_bool().unwrap_or(false),
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn instruct_entry(
    path: &Path,
    text: &str,
    scope: &str,
    expect_session: &str,
    expect_revision: u64,
    request_id: Option<&str>,
    idempotency_key: Option<&str>,
    ttl_ms: Option<i64>,
    json_out: bool,
) -> AgentResult<()> {
    let c = load_current(path)?;
    let value = instruct(
        &c,
        text,
        scope,
        expect_session,
        expect_revision,
        request_id,
        idempotency_key,
        ttl_ms,
        super::millis(),
    )?;
    print_value(&value, json_out, |value| {
        format!(
            "status={} id={} revision={} expires_at_ms={} target=owner trading_effect=none",
            value["status"].as_str().unwrap_or_default(),
            value["id"].as_str().unwrap_or_default(),
            value["revision"].as_u64().unwrap_or_default(),
            value["expires_at_ms"].as_i64().unwrap_or_default(),
        )
    })
}

pub(crate) fn result_entry(path: &Path, request_id: &str, json_out: bool) -> AgentResult<()> {
    let c = load_current(path)?;
    let value = result(&c, request_id, super::millis())?;
    print_value(&value, json_out, |value| {
        format!(
            "status={} id={} kind={} session={}",
            value["status"].as_str().unwrap_or_default(),
            value["id"].as_str().unwrap_or_default(),
            value["kind"].as_str().unwrap_or_default(),
            value["session"].as_str().unwrap_or_default(),
        )
    })
}

pub(crate) fn cancel_entry(path: &Path, request_id: &str, json_out: bool) -> AgentResult<()> {
    let c = load_current(path)?;
    let value = cancel(&c, request_id, super::millis())?;
    print_value(&value, json_out, |value| {
        format!(
            "status={} id={} scope=request_only trading_effect=none",
            value["status"].as_str().unwrap_or_default(),
            value["id"].as_str().unwrap_or_default(),
        )
    })
}

#[cfg(test)]
pub(crate) fn exercise() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = super::tests::config(dir.path());
    c.session = "2026-11-04".parse().unwrap();
    c.previous_session = "2026-11-03".parse().unwrap();
    let session = c.session.to_string();
    let now = 1_000_000;

    // Ask is queued only in the existing read-only aside slot and is idempotent.
    let queued = ask(
        &c,
        "Why was the entry skipped?",
        Some("ask-1"),
        Some("key-1"),
        None,
        now,
    )
    .unwrap();
    assert_eq!(
        (queued["status"].as_str(), queued["id"].as_str()),
        (Some("queued"), Some("ask-1"))
    );
    let request = read_value(&aside_request_path(&c, &session)).unwrap();
    assert_eq!(request["id"], "ask-1");
    assert_eq!(request["question"], "Why was the entry skipped?");
    let duplicate = ask(
        &c,
        "Why was the entry skipped?",
        None,
        Some("key-1"),
        None,
        now + 1,
    )
    .unwrap();
    assert_eq!(duplicate["status"], "queued");
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(duplicate["id"], "ask-1");
    assert_eq!(
        ask(&c, "A second question", None, Some("key-2"), None, now + 2).unwrap_err(),
        "agent_ask_busy"
    );
    assert_eq!(result(&c, "ask-1", now + 3).unwrap()["status"], "queued");

    // Running is visible after the controller starts its read-only instance.
    rollover::atomic(
        &aside_start_path(&c, &session),
        &json!({"id":"ask-1","session":session,"started_at_ms":now + 4}),
    )
    .unwrap();
    assert_eq!(result(&c, "ask-1", now + 5).unwrap()["status"], "running");

    // A reply is answered once and can be polled again without resubmission.
    rollover::atomic(
        &aside_reply_path(&c, &session),
        &json!({"id":"ask-1","session":session,"reply":"No eligible quote.","read_only":true,"at_ms":now + 6}),
    )
    .unwrap();
    let answered = result(&c, "ask-1", now + 7).unwrap();
    assert_eq!(answered["status"], "answered");
    assert_eq!(answered["answer"], "No eligible quote.");
    assert_eq!(answered["resubmitted"], false);
    assert_eq!(result(&c, "ask-1", now + 8).unwrap()["status"], "answered");
    assert!(!aside_request_path(&c, &session).exists());

    // Cancellation removes only the matching queued request.
    let second = ask(
        &c,
        "Summarise the last fill.",
        None,
        Some("key-3"),
        None,
        now + 10,
    )
    .unwrap();
    let second_id = second["id"].as_str().unwrap().to_owned();
    let cancelled = cancel(&c, &second_id, now + 11).unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["cancellation_scope"], "request_only");
    assert!(!aside_request_path(&c, &session).exists());
    assert_eq!(
        cancel(&c, &second_id, now + 12).unwrap_err(),
        "agent_request_not_cancellable"
    );

    // Expiry and a provider-side error are distinct terminal states.
    let third = ask(
        &c,
        "Any stale note?",
        None,
        Some("key-4"),
        Some(1_000),
        now + 20,
    )
    .unwrap();
    let third_id = third["id"].as_str().unwrap().to_owned();
    assert_eq!(
        result(&c, &third_id, now + 1_021).unwrap()["status"],
        "expired"
    );
    assert!(!aside_request_path(&c, &session).exists());
    let fourth = ask(&c, "Check the quote.", None, Some("key-5"), None, now + 30).unwrap();
    let fourth_id = fourth["id"].as_str().unwrap().to_owned();
    rollover::atomic(
        &aside_reply_path(&c, &session),
        &json!({"id":fourth_id,"session":session,"error":"provider_timeout","read_only":true,"at_ms":now + 31}),
    )
    .unwrap();
    let failed = result(&c, &fourth_id, now + 32).unwrap();
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["reason"], "provider_timeout");

    // Status is a read-only projection with no model call or writes.
    let before = fs::read(store_path(&c)).unwrap();
    let status_value = status(&c, now + 33).unwrap();
    assert_eq!(status_value["model_call"], false);
    assert_eq!(status_value["writes"], false);
    assert_eq!(status_value["session"]["id"], session);
    assert_eq!(fs::read(store_path(&c)).unwrap(), before);

    // One explicitly scoped instruction applies to the owner directive store.
    let applied = instruct(
        &c,
        "Prefer cash when the quote is stale.",
        "decision_context",
        &session,
        0,
        Some("instruct-1"),
        Some("instruction-key-1"),
        None,
        now + 40,
    )
    .unwrap();
    assert_eq!(applied["status"], "applied");
    assert_eq!(applied["revision"], 1);
    assert_eq!(applied["expected_session"], session);
    assert_eq!(applied["target"], "owner");
    assert_eq!(applied["application"], "owner_context_only");
    let owner = directives::active(c.directory.as_path(), now + 41, directives::OWNER_TARGET);
    assert_eq!(owner.len(), 1);
    assert_eq!(owner[0].request_id.as_deref(), Some("instruct-1"));
    assert_eq!(
        owner[0].authorization_scope.as_deref(),
        Some("decision_context")
    );
    let decide_envelope = directives::envelope(c.directory.as_path(), now + 41, "decide", 3);
    assert!(
        decide_envelope
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == owner[0].id)
    );
    let duplicate = instruct(
        &c,
        "Prefer cash when the quote is stale.",
        "decision_context",
        &session,
        0,
        None,
        Some("instruction-key-1"),
        None,
        now + 42,
    )
    .unwrap();
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(duplicate["id"], "instruct-1");
    assert_eq!(
        instruct(
            &c,
            "Ignore the evidence.",
            "decision_context",
            &session,
            0,
            Some("instruct-stale"),
            Some("instruction-key-stale"),
            None,
            now + 43,
        )
        .unwrap()["status"],
        "refused"
    );
    let old_session = instruct(
        &c,
        "Use the old session.",
        "review_only",
        "2026-11-03",
        1,
        Some("instruct-old"),
        Some("instruction-key-old"),
        None,
        now + 44,
    )
    .unwrap();
    assert_eq!(old_session["reason"], "agent_session_changed");
    assert_eq!(old_session["expected_session"], "2026-11-03");
    assert_eq!(
        result(&c, "instruct-1", now + 45).unwrap()["status"],
        "applied"
    );
    let cancelled = cancel(&c, "instruct-1", now + 46).unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    assert!(
        directives::active(c.directory.as_path(), now + 47, directives::OWNER_TARGET).is_empty()
    );
    assert_eq!(
        result(&c, "instruct-1", now + 48).unwrap()["status"],
        "cancelled"
    );

    // An expired instruction is reported as expired and never re-applied.
    let expiring = instruct(
        &c,
        "Review only.",
        "review_only",
        &session,
        1,
        Some("instruct-2"),
        Some("instruction-key-2"),
        Some(1_000),
        now + 50,
    )
    .unwrap();
    assert_eq!(expiring["status"], "applied");
    assert_eq!(expiring["revision"], 2);
    assert_eq!(
        load(&c)
            .unwrap()
            .instruction_revisions
            .get(&session)
            .copied(),
        Some(2)
    );
    assert_eq!(
        result(&c, "instruct-2", now + 1_051).unwrap()["status"],
        "expired"
    );
    assert_eq!(
        result(&c, "missing", now + 60).unwrap_err(),
        "agent_request_unknown"
    );

    // A terminal ask receipt frees the single slot even before it is polled.
    let stale_ask = ask(
        &c,
        "Still there?",
        None,
        Some("key-6"),
        Some(1_000),
        now + 1_100,
    )
    .unwrap();
    let stale_id = stale_ask["id"].as_str().unwrap().to_owned();
    let next_ask = ask(&c, "Next question.", None, Some("key-7"), None, now + 2_101).unwrap();
    assert_eq!(next_ask["status"], "queued");
    assert_eq!(
        result(&c, &stale_id, now + 2_102).unwrap()["status"],
        "expired"
    );
}
