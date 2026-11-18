//! Credential-like keys the redactor drops and the bounded identifier shape
//! shared by retention and lane memory.

pub(super) fn secret_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "authorization"
            | "token"
            | "access_token"
            | "refresh_token"
            | "credentials"
            | "apitoken"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "id_token"
            | "client_secret"
            | "private_key"
            | "authorization_header"
            | "auth_state"
            | "api_key"
            | "apikey"
            | "api-key"
            | "x-api-key"
            | "secret"
            | "password"
            | "cookie"
            | "set-cookie"
            | ".env"
    )
}
pub(super) fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && !id.starts_with('-')
}
