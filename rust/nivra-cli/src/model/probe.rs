//! Offline parsing and preflight checks for historical provider frames.
use super::*;

/// The provider-reported cause of an incomplete response, allowlisted so no
/// provider text reaches evidence. Unknown causes are retained as `other`.
fn incomplete_reason(response: &Value) -> Option<&'static str> {
    let reason = response
        .get("incomplete_details")?
        .get("reason")?
        .as_str()?;
    Some(match reason {
        "max_output_tokens" => "max_output_tokens",
        "content_filter" => "content_filter",
        _ => "other",
    })
}
/// Retain the bounded incomplete cause and any provider-reported token counts.
/// A missing `incomplete_details` leaves the evidence empty; nothing is guessed.
fn record_incomplete(response: &Value, evidence: &mut Value) {
    if !evidence.is_object() {
        *evidence = json!({});
    }
    if let Some(reason) = incomplete_reason(response) {
        evidence["incomplete_reason"] = json!(reason);
    }
    record_usage(response, evidence);
}
/// Provider-reported token counts of one call, completed or not. Lane token
/// caps sum these from the call result (#241); absent counts stay absent.
pub(crate) fn record_usage(response: &Value, evidence: &mut Value) {
    if !evidence.is_object() {
        *evidence = json!({});
    }
    if let Some(tokens) = response
        .get("usage")
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
    {
        evidence["tokens_in"] = json!(tokens);
    }
    if let Some(tokens) = response
        .get("usage")
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
    {
        evidence["tokens_out"] = json!(tokens);
    }
}
// Bounded HTTP consumption precedes parsing. Ignore deltas; require one complete
// terminal snapshot, never promote a truncated/failed stream to probe success.
#[cfg(test)]
pub(crate) fn subscription_response(status: u16, bytes: &[u8], token: &str) -> Result<Value> {
    subscription_response_with_evidence(status, bytes, token, &mut Value::Null)
}
/// Keep the provider's error `code` (else `type`) when it is a bounded
/// identifier, so an opaque HTTP 200 rejection (#14) names its class in
/// evidence. Messages and any free text are never kept.
fn record_provider_error_code(error: &Value, evidence: &mut Value) {
    if !evidence.is_object() {
        return;
    }
    let candidates = [
        &error["error"]["code"],
        &error["error"]["type"],
        &error["code"],
    ];
    if let Some(code) = candidates
        .into_iter()
        .filter_map(Value::as_str)
        .find(|code| crate::runtime::planner::provider_error_code_ok(code))
    {
        evidence["provider_error_code"] = json!(code);
    }
}
/// Same decoder, retaining bounded incomplete-response evidence in `provenance`
/// for the planner failure record (#224).
pub(crate) fn subscription_response_with_evidence(
    status: u16,
    bytes: &[u8],
    token: &str,
    evidence: &mut Value,
) -> Result<Value> {
    if bytes.len() > 1_000_000 {
        return Err("model_body_invalid".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "model_response_invalid")?;
    // Some compatible Responses servers return a JSON snapshot even for stream=true.
    if text.trim_start().starts_with('{') {
        let value: Value = native::decode(bytes).map_err(|_| "model_response_invalid")?;
        if value.get("error").is_some_and(|v| !v.is_null()) {
            record_provider_error_code(&value, evidence);
            return Err(http_error(status, bytes, &[token]));
        }
        if value.get("status").is_some_and(|s| s != "completed") {
            record_incomplete(&value, evidence);
            return Err("model_response_incomplete".into());
        }
        return Ok(value);
    }
    let normalized = text.replace("\r\n", "\n");
    let mut terminal = None;
    for frame in normalized.split("\n\n") {
        let data = frame
            .lines()
            .filter_map(|line| {
                line.strip_prefix("data:")
                    .map(|s| s.strip_prefix(' ').unwrap_or(s))
            })
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let event: Value = native::decode(data.as_bytes()).map_err(|_| "model_stream_invalid")?;
        match event["type"].as_str() {
            Some("error" | "response.failed") => {
                let error = if event["type"] == "response.failed" {
                    &event["response"]
                } else {
                    &event
                };
                record_provider_error_code(error, evidence);
                return Err(http_error(
                    status,
                    &serde_json::to_vec(error).map_err(|_| "model_stream_invalid")?,
                    &[token],
                ));
            }
            Some("response.incomplete") => {
                record_incomplete(&event["response"], evidence);
                return Err("model_response_incomplete".into());
            }
            Some("response.completed") => {
                if terminal.is_some() || event["response"]["status"] != "completed" {
                    return Err("model_stream_invalid".into());
                }
                terminal = Some(event["response"].clone());
            }
            _ => {}
        }
    }
    if !normalized.ends_with("\n\n") {
        return Err("model_stream_incomplete".into());
    }
    terminal.ok_or("model_stream_incomplete".into())
}
/// Planner backend uses the same grant owner, refresh and bounded Responses decoder.
/// No paid API, subprocess or alternate endpoint. Only explicit HTTP 429
/// responses admit bounded retries within the original deadline.
#[cfg(test)]
pub(crate) fn planner_response(
    path: &Path,
    role: &crate::runtime::planner::ModelRole,
    prompt: &str,
    provenance: &mut Value,
) -> Result<String> {
    planner_response_with_alerts(path, role, prompt, provenance, None)
}
/// Validate a reconstructed provider frame against the host contract before any
/// credential or network work. Only a sealed supervisor review is final-only,
/// and it advertises no tools.
pub(crate) fn frame_preflight(
    frame: &crate::runtime::harness::Frame,
    role: &crate::runtime::planner::ModelRole,
) -> Result<String> {
    let instructions = crate::runtime::harness::frame_instructions(frame)
        .map_err(|_| "harness_citation_ids_invalid")?;
    if frame.review_only && (!frame.final_answer_only || frame.role != "supervisor") {
        return Err("harness_final_tools_invalid".into());
    }
    if frame.final_answer_only {
        if !frame.review_only || !frame.tools.is_empty() {
            return Err("harness_final_tools_invalid".into());
        }
    } else {
        let mut limits = frame.tool_limits.clone().unwrap_or_default();
        if let Some(max) = frame.max_instrument_keys {
            limits.max_instrument_keys = max;
        }
        let expected = crate::runtime::harness::call_tool_definitions(
            &frame.role,
            &role.tools,
            frame.decision_evidence,
            &limits,
            frame.owner_tools,
        )
        .map_err(|_| "harness_tool_invalid")?;
        if frame.tools != expected {
            return Err("harness_tool_definitions_changed".into());
        }
    }
    Ok(instructions)
}
pub(crate) fn planner_response_with_alerts(
    path: &Path,
    role: &crate::runtime::planner::ModelRole,
    prompt: &str,
    provenance: &mut Value,
    _webhook: Option<&Path>,
) -> Result<String> {
    *provenance = json!({"transport":role.transport,"origin":role.transport.responses_url(),"model":role.model_id});
    provenance["stage"] = json!("frame_decode");
    let frame: crate::runtime::harness::Frame = serde_json::from_value(
        native::decode_bounded(prompt.as_bytes(), 2_000_000)
            .map_err(|_| "harness_request_invalid")?,
    )
    .map_err(|_| "harness_request_invalid")?;
    provenance["stage"] = json!("tool_definitions");
    frame_preflight(&frame, role)?;
    provenance["stage"] = json!("application_config");
    let c = config(path)?;
    if c.transport != role.transport
        && (c.transport == Transport::OpenaiCodex || role.transport == Transport::OpenaiCodex)
    {
        return Err("planner_role_transport_config_mismatch".into());
    }
    Err("legacy_model_transport_retired".into())
}
