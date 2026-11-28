//! Bounded, read-only model readiness projection for the next paper session.
use super::agent::{AgentSettings, JournaledTurn, Lane};
use super::*;
use serde_json::{Value, json};

fn pi_runtime_issue(settings: &AgentSettings) -> Option<&'static str> {
    if !fs::metadata(&settings.node)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    {
        return Some("agent_node_unavailable");
    }
    if !settings.entry.is_file() {
        return Some("agent_entry_unavailable");
    }
    if !settings.agent_dir.is_dir() {
        return Some("agent_directory_unavailable");
    }
    if settings.prompts.as_ref().is_some_and(|path| !path.is_dir()) {
        return Some("agent_prompts_unavailable");
    }
    None
}

/// A lane's status from its latest turn this session: a clean OK turn is
/// ready; a failed, refused, aborted or fallen-back turn is degraded.
fn turn_status(turn: &JournaledTurn) -> (&'static str, String) {
    match (turn.status.as_str(), &turn.fallback) {
        ("ok", None) => ("ready", "agent_turn_ok".into()),
        ("ok", Some(_)) => ("degraded", "agent_fallback_used".into()),
        (status, _) => (
            "degraded",
            error_code(turn).map_or_else(|| format!("agent_turn_{status}"), str::to_owned),
        ),
    }
}

/// The turn's error when it is a reason code. Provider messages can carry
/// free text, so readiness never repeats them.
fn error_code(turn: &JournaledTurn) -> Option<&str> {
    turn.error.as_deref().filter(|error| {
        !error.is_empty()
            && error.len() <= 64
            && error.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_:".contains(&byte)
            })
    })
}

fn turn_evidence(turn: &JournaledTurn) -> Value {
    json!({"turn_id":turn.turn_id,"at_ms":turn.at_ms,"status":turn.status,"error":error_code(turn),
    "fallback":turn.fallback.as_ref().map(|fallback| json!({
        "from":fallback.from,"to":fallback.to,"reason":fallback.reason
    }))})
}

fn pi_report(c: &Config, settings: &AgentSettings, session: NaiveDate) -> Value {
    let runtime_issue = pi_runtime_issue(settings);
    let current = session == c.session;
    let spend = current.then(|| super::agent::read_spend(c));
    let (usage, usage_issue) = match &spend {
        Some(Ok(by_lane)) => (json!({"status":"reported","used":by_lane}), None),
        Some(Err(_)) => (
            json!({"status":"unavailable","reason":"agent_usage_unavailable"}),
            Some("agent_usage_unavailable"),
        ),
        None => (
            json!({"status":"unknown","reason":"future_session_namespace_not_created"}),
            None,
        ),
    };
    let turns = current.then(|| super::agent::latest_turns(c));
    let journal = match &turns {
        Some(Ok(_)) => json!({"status":"reported"}),
        Some(Err(_)) => json!({"status":"unavailable","reason":"agent_turns_unreadable"}),
        None => json!({"status":"unknown","reason":"future_session_namespace_not_created"}),
    };
    let mut decision = ("blocked", "agent_decision_lane_required".to_owned());
    let mut roles = Vec::new();
    for (lane, lane_settings) in &settings.lanes {
        let spent = match &spend {
            Some(Ok(by_lane)) => Some(by_lane.get(lane).cloned().unwrap_or_default()),
            _ => None,
        };
        let issue = runtime_issue.or(usage_issue).or_else(|| {
            spent.as_ref().and_then(|spent| {
                if !spent.cost_usd.is_finite() || spent.cost_usd < 0.0 {
                    Some("agent_usage_unavailable")
                } else if spent.calls >= lane_settings.daily_calls
                    || spent.cost_usd >= lane_settings.daily_cost_usd
                {
                    Some("daily_budget_exhausted")
                } else {
                    None
                }
            })
        });
        // No evidence means unknown: configuration alone never makes a lane ready.
        let last = match &turns {
            Some(Ok(latest)) => latest.get(lane),
            _ => None,
        };
        let (status, reason) = match (issue, &turns, last) {
            (Some(issue), _, _) => ("blocked", issue.to_owned()),
            (None, Some(Err(_)), _) => ("unknown", "agent_turns_unreadable".to_owned()),
            (None, _, Some(turn)) => turn_status(turn),
            (None, _, None) => ("unknown", "agent_provider_not_checked".to_owned()),
        };
        if *lane == Lane::Decision {
            decision = (status, reason.clone());
        }
        roles.push(json!({
            "role":lane.as_str(),"enabled":true,"model":lane_settings.model.id,
            "transport":"pi","provider":lane_settings.model.provider,
            "status":status,"reason":reason,
            "last_turn":last.map(turn_evidence),
            // Stored-credential presence is the pi preflight's check at service
            // start; readiness never reads credentials.
            "credential_check":{"status":"not_checked","reason":"agent_provider_not_checked"},
            "daily_call_budget":lane_settings.daily_calls,"budget_unit":"request",
            "daily_cost_usd":lane_settings.daily_cost_usd,
            "current_usage":spent.as_ref().map(|spent| spent.calls),
            "current_cost_usd":spent.as_ref().map(|spent| spent.cost_usd),
            "remaining_current":spent.as_ref().map(|spent| lane_settings.daily_calls.saturating_sub(spent.calls)),
            "remaining_cost_usd":spent.as_ref().map(|spent| (lane_settings.daily_cost_usd - spent.cost_usd).max(0.0))
        }));
    }
    json!({"status":format!("model_roles_{}", decision.0),
        "reason":decision.1,"harness":"pi",
        "planner_enabled":true,"roles":roles,"current_usage":usage,"turn_journal":journal,
        "runtime":{"status":if runtime_issue.is_some() {"blocked"} else {"present"},"reason":runtime_issue}})
}

/// Returns a redacted preflight for the configured next session. No provider
/// calls, refresh, journal mutation, or account payload is used.
pub(super) fn report(c: &Config, now_ms: i64) -> Result<Value> {
    let clock = DateTime::<Utc>::from_timestamp_millis(now_ms)
        .ok_or_else(|| Fail::new("readiness_clock_range"))?
        .with_timezone(&FixedOffset::east_opt(19800).expect("IST"));
    let today = clock.date_naive();
    let today_hours = c
        .calendar_at(today)?
        .hours(nivra_core::time::ExchangeDate::parse(&today.to_string())?)?;
    let target_day = if today_hours.is_some_and(|(_, close)| {
        chrono::NaiveTime::parse_from_str(&close.to_string(), "%H:%M:%S%.f")
            .is_ok_and(|close| clock.time() >= close)
    }) {
        today
            .succ_opt()
            .ok_or_else(|| Fail::new("session_date_range"))?
    } else {
        today
    };
    let (session, previous) = c.next_sessions(target_day)?;
    let hours = c
        .calendar_at(session)?
        .hours(nivra_core::time::ExchangeDate::parse(&session.to_string())?)?
        .ok_or_else(|| Fail::new("target_session_not_trading"))?;
    let start = DateTime::parse_from_str(
        &format!("{session} {}+05:30", hours.0),
        "%Y-%m-%d %H:%M:%S%:z",
    )
    .map_err(|_| Fail::new("target_session_time_invalid"))?;
    let close = DateTime::parse_from_str(
        &format!("{session} {}+05:30", hours.1),
        "%Y-%m-%d %H:%M:%S%:z",
    )
    .map_err(|_| Fail::new("target_session_time_invalid"))?;
    let mut research = json!({"status":"disabled","reason":"research_tools_not_configured"});
    let mut report = match c.planner_config.as_deref() {
        None => blocked("planner_config_missing"),
        Some(path) => match planner::PlannerConfig::load_for_session(path, session) {
            Ok(cfg) if cfg.uses_pi_paper() => {
                if let Some(settings) = cfg.research_tools.as_ref() {
                    research = super::harness::research_status(settings);
                }
                match cfg.agent.as_ref() {
                    Some(settings) => pi_report(c, settings, session),
                    None => blocked("agent_settings_missing"),
                }
            }
            Ok(_) => blocked("pi_paper_config_required"),
            Err(_) => blocked("planner_config_invalid"),
        },
    };
    report["target_session"] = json!(session);
    report["previous_session"] = json!(previous);
    report["opens_at"] = json!(start);
    report["closes_at"] = json!(close);
    report["research"] = research;
    report["future_usage"] =
        json!({"status":"unknown","reason":"future_session_namespace_not_created"});
    Ok(report)
}

fn blocked(reason: &str) -> Value {
    json!({"status":"model_roles_blocked","reason":reason,"harness":"pi","planner_enabled":false,
        "roles":[],"current_usage":{"status":"unknown","reason":reason}})
}

// The dashboard snapshot nests the model report. Only older snapshots without
// a models member may use the legacy top-level field; malformed/new reports
// must never become ready through a stale legacy value. History is independent.
fn models_ready(snapshot: &Value) -> bool {
    let status = match snapshot.get("models") {
        Some(models) => &models["status"],
        None => &snapshot["model_status"],
    };
    status == "model_roles_ready"
}

pub(super) fn snapshot_for_config(c: &Config, now_ms: i64) -> Result<Value> {
    let current = rollover::current(c)?;
    let pending_fresh = (!current.directory.join("paper.sqlite").exists())
        .then(|| rollover::pending_fresh_book(&current))
        .transpose()?
        .flatten();
    let models = report(&current, now_ms)?;
    let history = (|| -> Result<Value> {
        let target: NaiveDate = serde_json::from_value(models["target_session"].clone())?;
        let (instruments, universe_session) = wide::history_instruments_with_session(&current)?;
        let mut future = current.clone();
        future.session = target;
        future.previous_session = current.next_sessions(target)?.1;
        let at =
            DateTime::parse_from_rfc3339(&format!("{target}T08:30:00+05:30"))?.timestamp_millis();
        let request = wide::preparation_request(
            &future,
            &instruments,
            at,
            preparation::PreparationWindow::PreOpen,
        )?;
        let coverage = preparation::coverage_report(
            &request.config,
            &instruments,
            &request.required_sessions,
            target,
            request.previous_session,
        );
        let mut value = serde_json::to_value(coverage)?;
        value["universe_session"] = json!(universe_session);
        Ok(value)
    })()
    .unwrap_or_else(|_| json!({"status":"UNKNOWN","reason":"history_or_universe_unavailable"}));
    let portfolio = match &pending_fresh {
        Some(_) => wide::pending_fresh_portfolio(&current, now_ms)?,
        None => {
            let (_, body) = rollover::bound_state(&current)?;
            serde_json::from_str(&body)?
        }
    };
    Ok(wide::readiness_snapshot_for(
        &current,
        &portfolio,
        now_ms,
        &history,
        pending_fresh.as_ref().map(|p| p.predecessor_session),
    ))
}

/// CLI entry point for a read-only readiness preview with an injectable clock.
pub fn entry(path: &Path, now_ms: i64) -> ExitCode {
    let result = (|| -> Result<Value> {
        let bytes = crate::read_bounded(path, 16384).map_err(|_| Fail::new("config_unreadable"))?;
        let c: Config = serde_json::from_slice(&bytes).map_err(|_| Fail::new("config_invalid"))?;
        c.validate()?;
        snapshot_for_config(&c, now_ms)
    })();
    match result {
        Ok(value) => {
            let ready = models_ready(&value);
            println!("{}", value);
            if ready {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            println!(
                "{}",
                json!({"status":"blocked","reason":failure_code(error.as_ref())})
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[test]
fn pi_readiness_derives_lane_status_from_local_turn_evidence() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    let mut c = super::tests::config(dir);
    let planner = dir.join("planner.json");
    c.planner_config = Some(planner.clone());
    let node = dir.join("node");
    let entry = dir.join("agent.js");
    let agent_dir = dir.join("pi");
    fs::create_dir(&agent_dir).unwrap();
    fs::write(&node, b"fixture; do not execute").unwrap();
    fs::set_permissions(&node, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(&entry, b"fixture; do not execute").unwrap();
    fs::write(agent_dir.join("auth.json"), b"opaque provider credentials").unwrap();
    let journal_dir = dir.join("model/agent");
    fs::create_dir_all(&journal_dir).unwrap();
    let lane =
        json!({"model":{"provider":"fixture","id":"primary"},"daily_calls":2,"daily_cost_usd":3.0});
    fs::write(
        planner,
        json!({"enabled":true,"agent":{
            "mode":"paper","node":node,"entry":entry,"agent_dir":agent_dir,
            "lanes":{"decision":lane,"gate":lane}
        }})
        .to_string(),
    )
    .unwrap();
    let at = |time: &str| {
        DateTime::parse_from_rfc3339(&format!("2026-10-16T{time}+05:30"))
            .unwrap()
            .timestamp_millis()
    };
    let now = at("10:00:00");
    let turn = |id: &str, status: &str, error: Value, fallback: Value| {
        json!({"turn_id":id,"lane":"decision","at_ms":at("09:30:00"),"status":status,
            "error":error,"fallback":fallback,"model":"primary","usage":{}})
    };
    let fell_back = json!({"from":{"provider":"fixture","id":"primary"},
        "to":{"provider":"fixture","id":"spare"},"reason":"quota","error":"provider said no"});
    let yesterday = json!({"turn_id":"decision-old","lane":"decision",
        "at_ms":at("09:00:00") - 86_400_000,"status":"ok"});
    let ok = turn("decision-ok", "ok", Value::Null, Value::Null);
    let spent = |calls: u64| {
        json!({"date":c.session,"lanes":{"decision":{
            "input":0,"output":0,"cache_read":0,"cache_write":0,"cost_usd":0,"calls":calls
        }}})
    };
    // (case, journal lines, usage ledger, decision status, reason, evidenced turn, overall)
    let cases = [
        (
            "no turns",
            vec![],
            None,
            "unknown",
            "agent_provider_not_checked",
            None,
            "model_roles_unknown",
        ),
        (
            "only an older session",
            vec![yesterday],
            None,
            "unknown",
            "agent_provider_not_checked",
            None,
            "model_roles_unknown",
        ),
        (
            "ok turn",
            vec![ok.clone()],
            Some(spent(1)),
            "ready",
            "agent_turn_ok",
            Some("decision-ok"),
            "model_roles_ready",
        ),
        (
            "failed turn after an ok one",
            vec![
                ok.clone(),
                turn(
                    "decision-failed",
                    "error",
                    json!("agent_exited"),
                    Value::Null,
                ),
            ],
            None,
            "degraded",
            "agent_exited",
            Some("decision-failed"),
            "model_roles_degraded",
        ),
        (
            "failed turn with provider text",
            vec![turn(
                "decision-text",
                "error",
                json!("401 Unauthorized: token sk-redacted expired"),
                Value::Null,
            )],
            None,
            "degraded",
            "agent_turn_error",
            Some("decision-text"),
            "model_roles_degraded",
        ),
        (
            "fallback turn",
            vec![turn("decision-spare", "ok", Value::Null, fell_back.clone())],
            None,
            "degraded",
            "agent_fallback_used",
            Some("decision-spare"),
            "model_roles_degraded",
        ),
        (
            "exhausted budget",
            vec![ok.clone()],
            Some(spent(2)),
            "blocked",
            "daily_budget_exhausted",
            Some("decision-ok"),
            "model_roles_blocked",
        ),
    ];
    let journal = journal_dir.join("turns.ndjson");
    let usage = journal_dir.join("usage.json");
    for (case, lines, ledger, status, reason, evidence, overall) in cases {
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        fs::write(&journal, text).unwrap();
        let ledger_written = ledger.is_some();
        match ledger {
            Some(ledger) => fs::write(&usage, ledger.to_string()).unwrap(),
            None => drop(fs::remove_file(&usage)),
        }
        let report = report(&c, now).unwrap();
        // Readiness only reads: it never creates the usage ledger.
        assert_eq!(usage.exists(), ledger_written, "{case}");
        let decision = &report["roles"][0];
        assert_eq!(decision["role"], "decision", "{case}");
        assert_eq!(
            (decision["status"].as_str(), decision["reason"].as_str()),
            (Some(status), Some(reason)),
            "{case}"
        );
        assert_eq!(
            decision["last_turn"]["turn_id"].as_str(),
            evidence,
            "{case}"
        );
        assert_eq!(report["status"], overall, "{case}");
        assert_eq!(report["reason"], reason, "{case}");
        assert_eq!(
            decision["credential_check"]["status"], "not_checked",
            "{case}"
        );
        // The gate lane ran no turn, so it stays unknown whatever decision did.
        assert_eq!(report["roles"][1]["role"], "gate", "{case}");
        assert_eq!(report["roles"][1]["status"], "unknown", "{case}");
        assert!(!report.to_string().contains("opaque provider credentials"));
    }
    fs::remove_file(&usage).unwrap();
    fs::write(
        &journal,
        format!("{}\n", turn("decision-spare", "ok", Value::Null, fell_back)),
    )
    .unwrap();
    let named = report(&c, now).unwrap();
    assert_eq!(
        named["roles"][0]["last_turn"]["fallback"]["to"]["id"],
        "spare"
    );
    assert_eq!(
        named["roles"][0]["last_turn"]["fallback"]["reason"],
        "quota"
    );
    assert!(!named.to_string().contains("provider said no"));

    // A malformed journal is no evidence; a crash-cut final line is ignored.
    fs::write(&journal, format!("{ok}\n{{\"turn_id\"")).unwrap();
    assert_eq!(report(&c, now).unwrap()["status"], "model_roles_ready");
    fs::write(&journal, b"not json\n").unwrap();
    let unreadable = report(&c, now).unwrap();
    assert_eq!(unreadable["reason"], "agent_turns_unreadable");
    assert_eq!(unreadable["turn_journal"]["status"], "unavailable");
    fs::remove_file(&journal).unwrap();

    fs::write(&usage, b"invalid usage").unwrap();
    let blocked = report(&c, now).unwrap();
    assert_eq!(blocked["status"], "model_roles_blocked");
    assert_eq!(blocked["reason"], "agent_usage_unavailable");
    assert_eq!(fs::read(&usage).unwrap(), b"invalid usage");
    fs::remove_file(&usage).unwrap();

    // After the close the target is the next session, which has no turns yet.
    fs::write(&journal, format!("{ok}\n")).unwrap();
    let future = report(&c, at("15:30:01")).unwrap();
    assert_eq!(future["target_session"], "2026-10-19");
    assert_eq!(future["reason"], "agent_provider_not_checked");
    assert_eq!(future["roles"][0]["remaining_current"], Value::Null);
    assert_eq!(future["roles"][0]["last_turn"], Value::Null);

    fs::remove_file(entry).unwrap();
    assert_eq!(
        report(&c, now).unwrap()["reason"],
        "agent_entry_unavailable"
    );
}
