//! Bounded, read-only model readiness projection for the next paper session.
use super::*;
use serde_json::{Value, json};

// The orchestrator is a readiness identity; reservations retain trigger names.
// Alias only the selected trigger, preserving unknown/missing usage as unknown.
fn orchestrator_usage(usage: Option<&Value>, trigger: &str) -> Option<Value> {
    usage.map(|usage| json!({"orchestrator": usage[trigger]}))
}

fn remaining_calls(budget: u32, used: Option<u64>) -> Option<u32> {
    used.and_then(|n| u32::try_from(n).ok())
        .map(|n| budget.saturating_sub(n))
}

fn post_close_deadline(
    session: NaiveDate,
    now: DateTime<FixedOffset>,
    close: DateTime<FixedOffset>,
    cutoff_minute: u32,
    operations: &planner::ModelRole,
    strategy: &planner::ModelRole,
) -> Result<DateTime<FixedOffset>> {
    // Queued reviews may start late; cover the configured host cutoff,
    // rather than assuming both roles run immediately after close.
    let cutoff_at = DateTime::parse_from_rfc3339(&format!(
        "{session}T{:02}:{:02}:00+05:30",
        cutoff_minute / 60,
        cutoff_minute % 60
    ))?;
    // Readiness covers the next scheduled work window. A later
    // invocation re-reads the same app grant under its refresh owner.
    let start_at = now.max(close);
    Ok((start_at
        + chrono::Duration::milliseconds(
            (operations.invocation_budget_ms("operations")
                + strategy.invocation_budget_ms("strategy")) as i64,
        ))
    .min(cutoff_at))
}

fn role_report(
    name: &str,
    role: &planner::ModelRole,
    at: DateTime<FixedOffset>,
    deadline: DateTime<FixedOffset>,
    usage: Option<&Value>,
    fallback: Option<&Path>,
) -> Value {
    if !role.enabled {
        return json!({"role":name,"enabled":false,"status":"disabled",
            "model":role.model_id,"transport":role.transport});
    }
    let result = match role.model_config.as_deref().or(fallback) {
        Some(path) => crate::model::readiness(path, role.transport, &role.model_id, at, deadline),
        None => Err("model_config_required".into()),
    };
    // A reviewed limit fallback (#245) is reported beside the primary. It
    // never changes the primary's status: it serves only limit failures.
    let limit_fallback = role.fallback.as_ref().map_or(
        json!({"configured":false}),
        crate::model::fallback_readiness,
    );
    let mut report = role_status(name, role, result, deadline, usage);
    report["fallback"] = limit_fallback;
    report
}

fn role_status(
    name: &str,
    role: &planner::ModelRole,
    result: std::result::Result<Value, String>,
    deadline: DateTime<FixedOffset>,
    usage: Option<&Value>,
) -> Value {
    match result {
        Ok(details) => {
            let current = usage
                .and_then(|v| v[name].as_u64())
                .and_then(|v| u32::try_from(v).ok());
            let remaining = remaining_calls(role.daily_call_budget, current.map(u64::from));
            let budget_ready = remaining.is_some_and(|n| n > 0);
            let usage_known = remaining.is_some();
            let credential_unknown = details["status"] == "unknown";
            json!({"role":name,"enabled":true,"model":role.model_id,"transport":role.transport,
            "status":if budget_ready && !credential_unknown {"ready"} else if !budget_ready && usage_known {"blocked"} else {"unknown"},
            "reason":if budget_ready {details["reason"].clone()} else {json!(if usage_known {"daily_budget_exhausted"} else {"usage_namespace_unavailable"})},
            "approval_covers_until":deadline,"daily_call_budget":role.daily_call_budget,"budget_unit":role.budget_unit.as_deref().unwrap_or("request"),
            "current_usage":current,"remaining_current":remaining,
            "usage_status":if current.is_some() {"verified_original_namespace"} else {"unknown"},
            "credential_check":details})
        }
        Err(code) => {
            json!({"role":name,"enabled":true,"model":role.model_id,"transport":role.transport,
            "status":"blocked","reason":code.as_str(),"daily_call_budget":role.daily_call_budget,"budget_unit":role.budget_unit.as_deref().unwrap_or("request"),
            "current_usage":usage.and_then(|v|v[name].as_u64()),
            "remaining_current":remaining_calls(role.daily_call_budget,usage.and_then(|v|v[name].as_u64())),
            "usage_status":if usage.is_some() {"verified_original_namespace"} else {"unknown"},
            "credential_check":Value::Null})
        }
    }
}

// Alert reasons are stable lowercase codes, never provider text or paths.
fn safe_reason(value: &Value) -> Option<&str> {
    value.as_str().filter(|reason| {
        !reason.is_empty()
            && reason.len() <= 96
            && reason
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    })
}

/// Project only static role identifiers/statuses into alerts; never forward provider
/// diagnostics, paths, grant metadata or account payloads. Unknown future refresh
/// remains a warning, rather than an assertion of session coverage. If the role
/// is blocked, the real stable reason code survives instead of a generic
/// credential failure.
pub(super) fn credential_warnings(models: &Value) -> Vec<Value> {
    models["roles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|role| {
            if role["enabled"] != true {
                return None;
            }
            let name = role["role"].as_str()?;
            if !["digest", "decide", "operations", "strategy", "orchestrator"].contains(&name) {
                return None;
            }
            let credential = &role["credential_check"];
            let reason = if credential["status"] == "unknown" {
                safe_reason(&credential["reason"])
                    .unwrap_or("grant_refresh_required_or_expiry_unverified")
            } else if role["status"] == "blocked" && credential.is_null() {
                safe_reason(&role["reason"]).unwrap_or("model_credential_validation_failed")
            } else {
                return None;
            };
            Some(json!({"model":name,"reason":reason}))
        })
        .collect()
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
        .hours(sadr_core::time::ExchangeDate::parse(&today.to_string())?)?;
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
        .hours(sadr_core::time::ExchangeDate::parse(&session.to_string())?)?
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
    let mut roles = Vec::new();
    let usage: Value;
    let mut usage_unknown = false;
    let mut planner_enabled = false;
    let mut migration_required = false;
    let mut research = json!({"status":"disabled","reason":"research_tools_not_configured"});
    if let Some(path) = c.planner_config.as_deref() {
        match planner::PlannerConfig::load_for_session(path, session) {
            Ok(cfg) => {
                migration_required = cfg.enabled && !cfg.orchestrator_owns_decisions();
                planner_enabled = cfg.enabled;
                if let Some(settings) = cfg.research_tools.as_ref() {
                    // Registered inputs are checked before a post-close review can
                    // discover a stale runtime path (#114).
                    research = super::harness::research_status(settings);
                }
                let fallback = cfg.model_config.clone();
                let now = local(now_ms);
                let has_original_namespace =
                    c.directory.join("model/usage-identity.json").is_file();
                let usage_result = if has_original_namespace {
                    planner::usage(c).ok()
                } else {
                    None
                };
                let digest_deadline = close
                    + chrono::Duration::milliseconds(
                        cfg.digest
                            .invocation_budget_ms("digest")
                            .max(cfg.digest.timeout_ms) as i64,
                    );
                // Digest and orchestrator retain their independent credentials.
                // Old decide usage remains in the journal, never a live peer.
                roles.push(role_report(
                    "digest",
                    &cfg.digest,
                    now,
                    digest_deadline,
                    usage_result.as_ref(),
                    fallback.as_deref(),
                ));
                if let Some(orchestrator) = cfg.orchestrator.as_ref() {
                    use super::orchestrator_model::Trigger;
                    let mut deadline = close;
                    for trigger in [Trigger::Operations, Trigger::Strategy] {
                        if !orchestrator
                            .triggers
                            .get(&trigger)
                            .is_some_and(|settings| settings.enabled)
                        {
                            continue;
                        }
                        let role = orchestrator.role_for(trigger);
                        deadline = deadline.max(
                            close
                                + chrono::Duration::milliseconds(
                                    role.invocation_budget_ms(trigger.as_str())
                                        .max(role.timeout_ms)
                                        as i64,
                                ),
                        );
                    }
                    if let Some(review) = cfg.post_close.as_ref() {
                        let post_close = post_close_deadline(
                            session,
                            now,
                            close,
                            review.cutoff_minute,
                            &orchestrator.role_for(Trigger::Operations),
                            &orchestrator.role_for(Trigger::Strategy),
                        )?;
                        deadline = deadline.max(post_close);
                    }
                    let trigger = [
                        Trigger::Supervisor,
                        Trigger::Operations,
                        Trigger::Strategy,
                        Trigger::Scorer,
                    ]
                    .into_iter()
                    .find(|trigger| {
                        orchestrator
                            .triggers
                            .get(trigger)
                            .is_some_and(|settings| settings.enabled)
                    })
                    .unwrap_or(Trigger::Operations);
                    let mut role = orchestrator.role_for(trigger);
                    role.enabled = orchestrator.enabled
                        && orchestrator
                            .triggers
                            .values()
                            .any(|settings| settings.enabled);
                    let trigger_usage = orchestrator_usage(usage_result.as_ref(), trigger.as_str());
                    roles.push(role_report(
                        "orchestrator",
                        &role,
                        now,
                        deadline,
                        trigger_usage.as_ref(),
                        fallback.as_deref(),
                    ));
                } else if let Some(review) = cfg.post_close {
                    let deadline = post_close_deadline(
                        session,
                        now,
                        close,
                        review.cutoff_minute,
                        &review.operations,
                        &review.strategy,
                    )?;
                    roles.push(role_report(
                        "operations",
                        &review.operations,
                        now,
                        deadline,
                        usage_result.as_ref(),
                        fallback.as_deref(),
                    ));
                    roles.push(role_report(
                        "strategy",
                        &review.strategy,
                        now,
                        deadline,
                        usage_result.as_ref(),
                        fallback.as_deref(),
                    ));
                }
                usage = if usage_result.is_some() {
                    json!({"status":"verified_original_namespace","used":usage_result})
                } else {
                    usage_unknown = true;
                    json!({"status":"unknown","reason":"original_usage_namespace_unavailable"})
                };
            }
            Err(_) => return Err(Fail::new("planner_config_invalid").into()),
        }
    } else {
        usage_unknown = true;
        usage = json!({"status":"unknown","reason":"planner_config_missing"});
        roles.push(json!({"status":"unknown","reason":"planner_config_missing"}));
    }
    let blocked = roles
        .iter()
        .any(|r| r["enabled"] == true && r["status"] == "blocked")
        || !planner_enabled
        || migration_required
        || !roles.iter().any(|r| r["enabled"] == true);
    let unknown = roles
        .iter()
        .any(|r| r["enabled"] == true && r["status"] == "unknown")
        || usage_unknown;
    let status = if blocked {
        "model_roles_blocked"
    } else if unknown {
        "model_roles_unknown"
    } else {
        "model_roles_ready"
    };
    Ok(
        json!({"status":status,"reason":if migration_required {Some("decision_owner_migration_required")} else {None},
        "target_session":session,"previous_session":previous,"opens_at":start,"closes_at":close,
        "planner_enabled":planner_enabled,"roles":roles,"current_usage":usage,
        "research":research,
        "future_usage":{"status":"unknown","reason":"future_session_namespace_not_created"}}),
    )
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
            let request = wide::preparation_request(&future, &instruments, at)?;
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
        let portfolio: sadr_core::execution::Portfolio = serde_json::from_str(&body)?;
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
    assert_eq!(after["roles"][0]["reason"], "planner_config_missing");
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
    assert_eq!(remaining_calls(8, Some(8)), Some(0));
    assert_eq!(remaining_calls(8, None), None);
    assert_eq!(
        role_report(
            "disabled",
            &planner::ModelRole::default(),
            after_close,
            after_close,
            None,
            None,
        )["status"],
        "disabled"
    );
    let planner = root.path().join("disabled-planner.json");
    std::fs::write(&planner, br#"{"enabled":true}"#).unwrap();
    let mut with_planner = c.clone();
    with_planner.planner_config = Some(planner);
    let disabled = report(&with_planner, after_close.timestamp_millis()).unwrap();
    assert_eq!(disabled["status"], "model_roles_blocked");
    assert_eq!(disabled["planner_enabled"], true);

    let disabled_roles = root.path().join("disabled-roles.json");
    std::fs::write(
        &disabled_roles,
        br#"{"enabled":true,"digest":{"enabled":false},"decide":{"enabled":false}}"#,
    )
    .unwrap();
    let mut no_roles = with_planner.clone();
    no_roles.planner_config = Some(disabled_roles);
    let no_roles_report = report(&no_roles, after_close.timestamp_millis()).unwrap();
    assert_eq!(no_roles_report["status"], "model_roles_blocked");
    assert_eq!(no_roles_report["planner_enabled"], true);
    assert_eq!(no_roles_report["roles"][0]["status"], "disabled");
    // A disabled role still reports the model the policy declares for it, so the
    // dashboard shows the switch target without a second configuration read.
    assert_eq!(no_roles_report["roles"][0]["model"], "");
    assert_eq!(
        no_roles_report["roles"][0]["transport"],
        "subscription_proxy"
    );
    assert_eq!(no_roles_report["roles"].as_array().unwrap().len(), 1);

    // A stale registered runtime is reported as a blocked research status with
    // the exact artifact code, not hidden until the post-close review call.
    let research_code = root.path().join("research-code.py");
    let research_input = root.path().join("research-input.json");
    std::fs::write(&research_code, b"code").unwrap();
    std::fs::write(&research_input, b"input").unwrap();
    let research_planner = root.path().join("research-planner.json");
    std::fs::write(
        &research_planner,
        serde_json::to_vec(&json!({
            "enabled":true,
            "research_tools":{
                "queue":root.path().join("research.sqlite"),
                "stage_root":root.path().join("research-stage"),
                "registrations":[{
                    "analysis_id":"shared_core_backtest",
                    "dataset_id":"synthetic_no_quotes_fixture",
                    "code_file":research_code,
                    "runtime_file":root.path().join("stale-sadr-analysis"),
                    "input_file":research_input,
                    "code_sha256":digest(b"code"),
                    "runtime_sha256":digest(b"runtime"),
                    "input_sha256":digest(b"input"),
                    "memory_bytes":67108864,"wall_millis":5000,
                    "limits":{"cpu_millis":1000,"tasks":2,"workspace_bytes":1048576,"file_bytes":65536}
                }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let mut research_config = with_planner.clone();
    research_config.planner_config = Some(research_planner);
    let research_report = report(&research_config, after_close.timestamp_millis()).unwrap();
    assert_eq!(research_report["research"]["status"], "blocked");
    assert_eq!(
        research_report["research"]["registrations"][0]["reason"],
        "research_runtime_unreadable"
    );
    assert_eq!(
        research_report["research"]["registrations"][0]["files"][1]["artifact"],
        "runtime"
    );

    // Internal callers must receive an error for an unrepresentable clock.
    assert_eq!(
        report(&c, i64::MAX).unwrap_err().to_string(),
        "readiness_clock_range"
    );
}

#[cfg(test)]
mod warning_tests {
    use super::*;

    #[test]
    fn session_credential_warnings_are_independent_of_usage_and_redacted() {
        // Validate cadence without opening any path or creating auth fixtures.
        let mut config = super::super::tests::config(Path::new("/synthetic-unused"));
        assert_eq!(config.model_readiness_check_interval_ms, 60_000);
        assert!(config.validate().is_ok());
        config.model_readiness_check_interval_ms = 1234;
        assert!(config.validate().is_ok());
        for invalid in [0, -1] {
            config.model_readiness_check_interval_ms = invalid;
            assert!(config.validate().is_err());
        }
        let models = json!({"roles":[
            {"role":"digest","enabled":true,"status":"unknown",
             "reason":"usage_namespace_unavailable",
             "credential_check":{"status":"unknown","secret":"synthetic-private"}},
            {"role":"decide","enabled":true,"status":"blocked",
             "reason":"grant_login_required"},
            {"role":"operations","enabled":true,"status":"blocked",
             "reason":"daily_budget_exhausted","credential_check":{"status":"validated"}},
            {"role":"strategy","enabled":false,"status":"blocked"}
        ]});
        assert_eq!(
            credential_warnings(&models),
            vec![
                json!({"model":"digest","reason":"grant_refresh_required_or_expiry_unverified"}),
                json!({"model":"decide","reason":"grant_login_required"})
            ]
        );
        assert!(credential_warnings(&json!({})).is_empty());
        assert_eq!(
            credential_warnings(&json!({"roles":[
                {"role":"strategy","enabled":true,"status":"blocked",
                 "reason":"provider text is not an alert code"}
            ]})),
            vec![json!({"model":"strategy","reason":"model_credential_validation_failed"})]
        );
        assert!(
            credential_warnings(&json!({"roles":[
                {"role":"decide","enabled":true,"status":"ready",
                 "credential_check":{"status":"validated"}}
            ]}))
            .is_empty()
        );
    }
}

#[cfg(test)]
mod orchestrator_tests {
    use super::*;

    fn report_for(root: &Path, planner: &str) -> Value {
        let planner_path = root.join("planner.json");
        std::fs::write(&planner_path, planner).unwrap();
        let c: Config = serde_json::from_value(json!({
            "account":"tharun-upstox","session":"2026-10-16","previous_session":"2026-10-15",
            "calendar_source":"NSE/CMTR/71775","directory":root,
            "token_file":root.join("unused-token"),
            "dashboard_token_file":root.join("unused-dashboard"),
            "read_journal":root.join("unused-journal"),
            "read_scope":"upstox-market-tharun-upstox","read_journal_device":0,
            "read_journal_inode":0,"strategy":"model_planner_v1",
            "planner_config":planner_path
        }))
        .unwrap();
        let at = DateTime::parse_from_rfc3339("2026-10-21T15:30:01+05:30").unwrap();
        report(&c, at.timestamp_millis()).unwrap()
    }

    #[test]
    fn two_live_readiness_identities_preserve_legacy_usage() {
        let root = tempfile::tempdir_in("/var/tmp").unwrap();
        let value = report_for(
            root.path(),
            r#"{"enabled":true,"digest":{"enabled":true,"model_id":"gpt-6-luna","transport":"openai_codex","daily_call_budget":5},"decide":{"enabled":true,"model_id":"grok-4.7","transport":"direct_oauth","daily_call_budget":5},"orchestrator":{"enabled":true,"scorer_after_minute":960,"model":{"model_id":"grok-4.7","transport":"direct_oauth"}}}"#,
        );
        let roles = value["roles"].as_array().unwrap();
        assert_eq!(roles.len(), 2, "{value}");
        assert_eq!(roles[0]["role"], "digest");
        assert_eq!(roles[1]["role"], "orchestrator");
        assert!(roles.iter().all(|role| role["enabled"] == true));
        // This fixture previews a future session without an original namespace.
        assert_eq!(roles[1]["current_usage"], Value::Null, "{value}");
        assert_eq!(roles[1]["remaining_current"], Value::Null);
        assert_ne!(roles[1]["reason"], "usage_namespace_unavailable");

        // Trigger counts, rather than a new role namespace, carry existing usage.
        let zero = orchestrator_usage(Some(&json!({"operations":0})), "operations").unwrap();
        assert_eq!(remaining_calls(3, zero["orchestrator"].as_u64()), Some(3));
        let usage = json!({"digest":4,"decide":2,"operations":3,"strategy":1});
        let before = usage.clone();
        for (trigger, count) in [("operations", 3), ("strategy", 1)] {
            let projected = orchestrator_usage(Some(&usage), trigger).unwrap();
            assert_eq!(projected["orchestrator"], count);
            assert_eq!(
                remaining_calls(3, projected["orchestrator"].as_u64()),
                Some(3 - count)
            );
        }
        assert_eq!(usage, before);
        assert!(orchestrator_usage(None, "operations").is_none());
        assert_eq!(
            orchestrator_usage(Some(&json!({})), "operations").unwrap()["orchestrator"],
            Value::Null
        );

        // Legacy configurations still keep their two role reports.
        let legacy = report_for(
            root.path(),
            r#"{"enabled":true,"digest":{"enabled":false},"decide":{"enabled":false}}"#,
        );
        let roles = legacy["roles"].as_array().unwrap();
        assert_eq!(roles.len(), 1, "{legacy}");
        assert_eq!(roles[0]["role"], "digest");
    }
}
