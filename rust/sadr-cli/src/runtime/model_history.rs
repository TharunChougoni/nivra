//! Read-only model history. Paths come only from the configured session root.
use super::*;
use serde_json::{Value, json};

#[cfg(test)]
thread_local! { static OPENED: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) }; }
fn read(path: &Path, settings: &super::DashboardSettings) -> Option<Value> {
    #[cfg(test)]
    OPENED.with(|opened| opened.borrow_mut().push(path.to_owned()));
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > settings.history_read_max_bytes as u64 {
        return None;
    }
    serde_json::from_slice(&crate::read_bounded(path, settings.history_read_max_bytes as u64).ok()?)
        .ok()
}
fn directories(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect()
}
fn stamp(path: &Path) -> Option<i64> {
    fs::symlink_metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()
}
// Never project requests wholesale: they include frozen account context. Expose
// only tool names/arguments and assistant output, with credential-like text scrubbed.
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
pub(super) fn safe(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(
            s.split_whitespace()
                .map(|word| {
                    if word.contains("sk-")
                        || word.contains("Bearer")
                        || word.contains("access_token")
                        || word.contains("refresh_token")
                    {
                        "[redacted]"
                    } else {
                        word
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(16000)
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().take(64).map(safe).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(k, _)| {
                    !secret_key(k)
                        && !["account", "cash", "positions"]
                            .contains(&k.to_ascii_lowercase().as_str())
                })
                .take(64)
                .map(|(k, v)| (k.clone(), safe(v)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn text(response: &Value) -> Value {
    if response.get("output").is_none() {
        return safe(response);
    }
    let has_final = response["output"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|i| i["phase"] == "final_answer");
    let body = response["output"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| {
            i["type"] == "message"
                && i["role"] == "assistant"
                && (!has_final || i["phase"] == "final_answer")
        })
        .flat_map(|i| i["content"].as_array().into_iter().flatten())
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str::<Value>(&body)
        .map(|v| safe(&v))
        .unwrap_or_else(|_| safe(&json!(body)))
}
fn outcome(output: &Value) -> Value {
    output["handoff"]["decision"]
        .as_str()
        .or(output["stance"].as_str())
        .map(|s| json!(s))
        .unwrap_or_else(|| {
            if output["actions"].as_array().is_some_and(|a| !a.is_empty()) {
                json!("decide")
            } else {
                json!("completed")
            }
        })
}
fn started(path: &Path) -> Option<i64> {
    path.file_name()?
        .to_str()?
        .strip_prefix("harness-")?
        .split('-')
        .nth(1)?
        .parse()
        .ok()
}
// One stable, bounded identifier shared by history rows and detail lookup.
// Invalid directory names are omitted rather than producing an unresolvable row.
pub(super) fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && !id.starts_with('-')
}
fn audit_for(path: &Path, settings: &super::DashboardSettings) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    if !name.starts_with("harness-digest-") && !name.starts_with("harness-decide-") {
        return None;
    }
    if let Some(invocation) = read(&path.join("run.json"), settings)
        .and_then(|v| v["invocation_id"].as_str().map(str::to_owned))
    {
        return directories(path.parent()?).into_iter().find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.ends_with(&format!("-{invocation}")))
        });
    }
    let at = started(path)?;
    directories(path.parent()?)
        .into_iter()
        .filter(|p| {
            !p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .starts_with("harness-")
        })
        .filter_map(|p| Some((stamp(&p.join("input.json"))?, p)))
        .filter(|(t, _)| *t <= at)
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}
fn run(path: &Path, id: &str, settings: &super::DashboardSettings) -> Value {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mut meta = read(&path.join("run.json"), settings).unwrap_or(Value::Null);
    let audit = audit_for(path, settings).unwrap_or_else(|| path.to_owned());
    let audit_input = read(&audit.join("input.json"), settings).unwrap_or(Value::Null);
    let review = read(&audit.join("luna-review.json"), settings).unwrap_or(Value::Null);
    let role = meta["role"]
        .as_str()
        .or_else(|| name.strip_prefix("harness-")?.split('-').next())
        .unwrap_or("decide")
        .to_owned();
    if meta.is_null() {
        meta = json!({});
    }
    if meta["trigger"].is_null() {
        meta["trigger"] = audit_input["input"]["trigger"].clone();
    }
    if meta["model"].is_null() {
        meta["model"] = audit_input[&role]["model_id"].clone();
    }
    let mut files: Vec<_> = fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| {
            e.file_name().to_string_lossy().starts_with("turn-")
                && (e.file_name().to_string_lossy().ends_with("-response.json")
                    || e.file_name().to_string_lossy().ends_with("-request.json")
                    || e.file_name()
                        .to_string_lossy()
                        .ends_with("-request-delta.json"))
        })
        .map(|e| e.path())
        .collect();
    files = files
        .into_iter()
        .map(|p| {
            let file = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .replace("-request-delta.json", "-response.json")
                .replace("-request.json", "-response.json");
            p.with_file_name(file)
        })
        .collect();
    files.sort();
    files.dedup();
    let mut turns = vec![];
    let mut final_output = Value::Null;
    let mut model = meta["model"].clone();
    let mut tokens = 0u64;
    let mut tokens_known = false;
    for file in files.iter().take(settings.history_max_files.min(256)) {
        let Some(response) = read(file, settings) else {
            turns.push(json!({"turn":file.file_name().unwrap_or_default().to_string_lossy().split('-').nth(1),"tools":[],"output":null,"status":"response_missing_or_unreadable"}));
            continue;
        };
        if model.is_null() {
            model = response["model"].clone();
        }
        if let Some(n) = response["usage"]["total_tokens"].as_u64().or_else(|| {
            Some(
                response["usage"]["input_tokens"].as_u64()?
                    + response["usage"]["output_tokens"].as_u64()?,
            )
        }) {
            tokens += n;
            tokens_known = true;
        }
        let mut calls = vec![];
        for item in response["output"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["type"] == "function_call")
        {
            let id = item["call_id"].as_str().unwrap_or("");
            let result = read(
                &path.join(format!("tool-{}.result.json", digest(id.as_bytes()))),
                settings,
            );
            let args = item["arguments"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or(Value::Null);
            let summary = result.map(|v|json!({"status":v["status"],"reason":v["reason"],"evidence_id":v["evidence_id"],"received_ms":v["received_ms"],"quotes":v["quotes"].as_array().map(Vec::len),"features":v["features"].as_array().map(Vec::len),"sections":v.as_object().map(|o|o.keys().filter(|k| !["data","quotes","features"].contains(&k.as_str())).collect::<Vec<_>>())}));
            calls.push(json!({"name":safe(&item["name"]),"arguments":safe(&args),"result":safe(&json!(summary))}));
        }
        for item in response["output"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| {
                matches!(
                    i["type"].as_str(),
                    Some("web_search_call" | "x_search_call" | "custom_tool_call")
                )
            })
        {
            calls.push(json!({"name":item["name"].as_str().or(item["type"].as_str()),"arguments":safe(&item["action"]),"result":safe(&json!({"status":item["status"],"output":item["output"]}))}));
        }
        let output = text(&response);
        if !output.is_null() && output != "" {
            final_output = output.clone();
        }
        turns.push(json!({"turn":file.file_name().unwrap_or_default().to_string_lossy().split('-').nth(1),"tools":calls,"output":output}));
    }
    if final_output.is_null() || final_output == "" {
        if role == "digest" && review["digest"].is_object() {
            final_output = safe(&review["digest"]);
        } else if role == "decide"
            && let Some(raw) = read(&audit.join("output.json"), settings)
                .and_then(|v| v["raw_output"].as_str().map(str::to_owned))
        {
            final_output = safe(&serde_json::from_str::<Value>(&raw).unwrap_or(json!(raw)));
        }
    }
    let failure = read(&path.join("failure.json"), settings).or_else(|| {
        (role == "decide" || final_output.is_null() || final_output == "")
            .then(|| read(&audit.join("failure.json"), settings))
            .flatten()
    });
    let terminal = read(&path.join("output.json"), settings);
    if let Some(t) = terminal {
        final_output = t
            .get("output")
            .map(safe)
            .or_else(|| {
                t["raw_output"]
                    .as_str()
                    .map(|s| safe(&serde_json::from_str::<Value>(s).unwrap_or(json!(s))))
            })
            .unwrap_or_else(|| text(&t));
    }
    let started = meta["started_ms"]
        .as_i64()
        .or_else(|| started(path))
        .or_else(|| stamp(&path.join("input.json")));
    let ended = meta["completed_ms"]
        .as_i64()
        .or_else(|| stamp(&path.join("failure.json")))
        .or_else(|| stamp(&path.join("output.json")))
        .or_else(|| {
            (!final_output.is_null() && final_output != "")
                .then(|| files.last().and_then(|p| stamp(p)))
                .flatten()
        });
    json!({"id":id,"role":role,"model":model,"trigger":safe(&meta["trigger"]),"started_ms":started,"duration_ms":started.zip(ended).map(|(a,b)|b.saturating_sub(a)),"tokens":tokens_known.then_some(tokens),"outcome":if failure.is_some(){json!("failure")}else if final_output.is_null() || final_output == ""{json!("incomplete") }else{outcome(&final_output)},"reason":safe(&failure.as_ref().map(|v|v["reason"].clone()).unwrap_or_else(||final_output["handoff"].get("reason").or(final_output.get("notes")).cloned().unwrap_or(Value::Null))),"failure":failure.map(|v|safe(&v)),"turns":turns,"output":final_output,"recorded_ms":stamp(path)})
}

// List rows never follow audit links, load inputs or open tool results.
pub(super) fn summary_row(path: &Path, id: &str, settings: &super::DashboardSettings) -> Value {
    let meta = read(&path.join("run.json"), settings).unwrap_or(Value::Null);
    let role = meta["role"]
        .as_str()
        .or_else(|| {
            path.file_name()?
                .to_str()?
                .strip_prefix("harness-")?
                .split('-')
                .next()
        })
        .unwrap_or("decide");
    let terminal = read(&path.join("output.json"), settings);
    let failure = read(&path.join("failure.json"), settings);
    let mut output = terminal
        .as_ref()
        .map(|v| {
            v.get("output")
                .cloned()
                .or_else(|| {
                    v["raw_output"]
                        .as_str()
                        .map(|raw| serde_json::from_str(raw).unwrap_or(Value::Null))
                })
                .unwrap_or_else(|| text(v))
        })
        .unwrap_or(Value::Null);
    let mut responses: Vec<_> = fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("turn-") && n.ends_with("-response.json"))
        })
        .collect();
    responses.sort();
    let mut model = meta["model"].clone();
    let mut tokens = None::<u64>;
    for file in responses.iter().take(settings.history_max_files) {
        if let Some(response) = read(file, settings) {
            if model.is_null() {
                model = response["model"].clone();
            }
            // Legacy runs may have only a final response. Preserve their outcome
            // without following tool calls or loading request/audit context.
            if terminal.is_none() {
                let candidate = text(&response);
                if !candidate.is_null() && candidate != "" {
                    output = candidate;
                }
            }
            if let Some(n) = response["usage"]["total_tokens"].as_u64().or_else(|| {
                response["usage"]["input_tokens"]
                    .as_u64()?
                    .checked_add(response["usage"]["output_tokens"].as_u64()?)
            }) {
                tokens = Some(tokens.unwrap_or(0).saturating_add(n));
            }
        }
    }
    let start = meta["started_ms"]
        .as_i64()
        .or_else(|| started(path))
        .or_else(|| stamp(&path.join("input.json")));
    let end = meta["completed_ms"]
        .as_i64()
        .or_else(|| stamp(&path.join("failure.json")))
        .or_else(|| stamp(&path.join("output.json")))
        .or_else(|| {
            (!output.is_null() && output != "")
                .then(|| responses.last().and_then(|p| stamp(p)))
                .flatten()
        });
    json!({"id":id,"role":role,"model":safe(&model),"trigger":{"kind":safe(&meta["trigger"]["kind"])},"started_ms":start,"duration_ms":start.zip(end).map(|(a,b)|b.saturating_sub(a)),"tokens":tokens,"outcome":if failure.is_some(){json!("failure")}else if !output.is_null() && output != ""{outcome(&output)}else{json!("incomplete")}})
}
// An explicit model source authorizes only sibling dated session archives.
pub(super) fn model_dirs(source: &Path) -> Option<Vec<PathBuf>> {
    let session = source.parent().and_then(Path::parent)?;
    let Some(root) = session.parent() else {
        return Some(vec![]);
    };
    Some(
        directories(root)
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok())
            })
            .filter_map(|day| {
                let model = day.join("model");
                if fs::symlink_metadata(&model).is_ok_and(|m| m.file_type().is_symlink()) {
                    None
                } else {
                    Some(model)
                }
            })
            .collect(),
    )
}
// Fixture adapter: exercise current v1 discovery, never the retired HTTP listing.
#[cfg(test)]
pub(super) fn list_with_settings(
    source: &Path,
    offset: usize,
    limit: usize,
    settings: &super::DashboardSettings,
    summary: bool,
) -> Value {
    let page = super::transcript::list_runs(
        source,
        &super::transcript::RunFilter {
            limit: 100,
            ..Default::default()
        },
        settings,
    )
    .unwrap()
    .body;
    let rows = page["runs"].as_array().unwrap();
    assert!(page["next"].is_null(), "fixture exceeds adapter bound");
    let runs: Vec<_> = rows
        .iter()
        .skip(offset)
        .take(limit)
        .map(|row| {
            if summary {
                row.clone()
            } else {
                run_detail_with_settings(source, row["run"].as_str().unwrap(), settings)
                        .unwrap()["run"]
                        .clone()
            }
        })
        .collect();
    json!({"runs":runs,"next":(offset.saturating_add(limit)<rows.len()).then_some(offset.saturating_add(limit))})
}
#[cfg(test)]
pub(super) fn page(source: &Path, offset: usize, limit: usize) -> Value {
    list_with_settings(
        source,
        offset,
        limit,
        &super::DashboardSettings::default(),
        false,
    )
}
pub(super) fn run_detail_with_settings(
    source: &Path,
    id: &str,
    settings: &super::DashboardSettings,
) -> Option<Value> {
    if !valid_id(id) {
        return None;
    }
    let path = model_dirs(source)?
        .into_iter()
        .flat_map(|model| directories(&model))
        .find(|p| {
            p.file_name().and_then(|s| s.to_str()) == Some(id)
                && fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        })?;
    let audit = read(&path.join("run-audit.json"), settings)
        .map(|value| safe(&value))
        .unwrap_or(Value::Null);
    Some(json!({"id": id, "run": run(&path, id, settings), "audit": audit}))
}

#[cfg(test)]
pub(super) fn check_archive() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("sessions/2026-10-23/model");
    fs::create_dir_all(&session).unwrap();
    let old = session.join("harness-digest-1000-old");
    let newest = session.join("harness-decide-2000-new");
    for path in [&old, &newest] {
        fs::create_dir(path).unwrap();
    }
    let write = |p: PathBuf, v: Value| fs::write(p, serde_json::to_vec(&v).unwrap()).unwrap();
    write(
        old.join("turn-00-request.json"),
        json!({"private":"must-not-be-exposed"}),
    );
    write(
        old.join("turn-00-response.json"),
        json!({"model":"luna","usage":{"total_tokens":12},"output":[{"type":"function_call","call_id":"c1","name":"get_market_snapshot","arguments":"{\"instrument_keys\":[\"A\"]}"}]}),
    );
    write(
        old.join(format!("tool-{}.result.json", digest(b"c1"))),
        json!({"evidence_id":"receipt","quotes":[{}],"access_token":"must-not-be-exposed"}),
    );
    write(old.join("turn-01-request.json"), json!({}));
    write(
        old.join("turn-01-response.json"),
        json!({"usage":{"total_tokens":8},"summary":"seen","handoff":{"decision":"continue"}}),
    );
    write(newest.join("turn-00-request.json"), json!({}));
    write(
        newest.join("failure.json"),
        json!({"reason":"planner_call_timeout"}),
    );
    let source = session.join("live-stream.json");
    let settings = super::DashboardSettings::default();
    let detail = run_detail_with_settings(&source, "harness-decide-2000-new", &settings).unwrap();
    assert_eq!(detail["run"]["outcome"], "failure");
    assert_eq!(
        detail["run"]["turns"][0]["status"],
        "response_missing_or_unreadable"
    );
    let second = run_detail_with_settings(&source, "harness-digest-1000-old", &settings).unwrap();
    assert_eq!(second["run"]["tokens"], 20);
    assert_eq!(second["run"]["outcome"], "continue");
    assert_eq!(
        second["run"]["turns"][0]["tools"][0]["result"]["evidence_id"],
        "receipt"
    );
    assert!(!second.to_string().contains("must-not-be-exposed"));
    assert_eq!(
        run_detail_with_settings(&source, "harness-digest-1000-old", &settings).unwrap(),
        second
    );
    let external = dir.path().join("external");
    fs::create_dir(&external).unwrap();
    std::os::unix::fs::symlink(&external, session.join("harness-digest-3000-link")).unwrap();
    assert_eq!(page(&source, 0, 20)["runs"].as_array().unwrap().len(), 2);
    write(
        old.join("turn-01-response.json"),
        json!({"summary":"sk-private","handoff":{"decision":"wait"}}),
    );
    assert!(!page(&source, 0, 20).to_string().contains("sk-private"));
    let audit = session.join("audit-only");
    fs::create_dir(&audit).unwrap();
    write(
        audit.join("input.json"),
        json!({"decide":{"model_id":"grok"},"input":{"trigger":{"kind":"fill"}}}),
    );
    write(
        audit.join("output.json"),
        json!({"raw_output":"{\"actions\":[],\"stance\":\"abstain\"}"}),
    );
    let audit_page = page(&source, 0, 20);
    let audit_row = audit_page["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "audit-only")
        .unwrap();
    assert_eq!(audit_row["model"], "grok");
    assert_eq!(audit_row["trigger"]["kind"], "fill");
    let detail = run_detail_with_settings(
        &source,
        audit_row["id"].as_str().unwrap(),
        &super::DashboardSettings::default(),
    )
    .unwrap();
    assert_eq!(detail["id"], "audit-only");
    assert_eq!(detail["run"]["id"], "audit-only");
    assert_eq!(detail["run"]["outcome"], "abstain");
    assert_eq!(detail["audit"], Value::Null);
}

#[cfg(test)]
mod phase1_tests {
    use super::*;
    #[test]
    fn large_detail_summary_open_bounds_and_directory_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("sessions/2026-10-28/model");
        let audit = model.join("legacy-audit");
        fs::create_dir_all(&audit).unwrap();
        fs::write(audit.join("input.json"), serde_json::to_vec(&json!({"decide":{"model_id":"fixture-model"},"input":{"trigger":{"kind":"fill"}},"padding":"x".repeat(2_200_000)})).unwrap()).unwrap();
        fs::write(
            audit.join("output.json"),
            br#"{"raw_output":"{\"stance\":\"abstain\",\"actions\":[]}"}"#,
        )
        .unwrap();
        let source = model.join("live-stream.json");
        let settings = super::super::DashboardSettings::default();
        let detail = run_detail_with_settings(&source, "legacy-audit", &settings).unwrap();
        assert_eq!(detail["run"]["model"], "fixture-model");
        assert_eq!(detail["run"]["trigger"]["kind"], "fill");
        let small = super::super::DashboardSettings {
            history_read_max_bytes: 1_000_000,
            ..settings.clone()
        };
        assert!(
            run_detail_with_settings(&source, "legacy-audit", &small).unwrap()["run"]["model"]
                .is_null()
        );
        let harness = model.join("harness-digest-2000-one");
        fs::create_dir(&harness).unwrap();
        fs::write(
            harness.join("run.json"),
            br#"{"model":"fixture-model","trigger":{"kind":"fill"}}"#,
        )
        .unwrap();
        fs::write(harness.join("turn-00-response.json"), br#"{"usage":{"total_tokens":12},"output":[{"type":"function_call","call_id":"c1","name":"get_market_snapshot"}]}"#).unwrap();
        fs::write(
            harness.join(format!("tool-{}.result.json", digest(b"c1"))),
            b"{}",
        )
        .unwrap();
        OPENED.with(|opened| opened.borrow_mut().clear());
        let page = list_with_settings(&source, 0, 20, &settings, true);
        assert!(
            page["runs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == "harness-digest-2000-one" && row["tokens"] == 12)
        );
        OPENED.with(|opened| {
            let paths = opened.borrow();
            assert!(!paths.iter().any(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("tool-")
            }));
            assert!(!paths.iter().any(|p| p.file_name().unwrap() == "input.json"));
        });
        let newer = model.join("harness-decide-3000-new");
        fs::create_dir(&newer).unwrap();
        let updated = list_with_settings(&source, 0, 20, &settings, true);
        assert!(
            updated["runs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == "harness-decide-3000-new")
        );
        let uncached = super::super::DashboardSettings {
            history_list_cache: false,
            ..settings
        };
        assert_eq!(updated, list_with_settings(&source, 0, 20, &uncached, true));
        let next_model = dir
            .path()
            .join("sessions/2026-10-29/model/harness-digest-4000-next");
        fs::create_dir_all(&next_model).unwrap();
        assert!(
            list_with_settings(&source, 0, 20, &uncached, true)["runs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["id"] == "harness-digest-4000-next")
        );
    }
}
