use serde::{Deserialize, Serialize};

pub const MUSL_TARGET: &str = "x86_64-unknown-linux-musl";
pub const MUSL_BINARY_ASSET: &str = "nivra-x86_64-unknown-linux-musl";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub sha256: String,
    pub source_commit: String,
    pub tag: String,
    pub target: String,
}
