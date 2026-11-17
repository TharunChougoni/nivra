use super::*;

pub(crate) fn probe_body(model: &str, tool: Tool) -> Value {
    let mut body = json!({"model":model,"input":[{"role":"user","content":"Reply with OK."}],"max_output_tokens":64,"store":false,"stream":true});
    // OpenClaw extensions/xai/src/{x-search-shared,web-search-shared}.ts
    let (name, prompt) = match tool {
        Tool::None => return body,
        Tool::XSearch => (
            "x_search",
            "Use X search to find one post from @xai. Reply with OK.",
        ),
        Tool::WebSearch => (
            "web_search",
            "Use web search to find xAI's official website. Reply with OK.",
        ),
    };
    body["input"][0]["content"] = json!(prompt);
    body["tools"] = json!([{"type":name}]);
    body
}
pub(crate) fn probe_body_with_shape(model: &str, tool: Tool, shape: ProbeShape) -> Value {
    let mut body = probe_body(model, tool);
    if matches!(shape, ProbeShape::HermesText) {
        // Hermes d5aa9d7: codex.py request kwargs + _role_message_item input.
        body["instructions"] = json!("You are a helpful assistant.");
        body["input"][0]["type"] = json!("message");
    }
    body
}
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
pub(crate) fn subscription_response(status: u16, bytes: &[u8], token: &str) -> Result<Value> {
    subscription_response_with_evidence(status, bytes, token, &mut Value::Null)
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
pub(crate) fn probe_report(
    status: u16,
    bytes: &[u8],
    token: &str,
    latency_ms: u128,
) -> Result<Value> {
    check_http(status, bytes, token)?;
    let value = subscription_response(status, bytes, token)?;
    let model = value["model"].as_str().ok_or("model_response_invalid")?;
    let output = value["output"].as_array().ok_or("model_response_invalid")?;
    // Tool evidence remains diagnostic; it never executes a client-side tool.
    let called = output.iter().any(|item| {
        matches!(
            item["type"].as_str(),
            Some("web_search_call" | "custom_tool_call" | "function_call" | "x_search_call")
        )
    }) || ["web_search_calls", "x_search_calls"].iter().any(|key| {
        value["usage"]["server_side_tool_usage_details"][key]
            .as_u64()
            .is_some_and(|n| n > 0)
    });
    let text = output
        .iter()
        .filter(|item| item["type"] == "message")
        .filter_map(|item| item["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "output_text")
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    Ok(
        json!({"http_status":status,"model":safe_text(model, token, 128),"latency_ms":latency_ms,
        "tool_call_made":called,"output_text":safe_text(&text, token, 80)}),
    )
}
pub(crate) fn checked_probe_report(
    status: u16,
    bytes: &[u8],
    token: &str,
    latency_ms: u128,
    requested: &str,
) -> Result<Value> {
    let mut report = probe_report(status, bytes, token, latency_ms)?;
    let response = subscription_response(status, bytes, token)?;
    let received = response["model"].as_str().ok_or("model_response_invalid")?;
    let matched = received == requested;
    report["requested_model"] = json!(safe_text(requested, token, 128));
    report["model_matches_request"] = json!(matched);
    report["received_model_sha256"] = json!(hash(received.as_bytes()));
    Ok(report)
}
pub(crate) fn probe(
    c: &Config,
    model: &str,
    tool: Tool,
    shape: ProbeShape,
    conversation_id: Option<String>,
) -> Result<()> {
    let mut bounded = c.clone();
    sadr_cli::native_transport::context_headers(c.transport, conversation_id.as_deref())?;
    bounded.conversation_id = conversation_id;
    bounded.deadline = Some(Instant::now() + Duration::from_millis(c.timeout_ms));
    let c = &bounded;
    sadr_cli::native_transport::response_headers(
        c.transport,
        model,
        c.proxy_client_version.as_deref(),
    )?;
    if c.transport == Transport::OpenaiCodex && !matches!(tool, Tool::None) {
        return Err("openai_search_belongs_on_xai_decision".into());
    }
    if model.is_empty()
        || model.len() > 128
        || !model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._:/".contains(c))
    {
        return Err("model_id_invalid".into());
    }
    let owner = sadr_store::native_grant::FileOwner::acquire(&c.grant_file)
        .map_err(|_| "grant_busy_or_unavailable")?;
    let g = grant(c, &owner)?;
    let token = g["access_token"].as_str().ok_or("grant_invalid")?;
    if matches!(c.transport, Transport::DirectOauth | Transport::OpenaiCodex) {
        let ids = catalog(c, token)?;
        admit_model(model, &ids)?;
    }
    let mut body = probe_body_with_shape(model, tool, shape);
    if let Some(context) = c.conversation_id.as_deref() {
        body["prompt_cache_key"] = json!(context);
    }
    let start = Instant::now();
    // OpenClaw subscription Responses route, no inference retry or catalog preflight.
    let (status, bytes) = request(
        c,
        c.transport.responses_url(),
        None,
        Some(&body),
        Some(token),
    )?;
    let mut report =
        checked_probe_report(status, &bytes, token, start.elapsed().as_millis(), model)?;
    report["conversation_id_sha256"] =
        json!(c.conversation_id.as_deref().map(|id| hash(id.as_bytes())));
    report["request_shape"] = json!(shape);
    report["request_body_sha256"] = json!(hash(
        &serde_json::to_vec(&body).map_err(|_| "model_request_invalid")?
    ));
    report["transport"] = json!(c.transport);
    report["origin"] = json!(c.transport.responses_url());
    report["model_admission"] = json!("provider_listed_required");
    report["billing_verified_by_probe"] = json!(false);
    println!("{report}");
    if report["model_matches_request"] != true {
        return Err("model_response_model_mismatch".into());
    }
    Ok(())
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
    webhook: Option<&Path>,
) -> Result<String> {
    *provenance = json!({"transport":role.transport,"origin":role.transport.responses_url(),"model":role.model_id});
    provenance["stage"] = json!("frame_decode");
    let frame: crate::runtime::harness::Frame = serde_json::from_value(
        native::decode_bounded(prompt.as_bytes(), 2_000_000)
            .map_err(|_| "harness_request_invalid")?,
    )
    .map_err(|_| "harness_request_invalid")?;
    #[cfg(not(test))]
    if frame.role == "decide" {
        return Err("decision_owner_migration_required".into());
    }
    provenance["stage"] = json!("tool_definitions");
    let instructions = frame_preflight(&frame, role)?;
    provenance["stage"] = json!("application_config");
    let mut c = config(path)?;
    if let Some(webhook) = webhook {
        c.alert_webhook_file = Some(webhook.to_path_buf());
    }
    // A role must never reinterpret an xAI grant as an OpenAI credential.
    if c.transport != role.transport
        && (c.transport == Transport::OpenaiCodex || role.transport == Transport::OpenaiCodex)
    {
        return Err("planner_role_transport_config_mismatch".into());
    }
    c.transport = role.transport;
    c.timeout_ms = role.timeout_ms;
    c.deadline = Some(Instant::now() + Duration::from_millis(role.timeout_ms));
    provenance["stage"] = json!("grant_owner");
    let owner = sadr_store::native_grant::FileOwner::acquire(&c.grant_file)
        .map_err(|_| "grant_busy_or_unavailable")?;
    let g = grant(&c, &owner)?;
    let token = g["access_token"].as_str().ok_or("grant_invalid")?;
    provenance["stage"] = json!("catalog");
    let ids = catalog(&c, token)?;
    provenance["catalog_hash"] = json!(hash(
        &serde_json::to_vec(&ids).map_err(|_| "model_list_invalid")?
    ));
    provenance["provider_listed"] = json!(ids.iter().any(|id| id == &role.model_id));
    admit_model(&role.model_id, &ids)?;
    provenance["stage"] = json!("inference");
    let mut body = json!({"model":role.model_id,"instructions":instructions,"input":frame.input,
        "store":false,"stream":true,"max_output_tokens":role.output_cap});
    if !frame.final_answer_only {
        body["tools"] = serde_json::Value::Array(frame.tools.clone());
    }
    if c.transport == Transport::DirectOauth {
        body["max_turns"] = json!(4);
    }
    // The supervisor bounds the full provider request, not only the rendered
    // prompt (#224). These are measured here, where the exact body exists. The
    // envelope is the request beyond the JSON-escaped first prompt (standing
    // instructions, tool definitions and envelope fields); it is only measured
    // on the first turn, before tool round trips grow the input.
    provenance["request_bytes"] = json!(
        serde_json::to_vec(&body)
            .map_err(|_| "model_request_invalid")?
            .len()
    );
    provenance["input_items"] = json!(frame.input.len());
    provenance["prompt_bytes"] = json!(
        frame
            .input
            .iter()
            .filter_map(|item| item["content"].as_str())
            .map(str::len)
            .sum::<usize>()
    );
    if frame.input.len() == 1
        && let Some(content) = frame.input[0]["content"].as_str()
    {
        provenance["escaped_prompt_bytes"] = json!(
            serde_json::to_string(content)
                .map_err(|_| "model_request_invalid")?
                .len()
        );
    }
    provenance["max_output_tokens"] = json!(role.output_cap);
    let stream_id = hash(prompt.as_bytes());
    sadr_cli::model_stream::publish(
        &frame.role,
        &role.model_id,
        &stream_id,
        "request_started",
        "Partial output is unvalidated",
    );
    let mut stream = sadr_cli::model_stream::Decoder::new(
        &frame.role,
        &role.model_id,
        &stream_id,
        ["access_token", "refresh_token", "id_token"]
            .iter()
            .filter_map(|k| g[*k].as_str().map(str::to_owned))
            .collect(),
    );
    let mut observer = |bytes: &[u8]| stream.bytes(bytes);
    provenance["stage"] = json!("transport");
    let deadline = c.deadline.ok_or("model_timeout")?;
    let response = retry_model_429(
        deadline,
        || {
            let timeout_ms = deadline
                .checked_duration_since(Instant::now())
                .map(|remaining| remaining.as_millis() as u64)
                .filter(|remaining| *remaining > 0)
                .ok_or_else(|| "model_timeout".to_owned())?
                .min(c.timeout_ms);
            sadr_cli::native_transport::http_observed_with_retry_after(
                timeout_ms,
                c.transport.responses_url(),
                None,
                Some(&body),
                Some(token),
                c.proxy_client_version.as_deref(),
                c.conversation_id.as_deref(),
                Some(&mut observer),
            )
            .map_err(str::to_owned)
        },
        std::thread::sleep,
    )?;
    let status = response.status;
    let bytes = response.body;
    provenance["stage"] = json!("http_response");
    provenance["http_status"] = json!(status);
    check_http(status, &bytes, token)?;
    provenance["stage"] = json!("response_validation");
    let value = subscription_response_with_evidence(status, &bytes, token, provenance)?;
    if let Some(received) = value["model"].as_str() {
        provenance["received_model_sha256"] = json!(hash(received.as_bytes()));
        provenance["response_model"] = json!(safe_text(received, token, 128));
    }
    if value["model"] != role.model_id {
        return Err("planner_response_model_mismatch".into());
    }
    record_usage(&value, provenance);
    stream.final_output(&value);
    sadr_cli::model_stream::publish(
        &frame.role,
        &role.model_id,
        &stream_id,
        "response_complete_unvalidated",
        "Provider response completed; host validation pending",
    );
    provenance["stage"] = json!("output_validation");
    let output = value["output"].as_array().ok_or("planner_output_invalid")?;
    let mut text = serde_json::to_string(
        &json!({"output":output,"model":value["model"],"usage":value["usage"]}),
    )
    .map_err(|_| "harness_output_invalid")?;
    for field in ["access_token", "refresh_token", "id_token"] {
        if let Some(secret) = g[field].as_str().filter(|s| !s.is_empty()) {
            text = text.replace(secret, "[redacted]");
        }
    }
    if let Some(code) = role.provider_envelope_code(&text) {
        return Err(code.into());
    }
    Ok(text)
}
