//! `GET /api/v1/agent/transcript`: one pi lane as a chat. Each pi session
//! file under `model/agent/pi/<lane>/` is read through the journal cache,
//! parsed once into [`Line`] values and folded into exchanges (one brief and
//! the reply it drew). Read only; nothing here can place or change an order.
use super::journal::{self, Journal, Project};
use super::*;
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::collections::HashMap;

/// Newest session files read per lane.
const MAX_FILES: usize = 8;
/// Newest bytes held per session file.
const FILE_WINDOW_BYTES: u64 = 4 * 1024 * 1024;
/// A turn row is stamped this long, at most, before its user message.
const TURN_LEAD_MS: i64 = 5000;

static NULL: Value = Value::Null;

/// Keeps the named fields of `from`, in the order given.
fn pick(from: &mut Map<String, Value>, keys: &[&str]) -> Map<String, Value> {
    keys.iter()
        .filter_map(|key| from.remove(*key).map(|value| ((*key).to_owned(), value)))
        .collect()
}

/// Reduces one pi session line to what the transcript shows: the system
/// message, usage, signatures and paths are dropped before the line is held.
/// Anything else becomes null and parses as [`Line::Ignored`].
fn project_line(line: Value) -> Value {
    let Value::Object(mut line) = line else {
        return Value::Null;
    };
    let kind = line.get("type").and_then(Value::as_str).unwrap_or("");
    let kept = match kind {
        "session" => pick(&mut line, &["type", "id", "timestamp"]),
        "model_change" => pick(&mut line, &["type", "provider", "modelId"]),
        "thinking_level_change" => pick(&mut line, &["type", "thinkingLevel"]),
        "compaction" => pick(&mut line, &["type"]),
        "message" => {
            let mut kept = pick(&mut line, &["type", "timestamp"]);
            let Some(Value::Object(mut message)) = line.remove("message") else {
                return Value::Null;
            };
            if !matches!(
                message.get("role").and_then(Value::as_str),
                Some("user" | "assistant" | "toolResult")
            ) {
                return Value::Null;
            }
            let mut message_kept = pick(
                &mut message,
                &[
                    "role",
                    "timestamp",
                    "stopReason",
                    "errorMessage",
                    "toolCallId",
                    "isError",
                    "durationMs",
                ],
            );
            if let Some(content) = message.remove("content") {
                message_kept.insert("content".to_owned(), strip_signatures(content));
            }
            kept.insert("message".to_owned(), Value::Object(message_kept));
            kept
        }
        _ => return Value::Null,
    };
    Value::Object(kept)
}

fn strip_signatures(content: Value) -> Value {
    let Value::Array(blocks) = content else {
        return content;
    };
    Value::Array(
        blocks
            .into_iter()
            .map(|block| match block {
                Value::Object(mut block) => Value::Object(pick(
                    &mut block,
                    &["type", "text", "thinking", "id", "name", "arguments"],
                )),
                other => other,
            })
            .collect(),
    )
}

/// One tool call the model made and, once it arrived, its result.
struct Tool<'a> {
    id: &'a str,
    name: &'a str,
    args: &'a Value,
    outcome: Option<Outcome<'a>>,
}

struct Outcome<'a> {
    text: Cow<'a, str>,
    is_error: bool,
    duration_ms: Option<u64>,
}

enum Item<'a> {
    Thinking(&'a str),
    Text(&'a str),
    Tool(Tool<'a>),
    Error(&'a str),
    Note(&'static str),
}

/// One projected session line, typed at the boundary.
enum Line<'a> {
    Session {
        id: Option<&'a str>,
        started_ms: Option<i64>,
    },
    ModelChange {
        provider: Option<&'a str>,
        model: Option<&'a str>,
    },
    ThinkingChange(Option<&'a str>),
    Compaction,
    User {
        at_ms: i64,
        text: Cow<'a, str>,
    },
    Assistant {
        at_ms: i64,
        items: Vec<Item<'a>>,
        /// The model stopped for a reason other than asking for tools.
        finished: bool,
    },
    ToolResult {
        at_ms: i64,
        call_id: &'a str,
        outcome: Outcome<'a>,
    },
    Ignored,
}

fn rfc3339_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.timestamp_millis())
}

/// The text blocks of a message body, which pi writes as a string or an array.
fn text_of(content: &Value) -> Cow<'_, str> {
    let Some(blocks) = content.as_array() else {
        return Cow::Borrowed(content.as_str().unwrap_or(""));
    };
    let mut texts = blocks
        .iter()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str());
    match (texts.next(), texts.next()) {
        (None, _) => Cow::Borrowed(""),
        (Some(only), None) => Cow::Borrowed(only),
        (Some(first), Some(second)) => {
            let mut joined = format!("{first}\n{second}");
            for text in texts {
                joined.push('\n');
                joined.push_str(text);
            }
            Cow::Owned(joined)
        }
    }
}

fn assistant_items(content: &Value) -> Vec<Item<'_>> {
    if let Some(text) = content.as_str() {
        return vec![Item::Text(text)];
    }
    let mut items = Vec::new();
    for block in content.as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("thinking") => items.extend(
                block["thinking"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .map(Item::Thinking),
            ),
            Some("text") => items.extend(
                block["text"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .map(Item::Text),
            ),
            Some("toolCall") => {
                if let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) {
                    items.push(Item::Tool(Tool {
                        id,
                        name,
                        args: block.get("arguments").unwrap_or(&NULL),
                        outcome: None,
                    }));
                }
            }
            _ => {}
        }
    }
    items
}

impl<'a> Line<'a> {
    fn parse(row: &'a Value) -> Self {
        match row["type"].as_str() {
            Some("session") => Line::Session {
                id: row["id"].as_str(),
                started_ms: row["timestamp"].as_str().and_then(rfc3339_ms),
            },
            Some("model_change") => Line::ModelChange {
                provider: row["provider"].as_str(),
                model: row["modelId"].as_str(),
            },
            Some("thinking_level_change") => Line::ThinkingChange(row["thinkingLevel"].as_str()),
            Some("compaction") => Line::Compaction,
            Some("message") => Self::parse_message(row),
            _ => Line::Ignored,
        }
    }

    fn parse_message(row: &'a Value) -> Self {
        let message = &row["message"];
        let at_ms = message["timestamp"]
            .as_i64()
            .or_else(|| row["timestamp"].as_str().and_then(rfc3339_ms))
            .unwrap_or(0);
        match message["role"].as_str() {
            Some("user") => Line::User {
                at_ms,
                text: text_of(&message["content"]),
            },
            Some("assistant") => {
                let mut items = assistant_items(&message["content"]);
                items.extend(
                    message["errorMessage"]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .map(Item::Error),
                );
                Line::Assistant {
                    at_ms,
                    items,
                    finished: message["stopReason"].as_str() != Some("toolUse"),
                }
            }
            Some("toolResult") => match message["toolCallId"].as_str() {
                Some(call_id) => Line::ToolResult {
                    at_ms,
                    call_id,
                    outcome: Outcome {
                        text: text_of(&message["content"]),
                        is_error: message["isError"].as_bool().unwrap_or(false),
                        duration_ms: message["durationMs"].as_u64(),
                    },
                },
                None => Line::Ignored,
            },
            _ => Line::Ignored,
        }
    }
}

/// One pi session file.
struct Session<'a> {
    file: &'a str,
    id: &'a str,
    started_ms: Option<i64>,
    provider: Option<&'a str>,
    model: Option<&'a str>,
    thinking: Option<&'a str>,
}

impl<'a> Session<'a> {
    /// A file without its header line (an older window) is named
    /// `<timestamp>_<id>.jsonl`.
    fn new(file: &'a str) -> Self {
        let id = file
            .strip_suffix(".jsonl")
            .and_then(|stem| stem.rsplit_once('_'))
            .map_or(file, |(_, id)| id);
        Self {
            file,
            id,
            started_ms: None,
            provider: None,
            model: None,
            thinking: None,
        }
    }
}

/// One brief and the model's reply to it.
struct Exchange<'a> {
    session: usize,
    at_ms: i64,
    ended_ms: Option<i64>,
    complete: bool,
    brief: Cow<'a, str>,
    items: Vec<Item<'a>>,
}

/// Folds ordered lines, oldest first, into sessions and exchanges.
#[derive(Default)]
struct Folder<'a> {
    sessions: Vec<Session<'a>>,
    exchanges: Vec<Exchange<'a>>,
    /// The last exchange still takes replies.
    open: bool,
    /// Tool call id to its item index in the open exchange.
    calls: HashMap<&'a str, usize>,
}

impl<'a> Folder<'a> {
    fn start_file(&mut self, file: &'a str) {
        self.sessions.push(Session::new(file));
        self.open = false;
        self.calls.clear();
    }

    fn push(&mut self, line: Line<'a>) {
        let Some(session) = self.sessions.last_mut() else {
            return;
        };
        let exchange = if self.open {
            self.exchanges.last_mut()
        } else {
            None
        };
        match line {
            Line::Session { id, started_ms } => {
                session.id = id.unwrap_or(session.id);
                session.started_ms = started_ms;
            }
            Line::ModelChange { provider, model } => {
                session.provider = provider;
                session.model = model;
            }
            Line::ThinkingChange(level) => session.thinking = level,
            Line::Compaction => {
                if let Some(exchange) = exchange {
                    exchange.items.push(Item::Note("context compacted"));
                }
            }
            Line::User { at_ms, text } => {
                self.exchanges.push(Exchange {
                    session: self.sessions.len() - 1,
                    at_ms,
                    ended_ms: None,
                    complete: false,
                    brief: text,
                    items: Vec::new(),
                });
                self.open = true;
                self.calls.clear();
            }
            Line::Assistant {
                at_ms,
                items,
                finished,
            } => {
                let Some(exchange) = exchange else { return };
                for item in items {
                    if let Item::Tool(tool) = &item {
                        self.calls.insert(tool.id, exchange.items.len());
                    }
                    exchange.items.push(item);
                }
                exchange.ended_ms = Some(at_ms);
                exchange.complete |= finished;
            }
            Line::ToolResult {
                at_ms,
                call_id,
                outcome,
            } => {
                let Some(exchange) = exchange else { return };
                let slot = self
                    .calls
                    .get(call_id)
                    .and_then(|index| exchange.items.get_mut(*index));
                if let Some(Item::Tool(tool)) = slot {
                    tool.outcome = Some(outcome);
                    exchange.ended_ms = Some(at_ms);
                }
            }
            Line::Ignored => {}
        }
    }
}

/// The longest prefix of `text` within `max` bytes that ends on a char boundary.
fn clip(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    (&text[..text.floor_char_boundary(max)], true)
}

struct Caps {
    text: usize,
    args: usize,
}

impl Caps {
    fn text(&self, text: &str) -> Value {
        let (text, truncated) = clip(text, self.text);
        json!({"text":text,"truncated":truncated})
    }

    /// Tool arguments as compact JSON text. A string value that holds JSON is
    /// parsed and compacted; any other string is shown as written.
    fn args(&self, args: &Value) -> Value {
        let rendered = match args {
            Value::String(raw) => serde_json::from_str::<Value>(raw)
                .map_or_else(|_| raw.clone(), |parsed| parsed.to_string()),
            other => other.to_string(),
        };
        let (text, truncated) = clip(&rendered, self.args);
        json!({"text":text,"truncated":truncated})
    }

    fn item(&self, item: &Item<'_>) -> Value {
        match item {
            Item::Thinking(text) => {
                let mut body = self.text(text);
                body["kind"] = json!("thinking");
                body
            }
            Item::Text(text) => {
                let mut body = self.text(text);
                body["kind"] = json!("text");
                body
            }
            Item::Error(text) => {
                let mut body = self.text(text);
                body["kind"] = json!("error");
                body
            }
            Item::Note(text) => json!({"kind":"note","text":text}),
            Item::Tool(tool) => json!({
                "kind":"tool","id":tool.id,"name":tool.name,"args":self.args(tool.args),
                "result":tool.outcome.as_ref().map(|outcome| self.text(&outcome.text)),
                "is_error":tool.outcome.as_ref().map(|outcome| outcome.is_error),
                "duration_ms":tool.outcome.as_ref().and_then(|outcome| outcome.duration_ms)
            }),
        }
    }

    fn exchange(&self, exchange: &Exchange<'_>, id: &str, turn: Option<&Value>) -> Value {
        let mut brief = self.text(&exchange.brief);
        brief["bytes"] = json!(exchange.brief.len());
        json!({
            "session":id,"at_ms":exchange.at_ms,"ended_ms":exchange.ended_ms,
            "complete":exchange.complete,"brief":brief,
            "items":exchange.items.iter().map(|item| self.item(item)).collect::<Vec<_>>(),
            "turn":turn
        })
    }
}

/// The latest turn row of `lane` stamped within [`TURN_LEAD_MS`] before `at_ms`.
fn turn_for<'t>(turns: &[&'t Value], at_ms: i64) -> Option<&'t Value> {
    turns
        .iter()
        .filter_map(|turn| Some((turn["at_ms"].as_i64()?, *turn)))
        .filter(|(turn_ms, _)| (at_ms - TURN_LEAD_MS..=at_ms).contains(turn_ms))
        .max_by_key(|(turn_ms, _)| *turn_ms)
        .map(|(_, turn)| turn)
}

fn lane_is_valid(lane: &str) -> bool {
    let mut chars = lane.bytes();
    lane.len() <= 32
        && chars.next().is_some_and(|b| b.is_ascii_lowercase())
        && chars.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn bounded(
    query: &router::Query,
    key: &str,
    range: std::ops::RangeInclusive<usize>,
    default: usize,
    reason: &'static str,
) -> std::result::Result<usize, &'static str> {
    match query.get(key) {
        None => Ok(default),
        Some(value) if value.len() <= 6 && value.bytes().all(|b| b.is_ascii_digit()) => value
            .parse()
            .ok()
            .filter(|n| range.contains(n))
            .ok_or(reason),
        Some(_) => Err(reason),
    }
}

/// Names of the newest [`MAX_FILES`] regular `*.jsonl` files directly in
/// `dir`, oldest first, and whether older ones were left out. Symlinks,
/// directories and other names are skipped. A missing directory has none.
fn session_files(dir: &Path) -> std::io::Result<(Vec<String>, bool)> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(std::io::ErrorKind::InvalidInput.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), false));
        }
        Err(error) => return Err(error),
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && let Ok(name) = entry.file_name().into_string()
            && name.ends_with(".jsonl")
        {
            names.push(name);
        }
    }
    names.sort();
    let older = names.len() > MAX_FILES;
    Ok((
        names.split_off(names.len().saturating_sub(MAX_FILES)),
        older,
    ))
}

pub(crate) fn agent_transcript(
    head: &str,
    query: router::Query,
    root: Option<&Path>,
    settings: &DashboardSettings,
) -> Response {
    let Some(root) = root else {
        return error("503 Service Unavailable", "session_unavailable");
    };
    let lane = match query.get("lane") {
        Some(lane) if lane_is_valid(lane) => lane,
        _ => return error("400 Bad Request", "transcript_lane_invalid"),
    };
    let limit = match bounded(&query, "limit", 1..=50, 20, "transcript_limit_invalid") {
        Ok(limit) => limit,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let offset = match bounded(
        &query,
        "cursor",
        0..=100_000,
        0,
        "transcript_cursor_invalid",
    ) {
        Ok(offset) => offset,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let unavailable = || error("503 Service Unavailable", "agent_transcript_unavailable");
    let agent = root.join("model").join("agent");
    let lane_dir = agent.join("pi").join(lane);
    let Ok((names, older_files)) = session_files(&lane_dir) else {
        return unavailable();
    };
    let mut files: Vec<(PathBuf, Project, u64)> = names
        .iter()
        .map(|name| {
            (
                lane_dir.join(name),
                project_line as Project,
                FILE_WINDOW_BYTES,
            )
        })
        .collect();
    files.push((
        agent.join(AGENT_FILES[0]),
        agent_turn_row as Project,
        AGENT_JOURNAL_MAX_BYTES,
    ));
    let caps = Caps {
        text: settings.transcript_text_max_bytes,
        args: settings.transcript_args_max_bytes,
    };
    let read = journal::with_journal_list(&files, |held: &[&Journal]| {
        let (turns, sessions) = held.split_last().expect("the turn journal is last");
        let mut folder = Folder::default();
        for (name, journal) in names.iter().zip(sessions) {
            folder.start_file(name);
            for row in journal.rows() {
                folder.push(Line::parse(row));
            }
        }
        let lane_turns: Vec<&Value> = turns.rows().filter(|turn| turn["lane"] == lane).collect();
        let total = folder.exchanges.len();
        let page: Vec<Value> = folder
            .exchanges
            .iter()
            .rev()
            .skip(offset)
            .take(limit)
            .map(|exchange| {
                caps.exchange(
                    exchange,
                    folder.sessions[exchange.session].id,
                    turn_for(&lane_turns, exchange.at_ms),
                )
            })
            .collect();
        let next = (offset + page.len() < total).then(|| (offset + page.len()).to_string());
        json!({
            "v":1,"lane":lane,
            "sessions":folder.sessions.iter().map(|s| json!({
                "id":s.id,"file":s.file,"started_ms":s.started_ms,
                "provider":s.provider,"model":s.model,"thinking":s.thinking
            })).collect::<Vec<_>>(),
            "exchanges":page,"total":total,"next":next,
            "truncated":older_files || sessions.iter().any(|journal| journal.not_loaded > 0),
            "execution_authorized":false
        })
    });
    match read {
        Ok(body) => json_reply(head, body),
        Err(_) => unavailable(),
    }
}
