use super::*;

pub(crate) fn runs(
    head: &str,
    query: router::Query,
    source: Option<&PathBuf>,
    settings: &DashboardSettings,
) -> Response {
    use super::super::super::transcript::{RunFilter, list_runs};
    let Some(limit) = query.get("limit").map_or(Some(50), |v| {
        v.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| v.parse::<usize>().ok())
            .flatten()
            .filter(|n| (1..=100).contains(n))
    }) else {
        return error("400 Bad Request", "run_limit_invalid");
    };
    let date = match query.get("date") {
        Some(value) => match NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .ok()
            .filter(|date| date.to_string() == value)
        {
            Some(date) => Some(date),
            None => return error("400 Bad Request", "run_date_invalid"),
        },
        None => None,
    };
    if query.get("failed").is_some_and(|v| v != "1") {
        return error("400 Bad Request", "run_failed_filter_invalid");
    }
    let identity = match query.get("identity") {
        None | Some("all") => None,
        Some("digest" | "orchestrator") => query.get("identity").map(str::to_owned),
        Some(_) => return error("400 Bad Request", "run_identity_invalid"),
    };
    let filter = RunFilter {
        limit,
        cursor: query.get("cursor").map(str::to_owned),
        role: query.get("role").map(str::to_owned),
        identity,
        date,
        trigger: query.get("trigger").map(str::to_owned),
        outcome: query.get("outcome").map(str::to_owned),
        instrument: query.get("instrument").map(str::to_owned),
        failed: query.get("failed").is_some(),
    };
    let Some(source) = source else {
        if filter.cursor.is_some() {
            return error("400 Bad Request", "run_cursor_invalid");
        }
        let body = serde_json::json!({"v":1,"runs":[],"next":null,"unavailable":true}).to_string();
        let etag = format!("\"{}\"", &digest(body.as_bytes())[..16]);
        let matched = values(head, "if-none-match") == [etag.as_str()];
        let mut reply = response(
            if matched {
                "304 Not Modified"
            } else {
                "200 OK"
            },
            "application/json",
            if matched { "" } else { &body },
        );
        reply.cache = Cow::Borrowed("private, no-cache");
        reply.etag = Some(etag);
        return reply;
    };
    match list_runs(source, &filter, settings) {
        Ok(page) => {
            let matched = values(head, "if-none-match") == [page.etag.as_str()];
            let mut reply = response(
                if matched {
                    "304 Not Modified"
                } else {
                    "200 OK"
                },
                "application/json",
                if matched {
                    String::new()
                } else {
                    page.body.to_string()
                },
            );
            reply.cache = Cow::Borrowed("private, no-cache");
            reply.etag = Some(page.etag);
            reply
        }
        Err(err) if err.to_string() == "run_cursor_invalid" => {
            error("400 Bad Request", "run_cursor_invalid")
        }
        Err(_) => error("503 Service Unavailable", "runs_unavailable"),
    }
}

pub(crate) struct RunLocation {
    model: PathBuf,
    dir: Option<PathBuf>,
    transcript: Option<PathBuf>,
}
pub(crate) fn regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.nlink() == 1)
}
pub(crate) fn find_run(source: &Path, id: &str) -> Option<RunLocation> {
    if !super::super::super::model_history::valid_id(id) {
        return None;
    }
    let mut found = None;
    for model in super::super::super::model_history::model_dirs(source)? {
        let raw = model.join(id);
        let dir = fs::symlink_metadata(&raw)
            .ok()
            .filter(|m| m.is_dir())
            .map(|_| raw);
        let direct = dir.as_ref().map(|p| p.join("transcript.ndjson"));
        if direct
            .as_ref()
            .is_some_and(|p| fs::symlink_metadata(p).is_ok() && !regular(p))
        {
            return None;
        }
        let retained_dir = model.join("transcripts");
        let retained = fs::symlink_metadata(&retained_dir)
            .ok()
            .filter(|m| m.is_dir())
            .map(|_| retained_dir.join(format!("{id}.ndjson")));
        let transcript = direct
            .filter(|p| regular(p))
            .or_else(|| retained.filter(|p| regular(p)));
        if dir.is_none() && transcript.is_none() {
            continue;
        }
        if found.is_some() {
            return None;
        } // ambiguous dated identity is never guessed
        found = Some(RunLocation {
            model,
            dir,
            transcript,
        });
    }
    found
}
pub(crate) fn legacy_events(
    source: &Path,
    id: &str,
    location: &RunLocation,
    settings: &DashboardSettings,
) -> Option<Vec<serde_json::Value>> {
    use serde_json::{Value, json};
    let dir = location.dir.as_ref()?;
    let detail =
        super::super::super::model_history::run_detail_with_settings(source, id, settings)?;
    let data = &detail["run"];
    let mut events = Vec::new();
    let mut push = |kind: &str, mut value: Value| {
        value["v"] = json!(1);
        value["run"] = json!(id);
        value["seq"] = json!(events.len());
        value["at_ms"] = data["started_ms"].clone();
        value["kind"] = json!(kind);
        events.push(value);
    };
    push(
        "run_started",
        json!({"role":data["role"],"model":data["model"],"trigger":data["trigger"],"session":location.model.parent()?.file_name()?.to_str(),"mode":null,"prompt_sha":null}),
    );
    let input = ["turn-00-request.json", "input.json"]
        .into_iter()
        .find(|name| regular(&dir.join(name)));
    push(
        "input",
        json!({"turn":0,"bytes":input.and_then(|name|fs::metadata(dir.join(name)).ok().map(|m|m.len())),"summary":{},"blob":input.map(|name|if name.starts_with("turn-"){format!("{name}#/request/input")}else{name.into()})}),
    );
    for (turn_index, turn) in data["turns"].as_array().into_iter().flatten().enumerate() {
        let number = turn["turn"].as_str().and_then(|v| v.parse::<usize>().ok());
        let response = number
            .and_then(|n| {
                crate::read_bounded(
                    &dir.join(format!("turn-{n:02}-response.json")),
                    settings.history_read_max_bytes as u64,
                )
                .ok()
            })
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .unwrap_or(Value::Null);
        let calls: Vec<_> = response["output"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v["type"] == "function_call")
            .collect();
        if !turn["output"].is_null() && turn["output"] != "" {
            push(
                "assistant_text",
                json!({"turn":number,"text":turn["output"].as_str().map(str::to_owned).unwrap_or_else(||turn["output"].to_string()),"truncated":false}),
            );
        }
        for (index, tool) in turn["tools"].as_array().into_iter().flatten().enumerate() {
            let name = tool["name"].as_str().unwrap_or("unknown");
            if matches!(
                name,
                "web_search" | "web_search_call" | "x_search" | "x_search_call"
            ) {
                push(
                    "provider_tool",
                    json!({"turn":number,"name":name.strip_suffix("_call").unwrap_or(name),"action":tool["arguments"]}),
                );
                continue;
            }
            let original = calls.get(index).and_then(|v| v["call_id"].as_str());
            let call = original
                .map(str::to_owned)
                .unwrap_or_else(|| format!("legacy-{turn_index}-{index}"));
            push(
                "tool_call",
                json!({"turn":number,"call":call,"name":tool["name"],"args":tool["arguments"],"instruments":[],"legacy_call":original.is_none()}),
            );
            let pointer = original
                .map(|id| format!("tool-{}.result.json", digest(id.as_bytes())))
                .filter(|name| regular(&dir.join(name)));
            let status = match tool["result"]["status"].as_str() {
                Some("ok" | "completed") => "ok",
                Some("refused") => "refused",
                Some("failed" | "unavailable_no_retry") => "failed",
                _ => "unknown",
            };
            push(
                "tool_result",
                json!({"turn":number,"call":call,"name":tool["name"],"status":status,"duration_ms":null,"bytes":pointer.as_ref().and_then(|name|fs::metadata(dir.join(name)).ok().map(|m|m.len())),"summary":tool["result"],"blob":pointer}),
            );
        }
    }
    if !data["output"].is_null() {
        push("final_output", json!({"output":data["output"]}));
    }
    let terminal = regular(&dir.join("output.json"))
        || regular(&dir.join("failure.json"))
        || crate::read_bounded(
            &dir.join("run.json"),
            settings.history_read_max_bytes as u64,
        )
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|meta| meta["completed_ms"].as_i64().is_some());
    if terminal {
        push(
            "host_note",
            json!({"text":"Legacy, partial archive; unavailable host validation and timing fields remain unknown"}),
        );
        push(
            "run_ended",
            json!({"outcome":data["outcome"],"reason":data["reason"],"duration_ms":data["duration_ms"],"tokens_in":null,"tokens_out":null,"cost_estimate_inr":null,"cost_basis":null,"tool_calls":null,"tool_failures":null}),
        );
    }
    Some(events)
}
pub(crate) fn event_source(
    source: &Path,
    id: &str,
    location: &RunLocation,
    settings: &DashboardSettings,
) -> Option<(Vec<serde_json::Value>, bool)> {
    if let Some(path) = &location.transcript {
        let events =
            super::super::super::transcript::read(path, settings.history_read_max_bytes as u64)
                .ok()?;
        if events.iter().any(|event| event["run"] != id) {
            return None;
        }
        Some((events, false))
    } else {
        Some((legacy_events(source, id, location, settings)?, true))
    }
}
pub(crate) fn private_reply(
    head: &str,
    kind: &'static str,
    body: Vec<u8>,
    immutable: bool,
) -> Response {
    let etag = format!("\"{}\"", &digest(&body)[..16]);
    let matched = values(head, "if-none-match") == [etag.as_str()];
    let mut reply = response(
        if matched {
            "304 Not Modified"
        } else {
            "200 OK"
        },
        kind,
        if matched { vec![] } else { body },
    );
    reply.etag = Some(etag);
    reply.cache = if immutable {
        Cow::Borrowed("private, max-age=31536000, immutable")
    } else {
        Cow::Borrowed("private, no-cache")
    };
    reply
}
pub(crate) fn run(
    head: &str,
    id: &str,
    source: Option<&PathBuf>,
    settings: &DashboardSettings,
) -> Response {
    use serde_json::json;
    let Some(source) = source else {
        return error("404 Not Found", "run_not_found");
    };
    let Some(location) = find_run(source, id) else {
        return error("404 Not Found", "run_not_found");
    };
    let Some((mut events, legacy)) = event_source(source, id, &location, settings) else {
        return error("503 Service Unavailable", "transcript_unavailable");
    };
    let ended = events.iter().any(|v| v["kind"] == "run_ended");
    let started = events.iter().find(|v| v["kind"] == "run_started");
    // The same bounded window as the list projection: an archived run without a
    // terminal event is interrupted evidence, never a live claim (#19).
    let running = super::super::super::transcript::run_is_running(
        started.and_then(|v| v["at_ms"].as_i64()),
        ended,
        source.parent() == Some(location.model.as_path()),
        started.and_then(|v| v["role"].as_str()),
        started.and_then(|v| v["trigger"]["kind"].as_str()),
        millis(),
    );
    for event in &mut events {
        for field in ["text", "args", "output", "summary"] {
            if event
                .get(field)
                .is_some_and(|v| serde_json::to_vec(v).is_ok_and(|b| b.len() > 4096))
            {
                let pointer = if field == "output" && event["kind"] == "final_output" {
                    "output.json".into()
                } else if let Some(blob) = event["blob"].as_str() {
                    blob.to_owned()
                } else if let Some(turn) = event["turn"].as_u64() {
                    format!("turn-{turn:02}-response.json")
                } else {
                    "transcript.ndjson".into()
                };
                event.as_object_mut().unwrap().remove(field);
                event["blob"] = json!(pointer);
            }
        }
    }
    private_reply(
        head,
        "application/json",
        json!({"v":1,"run":id,"legacy":legacy,"running":running,"events":events})
            .to_string()
            .into_bytes(),
        ended,
    )
}
pub(crate) fn turn_file(name: &str, suffix: &str) -> Option<usize> {
    let digits = name.strip_prefix("turn-")?.strip_suffix(suffix)?;
    if digits.is_empty() || !digits.bytes().all(|v| v.is_ascii_digit()) {
        return None;
    }
    let turn = digits.parse::<usize>().ok()?;
    (format!("turn-{turn:02}{suffix}") == name).then_some(turn)
}
pub(crate) fn blob(
    head: &str,
    id: &str,
    name: &str,
    query: router::Query,
    source: Option<&PathBuf>,
    settings: &DashboardSettings,
) -> Response {
    use serde_json::{Value, json};
    let (base, pointer) = name
        .split_once('#')
        .map_or((name, None), |(a, b)| (a, Some(b)));
    if base.is_empty()
        || base.starts_with('.')
        || !base
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        || base.contains("..")
        || pointer.is_some_and(|p| !p.is_empty() && !p.starts_with('/'))
    {
        return error("404 Not Found", "blob_not_found");
    }
    let Some(source) = source else {
        return error("404 Not Found", "blob_not_found");
    };
    let Some(location) = find_run(source, id) else {
        return error("404 Not Found", "run_not_found");
    };
    let Some((events, _)) = event_source(source, id, &location, settings) else {
        return error("404 Not Found", "blob_not_found");
    };
    let request_turn = turn_file(base, "-request.json").or_else(|| turn_file(base, "-request"));
    let declared = events.iter().any(|v| {
        v["blob"]
            .as_str()
            .is_some_and(|blob| blob.split('#').next() == Some(base))
    });
    let response_turn = turn_file(base, "-response.json");
    if !declared
        && !matches!(base, "output.json" | "failure.json" | "transcript.ndjson")
        && request_turn.is_none()
        && response_turn.is_none()
    {
        return error("404 Not Found", "blob_not_found");
    }
    let raw = (|| -> Result<Value> {
        if let Some(turn) = request_turn {
            return super::super::super::transcript::full_request(
                location.dir.as_deref().ok_or("blob_unavailable")?,
                turn,
                settings,
            );
        }
        if base == "transcript.ndjson" {
            return Ok(json!(events));
        }
        let path = location.dir.as_ref().ok_or("blob_unavailable")?.join(base);
        if !regular(&path) {
            return Err("blob_unavailable".into());
        }
        let bytes = crate::read_bounded(&path, settings.history_read_max_bytes as u64)
            .map_err(|_| "blob_unavailable")?;
        serde_json::from_slice(&bytes).map_err(|_| "blob_invalid".into())
    })();
    let Ok(raw) = raw else {
        return error("404 Not Found", "blob_not_found");
    };
    let redacted = super::super::super::transcript::redact(&raw);
    let value = match pointer {
        Some(pointer) => match redacted.pointer(pointer) {
            Some(value) => value,
            None => return error("404 Not Found", "blob_not_found"),
        },
        None => &redacted,
    };
    let bytes = match serde_json::to_vec(value) {
        Ok(bytes) => bytes,
        Err(_) => return error("503 Service Unavailable", "blob_unavailable"),
    };
    let number = |key: &str, default: usize| {
        query.get(key).map_or(Some(default), |v| {
            v.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| v.parse::<usize>().ok())
                .flatten()
        })
    };
    let (Some(offset), Some(len)) = (
        number("offset", 0),
        number("len", settings.blob_slice_max_bytes),
    ) else {
        return error("400 Bad Request", "blob_slice_invalid");
    };
    if len > settings.blob_slice_max_bytes {
        return error("400 Bad Request", "blob_slice_invalid");
    }
    if offset > bytes.len() {
        return error("416 Range Not Satisfiable", "blob_slice_invalid");
    }
    let end = offset.saturating_add(len).min(bytes.len());
    let ended = events.iter().any(|v| v["kind"] == "run_ended");
    let mut reply = private_reply(
        head,
        "application/octet-stream",
        bytes[offset..end].to_vec(),
        ended,
    );
    reply.blob_size = Some(bytes.len());
    reply
}
pub(crate) fn prompt(
    head: &str,
    sha: &str,
    source: Option<&PathBuf>,
    settings: &DashboardSettings,
) -> Response {
    let Some(source) = source else {
        return error("404 Not Found", "prompt_not_found");
    };
    for model in super::super::super::model_history::model_dirs(source).unwrap_or_default() {
        let directory = model.join("prompts");
        if !fs::symlink_metadata(&directory).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let path = directory.join(format!("{sha}.md"));
        if !regular(&path) {
            continue;
        }
        let Ok(bytes) = crate::read_bounded(&path, settings.history_read_max_bytes as u64) else {
            continue;
        };
        if digest(&bytes) != sha {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let text = super::super::super::transcript::redact(&serde_json::json!(text));
        return private_reply(
            head,
            "text/plain; charset=utf-8",
            text.as_str().unwrap_or_default().as_bytes().to_vec(),
            true,
        );
    }
    error("404 Not Found", "prompt_not_found")
}
