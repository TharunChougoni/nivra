use super::*;

// Refresh is fenced durably before sending a rotating credential. An ambiguous
// failure leaves `refreshing`, requiring operator review rather than another attempt.
pub(crate) fn grant_with_observer<F>(
    owner: &nivra_store::native_grant::FileOwner,
    at: chrono::DateTime<chrono::FixedOffset>,
    send: F,
    failed: impl FnMut(Value),
) -> Result<Value>
where
    F: FnMut(&str, Option<&[(&str, &str)]>, Option<&Value>, Option<&str>) -> Result<(u16, Vec<u8>)>,
{
    let bytes = owner.read().map_err(|_| "grant_unavailable")?;
    refresh_with(
        bytes,
        at,
        send,
        |old, new| owner.replace(old, new).map_err(|_| ()),
        failed,
    )
}
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
pub(crate) fn grant(c: &Config, owner: &nivra_store::native_grant::FileOwner) -> Result<Value> {
    if c.transport == Transport::OpenaiCodex {
        return grant_openai(c, owner);
    }
    let value = grant_with_observer(
        owner,
        now(),
        |url, form, body, token| request(c, url, form, body, token),
        |details| {
            let alerts = crate::alert::Alerts::start(c.alert_webhook_file.as_deref(), false);
            alerts.emit(
                crate::alert::Event::ModelRefreshFailed,
                now().date_naive(),
                details,
            );
            alerts.finish();
        },
    )?;
    let record = GrantRecord::parse(&serde_json::to_vec(&value).map_err(|_| "grant_invalid")?)
        .map_err(|_| "grant_invalid")?;
    if record.status(now()).state != "authenticated" {
        return Err("grant_expired_during_refresh".into());
    }
    Ok(value)
}
pub(crate) fn grant_openai(
    c: &Config,
    owner: &nivra_store::native_grant::FileOwner,
) -> Result<Value> {
    let at = now();
    let bytes = owner.read_openai().map_err(|_| "grant_unavailable")?;
    nivra_core::openai_grant::GrantRecord::parse(&bytes).map_err(|_| "grant_invalid")?;
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
    let refreshing = refresh_pending(&value, at)?;
    let pending = serde_json::to_vec(&refreshing).map_err(|_| "grant_invalid")?;
    owner
        .replace_openai(&bytes, &pending)
        .map_err(|_| "grant_store_failed")?;
    let access = value["access_token"].as_str().ok_or("grant_invalid")?;
    let refresh = value["refresh_token"].as_str().ok_or("grant_invalid")?;
    let (status, response) = request(
        c,
        nivra_cli::native_transport::OPENAI_OAUTH_TOKEN,
        Some(&[
            ("grant_type", "refresh_token"),
            ("client_id", nivra_core::openai_grant::CLIENT),
            ("refresh_token", refresh),
        ]),
        None,
        None,
    )?;
    check_http_secrets(status, &response, &[access, refresh])?;
    value = refresh_rotation(&value, refreshing, &response, at, |bytes| {
        nivra_core::openai_grant::GrantRecord::parse(bytes)
            .map(|_| ())
            .map_err(|_| ())
    })?;
    let access = value["access_token"].as_str().ok_or("grant_invalid")?;
    let identity = nivra_cli::native_transport::codex_identity(access)?;
    if value["account_hash"].as_str() != Some(hash(identity.account_id.as_bytes()).as_str()) {
        return Err("account_identity_changed".into());
    }
    let rotated = serde_json::to_vec(&value).map_err(|_| "grant_invalid")?;
    owner
        .replace_openai(&pending, &rotated)
        .map_err(|_| "grant_store_failed")?;
    let record =
        nivra_core::openai_grant::GrantRecord::parse(&rotated).map_err(|_| "grant_invalid")?;
    if record.status(now()).state != "authenticated" {
        return Err("grant_expired_during_refresh".into());
    }
    Ok(value)
}
pub(crate) fn request(
    c: &Config,
    url: &str,
    form: Option<&[(&str, &str)]>,
    body: Option<&Value>,
    token: Option<&str>,
) -> Result<(u16, Vec<u8>)> {
    let timeout_ms = if let Some(deadline) = c.deadline {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("model_timeout")?
            .as_millis();
        if remaining == 0 {
            return Err("model_timeout".into());
        }
        c.timeout_ms.min(remaining as u64)
    } else {
        c.timeout_ms
    };
    nivra_cli::native_transport::http(
        timeout_ms,
        url,
        form,
        body,
        token,
        c.proxy_client_version.as_deref(),
        c.conversation_id.as_deref(),
    )
    .map_err(str::to_owned)
}

pub(crate) const MODEL_429_MAX_RETRIES: u32 = 3;
/// One bounded retry for a malformed or incomplete response body. Schema,
/// authority, credential and deadline failures are never retried.
pub(crate) const MODEL_BODY_MAX_RETRIES: u32 = 1;

pub(crate) fn transient_model_body_error(error: &str) -> bool {
    matches!(
        error,
        "model_body_invalid" | "model_response_invalid" | "model_response_incomplete"
    )
}

pub(crate) fn parse_retry_after(
    value: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let retry_at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let wait = retry_at
        .with_timezone(&chrono::Utc)
        .signed_duration_since(now);
    if wait <= chrono::Duration::zero() {
        Some(Duration::ZERO)
    } else {
        wait.to_std().ok()
    }
}

pub(crate) fn model_429_backoff(
    retry_after: Option<&str>,
    retry_index: u32,
    now: chrono::DateTime<chrono::Utc>,
) -> Duration {
    retry_after
        .and_then(|value| parse_retry_after(value, now))
        .unwrap_or_else(|| Duration::from_millis(250_u64 << retry_index.min(3)))
}

pub(crate) fn retry_model_429<F, W>(
    deadline: Instant,
    mut send: F,
    mut wait: W,
) -> Result<nivra_cli::native_transport::HttpResponse>
where
    F: FnMut() -> Result<nivra_cli::native_transport::HttpResponse>,
    W: FnMut(Duration),
{
    let mut retries_429 = 0u32;
    let mut retries_body = 0u32;
    loop {
        let response = match send() {
            Ok(response) => response,
            Err(error)
                if retries_body < MODEL_BODY_MAX_RETRIES && transient_model_body_error(&error) =>
            {
                retries_body += 1;
                let delay = Duration::from_millis(250 * u64::from(retries_body));
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return Err(error);
                };
                if delay >= remaining {
                    return Err(error);
                }
                wait(delay);
                continue;
            }
            Err(error) => return Err(error),
        };
        if response.status != 429 || retries_429 == MODEL_429_MAX_RETRIES {
            return Ok(response);
        }
        let delay = model_429_backoff(
            response.retry_after.as_deref(),
            retries_429,
            now().with_timezone(&chrono::Utc),
        );
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(response);
        };
        if delay >= remaining {
            return Ok(response);
        }
        wait(delay);
        retries_429 += 1;
        if Instant::now() >= deadline {
            return Ok(response);
        }
    }
}

/// Moves an expired grant aside so login can replace it; never touches a live grant.
pub(crate) fn retire_expired_grant(
    path: &Path,
    at: chrono::DateTime<chrono::FixedOffset>,
) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let value: Value =
        serde_json::from_slice(&std::fs::read(path).map_err(|_| "grant_unreadable")?)
            .map_err(|_| "grant_exists_preserve_and_review")?;
    let expires = value["expires_at"]
        .as_str()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .ok_or("grant_exists_preserve_and_review")?;
    if expires > at {
        return Err("grant_exists_preserve_and_review".into());
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("grant_path_invalid")?;
    let backup = path.with_file_name(format!("{name}.expired-{}", at.timestamp()));
    if backup.exists() {
        return Err("grant_backup_exists".into());
    }
    std::fs::rename(path, &backup).map_err(|_| "grant_backup_failed")?;
    Ok(Some(backup))
}
pub(crate) fn login(c: &Config) -> Result<()> {
    if c.transport == Transport::OpenaiCodex {
        return login_openai(c);
    }
    if c.grant_file.exists() {
        return Err("grant_exists_preserve_and_review".into());
    }
    let (status, bytes) = request(
        c,
        "https://auth.x.ai/.well-known/openid-configuration",
        None,
        None,
        None,
    )?;
    nivra_core::discovery::validate(status, &bytes).map_err(|_| "discovery_changed")?;
    let (status, bytes) = request(
        c,
        "https://auth.x.ai/oauth2/device/code",
        Some(&[("client_id", CLIENT), ("scope", SCOPE)]),
        None,
        None,
    )?;
    if status != 200 {
        return Err("device_authorization_failed".into());
    }
    let v = native::decode(&bytes).map_err(|_| "device_challenge_invalid")?;
    let display = json!({"verification_uri":v["verification_uri"],"user_code":v["user_code"],"expires_in":v["expires_in"]});
    let challenge = nivra_core::device_challenge::DeviceChallenge::parse(
        &serde_json::to_vec(&display).map_err(|_| "device_challenge_invalid")?,
    )
    .map_err(|_| "device_challenge_invalid")?;
    let code = v["device_code"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 16384)
        .ok_or("device_code_invalid")?;
    let mut interval = v
        .get("interval")
        .map_or(Some(5), Value::as_u64)
        .filter(|v| (1..=1800).contains(v))
        .ok_or("device_interval_invalid")?;
    println!(
        "{}\n{}\n{}",
        challenge.verification_uri(),
        challenge.user_code(),
        challenge.expires_in_seconds()
    );
    let deadline = Instant::now() + Duration::from_secs(challenge.expires_in_seconds().into());
    loop {
        if Instant::now() + Duration::from_secs(interval) >= deadline {
            return Err("device_code_expired".into());
        }
        std::thread::sleep(Duration::from_secs(interval));
        let (status, bytes) = request(
            c,
            "https://auth.x.ai/oauth2/token",
            Some(&[
                ("client_id", CLIENT),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", code),
            ]),
            None,
            None,
        )?;
        let token = native::decode(&bytes).map_err(|_| "token_response_invalid")?;
        if status != 200 {
            match token["error"].as_str() {
                Some("authorization_pending") => continue,
                Some("slow_down") => {
                    interval += 5;
                    continue;
                }
                _ => return Err("device_login_denied".into()),
            }
        }
        let now = now();
        let metadata = nivra_core::token_envelope::validate(&bytes, now)
            .map_err(|_| "token_response_invalid")?;
        let access = token["access_token"]
            .as_str()
            .ok_or("token_response_invalid")?;
        let (status, user) = request(
            c,
            "https://auth.x.ai/oauth2/userinfo",
            None,
            None,
            Some(access),
        )?;
        if status != 200 {
            return Err("account_identity_unavailable".into());
        }
        let user = native::decode(&user).map_err(|_| "account_identity_invalid")?;
        let sub = user["sub"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("account_identity_invalid")?;
        let account = hash(sub.as_bytes());
        let id = hash(format!("{}:{now}", account).as_bytes());
        let record = json!({"state":"authenticated","grant_id":&id[..32],"revision":1,"login_attempts":1,"updated_at":now,"expires_at":metadata.expires_at,"account_hash":account,"access_token":token["access_token"],"refresh_token":token["refresh_token"]});
        return nivra_store::native_grant::create_file(
            &c.grant_file,
            &serde_json::to_vec(&record).map_err(|_| "grant_invalid")?,
        )
        .map_err(|_| "grant_store_failed".to_owned());
    }
}
pub(crate) fn login_openai(c: &Config) -> Result<()> {
    if c.grant_file.exists() {
        return Err("grant_exists_preserve_and_review".into());
    }
    let (status, bytes) = request(
        c,
        nivra_cli::native_transport::OPENAI_DEVICE_CODE,
        None,
        Some(&json!({"client_id": nivra_core::openai_grant::CLIENT})),
        None,
    )?;
    if status != 200 {
        return Err("device_authorization_failed".into());
    }
    let v = native::decode(&bytes).map_err(|_| "device_challenge_invalid")?;
    let user_code = v["user_code"]
        .as_str()
        .filter(|s| (4..=64).contains(&s.len()))
        .ok_or("device_challenge_invalid")?;
    let device_auth_id = v["device_auth_id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 256 && s.bytes().all(|b| (33..=126).contains(&b)))
        .ok_or("device_code_invalid")?;
    let interval = v
        .get("interval")
        .map_or(Some(5), |value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
        .filter(|value| (1..=1800).contains(value))
        .ok_or("device_interval_invalid")?;
    let expires = v
        .get("expires_in")
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        })
        .filter(|value| (1..=1800).contains(value))
        .unwrap_or(900);
    let display = json!({"verification_uri":nivra_cli::native_transport::OPENAI_DEVICE_URI,"user_code":user_code,"expires_in":expires});
    let challenge = nivra_core::device_challenge::DeviceChallenge::parse(
        &serde_json::to_vec(&display).map_err(|_| "device_challenge_invalid")?,
    )
    .map_err(|_| "device_challenge_invalid")?;
    println!(
        "{}\n{}\n{}",
        challenge.verification_uri(),
        challenge.user_code(),
        challenge.expires_in_seconds()
    );
    let deadline = Instant::now() + Duration::from_secs(challenge.expires_in_seconds().into());
    loop {
        if Instant::now() + Duration::from_secs(interval) >= deadline {
            return Err("device_code_expired".into());
        }
        std::thread::sleep(Duration::from_secs(interval));
        let (status, bytes) = request(
            c,
            nivra_cli::native_transport::OPENAI_DEVICE_TOKEN,
            None,
            Some(&json!({"device_auth_id":device_auth_id,"user_code":user_code})),
            None,
        )?;
        if matches!(status, 403 | 404) {
            continue;
        }
        if status != 200 {
            return Err("device_login_denied".into());
        }
        let pending = native::decode(&bytes).map_err(|_| "token_response_invalid")?;
        let code = printable(pending["authorization_code"].as_str())?;
        let verifier = printable(pending["code_verifier"].as_str())?;
        let (status, bytes) = request(
            c,
            nivra_cli::native_transport::OPENAI_OAUTH_TOKEN,
            Some(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                (
                    "redirect_uri",
                    "https://auth.openai.com/deviceauth/callback",
                ),
                ("client_id", nivra_core::openai_grant::CLIENT),
                ("code_verifier", verifier),
            ]),
            None,
            None,
        )?;
        if status != 200 {
            return Err("token_response_invalid".into());
        }
        let now = now();
        let metadata = nivra_core::token_envelope::validate_codex(&bytes, now)
            .map_err(|_| "token_response_invalid")?;
        let token = native::decode(&bytes).map_err(|_| "token_response_invalid")?;
        let access = token["access_token"]
            .as_str()
            .ok_or("token_response_invalid")?;
        let identity = nivra_cli::native_transport::codex_identity(access)?;
        let account = hash(identity.account_id.as_bytes());
        let id = hash(format!("{account}:{now}").as_bytes());
        let record = json!({"version":"ind-sadr-openai-grant-v1","issuer":"https://auth.openai.com","client_id":nivra_core::openai_grant::CLIENT,"scope":"openai-codex-device","state":"authenticated","grant_id":&id[..32],"revision":1,"login_attempts":1,"updated_at":now,"expires_at":metadata.expires_at,"account_hash":account,"access_token":token["access_token"],"refresh_token":token["refresh_token"]});
        return nivra_store::native_grant::create_openai_file(
            &c.grant_file,
            &serde_json::to_vec(&record).map_err(|_| "grant_invalid")?,
        )
        .map_err(|_| "grant_store_failed".to_owned());
    }
}
pub(crate) fn printable(value: Option<&str>) -> Result<&str> {
    value
        .filter(|text| {
            !text.is_empty() && text.len() <= 16384 && text.bytes().all(|b| (33..=126).contains(&b))
        })
        .ok_or("token_response_invalid".into())
}
/// Read-only preflight for a configured model role. It validates the application's
/// own grant through the supplied work deadline; it never refreshes credentials or
/// contacts a provider.
pub(crate) fn readiness(
    model_config: &Path,
    expected_transport: Transport,
    _expected_model: &str,
    at: chrono::DateTime<chrono::FixedOffset>,
    deadline: chrono::DateTime<chrono::FixedOffset>,
) -> Result<Value> {
    let c = config(model_config)?;
    if c.transport != expected_transport {
        return Err("model_transport_mismatch".into());
    }
    let bytes = if c.transport == Transport::OpenaiCodex {
        nivra_store::native_grant::read_openai_file(&c.grant_file)
    } else {
        nivra_store::native_grant::read_file(&c.grant_file)
    }
    .map_err(|_| "grant_unavailable")?;
    if c.transport == Transport::OpenaiCodex {
        nivra_core::openai_grant::GrantRecord::parse(&bytes).map_err(|_| "grant_invalid")?;
    } else {
        GrantRecord::parse(&bytes).map_err(|_| "grant_invalid")?;
    }
    let g = native::decode(&bytes).map_err(|_| "grant_invalid")?;
    if g["state"] != "authenticated" {
        return Err("grant_login_required".into());
    }
    let expires =
        chrono::DateTime::parse_from_rfc3339(g["expires_at"].as_str().ok_or("grant_invalid")?)
            .map_err(|_| "grant_invalid")?;
    let grant_ready = expires > deadline
        || (c.transport != Transport::OpenaiCodex && verified_refresh_covers(&g, at));
    if matches!(c.transport, Transport::DirectOauth | Transport::OpenaiCodex) {
        Ok(
            json!({"status":if grant_ready {"validated"} else {"unknown"},"required_until":deadline,
            "reason":if grant_ready {Value::Null} else {json!("grant_refresh_required_or_expiry_unverified")},
            "grant_expires_at":expires,"grant_binding_sha256":crate::model::hash(format!("{}:{}",g["account_hash"],g["grant_id"]).as_bytes()),"refresh_owner":"same_application_grant"}),
        )
    } else {
        Ok(
            json!({"status":if grant_ready {"validated"} else {"unknown"},"grant_expires_at":expires}),
        )
    }
}
// A verified refresh is evidence of refreshability, not a promise by the provider.
// Only the current authenticated revision may supply this evidence.
pub(crate) const REFRESH_EVIDENCE_MAX_AGE: chrono::Duration = chrono::Duration::hours(36);

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

#[cfg(test)]
pub(crate) fn grant_with<F>(
    owner: &nivra_store::native_grant::FileOwner,
    at: chrono::DateTime<chrono::FixedOffset>,
    send: F,
) -> Result<Value>
where
    F: FnMut(&str, Option<&[(&str, &str)]>, Option<&Value>, Option<&str>) -> Result<(u16, Vec<u8>)>,
{
    grant_with_observer(owner, at, send, |_| {})
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
