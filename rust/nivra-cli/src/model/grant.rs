use super::*;

#[cfg(test)]
pub(crate) fn refresh_with<F>(
    bytes: Vec<u8>,
    at: chrono::DateTime<chrono::FixedOffset>,
    mut send: F,
    mut replace: impl FnMut(&[u8], &[u8]) -> std::result::Result<(), ()>,
    mut failed: impl FnMut(Value),
) -> Result<Value>
where
    F: FnMut(&str, Option<&[(&str, &str)]>, Option<&Value>, Option<&str>) -> Result<(u16, Vec<u8>)>,
{
    GrantRecord::parse(&bytes).map_err(|_| "grant_invalid")?;
    let mut value = native::decode(&bytes).map_err(|_| "grant_invalid")?;
    if value["state"] != "authenticated" {
        return Err("grant_login_required".into());
    }
    let expiry =
        chrono::DateTime::parse_from_rfc3339(value["expires_at"].as_str().ok_or("grant_invalid")?)
            .map_err(|_| "grant_invalid")?;
    if expiry > at + chrono::Duration::seconds(60) {
        return Ok(value);
    }
    let result = (|| {
        let refreshing = refresh_pending(&value, at)?;
        let pending = serde_json::to_vec(&refreshing).map_err(|_| "grant_invalid")?;
        replace(&bytes, &pending).map_err(|_| "grant_store_failed")?;
        let access = value["access_token"].as_str().ok_or("grant_invalid")?;
        let refresh = value["refresh_token"].as_str().ok_or("grant_invalid")?;
        let (status, discovery) = send(
            "https://auth.x.ai/.well-known/openid-configuration",
            None,
            None,
            None,
        )?;
        check_http_secrets(status, &discovery, &[access, refresh])?;
        nivra_core::discovery::validate(status, &discovery).map_err(|_| "discovery_changed")?;
        let (status, response) = send(
            "https://auth.x.ai/oauth2/token",
            Some(&[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT),
                ("refresh_token", refresh),
            ]),
            None,
            None,
        )?;
        check_http_secrets(status, &response, &[access, refresh])?;
        value = refresh_rotation(&value, refreshing, &response, at, |bytes| {
            GrantRecord::parse(bytes).map(|_| ()).map_err(|_| ())
        })?;
        // Bind the rotated credential to the original explicit application account.
        let access = value["access_token"].as_str().ok_or("grant_invalid")?;
        let (status, user) = send(
            "https://auth.x.ai/oauth2/userinfo",
            None,
            None,
            Some(access),
        )?;
        // Account responses and auth bodies never become diagnostics.
        if status != 200 {
            return Err(format!("model_http_{status}"));
        }
        let user = native::decode(&user).map_err(|_| "account_identity_invalid")?;
        let sub = user["sub"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("account_identity_invalid")?;
        if value["account_hash"].as_str() != Some(hash(sub.as_bytes()).as_str()) {
            return Err("account_identity_changed".into());
        }
        value["refresh_verified_at"] = json!(at);
        let rotated = serde_json::to_vec(&value).map_err(|_| "grant_invalid")?;
        replace(&pending, &rotated).map_err(|_| "grant_store_failed")?;
        Ok(value)
    })();
    if result.is_err() {
        failed(
            json!({"provider":"xai","reason":"refresh_failed","action":"operator_review_required"}),
        );
    }
    result
}
#[cfg(test)]
pub(crate) fn refresh_pending(
    value: &Value,
    at: chrono::DateTime<chrono::FixedOffset>,
) -> Result<Value> {
    let mut pending = value.clone();
    if at
        < chrono::DateTime::parse_from_rfc3339(value["updated_at"].as_str().ok_or("grant_invalid")?)
            .map_err(|_| "grant_invalid")?
    {
        return Err("grant_clock_invalid".into());
    }
    for key in ["revision", "refresh_attempts"] {
        pending[key] = json!(
            value
                .get(key)
                .map_or(Some(0), Value::as_u64)
                .and_then(|n| n.checked_add(1))
                .ok_or("grant_invalid")?
        );
    }
    pending
        .as_object_mut()
        .ok_or("grant_invalid")?
        .remove("refresh_verified_at");
    pending["state"] = json!("refreshing");
    pending["updated_at"] = json!(at);
    Ok(pending)
}
#[cfg(test)]
pub(crate) fn refresh_rotation(
    old: &Value,
    mut pending: Value,
    response: &[u8],
    at: chrono::DateTime<chrono::FixedOffset>,
    accept: impl FnOnce(&[u8]) -> std::result::Result<(), ()>,
) -> Result<Value> {
    use base64::Engine;
    let token = native::decode(response).map_err(|_| "token_response_invalid")?;
    if token
        .get("token_type")
        .is_some_and(|v| !v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("bearer")))
    {
        return Err("token_response_invalid".into());
    }
    let duration_expiry = token["expires_in"].as_u64().and_then(|seconds| {
        i64::try_from(seconds)
            .ok()
            .filter(|n| *n > 0)
            .and_then(chrono::Duration::try_seconds)
            .and_then(|duration| at.checked_add_signed(duration))
    });
    // Match OpenClaw: only the access token's exp may replace expires_in,
    // never the id_token's expiry. This reads expiry metadata, not JWT authority.
    let expiry = duration_expiry
        .or_else(|| {
            let part = token["access_token"].as_str()?.split('.').nth(1)?;
            let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(part.trim_end_matches('='))
                .ok()?;
            let payload = native::decode(&payload).ok()?;
            chrono::DateTime::from_timestamp(payload["exp"].as_i64()?, 0)
                .map(|date| date.fixed_offset())
        })
        .filter(|expiry| *expiry > at)
        .ok_or("token_response_invalid")?;
    pending["access_token"] = token["access_token"].clone();
    pending["refresh_token"] = token
        .get("refresh_token")
        .filter(|v| v.as_str().is_some_and(|s| !s.trim().is_empty()))
        .unwrap_or(&old["refresh_token"])
        .clone();
    pending["expires_at"] = json!(expiry);
    pending["state"] = json!("authenticated");
    // Reuse the existing grant schema's bounds and printable credential checks.
    accept(&serde_json::to_vec(&pending).map_err(|_| "token_response_invalid")?)
        .map_err(|_| "token_response_invalid")?;
    Ok(pending)
}

/// Read-only preflight for a configured model role. It validates the application's
/// own grant through the supplied work deadline; it never refreshes credentials or
/// contacts a provider.
#[cfg(test)]
pub(crate) const REFRESH_EVIDENCE_MAX_AGE: chrono::Duration = chrono::Duration::hours(36);

#[cfg(test)]
pub(crate) fn verified_refresh_covers(
    g: &Value,
    at: chrono::DateTime<chrono::FixedOffset>,
) -> bool {
    let date = |key: &str| {
        g[key]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
    };
    g["state"] == "authenticated"
        && g["refresh_attempts"].as_u64().is_some_and(|n| n > 0)
        && date("refresh_verified_at").is_some_and(|verified| {
            Some(verified) == date("updated_at")
                && verified <= at
                && at - verified <= REFRESH_EVIDENCE_MAX_AGE
        })
}

/// Opt-in policy. Dates are IST calendar dates; no trading calendar is consulted.
#[derive(Clone, serde::Serialize, Deserialize)]
#[serde(try_from = "AutoRenewInput")]
#[allow(dead_code)] // retained so deployed planner JSON with this block still parses
pub(crate) struct AutoRenewSettings {
    pub enabled: bool,
    pub lead_hours: u32,
    pub expiry_time_ist: String,
    pub validity_days: u32,
    #[serde(skip_serializing_if = "default_owner_wait_attempts")]
    pub owner_wait_attempts: u32,
    #[serde(skip_serializing_if = "default_owner_wait_ms")]
    pub owner_wait_ms: u64,
}
pub(crate) fn owner_wait_attempts_default() -> u32 {
    120
}
pub(crate) fn owner_wait_ms_default() -> u64 {
    1000
}
pub(crate) fn default_owner_wait_attempts(v: &u32) -> bool {
    *v == owner_wait_attempts_default()
}
pub(crate) fn default_owner_wait_ms(v: &u64) -> bool {
    *v == owner_wait_ms_default()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AutoRenewInput {
    enabled: bool,
    lead_hours: u32,
    expiry_time_ist: String,
    validity_days: u32,
    #[serde(default = "owner_wait_attempts_default")]
    owner_wait_attempts: u32,
    #[serde(default = "owner_wait_ms_default")]
    owner_wait_ms: u64,
}
impl TryFrom<AutoRenewInput> for AutoRenewSettings {
    type Error = &'static str;
    fn try_from(v: AutoRenewInput) -> std::result::Result<Self, Self::Error> {
        if !(1..=30).contains(&v.validity_days)
            || v.lead_hours > 720
            || v.owner_wait_attempts > 300
            || !(1..=60_000).contains(&v.owner_wait_ms)
            || u64::from(v.owner_wait_attempts) * v.owner_wait_ms > 300_000
            || v.expiry_time_ist.len() != 5
            || chrono::NaiveTime::parse_from_str(&v.expiry_time_ist, "%H:%M").map_or(true, |time| {
                time.format("%H:%M").to_string() != v.expiry_time_ist
            })
        {
            return Err("approval_auto_renew_config_invalid");
        }
        Ok(Self {
            enabled: v.enabled,
            lead_hours: v.lead_hours,
            expiry_time_ist: v.expiry_time_ist,
            validity_days: v.validity_days,
            owner_wait_attempts: v.owner_wait_attempts,
            owner_wait_ms: v.owner_wait_ms,
        })
    }
}
