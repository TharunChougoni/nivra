//! Archived fallback configuration and offline response fixtures.
use super::*;
#[cfg(test)]
use crate::runtime::planner::{FallbackProfile, ModelRole};
use nivra_cli::native_transport::FallbackTransport;

/// Application-owned fallback credential config, referenced by the profile's
/// `model_config`. It names a private key file; it never holds the key.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FallbackConfig {
    #[serde(rename = "transport")]
    _transport: FallbackTransport,
    api_key_file: PathBuf,
}

pub(crate) fn parse_fallback_config(bytes: &[u8]) -> Result<FallbackConfig> {
    let c: FallbackConfig = serde_json::from_slice(bytes).map_err(|_| "fallback_config_invalid")?;
    nivra_store::native_grant::file_path_ok(&c.api_key_file)
        .map_err(|_| "fallback_key_path_invalid")?;
    Ok(c)
}

/// Every reason code that may switch one call, in operator-facing order.
/// Readiness publishes this set so the reviewed policy is visible without
/// reading the source; each switch records exactly one of these static codes
/// in evidence, logs and the alert, never provider text.
#[cfg(test)]
pub(crate) const FALLBACK_REASONS: [&str; 5] = [
    "usage_limit_reached",
    "rate_limited",
    "quota_exceeded",
    "entitlement_auth_failed",
    "response_validation_rejected",
];

/// The bounded class of a primary failure that may switch one call to the
/// reviewed fallback (#245, #21), or `None` when the failure is anything else.
/// `primary` is the primary call's transport evidence and bounds the two
/// provider-shape classes by stage:
///
/// - `usage_limit_reached`, `rate_limited`, `quota_exceeded`: HTTP 429 after
///   the bounded retries or a provider limit error.
/// - `entitlement_auth_failed`: HTTP 401/403 from the catalog or inference
///   request (provider stage `catalog`, `inference` or `http_response`), for
///   example an expired subscription or a revoked grant.
/// - `response_validation_rejected`: an opaque HTTP 200 whose body is a
///   provider error object or stream failure at response validation
///   (`model_http_200`), for example a provider that rejects the supervisor
///   prompt.
///
/// Timeouts, HTTP 5xx, cancellation, malformed output from a real response, a
/// grant-refresh auth failure (`grant_owner`) and local refusals never switch.
#[cfg(test)]
pub(crate) fn fallback_reason(error: &str, primary: &Value) -> Option<&'static str> {
    let code = error.split_whitespace().next()?;
    if !code.starts_with("model_http_") {
        return None;
    }
    let lower = error.to_ascii_lowercase();
    if lower.contains("usage_limit") || lower.contains("usage limit") {
        Some("usage_limit_reached")
    } else if lower.contains("quota") {
        Some("quota_exceeded")
    } else if code == "model_http_429"
        || lower.contains("rate_limit")
        || lower.contains("rate limit")
    {
        Some("rate_limited")
    } else if matches!(code, "model_http_401" | "model_http_403")
        && matches!(
            primary["stage"].as_str(),
            Some("catalog" | "inference" | "http_response")
        )
    {
        Some("entitlement_auth_failed")
    } else if code == "model_http_200" && primary["stage"] == "response_validation" {
        Some("response_validation_rejected")
    } else {
        None
    }
}

#[cfg(test)]
fn text_of(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter(|part| {
                    matches!(
                        part["type"].as_str(),
                        Some("input_text" | "output_text" | "text") | None
                    )
                })
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join(""),
        ),
        Value::Null => None,
        _ => None,
    }
}

/// Responses input items to chat messages. Consecutive assistant text and
/// function calls form one assistant message; reasoning items are dropped.
#[cfg(test)]
pub(crate) fn chat_messages(instructions: &str, input: &[Value]) -> Result<Vec<Value>> {
    let mut messages = Vec::new();
    if !instructions.is_empty() {
        messages.push(json!({"role":"system","content":instructions}));
    }
    let mut assistant: Option<Value> = None;
    let flush = |assistant: &mut Option<Value>, messages: &mut Vec<Value>| {
        if let Some(message) = assistant.take() {
            messages.push(message);
        }
    };
    for item in input {
        match item["type"].as_str() {
            Some("reasoning") => {}
            Some("function_call") => {
                let call = json!({"id":item["call_id"],"type":"function","function":{
                    "name":item["name"],"arguments":item["arguments"]}});
                if item["call_id"].as_str().is_none()
                    || item["name"].as_str().is_none()
                    || item["arguments"].as_str().is_none()
                {
                    return Err("fallback_input_invalid".into());
                }
                let message = assistant
                    .get_or_insert_with(|| json!({"role":"assistant","content":Value::Null}));
                match message["tool_calls"].as_array_mut() {
                    Some(calls) => calls.push(call),
                    None => message["tool_calls"] = json!([call]),
                }
            }
            Some("function_call_output") => {
                flush(&mut assistant, &mut messages);
                let output = match &item["output"] {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                let id = item["call_id"].as_str().ok_or("fallback_input_invalid")?;
                messages.push(json!({"role":"tool","tool_call_id":id,"content":output}));
            }
            Some("message") | None => {
                let role = item["role"].as_str().ok_or("fallback_input_invalid")?;
                let text = text_of(&item["content"]).unwrap_or_default();
                match role {
                    "assistant" => {
                        if assistant
                            .as_ref()
                            .is_some_and(|message| message.get("tool_calls").is_some())
                        {
                            flush(&mut assistant, &mut messages);
                        }
                        let message = assistant.get_or_insert_with(
                            || json!({"role":"assistant","content":Value::Null}),
                        );
                        let joined = format!("{}{text}", message["content"].as_str().unwrap_or(""));
                        message["content"] = json!(joined);
                    }
                    "user" | "system" | "developer" => {
                        flush(&mut assistant, &mut messages);
                        let role = if role == "user" { "user" } else { "system" };
                        messages.push(json!({"role":role,"content":text}));
                    }
                    _ => return Err("fallback_input_invalid".into()),
                }
            }
            Some(_) => return Err("fallback_input_unsupported".into()),
        }
    }
    flush(&mut assistant, &mut messages);
    Ok(messages)
}

/// Responses function tools to chat tools. Provider-native tools (search) have
/// no chat equivalent and refuse the fallback instead of being dropped.
#[cfg(test)]
pub(crate) fn chat_tools(tools: &[Value]) -> Result<Vec<Value>> {
    tools
        .iter()
        .map(|tool| {
            if tool["type"] != "function" || tool["name"].as_str().is_none() {
                return Err("fallback_tool_unsupported".into());
            }
            let mut function = json!({"name":tool["name"],"parameters":tool["parameters"]});
            if let Some(description) = tool["description"].as_str() {
                function["description"] = json!(description);
            }
            Ok(json!({"type":"function","function":function}))
        })
        .collect()
}

/// The exact chat completions request: `max_tokens`, no `store`, no stream,
/// system role, and `reasoning_effort` only when the profile configures it.
#[cfg(test)]
pub(crate) fn chat_body(
    profile: &FallbackProfile,
    role: &ModelRole,
    instructions: &str,
    input: &[Value],
    tools: Option<&[Value]>,
) -> Result<Value> {
    let mut body = json!({"model":profile.model_id,"messages":chat_messages(instructions, input)?,
        "max_tokens":profile.max_tokens(role),"stream":false});
    if let Some(tools) = tools.filter(|tools| !tools.is_empty()) {
        body["tools"] = Value::Array(chat_tools(tools)?);
    }
    if let Some(effort) = &profile.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    Ok(body)
}

/// A chat completion to the Responses envelope the harness consumes. Thinking
/// parts are dropped; a length stop is an incomplete response, never output.
#[cfg(test)]
pub(crate) fn chat_envelope(
    status: u16,
    bytes: &[u8],
    key: &str,
    evidence: &mut Value,
) -> Result<Value> {
    check_http(status, bytes, key)?;
    let value: Value =
        native::decode_bounded(bytes, 1_000_000).map_err(|_| "model_response_invalid")?;
    if value.get("error").is_some_and(|v| !v.is_null()) || value["object"] == "error" {
        return Err(http_error(status, bytes, &[key]));
    }
    let usage = &value["usage"];
    let mut mapped = json!({});
    if let Some(n) = usage["prompt_tokens"].as_u64() {
        mapped["input_tokens"] = json!(n);
    }
    if let Some(n) = usage["completion_tokens"].as_u64() {
        mapped["output_tokens"] = json!(n);
    }
    if let Some(n) = usage["prompt_tokens_details"]["cached_tokens"].as_u64() {
        mapped["input_tokens_details"] = json!({"cached_tokens":n});
    }
    record_usage(&json!({"usage":mapped}), evidence);
    let choice = value["choices"]
        .as_array()
        .and_then(|choices| choices.first())
        .ok_or("model_response_invalid")?;
    match choice["finish_reason"].as_str() {
        Some("stop" | "tool_calls") | None => {}
        Some("length" | "model_length") => {
            evidence["incomplete_reason"] = json!("max_output_tokens");
            return Err("model_response_incomplete".into());
        }
        Some(_) => {
            evidence["incomplete_reason"] = json!("other");
            return Err("model_response_incomplete".into());
        }
    }
    let message = &choice["message"];
    let mut output = Vec::new();
    let text = match &message["content"] {
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part["type"] == "text")
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        other => other.as_str().unwrap_or_default().to_owned(),
    };
    if !text.trim().is_empty() {
        output.push(json!({"type":"message","role":"assistant",
            "content":[{"type":"output_text","text":text}]}));
    }
    for call in message["tool_calls"].as_array().into_iter().flatten() {
        let arguments = match &call["function"]["arguments"] {
            Value::String(text) => text.clone(),
            Value::Null => return Err("model_response_invalid".into()),
            other => other.to_string(),
        };
        let (Some(id), Some(name)) = (call["id"].as_str(), call["function"]["name"].as_str())
        else {
            return Err("model_response_invalid".into());
        };
        output.push(json!({"type":"function_call","call_id":id,"name":name,"arguments":arguments}));
    }
    Ok(json!({"model":value["model"],"output":output,"usage":mapped}))
}
