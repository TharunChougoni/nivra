//! Controller shutdown intent. Only an explicit `nivra stop` request turns a
//! SIGTERM/SIGINT into an operator stop; every other stop is a service stop
//! (restart, updater, host shutdown) and the same dated session continues when
//! the controller comes back.
use super::*;

/// Why the controller is exiting, decided once when the stop signal arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShutdownKind {
    /// The operator asked for the book to stop; only a reviewed resume restarts it.
    Operator,
    /// The service stopped; the book is not stopped and restart continues it.
    Service,
}

impl ShutdownKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Service => "service",
        }
    }
}

/// A private operator-stop request bound to one account and dated session.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StopRequest {
    pub(crate) account: String,
    pub(crate) session: NaiveDate,
    pub(crate) requested_at_ms: i64,
}

/// A request older than this is stale: the stop it asked for never arrived.
pub(crate) const STOP_REQUEST_TTL_MS: i64 = 600_000;
const STOP_REQUEST_FILE: &str = "stop-request.json";
const STOP_REQUEST_ARCHIVE: &str = "stop-requests";

pub(crate) fn request_path(c: &Config) -> Result<PathBuf> {
    Ok(rollover::root(c)?.join(STOP_REQUEST_FILE))
}

/// Record the operator's stop request for the controller to consume on SIGTERM.
pub(super) fn write_request(c: &Config, now: i64) -> Result<StopRequest> {
    let request = StopRequest {
        account: c.account.clone(),
        session: c.session,
        requested_at_ms: now,
    };
    let path = request_path(c)?;
    fs::create_dir_all(path.parent().ok_or("stop_request_root")?)?;
    rollover::atomic(&path, &serde_json::to_value(&request)?)?;
    Ok(request)
}

/// The shutdown decision plus the request evidence for the journal.
pub(super) struct Intent {
    pub(super) kind: ShutdownKind,
    pub(super) request: serde_json::Value,
    /// Disposition of a cancelled history preparation, when one was running.
    pub(super) history: serde_json::Value,
}

/// Decide the shutdown kind. A request bound to this account, session and a
/// fresh time is consumed as an operator stop; a stale, foreign or malformed
/// one is ignored. Either way the file is archived as evidence, never deleted.
pub(super) fn intent(c: &Config, now: i64) -> Intent {
    let Ok(path) = request_path(c) else {
        return Intent {
            kind: ShutdownKind::Service,
            request: serde_json::json!({"status":"unavailable"}),
            history: serde_json::Value::Null,
        };
    };
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Intent {
                kind: ShutdownKind::Service,
                request: serde_json::json!({"status":"absent"}),
                history: serde_json::Value::Null,
            };
        }
        _ => {}
    }
    let parsed = private(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<StopRequest>(&raw).ok());
    let verdict = match &parsed {
        None => Err("stop_request_invalid"),
        Some(r) if r.account != c.account || r.session != c.session => {
            Err("stop_request_foreign_session")
        }
        Some(r) if r.requested_at_ms > now || now - r.requested_at_ms > STOP_REQUEST_TTL_MS => {
            Err("stop_request_stale")
        }
        Some(_) => Ok(()),
    };
    let label = if verdict.is_ok() {
        "consumed"
    } else {
        "ignored"
    };
    let archived = archive(c, &path, now, label);
    Intent {
        kind: if verdict.is_ok() {
            ShutdownKind::Operator
        } else {
            ShutdownKind::Service
        },
        request: serde_json::json!({
            "status":label,
            "reason":verdict.err(),
            "request":parsed,
            "archived":archived.ok(),
        }),
        history: serde_json::Value::Null,
    }
}

fn archive(c: &Config, path: &Path, now: i64, label: &str) -> Result<String> {
    let dir = rollover::root(c)?.join(STOP_REQUEST_ARCHIVE);
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    let name = format!("{now}-{label}.json");
    let target = dir.join(&name);
    if fs::symlink_metadata(&target).is_ok() {
        return Err("stop_request_archive_exists".into());
    }
    // rename moves the directory entry itself, so a symlink is archived, not followed.
    fs::rename(path, &target)?;
    fs::File::open(&dir)?.sync_all()?;
    Ok(name)
}

/// Settle the book and planner journal for a controller shutdown and return
/// the Stop alert details. `in_flight` is the claim whose worker was cancelled.
pub(super) fn settle(
    db: &mut Connection,
    c: &Config,
    p: &mut Portfolio,
    intent: &Intent,
    in_flight: Option<&redecision::Claim>,
    now: i64,
) -> Result<serde_json::Value> {
    match intent.kind {
        ShutdownKind::Operator => {
            if in_flight.is_some()
                && redecision::block(db, c, "operator_stopped_no_replay").is_err()
            {
                p.halted = true;
            }
            mark_operator_stopped(p);
            persist(db, c, p, now, None, None, "operator_stopped")?;
        }
        ShutdownKind::Service => {
            let planner = match in_flight {
                None => serde_json::Value::Null,
                Some(claim) => match redecision::retire_for_service_shutdown(db, c, claim) {
                    Ok(()) => serde_json::json!({
                        "attempt_id":claim.id,"retired":true,
                        "no_replay":true,"charges_preserved":true
                    }),
                    Err(error) => {
                        // Retirement could not be proven safe: keep the claim
                        // pending behind the reviewed interrupted-attempt block.
                        let fallback = redecision::ReviewedBlock::AttemptInterrupted.code();
                        let blocked = redecision::block(db, c, fallback).is_ok();
                        if !blocked {
                            p.halted = true;
                        }
                        serde_json::json!({
                            "attempt_id":claim.id,"retired":false,
                            "reason":failure_code(error.as_ref()),
                            "fallback_block":fallback,"fallback_block_recorded":blocked,
                            "no_replay":true
                        })
                    }
                },
            };
            let body = serde_json::json!({
                "shutdown":ShutdownKind::Service.as_str(),
                "stopped":p.stopped,
                "planner":planner,
                "history_preparation":intent.history,
                "stop_request":intent.request,
            });
            persist(
                db,
                c,
                p,
                now,
                None,
                Some(&body.to_string()),
                "service_shutdown",
            )?;
        }
    }
    Ok(serde_json::json!({
        "closed":p.closed,
        "open_unresolved":p.positions.len(),
        "operator_stopped":intent.kind == ShutdownKind::Operator,
        "stopped":p.stopped,
        "halted":p.halted,
        "shutdown":intent.kind.as_str(),
    }))
}
