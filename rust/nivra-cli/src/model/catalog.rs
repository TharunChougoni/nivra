use super::*;

pub(crate) fn admit_model(model: &str, listed: &[String]) -> Result<()> {
    if model == FAST_VARIANT || !listed.iter().any(|id| id == model) {
        Err("planner_model_not_in_live_catalog".into())
    } else {
        Ok(())
    }
}
pub(crate) fn catalog(c: &Config, token: &str) -> Result<Vec<String>> {
    if c.transport == Transport::OpenaiCodex {
        return codex_catalog(c, token);
    }
    let (status, bytes) = request(c, c.transport.models_url(), None, None, Some(token))?;
    check_http(status, &bytes, token)?;
    catalog_ids(subscription_catalog(&bytes)?, token)
}
pub(crate) fn subscription_catalog(bytes: &[u8]) -> Result<Vec<String>> {
    listed_ids(bytes, 262144)?.ok_or("model_list_invalid".into())
}
pub(crate) fn listed_ids(bytes: &[u8], limit: usize) -> Result<Option<Vec<String>>> {
    if bytes.len() > limit {
        return Err("model_list_invalid".into());
    }
    // The strict decoder requires an object root; wrap either upstream shape
    // without losing duplicate-key rejection within the original JSON.
    let mut wrapped = b"{\"catalog\":".to_vec();
    wrapped.extend_from_slice(bytes);
    wrapped.push(b'}');
    let root = native::decode(&wrapped).map_err(|_| "model_list_invalid")?;
    let value = &root["catalog"];
    let rows = value
        .as_array()
        .or_else(|| value["data"].as_array())
        .or_else(|| value["models"].as_array())
        .ok_or("model_list_invalid")?;
    if rows.is_empty() {
        return Ok(None);
    }
    let rows: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({"id": row.get("id").or_else(|| row.get("model")).or_else(|| row.get("slug"))})
        })
        .collect();
    native::catalog(&serde_json::to_vec(&json!({"data":rows})).map_err(|_| "model_list_invalid")?)
        .map(Some)
        .map_err(|_| "model_list_invalid".to_owned())
}
pub(crate) fn codex_catalog(c: &Config, token: &str) -> Result<Vec<String>> {
    let (status, bytes) = request(
        c,
        nivra_cli::native_transport::CODEX_MODELS,
        None,
        None,
        Some(token),
    )?;
    check_http(status, &bytes, token)?;
    if let Some(ids) = listed_ids(&bytes, 1_048_576)? {
        return catalog_ids(ids, token);
    }
    let (status, bytes) = request(
        c,
        nivra_cli::native_transport::CODEX_MODELS_LEGACY,
        None,
        None,
        Some(token),
    )?;
    check_http(status, &bytes, token)?;
    catalog_ids(
        listed_ids(&bytes, 1_048_576)?.ok_or("model_list_invalid")?,
        token,
    )
}
pub(crate) fn catalog_ids(ids: Vec<String>, token: &str) -> Result<Vec<String>> {
    if !token.is_empty() && ids.iter().any(|id| id.contains(token)) {
        return Err("model_list_invalid".into());
    }
    Ok(ids)
}
fn membership(ids: &[String], model_id: &str, transport: Transport) -> Value {
    json!({"status":if ids.iter().any(|id| id == model_id) {"listed"} else {"not_listed"},
        "model_id":model_id,"transport":transport,"catalog_size":ids.len()})
}
/// Read-only live-catalog membership for one reviewed model binding.
///
/// It reads the target's own application grant without refreshing it, never
/// prints or stores token content, and reports an unavailable precondition as a
/// status instead of raising it. Catalog presence is advisory: every real call
/// still revalidates the exact binding.
pub(crate) fn catalog_membership(
    model_config: &Path,
    expected_transport: Transport,
    model_id: &str,
) -> Value {
    let unavailable = |reason: &str| {
        json!({"status":"unavailable","reason":reason,"model_id":model_id,
            "transport":expected_transport})
    };
    let c = match config(model_config) {
        Ok(c) => c,
        Err(reason) => return unavailable(&reason),
    };
    if c.transport != expected_transport {
        return unavailable("model_transport_mismatch");
    }
    let bytes = if c.transport == Transport::OpenaiCodex {
        nivra_store::native_grant::read_openai_file(&c.grant_file)
    } else {
        nivra_store::native_grant::read_file(&c.grant_file)
    };
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(_) => return unavailable("grant_unavailable"),
    };
    let parsed = if c.transport == Transport::OpenaiCodex {
        nivra_core::openai_grant::GrantRecord::parse(&bytes)
            .map(|_| ())
            .map_err(|_| ())
    } else {
        GrantRecord::parse(&bytes).map(|_| ()).map_err(|_| ())
    };
    if parsed.is_err() {
        return unavailable("grant_invalid");
    }
    let grant = match native::decode(&bytes) {
        Ok(grant) => grant,
        Err(_) => return unavailable("grant_invalid"),
    };
    if grant["state"] != "authenticated" {
        return unavailable("grant_login_required");
    }
    let expires = grant["expires_at"]
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok());
    if expires.is_none_or(|expires| expires <= now()) {
        return unavailable("grant_expired");
    }
    let Some(token) = grant["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
    else {
        return unavailable("grant_invalid");
    };
    match catalog(&c, token) {
        Ok(ids) => membership(&ids, model_id, c.transport),
        Err(reason) => unavailable(&reason),
    }
}
