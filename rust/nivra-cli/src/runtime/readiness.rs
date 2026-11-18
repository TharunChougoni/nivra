//! Bounded, read-only model readiness projection for the next paper session.
use super::agent::{AgentSettings, Lane};
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

fn pi_report(c: &Config, settings: &AgentSettings, session: NaiveDate) -> Value {
    let runtime_issue = pi_runtime_issue(settings);
    let spend = (session == c.session).then(|| super::agent::read_spend(c));
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
    let mut decision_issue = Some("agent_decision_lane_required");
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
        if *lane == Lane::Decision {
            decision_issue = issue;
        }
        roles.push(json!({
            "role":lane.as_str(),"enabled":true,"model":lane_settings.model.id,
            "transport":"pi","provider":lane_settings.model.provider,
            "status":if issue.is_some() {"blocked"} else {"unknown"},
            "reason":issue.unwrap_or("agent_provider_not_checked"),
            "credential_check":{"status":"not_checked","reason":"agent_provider_not_checked"},
            "daily_call_budget":lane_settings.daily_calls,"budget_unit":"request",
            "daily_cost_usd":lane_settings.daily_cost_usd,
            "current_usage":spent.as_ref().map(|spent| spent.calls),
            "current_cost_usd":spent.as_ref().map(|spent| spent.cost_usd),
            "remaining_current":spent.as_ref().map(|spent| lane_settings.daily_calls.saturating_sub(spent.calls)),
            "remaining_cost_usd":spent.as_ref().map(|spent| (lane_settings.daily_cost_usd - spent.cost_usd).max(0.0))
        }));
    }
    json!({"status":if decision_issue.is_some() {"model_roles_blocked"} else {"model_roles_unknown"},
        "reason":decision_issue.unwrap_or("agent_provider_not_checked"),"harness":"pi",
        "planner_enabled":true,"roles":roles,"current_usage":usage,
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

/// CLI entry point for a read-only readiness preview with an injectable clock.
pub fn entry(path: &Path, now_ms: i64) -> ExitCode {
    let result = (|| -> Result<Value> {
        let bytes = crate::read_bounded(path, 16384).map_err(|_| Fail::new("config_unreadable"))?;
        let c: Config = serde_json::from_slice(&bytes).map_err(|_| Fail::new("config_invalid"))?;
        c.validate()?;
        let current = rollover::current(&c)?;
        let models = report(&current, now_ms)?;
        let history = (|| -> Result<Value> {
            let target: NaiveDate = serde_json::from_value(models["target_session"].clone())?;
            let instruments = wide::history_instruments(&current)?;
            let mut future = current.clone();
            future.session = target;
            future.previous_session = current.next_sessions(target)?.1;
            let at = DateTime::parse_from_rfc3339(&format!("{target}T08:30:00+05:30"))?
                .timestamp_millis();
            let request = wide::preparation_request(
                &future,
                &instruments,
                at,
                preparation::PreparationWindow::PreOpen,
            )?;
            let coverage = preparation::coverage_report(
                &request.history_root,
                &instruments,
                &request.required_sessions,
                target,
                request.previous_session,
            );
            let mut value = serde_json::to_value(coverage)?;
            value["universe_session"] = json!(current.session);
            Ok(value)
        })()
        .unwrap_or_else(|_| json!({"status":"UNKNOWN","reason":"history_or_universe_unavailable"}));
        let (_, body) = rollover::bound_state(&current)?;
        let portfolio: nivra_core::execution::Portfolio = serde_json::from_str(&body)?;
        let report = wide::readiness_snapshot(&current, &portfolio, now_ms, &history);
        Ok(report)
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

/// Boundary scenarios folded into the existing model verification test.
#[cfg(test)]
pub(crate) fn check_boundaries() {
    // CLI exit contract: partial/missing history does not change model readiness.
    for history in ["READY", "PARTIAL", "UNKNOWN", "BLOCKED"] {
        assert!(models_ready(&json!({"status":"partial",
            "models":{"status":"model_roles_ready"},"history":{"status":history}})));
    }
    assert!(models_ready(&json!({"model_status":"model_roles_ready"})));
    for status in ["model_roles_unknown", "model_roles_blocked", "", "ready"] {
        assert!(!models_ready(&json!({"models":{"status":status},
            "model_status":"model_roles_ready"})));
        assert!(!models_ready(&json!({"model_status":status})));
    }
    for models in [
        Value::Null,
        json!({}),
        json!([]),
        json!("model_roles_ready"),
    ] {
        assert!(!models_ready(
            &json!({"models":models,"model_status":"model_roles_ready"})
        ));
    }
    assert!(!models_ready(&json!({})));
    let root = tempfile::tempdir_in("/var/tmp").unwrap();
    let c: Config = serde_json::from_value(json!({
        "account":"tharun-upstox","session":"2026-10-16","previous_session":"2026-10-15",
        "calendar_source":"NSE/CMTR/71775","directory":root.path(),
        "token_file":root.path().join("unused-token"),
        "dashboard_token_file":root.path().join("unused-dashboard"),
        "read_journal":root.path().join("unused-journal"),
        "read_scope":"upstox-market-tharun-upstox","read_journal_device":0,
        "read_journal_inode":0,"strategy":"model_planner_v1"
    }))
    .unwrap();
    let after_close = DateTime::parse_from_rfc3339("2026-10-21T15:30:01+05:30").unwrap();
    let after = report(&c, after_close.timestamp_millis()).unwrap();
    assert_eq!(after["target_session"], "2026-10-22");
    assert_eq!(after["previous_session"], "2026-10-21");
    assert_eq!(after["status"], "model_roles_blocked");
    assert_eq!(after["reason"], "planner_config_missing");
    let holiday_morning = DateTime::parse_from_rfc3339("2026-10-20T08:00:00+05:30").unwrap();
    let holiday = report(&c, holiday_morning.timestamp_millis()).unwrap();
    assert_eq!(holiday["target_session"], "2026-10-21");
    assert_eq!(holiday["previous_session"], "2026-10-19");
    assert_eq!(
        DateTime::parse_from_rfc3339("2026-10-21T09:15:00+05:30")
            .unwrap()
            .to_rfc3339(),
        report(
            &c,
            DateTime::parse_from_rfc3339("2026-10-21T08:00:00+05:30")
                .unwrap()
                .timestamp_millis()
        )
        .unwrap()["opens_at"]
            .as_str()
            .unwrap()
    );
    let planner = root.path().join("legacy-planner.json");
    fs::write(&planner, br#"{"enabled":false,"digest":{"enabled":false}}"#).unwrap();
    let mut with_planner = c.clone();
    with_planner.planner_config = Some(planner);
    let legacy = report(&with_planner, after_close.timestamp_millis()).unwrap();
    assert_eq!(legacy["status"], "model_roles_blocked");
    assert_eq!(legacy["reason"], "pi_paper_config_required");
    assert_eq!(legacy["roles"], json!([]));

    // Internal callers must receive an error for an unrepresentable clock.
    assert_eq!(
        report(&c, i64::MAX).unwrap_err().to_string(),
        "readiness_clock_range"
    );
}

#[cfg(test)]
#[test]
fn pi_readiness_projects_only_locally_verified_facts() {
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
    fs::create_dir_all(dir.join("model/agent")).unwrap();
    fs::write(dir.join("model/usage-identity.json"), b"legacy evidence").unwrap();
    fs::write(
        planner,
        json!({"enabled":true,"agent":{
            "mode":"paper","node":node,"entry":entry,"agent_dir":agent_dir,
            "lanes":{"decision":{"model":{"provider":"fixture","id":"decision-model"},
                "daily_calls":2,"daily_cost_usd":3.0}}
        }})
        .to_string(),
    )
    .unwrap();
    let now = DateTime::parse_from_rfc3339("2026-10-16T08:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    let fresh = report(&c, now).unwrap();
    assert_eq!(fresh["harness"], "pi");
    assert_eq!(fresh["status"], "model_roles_unknown");
    assert_eq!(fresh["runtime"]["status"], "present");
    assert_eq!(fresh["roles"].as_array().unwrap().len(), 1);
    assert_eq!(fresh["roles"][0]["role"], "decision");
    assert_eq!(
        fresh["roles"][0]["credential_check"]["status"],
        "not_checked"
    );
    assert_eq!(fresh["roles"][0]["remaining_current"], 2);
    assert!(!fresh.to_string().contains("opaque provider credentials"));
    assert!(!dir.join("model/agent/usage.json").exists());

    let spend = dir.join("model/agent/usage.json");
    fs::write(&spend, b"invalid usage").unwrap();
    let blocked = report(&c, now).unwrap();
    assert_eq!(blocked["status"], "model_roles_blocked");
    assert_eq!(blocked["reason"], "agent_usage_unavailable");
    assert_eq!(fs::read(&spend).unwrap(), b"invalid usage");

    fs::write(
        &spend,
        json!({"date":c.session,"lanes":{"decision":{
            "input":0,"output":0,"cache_read":0,"cache_write":0,"cost_usd":3,"calls":1
        }}})
        .to_string(),
    )
    .unwrap();
    let exhausted = report(&c, now).unwrap();
    assert_eq!(exhausted["status"], "model_roles_blocked");
    assert_eq!(exhausted["reason"], "daily_budget_exhausted");

    let after_close = DateTime::parse_from_rfc3339("2026-10-16T15:30:01+05:30")
        .unwrap()
        .timestamp_millis();
    let future = report(&c, after_close).unwrap();
    assert_eq!(future["target_session"], "2026-10-19");
    assert_eq!(future["reason"], "agent_provider_not_checked");
    assert_eq!(future["roles"][0]["remaining_current"], Value::Null);

    fs::remove_file(entry).unwrap();
    assert_eq!(
        report(&c, now).unwrap()["reason"],
        "agent_entry_unavailable"
    );
}
