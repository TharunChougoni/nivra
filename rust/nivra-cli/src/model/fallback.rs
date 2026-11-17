//! Explicit limit fallback (#245). One planner call whose primary failed with
//! a usage, rate or quota limit is retried once on the role's configured
//! fallback: an OpenAI-compatible chat completions provider (Mistral). The
//! Responses-shaped harness frame is translated to chat messages and the reply
//! back to the same `{output, model, usage}` envelope, so the harness, tool
//! validation, usage journal and paper boundaries are unchanged.
use super::*;
use crate::runtime::planner::{FallbackProfile, ModelRole};
use nivra_cli::native_transport::FallbackTransport;

/// Application-owned fallback credential config, referenced by the profile's
/// `model_config`. It names a private key file; it never holds the key.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FallbackConfig {
    transport: FallbackTransport,
    api_key_file: PathBuf,
}

fn fallback_config(path: &Path) -> Result<FallbackConfig> {
    let bytes = crate::read_bounded(path, 16384).map_err(|_| "fallback_config_unreadable")?;
    parse_fallback_config(&bytes)
}

pub(crate) fn parse_fallback_config(bytes: &[u8]) -> Result<FallbackConfig> {
    let c: FallbackConfig = serde_json::from_slice(bytes).map_err(|_| "fallback_config_invalid")?;
    nivra_store::native_grant::file_path_ok(&c.api_key_file)
        .map_err(|_| "fallback_key_path_invalid")?;
    Ok(c)
}

fn fallback_key(profile: &FallbackProfile) -> Result<String> {
    let c = fallback_config(&profile.model_config)?;
    if c.transport != profile.transport {
        return Err("fallback_transport_mismatch".into());
    }
    nivra_store::native_grant::read_api_key_file(&c.api_key_file)
        .map_err(|_| "fallback_key_unavailable".into())
}

/// The bounded limit class of a primary failure, or `None` when the failure
/// is anything else (credentials, timeouts, invalid output, local refusals).
/// Only these provider limits may switch a call to the fallback.
pub(crate) fn limit_reason(error: &str) -> Option<&'static str> {
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
    } else {
        None
    }
}

/// Fallback readiness without a provider call: configured, credential config
/// parses, transport matches and the private key file is readable.
pub(crate) fn fallback_readiness(profile: &FallbackProfile) -> Value {
    let status = match fallback_key(profile) {
        Ok(_) => json!({"status":"ready","reason":Value::Null}),
        Err(reason) => json!({"status":"blocked","reason":reason}),
    };
    let mut report = json!({"configured":true,"model":profile.model_id,"transport":profile.transport,
        "origin":profile.transport.chat_url(),"budget":"none","admission":"reviewed_policy_fallback"});
    report["status"] = status["status"].clone();
    report["reason"] = status["reason"].clone();
    report
}

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

/// One fallback call for the same frame within the remaining call time.
/// Errors keep the existing bounded codes; the key is redacted everywhere.
pub(crate) fn fallback_response(
    profile: &FallbackProfile,
    role: &ModelRole,
    prompt: &str,
    remaining_ms: u64,
    provenance: &mut Value,
) -> Result<String> {
    *provenance = json!({"transport":profile.transport,"origin":profile.transport.chat_url(),
        "model":profile.model_id});
    provenance["stage"] = json!("frame_decode");
    let frame: crate::runtime::harness::Frame = serde_json::from_value(
        native::decode_bounded(prompt.as_bytes(), 2_000_000)
            .map_err(|_| "harness_request_invalid")?,
    )
    .map_err(|_| "harness_request_invalid")?;
    provenance["stage"] = json!("tool_definitions");
    let instructions = frame_preflight(&frame, role)?;
    provenance["stage"] = json!("application_config");
    let key = fallback_key(profile)?;
    if remaining_ms == 0 {
        return Err("model_timeout".into());
    }
    provenance["stage"] = json!("inference");
    let tools = (!frame.final_answer_only).then_some(frame.tools.as_slice());
    let body = chat_body(profile, role, &instructions, &frame.input, tools)?;
    provenance["request_bytes"] = json!(
        serde_json::to_vec(&body)
            .map_err(|_| "model_request_invalid")?
            .len()
    );
    provenance["max_output_tokens"] = json!(profile.max_tokens(role));
    nivra_cli::model_stream::publish(
        &frame.role,
        &profile.model_id,
        &hash(prompt.as_bytes()),
        "request_started",
        "Fallback request after a primary usage limit; output is unvalidated",
    );
    provenance["stage"] = json!("transport");
    let deadline = Instant::now() + Duration::from_millis(remaining_ms);
    let response = retry_model_429(
        deadline,
        || {
            let timeout_ms = deadline
                .checked_duration_since(Instant::now())
                .map(|remaining| remaining.as_millis() as u64)
                .filter(|remaining| *remaining > 0)
                .ok_or_else(|| "model_timeout".to_owned())?;
            nivra_cli::native_transport::http_observed_with_retry_after(
                timeout_ms,
                profile.transport.chat_url(),
                None,
                Some(&body),
                Some(&key),
                None,
                None,
                None,
            )
            .map_err(str::to_owned)
        },
        std::thread::sleep,
    )?;
    provenance["stage"] = json!("http_response");
    provenance["http_status"] = json!(response.status);
    provenance["stage"] = json!("response_validation");
    let value = chat_envelope(response.status, &response.body, &key, provenance)?;
    if value["model"] != profile.model_id.as_str() {
        return Err("planner_response_model_mismatch".into());
    }
    provenance["stage"] = json!("output_validation");
    let text = serde_json::to_string(&value)
        .map_err(|_| "harness_output_invalid")?
        .replace(&key, "[redacted]");
    if let Some(code) = role.provider_envelope_code(&text) {
        return Err(code.into());
    }
    nivra_cli::model_stream::publish(
        &frame.role,
        &profile.model_id,
        &hash(prompt.as_bytes()),
        "response_complete_unvalidated",
        "Fallback response completed; host validation pending",
    );
    Ok(text)
}

/// One minimal fallback inference that proves the configured key and model
/// work. It never starts a paper session or touches any journal.
pub(crate) fn probe_fallback(model_config: &Path, model: &str, effort: Option<&str>) -> Result<()> {
    let profile = FallbackProfile {
        model_id: model.to_owned(),
        transport: fallback_config(model_config)?.transport,
        model_config: model_config.to_path_buf(),
        output_cap: Some(64),
        reasoning_effort: effort.map(str::to_owned),
    };
    if profile.invalid() {
        return Err("fallback_profile_invalid".into());
    }
    let key = fallback_key(&profile)?;
    let role = ModelRole {
        output_cap: 64,
        ..ModelRole::default()
    };
    let body = chat_body(
        &profile,
        &role,
        "You are a terse health check.",
        &[json!({"role":"user","content":"Reply with OK."})],
        None,
    )?;
    let start = Instant::now();
    let response = nivra_cli::native_transport::http_observed_with_retry_after(
        60_000,
        profile.transport.chat_url(),
        None,
        Some(&body),
        Some(&key),
        None,
        None,
        None,
    )?;
    let mut evidence = json!({});
    let value = chat_envelope(response.status, &response.body, &key, &mut evidence)?;
    let received = value["model"].as_str().unwrap_or_default();
    let text = value["output"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["content"].as_array())
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<String>();
    let report = json!({"http_status":response.status,"transport":profile.transport,
        "origin":profile.transport.chat_url(),"requested_model":model,
        "model_matches_request":received == model,"latency_ms":start.elapsed().as_millis(),
        "output_text":safe_text(&text, &key, 80),"tokens_in":evidence["tokens_in"],
        "tokens_out":evidence["tokens_out"],"billing_verified_by_probe":false});
    println!("{report}");
    if received != model {
        return Err("model_response_model_mismatch".into());
    }
    Ok(())
}
