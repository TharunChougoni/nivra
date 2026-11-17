use serde_json::{Value, json};

/// A bounded provider failure detail retained in attempt evidence (#81).
/// `reason` is always a static allowlisted code or `model_http_<status>`;
/// any provider text after the code is discarded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderFailure {
    pub reason: String,
    pub http_status: Option<u16>,
}
const PROVIDER_FAILURE_CODES: &[&str] = &[
    "planner_call_timeout",
    "planner_output_or_timeout_cap",
    "planner_output_invalid",
    "planner_output_cap",
    "planner_response_model_mismatch",
    "model_body_invalid",
    "model_body_too_large",
    "model_response_incomplete",
    "model_response_invalid",
    "model_response_model_mismatch",
    "model_stream_invalid",
    "model_stream_incomplete",
    "model_transport_failed",
    "model_origin_refused",
    "model_request_invalid",
    "harness_output_invalid",
    "harness_citation_ids_invalid",
    "harness_final_tools_invalid",
    "harness_request_invalid",
    "harness_tool_definitions_changed",
    "harness_tool_invalid",
];
/// Map a raw model error to the bounded code stored in attempt evidence.
/// Unknown text returns `None`; callers fall back to `planner_provider_failed`.
pub(crate) fn provider_failure(text: &str) -> Option<ProviderFailure> {
    let code = text.split_whitespace().next()?;
    if code == "model_timeout" {
        return Some(ProviderFailure {
            reason: "planner_call_timeout".into(),
            http_status: None,
        });
    }
    if let Some(status) = code.strip_prefix("model_http_") {
        if status.len() != 3 || !status.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let status = status.parse::<u16>().ok()?;
        if !(100..=599).contains(&status) {
            return None;
        }
        return Some(ProviderFailure {
            reason: format!("model_http_{status}"),
            http_status: Some(status),
        });
    }
    PROVIDER_FAILURE_CODES
        .contains(&code)
        .then(|| ProviderFailure {
            reason: code.to_owned(),
            http_status: None,
        })
}
const PROVIDER_STAGES: &[&str] = &[
    "frame_decode",
    "tool_definitions",
    "application_config",
    "grant_owner",
    "subscription_approval",
    "catalog",
    "inference",
    "transport",
    "http_response",
    "response_validation",
    "output_validation",
    "provider",
];
/// Provider-reported incomplete causes retained in bounded failure evidence.
/// Unknown provider text is replaced by `other`; it is never echoed (#224).
const INCOMPLETE_REASONS: &[&str] = &["max_output_tokens", "content_filter", "other"];
/// Measured request/token sizes retained with a provider failure (#224).
const PROVIDER_SIZE_FIELDS: &[&str] = &[
    "request_bytes",
    "prompt_bytes",
    "max_output_tokens",
    "tokens_in",
    "tokens_out",
];
fn provider_stage(provider: &Value) -> Option<&'static str> {
    let stage = provider.get("stage")?.as_str()?;
    PROVIDER_STAGES
        .iter()
        .copied()
        .find(|known| *known == stage)
}
fn provider_http_status(provider: &Value) -> Option<u16> {
    let status = provider.get("http_status")?.as_u64()?;
    u16::try_from(status)
        .ok()
        .filter(|status| (100..=599).contains(status))
}
/// Provider fields belong on provider failures only. A local admission, usage or
/// budget error can occur after an earlier provider turn in the same run; copying
/// that turn's stage/status would misattribute the failure (#162/A0).
pub(crate) fn failure_evidence(status: &str, error: &str, provider: &Value) -> Value {
    if provider_failure(error).is_none() && status != "planner_provider_failed" {
        return Value::Null;
    }
    provider_failure_evidence(error, provider)
}
/// Bounded provider fields shared by planner audit and harness failure records.
/// Only allowlisted codes, stages and a numeric HTTP status pass through.
pub(crate) fn provider_failure_evidence(error: &str, provider: &Value) -> Value {
    let detail = provider_failure(error);
    let http_status = detail
        .as_ref()
        .and_then(|detail| detail.http_status)
        .or_else(|| provider_http_status(provider));
    let mut evidence = serde_json::Map::new();
    if let Some(detail) = detail {
        evidence.insert("provider_reason".into(), json!(detail.reason));
    }
    if let Some(stage) = provider_stage(provider) {
        evidence.insert("provider_stage".into(), json!(stage));
    }
    if let Some(status) = http_status {
        evidence.insert("http_status".into(), json!(status));
    }
    // A provider-incomplete response retains its allowlisted cause and the
    // measured request/token sizes so the session record explains the failure
    // without provider text (#224).
    if let Some(reason) = provider.get("incomplete_reason").and_then(Value::as_str)
        && INCOMPLETE_REASONS.contains(&reason)
    {
        evidence.insert("incomplete_reason".into(), json!(reason));
    }
    for key in PROVIDER_SIZE_FIELDS {
        if let Some(value) = provider.get(*key).and_then(Value::as_u64) {
            evidence.insert((*key).into(), json!(value));
        }
    }
    Value::Object(evidence)
}
pub(crate) fn reason(error: &(dyn std::error::Error + 'static)) -> &'static str {
    let text = error.to_string();
    match text.as_str() {
        "planner_budget_exhausted" => "planner_budget_exhausted",
        "model_plan_unavailable_no_trades" => "model_plan_unavailable_no_trades",
        super::NO_DECISION_WORK => super::NO_DECISION_WORK,
        "planner_provider_failed" => "planner_provider_failed",
        "planner_schema_invalid" | "planner_json_invalid" | "planner_plan_bounds" => {
            "planner_invalid_plan_no_entries"
        }
        "planner_config_invalid"
        | "planner_config_unreadable"
        | "planner_role_invalid"
        | "planner_model_config_required" => "planner_configuration_failed",
        "planner_event_configuration_changed" => "planner_event_configuration_changed",
        "planner_event_fill_history_changed" => "planner_event_fill_history_changed",
        "planner_event_deadline_expired" => "planner_event_deadline_expired",
        "planner_cancelled_no_replay" => "planner_cancelled_no_replay",
        "planner_data_failed_no_replay" => "planner_data_failed_no_replay",
        "planner_news_evidence_invalid" => "planner_news_evidence_invalid",
        "planner_worker_failed_no_replay" => "planner_worker_failed_no_replay",
        "planner_result_binding_invalid" => "planner_result_binding_invalid",
        "harness_deadline_expired" | "harness_turn_budget_exhausted" => "planner_budget_exhausted",
        "harness_digest_invalid"
        | "harness_digest_json_invalid"
        | "harness_digest_schema_invalid"
        | "harness_digest_bounds_invalid"
        | "luna_handoff_required"
        | "luna_handoff_invalid" => "planner_invalid_plan_no_entries",
        // A timeout keeps its existing block/status semantics; only its evidence
        // reason becomes the detailed planner_call_timeout code.
        "planner_call_timeout" | "model_timeout" => "planner_failed_no_new_entries",
        _ if provider_failure(&text).is_some() => "planner_provider_failed",
        _ => "planner_failed_no_new_entries",
    }
}
/// Status code plus the bounded underlying reason, logged and archived for every failure (#22).
#[cfg(test)]
pub(crate) fn failure_record(
    status: &str,
    error: &(dyn std::error::Error + 'static),
    latency_ms: u128,
) -> Value {
    failure_record_with_provider(status, error, latency_ms, &Value::Null)
}
/// Failure evidence with the provider stage and allowlisted reason. Unknown
/// provider text is replaced by the generic status instead of being echoed.
pub(crate) fn failure_record_with_provider(
    status: &str,
    error: &(dyn std::error::Error + 'static),
    latency_ms: u128,
    provider: &Value,
) -> Value {
    let text: String = error.to_string().chars().take(512).collect();
    let detail = provider_failure(&text);
    let reason = match &detail {
        Some(detail) => detail.reason.clone(),
        None if status == "planner_provider_failed" => status.to_owned(),
        None => text.clone(),
    };
    let mut record = json!({"status":status,"reason":reason,"latency_ms":latency_ms});
    if let Value::Object(fields) = failure_evidence(status, &text, provider) {
        for (key, value) in fields {
            record[key] = value;
        }
    }
    eprintln!(
        "planner_failure status={status} reason={:?} latency_ms={latency_ms} provider_stage={:?}",
        record["reason"].as_str().unwrap_or_default(),
        record.get("provider_stage").and_then(Value::as_str)
    );
    record
}
/// Maps an error to its status code and journals the bounded underlying reason.
pub(crate) fn logged_reason(error: &(dyn std::error::Error + 'static)) -> &'static str {
    let status = reason(error);
    let text: String = error.to_string().chars().take(512).collect();
    let text = if status == "planner_provider_failed" {
        provider_failure(&text)
            .map(|detail| detail.reason)
            .unwrap_or_else(|| status.to_owned())
    } else {
        text
    };
    eprintln!("planner_error status={status} reason={text:?}");
    status
}
