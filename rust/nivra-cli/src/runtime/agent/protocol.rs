//! JSONL messages between Rust and the agent; `agent/src/protocol.ts` mirrors them.
use super::{Lane, ModelSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SessionRef {
    pub date: String,
    pub dir: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ToAgent<'a> {
    Turn {
        v: u32,
        id: &'a str,
        lane: Lane,
        session: &'a SessionRef,
        model: &'a ModelSpec,
        trigger: &'a Value,
        brief: &'a Value,
        tools: &'a [Value],
    },
    ToolResult {
        v: u32,
        turn_id: &'a str,
        call_id: &'a str,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<&'a Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'a str>,
    },
    Abort {
        v: u32,
        turn_id: &'a str,
        reason: &'a str,
    },
    Shutdown {
        v: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnStatus {
    Ok,
    Error,
    Aborted,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_usd: f64,
    pub calls: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct TurnOutput {
    pub text: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum FromAgent {
    Ready {
        v: u32,
        pi_version: String,
        node: String,
        pid: u32,
    },
    ToolCall {
        v: u32,
        turn_id: String,
        call_id: String,
        name: String,
        args: Value,
    },
    Event {
        v: u32,
        turn_id: String,
        event: Value,
    },
    TurnResult {
        v: u32,
        id: String,
        status: TurnStatus,
        #[serde(default)]
        error: Option<String>,
        output: TurnOutput,
        usage: Usage,
        model: String,
        provider: String,
        session_file: Option<String>,
        prompt_sha256: String,
    },
    Error {
        v: u32,
        error: String,
    },
}

impl FromAgent {
    pub(crate) fn version(&self) -> u32 {
        match self {
            Self::Ready { v, .. }
            | Self::ToolCall { v, .. }
            | Self::Event { v, .. }
            | Self::TurnResult { v, .. }
            | Self::Error { v, .. } => *v,
        }
    }
}
