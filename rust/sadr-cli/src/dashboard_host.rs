use serde::Deserialize;

/// Exact optional HTTPS dashboard authority; never part of a financial book binding.
#[derive(Clone, Deserialize)]
#[serde(try_from = "String")]
pub struct DashboardPublicHost(String);
impl DashboardPublicHost {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for DashboardPublicHost {
    type Error = &'static str;
    fn try_from(host: String) -> std::result::Result<Self, Self::Error> {
        if host.len() > 253
            || !host.contains('.')
            || host.bytes().all(|b| b.is_ascii_digit() || b == b'.')
            || !host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label.as_bytes()[0].is_ascii_alphanumeric()
                    && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            })
        {
            return Err("invalid_dashboard_public_host");
        }
        Ok(Self(host))
    }
}
