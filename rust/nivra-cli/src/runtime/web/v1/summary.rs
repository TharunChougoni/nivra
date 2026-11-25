use super::*;

pub(crate) fn portfolio_equity(portfolio: &serde_json::Value) -> Option<String> {
    serde_json::from_value::<Portfolio>(portfolio.clone())
        .ok()
        .and_then(|p| p.equity().ok())
        .map(|equity| equity.to_string())
}
pub(crate) fn portfolio_positions(portfolio: &serde_json::Value) -> Vec<serde_json::Value> {
    portfolio["positions"]
        .as_object()
        .map(|positions| positions.values().cloned().collect())
        .unwrap_or_default()
}
pub(crate) fn fill_fees(portfolio: &serde_json::Value) -> Option<String> {
    let fills = portfolio["fills"].as_array()?;
    fills
        .iter()
        .try_fold(nivra_core::finance::Amount::ZERO, |sum, fill| {
            let fee = nivra_core::finance::Amount::parse(fill["fee"].as_str()?).ok()?;
            sum.checked_add(fee).ok()
        })
        .map(|total| total.to_string())
}
pub(crate) fn expected_operator_stop(portfolio: &serde_json::Value) -> bool {
    portfolio["stopped"] == true && portfolio["halted"] == false && portfolio["closed"] == false
}
pub(crate) fn stop_classification(portfolio: &serde_json::Value) -> Option<&'static str> {
    (portfolio["stopped"] == true).then(|| {
        if expected_operator_stop(portfolio) {
            "expected_not_failure"
        } else {
            "operator_review_required"
        }
    })
}
pub(crate) fn derived_alerts(view: &serde_json::Value) -> Vec<serde_json::Value> {
    use serde_json::json;
    let at = view["as_of_ms"].as_i64().unwrap_or(0);
    let mut alerts = Vec::new();
    let mut push = |id: &str, severity: &str, message: &str, source: &str| {
        alerts.push(json!({
            "id":id,"severity":severity,"message":message,"source":source,
            "observed_at_ms":at,"delivery":"workspace_only","execution_authority":false
        }));
    };
    if view["portfolio"]["halted"] == true {
        push(
            "book_halted",
            "critical",
            "The paper book is halted; protective exits only.",
            "portfolio",
        );
    }
    if view["portfolio"]["stopped"] == true {
        push(
            "book_stopped",
            if expected_operator_stop(&view["portfolio"]) {
                "info"
            } else {
                "critical"
            },
            if expected_operator_stop(&view["portfolio"]) {
                "Expected protection (expected_not_failure): the operator-stopped paper book requires explicit reviewed resume."
            } else {
                "The paper controller is stopped; inspect original halt and book state."
            },
            "portfolio",
        );
    }
    if view["portfolio"]["closed"] == true {
        push(
            "book_closed",
            "info",
            "The paper book is closed.",
            "portfolio",
        );
    }
    if view["read_failure_reason"].is_string() {
        push(
            "read_failure",
            "error",
            "The last market read failed; a valid receipt is required.",
            "market_data",
        );
    }
    if matches!(
        view["market_feed"]["state"].as_str(),
        Some("stale" | "unavailable" | "disconnected")
    ) {
        push(
            "feed_stale",
            "warning",
            "The market feed is not currently fresh.",
            "market_feed",
        );
    }
    if view["readiness"]["status"]
        .as_str()
        .is_some_and(|status| status.starts_with("model_roles_blocked"))
    {
        push(
            "model_roles_blocked",
            "warning",
            "At least one configured model identity is blocked.",
            "readiness",
        );
    }
    if view["history_preparation"]["status"]
        .as_str()
        .is_some_and(|status| matches!(status, "PARTIAL" | "FAILED" | "BLOCKED"))
    {
        push(
            "history_partial",
            "info",
            "History preparation is incomplete; gaps stay explicit.",
            "history_preparation",
        );
    }
    alerts
}
#[derive(Clone, Debug)]
pub(crate) struct PreparedSummary {
    pub(crate) json: String,
    pub(crate) gz: Vec<u8>,
    pub(crate) etag: String,
    pub(crate) revisions: serde_json::Value,
}

static PREPARED_SUMMARY: std::sync::RwLock<Option<Arc<PreparedSummary>>> =
    std::sync::RwLock::new(None);
static PUBLISH_GATE: Mutex<PublishGate> = Mutex::new(PublishGate::new());

/// Least time between two `summary_changed` frames.
const ANNOUNCE_MIN_MS: i64 = 1_000;
/// A summary with no announced change is still announced this often, so its
/// own figures (feed age, cash, clock) keep refreshing.
const REFRESH_MS: i64 = 5_000;
/// Least time between two announced changes of one revision; others have none.
const ROLL_MIN_MS: &[(&str, i64)] = &[("scanner", 15_000)];

/// Chooses which revisions clients are told about, and when.
pub(super) struct PublishGate {
    /// Each announced revision with the time it last rolled forward.
    announced: std::collections::BTreeMap<String, (serde_json::Value, i64)>,
    /// When `summary_changed` last fired.
    announced_at_ms: Option<i64>,
}

impl PublishGate {
    pub(super) const fn new() -> Self {
        Self {
            announced: std::collections::BTreeMap::new(),
            announced_at_ms: None,
        }
    }

    /// Folds in the current revisions at `now_ms` and says whether to fire
    /// `summary_changed`. A revision rolls forward only when its minimum
    /// interval has passed and the frame fires.
    pub(super) fn step(&mut self, current: &serde_json::Value, now_ms: i64) -> bool {
        let since = |at: i64| now_ms.saturating_sub(at);
        let rolled = current
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, revision)| match self.announced.get(key.as_str()) {
                None => true,
                Some((held, at)) => {
                    let min = ROLL_MIN_MS
                        .iter()
                        .find(|(name, _)| name == key)
                        .map_or(0, |(_, min)| *min);
                    held != *revision && since(*at) >= min
                }
            })
            .collect::<Vec<_>>();
        let quiet = self.announced_at_ms.map_or(i64::MAX, since);
        let fire = (!rolled.is_empty() && quiet >= ANNOUNCE_MIN_MS) || quiet >= REFRESH_MS;
        if fire {
            for (key, revision) in rolled {
                self.announced
                    .insert(key.clone(), (revision.clone(), now_ms));
            }
            self.announced_at_ms = Some(now_ms);
        }
        fire
    }

    /// The revisions the summary body embeds: the announced ones, never
    /// newer, because clients compare them to choose what to fetch.
    pub(super) fn announced(&self) -> serde_json::Value {
        self.announced
            .iter()
            .map(|(key, (revision, _))| (key.clone(), revision.clone()))
            .collect::<serde_json::Map<_, _>>()
            .into()
    }
}

pub(crate) fn update_prepared_summary(
    view: &serde_json::Value,
    root: Option<&Path>,
) -> Arc<PreparedSummary> {
    replace_prepared_summary(view, revisions(view, root))
}

/// Prepares the summary every loop and announces it when [`PublishGate`]
/// fires.
pub(crate) fn publish_summary(view: &serde_json::Value, root: Option<&Path>) {
    let current = revisions(view, root);
    let (fire, announced) = {
        let mut gate = match PUBLISH_GATE.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        (gate.step(&current, millis()), gate.announced())
    };
    let prepared = replace_prepared_summary(view, announced);
    if fire {
        nivra_cli::event_feed::summary_changed(&prepared.etag, &prepared.revisions);
    }
}

fn replace_prepared_summary(
    view: &serde_json::Value,
    revisions: serde_json::Value,
) -> Arc<PreparedSummary> {
    let mut body = build_summary_envelope(view);
    body["revisions"] = revisions.clone();
    let body = super::super::super::transcript::redact(&body);
    let text = body.to_string();
    let etag = format!("\"{}\"", &digest(text.as_bytes())[..16]);
    let gz = super::super::gzip(text.as_bytes(), 1);
    let prepared = Arc::new(PreparedSummary {
        json: text,
        gz,
        etag,
        revisions,
    });
    let mut lock = match PREPARED_SUMMARY.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *lock = Some(Arc::clone(&prepared));
    prepared
}

pub(crate) fn get_prepared_summary() -> Option<Arc<PreparedSummary>> {
    let lock = match PREPARED_SUMMARY.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    lock.clone()
}

#[cfg(test)]
pub(crate) fn reset_prepared_summary() {
    let mut lock = match PREPARED_SUMMARY.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *lock = None;
    *PUBLISH_GATE.lock().unwrap() = PublishGate::new();
}

pub(crate) fn build_summary_envelope(view: &serde_json::Value) -> serde_json::Value {
    let portfolio = &view["portfolio"];
    let mut body = envelope(view);
    body["status"] = view["status"].clone();
    body["model_status"] = view["model_status"].clone();
    body["entry_status"] = view["entry_status"].clone();
    body["model_led"] = view["model_led"].clone();
    body["capital"] = view["capital"].clone();
    body["cash"] = portfolio["cash"].clone();
    body["equity"] = portfolio_equity(portfolio)
        .map(serde_json::Value::String)
        .unwrap_or(serde_json::Value::Null);
    body["counts"] = serde_json::json!({
        "positions":portfolio["positions"].as_object().map_or(0,|v|v.len()),
        "fills":portfolio["fills"].as_array().map_or(0,|v|v.len()),
        "universe":view["universe_size"],
        "scanned":view["scanned_count"],
        "pending":view["pending_count"]
    });
    body["state"] = serde_json::json!({
        "stopped":portfolio["stopped"],"halted":portfolio["halted"],"closed":portfolio["closed"],
        "health":portfolio["health"],"phase":view["status"],"stop_classification":stop_classification(portfolio)
    });
    body["market_feed"] = view["market_feed"].clone();
    body["instrument_display"] = view["instrument_display"].clone();
    body["readiness"] = serde_json::json!({
        "status":view["readiness"]["status"],"reason":view["readiness"]["lifecycle"]["reason"],
        "history":view["readiness"]["history"]
    });
    body["planner"] = serde_json::json!({
        "status":view["planner"]["status"],"decision_id":view["planner"]["decision_id"],
        "pending_attempt":view["planner"]["pending_attempt"],"pending_events":view["planner"]["pending_events"]
    });
    body["research_library"] = view["research_library"].clone();
    // Copy the controller's already-captured read-only projections verbatim.
    // This GET never samples a clock, reads the queue/state, recomputes the
    // research status or renews freshness; an absent field stays absent.
    if let Some(value) = view.get("research_tasks") {
        body["research_tasks"] = value.clone();
    }
    if let Some(operational) = view.get("operational_health") {
        body["operational_health"] = operational.clone();
    }
    body["oldest_scan_age_ms"] = view["oldest_scan_age_ms"].clone();
    let alerts = derived_alerts(view);
    body["alert_count"] = serde_json::json!(alerts.len());
    body["alerts"] = serde_json::json!(alerts);
    body
}

pub(crate) fn summary(
    head: &str,
    view: &Arc<Mutex<serde_json::Value>>,
    root: Option<&Path>,
) -> Response {
    let prepared = if let Some(p) = get_prepared_summary() {
        p
    } else {
        let Ok(view) = view.lock() else {
            return error("503 Service Unavailable", "status_unavailable");
        };
        update_prepared_summary(&view, root)
    };
    let matched = values(head, "if-none-match") == [prepared.etag.as_str()];
    if matched {
        let mut reply = response("304 Not Modified", "application/json", "");
        reply.cache = Cow::Borrowed("private, no-cache");
        reply.etag = Some(prepared.etag.clone());
        return reply;
    }
    let (body, encoding) = if super::super::accepts_gzip(head) {
        (prepared.gz.clone(), Some("gzip"))
    } else {
        (prepared.json.as_bytes().to_vec(), None)
    };
    let mut reply = response("200 OK", "application/json", body);
    reply.cache = Cow::Borrowed("private, no-cache");
    reply.etag = Some(prepared.etag.clone());
    if let Some(enc) = encoding {
        reply.encoding = Some(enc);
    }
    reply.vary = true;
    reply
}
