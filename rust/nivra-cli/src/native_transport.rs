//! Bounded response consumption and fixed-origin model HTTP. No cookies or retry.
use nivra_core::{
    native::{self, ParsedResponse, ResponseKind},
    native_stream::{Error, Headers},
};
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};
use tokio::time::{Instant, timeout_at};
/// A future HTTP adapter must bound chunk allocation and deny redirects, environment
/// auth/proxies/cookies. Implementations own response cleanup when a future is dropped.
pub type ChunkFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, native::Error>> + 'a>>;
pub trait Body {
    fn next(&mut self) -> ChunkFuture<'_>;
}
fn check<G: FnMut() -> Result<(), Error>>(deadline: Instant, guard: &mut G) -> Result<(), Error> {
    if Instant::now() >= deadline {
        return Err(Error::Deadline);
    }
    guard()
}
/// One absolute deadline covers all awaits. Cancellation drops the active read;
/// no detached task or retry is created. Guards must verify original file/grant/
/// account/lease/current reservation/expiry. Synchronous guards require bounded
/// storage workers in the real adapter; async timeouts cannot preempt blocking code.
pub async fn read_response<B: Body, G: FnMut() -> Result<(), Error>>(
    body: &mut B,
    headers: Headers<'_>,
    kind: ResponseKind<'_>,
    deadline: Instant,
    mut guard: G,
) -> Result<ParsedResponse, Error> {
    if Instant::now() >= deadline {
        return Err(Error::Deadline);
    }
    timeout_at(deadline, async {
        check(deadline, &mut guard)?;
        native::validate_http_headers(headers.status, headers.content_type, headers.encoding)?;
        let limit = match kind {
            ResponseKind::Catalog => 262144,
            ResponseKind::Trading { .. } => 1000000,
        };
        let mut raw = Vec::new();
        while let Some(chunk) = body.next().await.map_err(Error::from)? {
            check(deadline, &mut guard)?;
            if chunk.len() > limit - raw.len() {
                return Err(native::Error::OutputInvalid.into());
            }
            raw.extend_from_slice(&chunk);
        }
        check(deadline, &mut guard)?;
        native::parse_http(
            headers.status,
            headers.content_type,
            headers.encoding,
            &raw,
            kind,
        )
        .map_err(Error::from)
    })
    .await
    .map_err(|_| Error::Deadline)?
}

/// Existing configurations keep the subscription proxy. Direct OAuth is an
/// explicit origin selection; neither transport authenticates model entitlement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    SubscriptionProxy,
    DirectOauth,
    OpenaiCodex,
}
impl Transport {
    pub const fn models_url(self) -> &'static str {
        match self {
            Self::SubscriptionProxy => SUBSCRIPTION_MODELS,
            Self::DirectOauth => DIRECT_MODELS,
            Self::OpenaiCodex => CODEX_MODELS,
        }
    }
    pub const fn responses_url(self) -> &'static str {
        match self {
            Self::SubscriptionProxy => SUBSCRIPTION_RESPONSES,
            Self::DirectOauth => DIRECT_RESPONSES,
            Self::OpenaiCodex => CODEX_RESPONSES,
        }
    }
}
/// Explicit fallback transports (#245). A fallback is never a primary role
/// transport: it only serves one call after the primary hit a usage, rate or
/// quota limit, and only when the reviewed policy configures it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackTransport {
    /// Mistral's OpenAI-compatible chat completions (no `store`, system role,
    /// `max_tokens`), authenticated with an application-owned API key file.
    MistralChat,
}
impl FallbackTransport {
    pub const fn chat_url(self) -> &'static str {
        match self {
            Self::MistralChat => MISTRAL_CHAT,
        }
    }
}
pub const MISTRAL_CHAT: &str = "https://api.mistral.ai/v1/chat/completions";
pub const DIRECT_MODELS: &str = "https://api.x.ai/v1/models";
pub const DIRECT_RESPONSES: &str = "https://api.x.ai/v1/responses";
pub const SUBSCRIPTION_MODELS: &str = "https://cli-chat-proxy.grok.com/v1/models";
pub const SUBSCRIPTION_RESPONSES: &str = "https://cli-chat-proxy.grok.com/v1/responses";
pub const CODEX_MODELS: &str = "https://chatgpt.com/backend-api/codex/models?client_version=99.0.0";
pub const CODEX_MODELS_LEGACY: &str =
    "https://chatgpt.com/backend-api/codex/models?client_version=0.0.0";
pub const CODEX_RESPONSES: &str = "https://chatgpt.com/backend-api/codex/responses";
pub const OPENAI_DEVICE_CODE: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
pub const OPENAI_DEVICE_TOKEN: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
pub const OPENAI_OAUTH_TOKEN: &str = "https://auth.openai.com/oauth/token";
pub const OPENAI_DEVICE_URI: &str = "https://auth.openai.com/codex/device";

/// Proxy-only identity from OpenClaw extensions/xai/stream.ts; see transport sources doc.
pub fn subscription_headers<'a>(
    model: &'a str,
    client_version: Option<&'a str>,
) -> Result<[(&'static str, &'a str); 3], &'static str> {
    let version = client_version.ok_or("model_proxy_client_version_missing")?;
    if version.len() > 32
        || !version.contains('.')
        || !version.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || version.split('.').any(str::is_empty)
    {
        return Err("model_proxy_client_version_invalid");
    }
    Ok([
        ("X-XAI-Token-Auth", "xai-grok-cli"),
        ("x-grok-client-version", version),
        ("x-grok-model-override", model),
    ])
}

/// Origin-specific non-secret Responses headers. Direct OAuth must never inherit
/// the CLI proxy's identity, version or model-override headers. Bearer credentials
/// and content type are set separately by the fixed-origin HTTP adapter.
pub fn response_headers<'a>(
    transport: Transport,
    model: &'a str,
    proxy_client_version: Option<&'a str>,
) -> Result<Vec<(&'static str, &'a str)>, &'static str> {
    let mut headers = Vec::new();
    if transport == Transport::SubscriptionProxy {
        headers.extend(subscription_headers(model, proxy_client_version)?);
    }
    if transport == Transport::OpenaiCodex {
        headers.push(("originator", "nivra"));
    }
    headers.push(("Accept", "text/event-stream"));
    headers.push(("Accept-Encoding", "identity"));
    Ok(headers)
}
/// Validate an explicitly supplied opaque conversation identity. No identity is
/// generated here, and invalid header/control characters are refused before HTTP.
pub fn validate_conversation_id(id: &str) -> Result<(), &'static str> {
    if !(16..=64).contains(&id.len())
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err("model_conversation_id_invalid");
    }
    Ok(())
}
/// Explicit context affinity is independent of authentication and model identity.
/// Callers may use this only on the selected fixed Responses endpoint; the proxy
/// alone receives its session header. Absence preserves the historical request.
pub fn context_headers(
    transport: Transport,
    conversation_id: Option<&str>,
) -> Result<Vec<(&'static str, &str)>, &'static str> {
    let Some(id) = conversation_id else {
        return Ok(Vec::new());
    };
    validate_conversation_id(id)?;
    if transport == Transport::OpenaiCodex {
        return Err("model_conversation_unsupported");
    }
    let mut headers = vec![("x-grok-conv-id", id)];
    if transport == Transport::SubscriptionProxy {
        headers.push(("x-grok-session-id", id));
    }
    Ok(headers)
}
/// Account headers taken from the Codex access token. The signature is not checked.
pub struct CodexIdentity {
    pub account_id: String,
    pub residency: Option<String>,
}
pub fn codex_identity(token: &str) -> Result<CodexIdentity, &'static str> {
    use base64::Engine;
    let part = token.split('.').nth(1).ok_or("account_identity_invalid")?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part.trim_end_matches('='))
        .map_err(|_| "account_identity_invalid")?;
    let payload: serde_json::Value =
        serde_json::from_slice(&payload).map_err(|_| "account_identity_invalid")?;
    let auth = &payload["https://api.openai.com/auth"];
    let account_id = auth["chatgpt_account_id"]
        .as_str()
        .filter(|id| (8..=64).contains(&id.len()))
        .filter(|id| id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .ok_or("account_identity_invalid")?
        .to_owned();
    let residency = auth["chatgpt_data_residency"]
        .as_str()
        .or_else(|| auth["chatgpt_compute_residency"].as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if let Some(value) = &residency
        && (value.len() > 32
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')))
    {
        return Err("account_identity_invalid");
    }
    Ok(CodexIdentity {
        account_id,
        residency,
    })
}
fn apply_codex_identity<B>(
    req: ureq::RequestBuilder<B>,
    identity: &CodexIdentity,
) -> ureq::RequestBuilder<B> {
    let req = req.header("ChatGPT-Account-ID", &identity.account_id);
    if let Some(residency) = &identity.residency {
        req.header("x-openai-internal-codex-residency", residency)
    } else {
        req
    }
}

/// The global request deadline also covers streaming the response body. A read
/// that hits it is a timeout, not a malformed body: keep the two evidence codes
/// distinct so a slow or stalled provider is not reported as invalid decoding.
pub fn body_read_error(error: &std::io::Error) -> &'static str {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) || error.get_ref().is_some_and(|cause| {
        matches!(
            cause.downcast_ref::<ureq::Error>(),
            Some(ureq::Error::Timeout(_))
        )
    }) {
        "model_timeout"
    } else {
        "model_body_invalid"
    }
}

/// JSON bodies and each SSE frame/retained terminal response share this bound.
pub fn response_body_limit(url: &str) -> u64 {
    if [
        SUBSCRIPTION_RESPONSES,
        DIRECT_RESPONSES,
        CODEX_RESPONSES,
        CODEX_MODELS,
        CODEX_MODELS_LEGACY,
        MISTRAL_CHAT,
    ]
    .contains(&url)
    {
        1_000_000
    } else {
        262144
    }
}

/// Consume a requested Responses stream without retaining redundant deltas.
/// Each frame and the retained terminal evidence remain bounded by `maximum`;
/// total wire consumption is at most eight times that bound, under the same
/// HTTP deadline. Observers still receive the original unvalidated chunks.
/// A JSON snapshot returned for a streaming request retains the ordinary bound.
pub fn read_response_body(
    reader: &mut impl std::io::Read,
    maximum: usize,
    streaming: bool,
    observer: Option<Observer<'_>>,
) -> Result<Vec<u8>, &'static str> {
    read_response_body_until(reader, maximum, streaming, observer, || false)
}

/// The injected deadline clock covers body reads, including generic TLS/IO
/// failures and EOF at expiry. Error text is never used for classification.
pub fn read_response_body_until(
    reader: &mut impl std::io::Read,
    maximum: usize,
    mut streaming: bool,
    mut observer: Option<Observer<'_>>,
    mut expired: impl FnMut() -> bool,
) -> Result<Vec<u8>, &'static str> {
    let mut pending = Vec::new();
    let mut retained = Vec::new();
    let mut total = 0usize;
    let mut classified = !streaming;
    let mut completed = false;
    loop {
        let mut chunk = [0; 4096];
        if expired() {
            return Err("model_timeout");
        }
        let read = reader.read(&mut chunk);
        if expired() {
            return Err("model_timeout");
        }
        let n = read.map_err(|error| body_read_error(&error))?;
        if n == 0 {
            break;
        }
        total = total.checked_add(n).ok_or("model_body_too_large")?;
        if total > maximum.saturating_mul(if streaming { 8 } else { 1 }) {
            return Err("model_body_too_large");
        }
        pending.extend_from_slice(&chunk[..n]);
        if !classified && let Some(first) = pending.iter().find(|byte| !byte.is_ascii_whitespace())
        {
            streaming = *first != b'{';
            classified = true;
        }
        if let Some(callback) = &mut observer {
            callback(&chunk[..n]);
        }
        if streaming {
            loop {
                let separator = pending
                    .windows(2)
                    .position(|v| v == b"\n\n")
                    .map(|at| (at, 2))
                    .into_iter()
                    .chain(
                        pending
                            .windows(4)
                            .position(|v| v == b"\r\n\r\n")
                            .map(|at| (at, 4)),
                    )
                    .min_by_key(|(at, _)| *at);
                let Some((at, width)) = separator else { break };
                if at + width > maximum {
                    return Err("model_body_too_large");
                }
                retain_response_frame(&pending[..at], maximum, &mut retained, &mut completed)?;
                pending.drain(..at + width);
            }
        }
        if pending.len() > maximum {
            return Err("model_body_too_large");
        }
    }
    if !streaming {
        return Ok(pending);
    }
    // A terminal snapshot never makes a truncated or invalid tail successful.
    if !pending.is_empty() || retained.is_empty() {
        return Err("model_stream_incomplete");
    }
    Ok(retained)
}

fn retain_response_frame(
    frame: &[u8],
    maximum: usize,
    retained: &mut Vec<u8>,
    completed: &mut bool,
) -> Result<(), &'static str> {
    let text = std::str::from_utf8(frame).map_err(|_| "model_stream_invalid")?;
    let normalized = text.replace("\r\n", "\n");
    let data = normalized
        .lines()
        .filter_map(|line| {
            line.strip_prefix("data:")
                .map(|s| s.strip_prefix(' ').unwrap_or(s))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    let event =
        native::decode_bounded(data.as_bytes(), maximum).map_err(|_| "model_stream_invalid")?;
    let kind = event["type"].as_str();
    if kind == Some("response.completed") {
        if *completed || event["response"]["status"] != "completed" {
            return Err("model_stream_invalid");
        }
        *completed = true;
    }
    if matches!(
        kind,
        Some("response.completed" | "response.failed" | "response.incomplete" | "error")
    ) {
        if retained
            .len()
            .saturating_add(normalized.len())
            .saturating_add(2)
            > maximum
        {
            return Err("model_body_too_large");
        }
        retained.extend_from_slice(normalized.as_bytes());
        retained.extend_from_slice(b"\n\n");
    }
    Ok(())
}

/// Fixed provider origins, no redirects/proxies/cookies, bounded body and deadline.
pub fn http(
    timeout_ms: u64,
    url: &str,
    form: Option<&[(&str, &str)]>,
    body: Option<&serde_json::Value>,
    token: Option<&str>,
    proxy_client_version: Option<&str>,
    conversation_id: Option<&str>,
) -> Result<(u16, Vec<u8>), &'static str> {
    http_observed(
        timeout_ms,
        url,
        form,
        body,
        token,
        proxy_client_version,
        conversation_id,
        None,
    )
}
pub type Observer<'a> = &'a mut dyn FnMut(&[u8]);

#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// Bounded standard Retry-After field. Callers must parse it as untrusted input.
    pub retry_after: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub fn http_observed(
    timeout_ms: u64,
    url: &str,
    form: Option<&[(&str, &str)]>,
    body: Option<&serde_json::Value>,
    token: Option<&str>,
    proxy_client_version: Option<&str>,
    conversation_id: Option<&str>,
    observer: Option<Observer<'_>>,
) -> Result<(u16, Vec<u8>), &'static str> {
    let response = http_observed_with_retry_after(
        timeout_ms,
        url,
        form,
        body,
        token,
        proxy_client_version,
        conversation_id,
        observer,
    )?;
    Ok((response.status, response.body))
}

#[allow(clippy::too_many_arguments)]
pub fn http_observed_with_retry_after(
    timeout_ms: u64,
    url: &str,
    form: Option<&[(&str, &str)]>,
    body: Option<&serde_json::Value>,
    token: Option<&str>,
    proxy_client_version: Option<&str>,
    conversation_id: Option<&str>,
    mut observer: Option<Observer<'_>>,
) -> Result<HttpResponse, &'static str> {
    if let Some(id) = conversation_id {
        validate_conversation_id(id)?;
    }
    if ![
        "https://auth.x.ai/.well-known/openid-configuration",
        "https://auth.x.ai/oauth2/device/code",
        "https://auth.x.ai/oauth2/token",
        "https://auth.x.ai/oauth2/userinfo",
        DIRECT_MODELS,
        DIRECT_RESPONSES,
        SUBSCRIPTION_MODELS,
        SUBSCRIPTION_RESPONSES,
        CODEX_MODELS,
        CODEX_MODELS_LEGACY,
        CODEX_RESPONSES,
        OPENAI_DEVICE_CODE,
        OPENAI_DEVICE_TOKEN,
        OPENAI_OAUTH_TOKEN,
        MISTRAL_CHAT,
    ]
    .contains(&url)
    {
        return Err("model_origin_refused");
    }
    let started = std::time::Instant::now();
    let timeout = std::time::Duration::from_millis(timeout_ms);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(timeout_ms)))
        .max_redirects(0)
        .proxy(None)
        .user_agent(concat!("nivra/", env!("CARGO_PKG_VERSION")))
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = if let Some(form) = form {
        agent
            .post(url)
            .header("Accept", "application/json")
            .send_form(form.iter().copied())
            .map_err(|e| {
                if matches!(e, ureq::Error::Timeout(_)) {
                    "model_timeout"
                } else {
                    "model_transport_failed"
                }
            })?
    } else if let Some(body) = body {
        let bytes = serde_json::to_vec(body).map_err(|_| "model_request_invalid")?;
        let device = matches!(url, OPENAI_DEVICE_CODE | OPENAI_DEVICE_TOKEN);
        let mut req = agent
            .post(url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json");
        if !device {
            let bearer = token.ok_or("grant_invalid")?;
            req = req.header("Authorization", &format!("Bearer {bearer}"));
            if url == CODEX_RESPONSES {
                req = apply_codex_identity(req, &codex_identity(bearer)?);
            }
        }
        let transport = match url {
            SUBSCRIPTION_RESPONSES => Some(Transport::SubscriptionProxy),
            DIRECT_RESPONSES => Some(Transport::DirectOauth),
            CODEX_RESPONSES => Some(Transport::OpenaiCodex),
            _ => None,
        };
        if let Some(transport) = transport {
            for (name, value) in response_headers(
                transport,
                body["model"].as_str().ok_or("model_request_invalid")?,
                proxy_client_version,
            )? {
                req = req.header(name, value);
            }
            for (name, value) in context_headers(transport, conversation_id)? {
                req = req.header(name, value);
            }
        }
        req.send(bytes.as_slice()).map_err(|e| {
            if matches!(e, ureq::Error::Timeout(_)) {
                "model_timeout"
            } else {
                "model_transport_failed"
            }
        })?
    } else {
        let mut req = agent.get(url).header("Accept", "application/json");
        if let Some(token) = token {
            req = req.header("Authorization", &format!("Bearer {token}"));
            if matches!(url, CODEX_MODELS | CODEX_MODELS_LEGACY) {
                req = apply_codex_identity(req, &codex_identity(token)?);
                req = req.header("originator", "nivra");
            }
        }
        req.call().map_err(|e| {
            if matches!(e, ureq::Error::Timeout(_)) {
                "model_timeout"
            } else {
                "model_transport_failed"
            }
        })?
    };
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 128 && value.is_ascii())
        .map(str::to_owned);
    let mut reader = response.body_mut().as_reader();
    let streaming = status == 200
        && matches!(
            url,
            DIRECT_RESPONSES | SUBSCRIPTION_RESPONSES | CODEX_RESPONSES
        )
        && body.is_some_and(|request| request["stream"] == true);
    let bytes = read_response_body_until(
        &mut reader,
        response_body_limit(url) as usize,
        streaming,
        if status == 200 { observer.take() } else { None },
        || started.elapsed() >= timeout,
    )?;
    Ok(HttpResponse {
        status,
        body: bytes,
        retry_after,
    })
}
