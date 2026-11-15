//! Explicit independent subscription login and verification only.
fn now() -> chrono::DateTime<chrono::FixedOffset> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    chrono::DateTime::from_timestamp(duration.as_secs() as i64, duration.subsec_nanos())
        .expect("clock range")
        .fixed_offset()
}

use clap::Subcommand;
use sadr_cli::native_transport::Transport;
use sadr_core::{
    native,
    native_grant::{CLIENT, GrantRecord, SCOPE},
};
use serde::Deserialize;
/// Catalog display suggestion; configured role profiles own inference admission.
pub(crate) const LUNA: &str = "gpt-6-luna";
/// Refused paid/fast variant. Config/profile admission never overrides this check.
pub(crate) const FAST_VARIANT: &str = "grok-4.7-build-fast";
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;
#[derive(Subcommand)]
pub enum Command {
    /// Device login. An expired grant is moved aside to a timestamped backup first;
    /// an unexpired grant is preserved and refused.
    Login {
        #[arg(long)]
        config: PathBuf,
        /// Accepted for compatibility with existing login scripts; ignored.
        #[arg(long)]
        confirm_paper_review: bool,
    },
    List {
        #[arg(long)]
        config: PathBuf,
    },
    /// Make exactly one inference request; never starts a paper session.
    Probe {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        model: String,
        #[arg(long, value_enum, default_value = "none")]
        tool: Tool,
        /// Explicit source-backed request shape for diagnostics; never a fallback.
        #[arg(long, value_enum, default_value = "minimal")]
        request_shape: ProbeShape,
        /// Explicit diagnostic context; never generated or stored by the application.
        #[arg(long)]
        conversation_id: Option<String>,
    },
    /// Make exactly one minimal request to a configured limit fallback
    /// (#245) to prove its key and model; never starts a paper session.
    ProbeFallback {
        /// Fallback credential config (the profile's `model_config`).
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        model: String,
        #[arg(long)]
        reasoning_effort: Option<String>,
    },
    Status {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Show only non-secret application grant/account identity hashes.
        #[arg(long)]
        show_identity: bool,
    },
    /// Switch one planner role to a model the reviewed policy already declares.
    /// Writes a new planner config and adopts it through the reviewed apply path;
    /// never switches a role while a paper session is open.
    Select {
        /// Paper config file whose planner config declares the switch.
        #[arg(long)]
        config: PathBuf,
        #[arg(long, value_enum)]
        role: crate::runtime::model_select::Role,
        /// Admitted profile name from the planner config.
        #[arg(long)]
        profile: Option<String>,
        /// Declared model id; requires `--transport` instead of `--profile`.
        #[arg(long)]
        model_id: Option<String>,
        /// Transport for `--model-id`; must match the declared profile.
        #[arg(long, value_parser = ["subscription_proxy", "direct_oauth", "openai_codex"])]
        transport: Option<String>,
        /// Confirm the operator reviewed the paper book; required to apply.
        #[arg(long)]
        confirm_paper_review: bool,
        /// Print the planner diff, grant and catalog preflight; change nothing.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
}
#[derive(Clone, Copy, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeShape {
    Minimal,
    HermesText,
}
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Tool {
    None,
    #[value(name = "x_search")]
    XSearch,
    #[value(name = "web_search")]
    WebSearch,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default)]
    alert_webhook_file: Option<PathBuf>,
    #[serde(default)]
    transport: Transport,
    #[serde(default)]
    #[allow(dead_code)] // parse-compatible unused leftover from removed approval files
    direct_oauth_approval_file: Option<PathBuf>,
    #[serde(default)]
    proxy_client_version: Option<String>,
    #[serde(skip)]
    deadline: Option<Instant>,
    #[serde(skip)]
    conversation_id: Option<String>,
    #[serde(default = "default_grant")]
    grant_file: PathBuf,
    #[serde(default, rename = "model_id")]
    _model_id: String,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    #[serde(default, rename = "enabled")]
    _enabled: bool,
}
fn default_timeout() -> u64 {
    20000
}
fn default_grant() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config/ind-sadr/xai-grant")
}
fn config(path: &Path) -> Result<Config> {
    let bytes = crate::read_bounded(path, 16384).map_err(|_| "model_config_unreadable")?;
    let c: Config = serde_json::from_slice(&bytes).map_err(|_| "model_config_invalid")?;
    if !(1..=120000).contains(&c.timeout_ms) {
        return Err("model_timeout_invalid".into());
    }
    if c.transport == Transport::OpenaiCodex && c.grant_file == default_grant() {
        return Err("openai_grant_path_required".into());
    }
    sadr_store::native_grant::file_path_ok(&c.grant_file).map_err(|_| "grant_path_invalid")?;
    Ok(c)
}
pub fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

mod catalog;
mod fallback;
mod grant;
mod probe;
pub(crate) use catalog::*;
pub(crate) use fallback::*;
pub(crate) use grant::*;
pub(crate) use probe::*;

pub fn entry(command: Command) -> ExitCode {
    // The planner switch owns its own config preflight, writes and refusal codes.
    let command = match command {
        Command::Select {
            config: path,
            role,
            profile,
            model_id,
            transport,
            dry_run,
            confirm_paper_review,
            json,
        } => {
            let transport = match transport.as_deref() {
                Some("subscription_proxy") => Some(Transport::SubscriptionProxy),
                Some("direct_oauth") => Some(Transport::DirectOauth),
                Some("openai_codex") => Some(Transport::OpenaiCodex),
                Some(_) => {
                    eprintln!("model_select_transport_unknown");
                    return ExitCode::FAILURE;
                }
                None => None,
            };
            return crate::runtime::model_select::entry(&crate::runtime::model_select::Request {
                path: &path,
                role,
                profile: profile.as_deref(),
                model_id: model_id.as_deref(),
                transport,
                dry_run,
                confirm: confirm_paper_review,
                json,
            });
        }
        Command::ProbeFallback {
            config,
            model,
            reasoning_effort,
        } => {
            return match probe_fallback(&config, &model, reasoning_effort.as_deref()) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            };
        }
        command => command,
    };
    let result: Result<()> = (|| {
        let c = match &command {
            Command::Login { config: path, .. }
            | Command::List { config: path }
            | Command::Probe { config: path, .. } => config(path)?,
            Command::Status {
                config: Some(path), ..
            }
            | Command::Select { config: path, .. } => config(path)?,
            Command::ProbeFallback { .. } => return Ok(()),
            Command::Status { config: None, .. } => Config {
                alert_webhook_file: None,
                transport: Transport::default(),
                direct_oauth_approval_file: None,
                proxy_client_version: None,
                deadline: None,
                conversation_id: None,
                grant_file: default_grant(),
                _model_id: String::new(),
                timeout_ms: default_timeout(),
                _enabled: false,
            },
        };
        match command {
            // Handled before this closure: these report their own codes.
            Command::Select { .. } | Command::ProbeFallback { .. } => {}
            Command::Login { .. } => {
                if let Some(backup) = retire_expired_grant(&c.grant_file, now())? {
                    eprintln!("expired_grant_moved_aside {}", backup.display());
                }
                login(&c)?;
            }
            Command::List { .. } => {
                let owner = sadr_store::native_grant::FileOwner::acquire(&c.grant_file)
                    .map_err(|_| "grant_busy_or_unavailable")?;
                let g = grant(&c, &owner)?;
                let ids = catalog(&c, g["access_token"].as_str().ok_or("grant_invalid")?)?;
                if c.transport == Transport::DirectOauth {
                    println!(
                        "{}",
                        json!({"transport":c.transport,"origin":c.transport.models_url(),"provider_listed":ids})
                    );
                } else if c.transport == Transport::OpenaiCodex {
                    println!(
                        "{}",
                        json!({"transport":c.transport,"origin":c.transport.models_url(),"provider_listed":ids,"data_model":{"id":LUNA,"provider_listed":ids.iter().any(|id|id==LUNA)},"entitlement":"not_proven_by_catalog"})
                    );
                } else {
                    for id in ids {
                        println!("{id}");
                    }
                }
            }
            Command::Probe {
                model,
                tool,
                request_shape,
                conversation_id,
                ..
            } => probe(&c, &model, tool, request_shape, conversation_id)?,
            Command::Status { show_identity, .. } => {
                if !c.grant_file.exists() {
                    println!("not_logged_in");
                } else {
                    let bytes = if c.transport == Transport::OpenaiCodex {
                        sadr_store::native_grant::read_openai_file(&c.grant_file)
                    } else {
                        sadr_store::native_grant::read_file(&c.grant_file)
                    }
                    .map_err(|_| "grant_unavailable")?;
                    let state = if c.transport == Transport::OpenaiCodex {
                        sadr_core::openai_grant::GrantRecord::parse(&bytes)
                            .map_err(|_| "grant_invalid")?
                            .status(now())
                            .state
                    } else {
                        GrantRecord::parse(&bytes)
                            .map_err(|_| "grant_invalid")?
                            .status(now())
                            .state
                    };
                    if show_identity {
                        let grant = native::decode(&bytes).map_err(|_| "grant_invalid")?;
                        println!(
                            "{}",
                            json!({"state":state,"account_hash":grant["account_hash"],"grant_id":grant["grant_id"],"transport":c.transport,"entitlement":"not_proven_by_login"})
                        );
                    } else {
                        println!("{state}");
                    }
                }
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

// Redact before truncating: neither a clipped credential nor terminal escapes may leak.
fn safe_text(raw: &str, token: &str, limit: usize) -> String {
    let raw = if token.is_empty() {
        raw.to_owned()
    } else {
        raw.replace(token, "[redacted]")
    };
    let mut hide_next = false;
    let mut words = Vec::new();
    for word in raw.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        let label = [
            "token",
            "bearer",
            "authorization",
            "secret",
            "api_key",
            "api-key",
            "password",
            "credential",
            "grant",
        ]
        .iter()
        .any(|s| lower.contains(s));
        let suspicious = word.len() >= 24
            || word.contains("eyJ")
            || lower.contains("xai-")
            || lower.contains("sk-")
            || word.contains('=')
            || word.contains('@');
        words.push(if hide_next || label || suspicious {
            "[redacted]".to_owned()
        } else {
            word.chars()
                .filter(|c| {
                    !c.is_control()
                        && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
                .collect()
        });
        hide_next = label;
    }
    words.join(" ").chars().take(limit).collect()
}
fn check_http(status: u16, bytes: &[u8], token: &str) -> Result<()> {
    check_http_secrets(status, bytes, &[token])
}
fn check_http_secrets(status: u16, bytes: &[u8], secrets: &[&str]) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(http_error(status, bytes, secrets))
}
fn http_error(status: u16, bytes: &[u8], secrets: &[&str]) -> String {
    // Only provider error fields, never the raw body, headers, account or request.
    let value: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    let error = &value["error"];
    let fields = [
        error.get("code"),
        error.get("type"),
        error.get("message"),
        error.as_str().map(|_| error),
        value.get("code"),
        value.get("message"),
        value.get("error_description"),
    ];
    let detail = fields
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let detail = secrets
        .iter()
        .filter(|s| !s.is_empty())
        .fold(detail, |text, secret| text.replace(secret, "[redacted]"));
    let detail = safe_text(&detail, "", 300);
    if detail.is_empty() {
        format!("model_http_{status}")
    } else {
        format!("model_http_{status} {detail}")
    }
}

#[cfg(test)]
mod fallback_tests;
#[cfg(test)]
mod grant_retire_tests;
#[cfg(test)]
mod verification_tests;

#[cfg(test)]
mod refresh_readiness_tests;
