//! Read-only evidence from the configured paper session's shadow run.
use super::protocol::Usage;
use super::triggers::{Trigger, TriggerKind};
use super::{AgentMode, Lane, lanes};
use crate::runtime::{Config, Result, book_binding, digest, open_book_immutable, rollover};
use chrono::{DateTime, NaiveDate};
use nivra_core::finance::Amount;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const MAX_JOURNAL_BYTES: u64 = 16_000_000;
const MAX_ROWS: usize = 4_096;

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Paper runtime config used to resolve the current book.
    #[arg(long)]
    config: PathBuf,
}

#[derive(Deserialize)]
struct TriggerRow {
    lane: Lane,
    #[serde(flatten)]
    event: TriggerEvent,
}

#[derive(Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum TriggerEvent {
    Fired {
        trigger: Trigger,
    },
    Dispatched {
        at_ms: i64,
        triggers: Vec<Trigger>,
    },
    Skipped {
        reason: String,
        triggers: Vec<Trigger>,
    },
    Deferred {
        at_ms: i64,
    },
}

#[derive(Deserialize)]
struct TurnRow {
    turn_id: String,
    lane: Lane,
    trigger: Batch,
    at_ms: i64,
    status: TurnStatus,
    usage: Option<Usage>,
    plan: Option<Value>,
}

#[derive(Deserialize)]
struct Batch {
    reason: String,
    triggers: Vec<Trigger>,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum TurnStatus {
    Ok,
    Error,
    Aborted,
    Refused,
}

#[derive(Deserialize)]
struct PlanRow {
    status: String,
    record: PlanRecord,
}

#[derive(Deserialize)]
struct PlanRecord {
    turn_id: String,
    mode: AgentMode,
    at_ms: i64,
    attempt: Value,
}

#[derive(Default, Serialize)]
struct Actions {
    waits: usize,
    proposed: usize,
    validated: usize,
    rejected: usize,
}

impl Actions {
    fn count(&mut self, plan: &Value, verdicts: &Value) -> Option<usize> {
        let proposed = plan["actions"].as_array()?.len();
        let verdicts = verdicts.as_array()?;
        if proposed != verdicts.len()
            || !verdicts.iter().enumerate().all(|(index, verdict)| {
                verdict["index"].as_u64() == Some(index as u64)
                    && verdict
                        .get("action")
                        .is_some_and(|action| action.is_object() || action.is_null())
            })
        {
            return None;
        }
        let validated = verdicts
            .iter()
            .filter(|row| row["action"].is_object())
            .count();
        self.waits += usize::from(proposed == 0);
        self.proposed += proposed;
        self.validated += validated;
        self.rejected += proposed - validated;
        Some(proposed - validated)
    }
}

#[derive(Default, Serialize)]
struct Coverage {
    fired: usize,
    dispatched: usize,
    validated_turns: usize,
}

impl Coverage {
    fn contains(batch: &[Trigger], reason: &str) -> bool {
        batch
            .iter()
            .any(|trigger| trigger.kind == TriggerKind::Schedule && trigger.reason == reason)
    }
}

fn in_session(at_ms: i64, session: NaiveDate) -> bool {
    // Treat the IST wall clock as UTC only to extract its date safely.
    at_ms
        .checked_add(19_800_000)
        .and_then(DateTime::from_timestamp_millis)
        .is_some_and(|time| time.date_naive() == session)
}

fn read_json<T: DeserializeOwned>(path: &Path, limit: u64) -> Option<T> {
    serde_json::from_slice(&crate::read_bounded(path, limit).ok()?).ok()
}

fn journal<T: DeserializeOwned>(path: &Path) -> Option<Vec<T>> {
    let bytes = crate::read_bounded(path, MAX_JOURNAL_BYTES).ok()?;
    let mut rows = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !line.ends_with(b"\n") || rows.len() == MAX_ROWS {
            return None;
        }
        rows.push(serde_json::from_slice(line).ok()?);
    }
    Some(rows)
}

fn decisions(
    session: NaiveDate,
    triggers: &[TriggerRow],
    turns: &[TurnRow],
    plans: Option<&[PlanRow]>,
) -> Value {
    let mut open = Coverage::default();
    let mut pre_close = Coverage::default();
    let mut dispatches = Vec::new();
    let (mut skipped, mut no_evidence, mut budget, mut worker, mut deferred) = (0, 0, 0, 0, 0);
    for row in triggers.iter().filter(|row| row.lane == Lane::Decision) {
        match &row.event {
            TriggerEvent::Fired { trigger } if in_session(trigger.observed_ms, session) => {
                open.fired +=
                    usize::from(Coverage::contains(std::slice::from_ref(trigger), "open"));
                pre_close.fired += usize::from(Coverage::contains(
                    std::slice::from_ref(trigger),
                    "pre_close",
                ));
            }
            TriggerEvent::Dispatched { at_ms, triggers } if in_session(*at_ms, session) => {
                open.dispatched += usize::from(Coverage::contains(triggers, "open"));
                pre_close.dispatched += usize::from(Coverage::contains(triggers, "pre_close"));
                dispatches.push((*at_ms, triggers.as_slice(), false));
            }
            TriggerEvent::Skipped { reason, triggers }
                if triggers
                    .iter()
                    .any(|trigger| in_session(trigger.observed_ms, session)) =>
            {
                skipped += 1;
                no_evidence += usize::from(reason == "agent_no_eligible_evidence");
                budget += usize::from(reason == "agent_budget_exhausted");
                worker += usize::from(reason == "agent_worker_unavailable");
            }
            TriggerEvent::Deferred { at_ms } if in_session(*at_ms, session) => deferred += 1,
            _ => {}
        }
    }

    let mut ids = BTreeSet::new();
    let mut matched_plan_ids = BTreeSet::new();
    let (mut manual, mut unmatched, mut failures, mut matched) = (0, 0, 0, 0);
    let (mut validated, mut refused, mut missing, mut unverified, mut malformed) = (0, 0, 0, 0, 0);
    let (mut usage_rows, mut calls, mut cost) = (0, Some(0u64), 0.0);
    let mut actions = Actions::default();
    for turn in turns
        .iter()
        .filter(|row| row.lane == Lane::Decision && in_session(row.at_ms, session))
    {
        if turn.trigger.reason == "agent_turn" {
            manual += 1;
            continue;
        }
        if !ids.insert(turn.turn_id.as_str()) {
            unmatched += 1;
            continue;
        }
        let Some((_, batch, seen)) = dispatches.iter_mut().rev().find(|(at, triggers, seen)| {
            !*seen
                && *at <= turn.at_ms
                && !triggers.is_empty()
                && *triggers == turn.trigger.triggers
        }) else {
            unmatched += 1;
            continue;
        };
        *seen = true;
        matched += 1;
        let ok = turn.status == TurnStatus::Ok;
        failures += usize::from(!ok);
        if let Some(usage) = turn
            .usage
            .as_ref()
            .filter(|usage| usage.cost_usd.is_finite() && usage.cost_usd >= 0.0)
        {
            usage_rows += 1;
            calls = calls.and_then(|sum| sum.checked_add(usage.calls));
            cost += usage.cost_usd;
        }

        let attempt = turn.plan.as_ref();
        let mut receipts = plans
            .into_iter()
            .flatten()
            .filter(|row| row.record.turn_id == turn.turn_id);
        let receipt = receipts.next();
        if attempt.is_none() && receipt.is_none() {
            missing += usize::from(ok);
            continue;
        }
        let Some((attempt, receipt)) = attempt.zip(receipt).filter(|(attempt, row)| {
            row.record.mode == AgentMode::Shadow
                && row.record.at_ms >= turn.at_ms
                && &row.record.attempt == *attempt
                && receipts.next().is_none()
        }) else {
            unverified += 1;
            continue;
        };
        matched_plan_ids.insert(receipt.record.turn_id.as_str());
        match (receipt.status.as_str(), attempt["status"].as_str()) {
            ("shadow_not_executed", Some("validated")) => {
                if let Some(rejected) = actions.count(&attempt["plan"], &attempt["verdicts"]) {
                    if ok && rejected == 0 {
                        validated += 1;
                        open.validated_turns += usize::from(Coverage::contains(batch, "open"));
                        pre_close.validated_turns +=
                            usize::from(Coverage::contains(batch, "pre_close"));
                    }
                } else {
                    malformed += 1;
                }
            }
            ("refused_by_validation", Some("refused")) => refused += 1,
            _ => malformed += 1,
        }
    }
    let unpaired_plans = plans.map(|rows| {
        rows.iter()
            .filter(|row| {
                row.record.turn_id.starts_with("decision-")
                    && in_session(row.record.at_ms, session)
                    && !matched_plan_ids.contains(row.record.turn_id.as_str())
            })
            .count()
    });
    json!({
        "open": open, "pre_close": pre_close,
        "skips": {"total": skipped, "no_eligible_evidence": no_evidence, "budget_exhausted": budget,
            "worker_unavailable": worker},
        "context_deferrals": deferred,
        "dispatches": dispatches.len(), "matched_turns": matched,
        "unmatched_dispatches": dispatches.iter().filter(|(_, _, seen)| !seen).count(),
        "unmatched_turn_rows": unmatched, "manual_turns_excluded": manual, "failed_turns": failures,
        "plans": {"validated_successful_turns": validated, "refused": refused, "missing_on_success": missing,
            "unverified": unverified, "malformed": malformed, "unpaired_receipts": unpaired_plans,
            "actions": actions},
        "journaled_usage": {"rows": usage_rows, "missing_rows": matched - usage_rows,
            "calls": calls.filter(|_| usage_rows > 0), "cost_usd": (usage_rows > 0 && cost.is_finite()).then_some(cost)},
    })
}

#[derive(Deserialize)]
struct Ledger {
    date: NaiveDate,
    lanes: BTreeMap<Lane, Usage>,
    #[serde(default)]
    in_flight: BTreeMap<String, InFlight>,
}

#[derive(Deserialize)]
struct InFlight {
    lane: Lane,
    usage: Usage,
}

fn ledger(path: &Path, session: NaiveDate) -> Option<Value> {
    let ledger: Ledger = read_json(path, 1_048_576)?;
    (ledger.date == session).then(|| {
        json!({
            "scope": "controller_and_manual",
            "settled": ledger.lanes.get(&Lane::Decision),
            "in_flight": ledger.in_flight.values().filter(|row| row.lane == Lane::Decision)
                .map(|row| &row.usage).collect::<Vec<_>>(),
        })
    })
}

fn sealed_report(c: &Config) -> Option<(Value, Value)> {
    let bytes = crate::read_bounded(&c.directory.join("report.json"), MAX_JOURNAL_BYTES).ok()?;
    let report: Value = serde_json::from_slice(&bytes).ok()?;
    let seal: Value = read_json(&c.directory.join("review.json"), 1_048_576)?;
    let session = c.session.to_string();
    (report["session"] == session
        && report["account"] == c.account
        && report["broker"] == "upstox"
        && report["paper_only"] == true
        && report["live_enabled"] == false
        && report["portfolio"]["closed"] == true
        && seal["session"] == session
        && seal["closed"] == true
        && seal["report_hash"] == digest(&bytes))
    .then_some((report, seal))
}

fn owner_plans(c: &Config, report: &Value, seal: &Value) -> Result<Value> {
    let db = open_book_immutable(&c.directory.join("paper.sqlite"))?;
    let binding = book_binding(c)?;
    let state: String = db.query_row(
        "SELECT body FROM state WHERE id=1 AND binding=? AND length(CAST(body AS BLOB))<=?",
        rusqlite::params![binding, MAX_JOURNAL_BYTES as i64],
        |row| row.get(0),
    )?;
    if seal["state_hash"] != digest(state.as_bytes())
        || report["portfolio"] != serde_json::from_str::<Value>(&state)?
    {
        return Err("agent_shadow_report_state_mismatch".into());
    }
    let mut stmt = db.prepare(
        "SELECT state, CASE WHEN length(result)<=65536 THEN result END
        FROM owner_digest_work WHERE binding=? LIMIT ?",
    )?;
    let mut rows = stmt.query(rusqlite::params![binding, MAX_ROWS as i64 + 1])?;
    let mut states = BTreeMap::<&str, usize>::new();
    let mut actions = Actions::default();
    let mut missing = 0;
    let mut decisions = 0;
    while let Some(row) = rows.next()? {
        decisions += 1;
        if decisions > MAX_ROWS {
            return Err("agent_shadow_report_owner_limit".into());
        }
        let state: String = row.get(0)?;
        let state = match state.as_str() {
            "committed" => "committed",
            "refused" => "refused",
            "superseded" => "superseded",
            "pending" => "pending",
            "running" => "running",
            _ => "unknown",
        };
        *states.entry(state).or_default() += 1;
        if state == "committed" {
            let receipt: Option<String> = row.get(1)?;
            let receipt = receipt.and_then(|value| serde_json::from_str::<Value>(&value).ok());
            let mut counts = None;
            if let Some(id) = receipt
                .as_ref()
                .and_then(|value| value["decision_id"].as_str())
            {
                let raw: Option<String> = db.query_row(
                    "SELECT body FROM planner_plan WHERE id=? AND length(CAST(body AS BLOB))<=?",
                    rusqlite::params![id, MAX_JOURNAL_BYTES as i64],
                    |row| row.get(0),
                ).optional()?;
                if let Some(plan) = raw
                    .and_then(|value| serde_json::from_str::<Value>(&value).ok())
                    .filter(|plan| {
                        plan["decision_id"] == id && plan["decision_owner"] == "orchestrator"
                    })
                {
                    counts = actions.count(&plan["parsed_plan"], &plan["verdicts"]);
                }
            }
            missing += usize::from(counts.is_none());
        }
    }
    Ok(
        json!({"work_items": decisions, "states": states, "actions": actions,
        "committed_plans_unavailable": missing}),
    )
}

fn owner_runs(c: &Config) -> Option<Value> {
    use crate::runtime::transcript::RunIndexLine;
    let runs = journal::<RunIndexLine>(&c.directory.join("model/runs.ndjson"))?;
    let mut ids = BTreeSet::new();
    let (mut failures, mut estimated) = (0, 0);
    let mut cost = Amount::ZERO;
    for run in runs.iter().filter(|run| {
        run.role == "supervisor"
            && run.mode == "paper"
            && run.trigger_kind.as_deref() == Some("decision")
            && in_session(run.started_ms, c.session)
    }) {
        if run.v != 1 || !ids.insert(&run.run) {
            return None;
        }
        failures += usize::from(run.outcome == "failure");
        if let Some(amount) = run
            .cost_estimate_inr
            .as_deref()
            .and_then(|value| Amount::parse(value).ok())
            .filter(|amount| *amount >= Amount::ZERO)
        {
            estimated += 1;
            cost = cost.checked_add(amount).ok()?;
        }
    }
    Some(
        json!({"indexed_runs": ids.len(), "indexed_failures": failures,
        "runs_with_estimated_cost": estimated, "configured_estimate_inr": (estimated > 0).then_some(cost)}),
    )
}

fn report(c: &Config) -> Value {
    let root = lanes::root(c);
    let triggers = journal::<TriggerRow>(&root.join("triggers.ndjson"));
    let turns = journal::<TurnRow>(&root.join("turns.ndjson"));
    let plans = journal::<PlanRow>(&root.join("plans.ndjson"));
    let sealed = sealed_report(c);
    let close = sealed.as_ref().map(|(_, seal)| json!({
        "closed": true, "halted": seal["halted"].as_bool(), "stopped": seal["stopped"].as_bool(),
        "unresolved_positions": seal["open_unresolved"].as_u64(),
    }));
    let owner = sealed
        .as_ref()
        .and_then(|(report, seal)| owner_plans(c, report, seal).ok());
    json!({
        "session": c.session,
        "coverage_basis": "observed controller dispatch and settlement journals",
        "continuous_session_proven": false,
        "sources": {"triggers_readable": triggers.is_some(), "turns_readable": turns.is_some(),
            "plans_readable": plans.is_some(), "matching_close_seal": sealed.is_some()},
        "decision": triggers.as_deref().zip(turns.as_deref())
            .map(|(triggers, turns)| decisions(c.session, triggers, turns, plans.as_deref())),
        "decision_usage_ledger": ledger(&root.join("usage.json"), c.session),
        "paper_close": close,
        "legacy_owner": {"sealed_book_plans": owner, "decision_run_index": owner_runs(c)},
    })
}

pub(crate) fn run(args: Args) -> ExitCode {
    let config = (|| -> Result<Config> {
        let config: Config = read_json(&args.config, 65_536).ok_or("config_unavailable")?;
        config.validate()?;
        rollover::current(&config)
    })();
    match config {
        Ok(config) => {
            println!("{}", report(&config));
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("agent_shadow_report_config_unavailable");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_journal(root: &Path, name: &str, entries: &[Value]) {
        let bytes = entries
            .iter()
            .map(|entry| format!("{entry}\n"))
            .collect::<String>();
        fs::write(root.join(name), bytes).unwrap();
    }

    #[test]
    fn shadow_report_reads_matching_session_evidence_and_exposes_gaps() {
        let tmp = tempfile::tempdir().unwrap();
        let c = crate::runtime::tests::config(tmp.path());
        let root = lanes::root(&c);
        fs::create_dir_all(&root).unwrap();
        let at_open = c
            .session
            .and_hms_opt(3, 45, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let at_pre_close = at_open + 6 * 3_600_000;
        let at_close = at_pre_close + 1_800_000;
        let open = Trigger::new(TriggerKind::Schedule, "open", at_open);
        let pre_close = Trigger::new(TriggerKind::Schedule, "pre_close", at_pre_close);
        let breached = Trigger::new(TriggerKind::PositionRisk, "near_stop", at_pre_close);
        let trigger_rows = [
            json!({"event":"fired","lane":"decision","trigger":open}),
            json!({"event":"dispatched","lane":"decision","at_ms":at_open,"triggers":[open]}),
            json!({"event":"fired","lane":"decision","trigger":pre_close}),
            json!({"event":"dispatched","lane":"decision","at_ms":at_pre_close,"triggers":[breached, pre_close]}),
        ];
        write_journal(&root, "triggers.ndjson", &trigger_rows);

        let wait = json!({"actions":[],"stance":"wait","notes":"fixture"});
        let attempt = json!({"status":"validated","plan":wait,"verdicts":[]});
        let usage =
            json!({"calls":2,"cost_usd":0.125,"input":2,"output":1,"cache_read":0,"cache_write":0});
        let turn = |id, reason, at_ms, triggers| {
            json!({
                "turn_id":id,"lane":"decision","status":"ok","trigger":{
                    "reason":reason,"triggers":triggers
                },
                "at_ms":at_ms,"usage":usage,"plan":attempt
            })
        };
        let manual = turn(
            "decision-manual",
            "agent_turn",
            at_pre_close,
            json!([breached, pre_close]),
        );
        let open_turn = turn("decision-open", "open", at_open, json!([open]));
        let near_close = turn(
            "decision-close",
            "near_stop",
            at_pre_close,
            json!([breached, pre_close]),
        );
        let receipt = |id, at_ms| {
            json!({
                "status":"shadow_not_executed",
                "record":{"mode":"shadow","turn_id":id,"at_ms":at_ms,"attempt":attempt}
            })
        };
        let open_receipt = receipt("decision-open", at_open);
        let close_receipt = receipt("decision-close", at_pre_close);
        write_journal(&root, "turns.ndjson", &[manual.clone(), open_turn.clone()]);
        write_journal(&root, "plans.ndjson", std::slice::from_ref(&open_receipt));
        let partial = report(&c);
        assert_eq!(partial["decision"]["unmatched_dispatches"], 1);
        assert_eq!(partial["decision"]["pre_close"]["validated_turns"], 0);
        assert_eq!(partial["decision"]["manual_turns_excluded"], 1);
        assert!(partial["paper_close"].is_null());

        let portfolio = json!({"closed":true,"halted":false,"stopped":false});
        let body = portfolio.to_string();
        let closed =
            json!({"session":c.session,"account":c.account,"broker":"upstox","paper_only":true,
            "live_enabled":false,"as_of_ms":at_close,"portfolio":portfolio})
            .to_string();
        let seal = json!({"session":c.session,"closed":true,"halted":false,"stopped":false,
            "open_unresolved":0,"report_hash":digest(closed.as_bytes()),"state_hash":digest(body.as_bytes())});
        fs::write(c.directory.join("report.json"), closed).unwrap();
        fs::write(c.directory.join("review.json"), seal.to_string()).unwrap();
        let db_path = c.directory.join("paper.sqlite");
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE state(id INTEGER, binding TEXT, body TEXT);
            CREATE TABLE owner_digest_work(binding TEXT, state TEXT, result TEXT);
            CREATE TABLE planner_plan(id TEXT, body TEXT);",
        )
        .unwrap();
        let binding = book_binding(&c).unwrap();
        db.execute(
            "INSERT INTO state VALUES(1,?,?)",
            rusqlite::params![binding, body],
        )
        .unwrap();
        let owner_receipt = json!({"decision_id":"owner-one"}).to_string();
        db.execute(
            "INSERT INTO owner_digest_work VALUES(?, 'committed', ?)",
            rusqlite::params![binding, owner_receipt],
        )
        .unwrap();
        let owner_plan = json!({"decision_id":"owner-one","decision_owner":"orchestrator","parsed_plan":wait,"verdicts":[]}).to_string();
        db.execute(
            "INSERT INTO planner_plan VALUES('owner-one', ?)",
            [&owner_plan],
        )
        .unwrap();
        drop(db);
        write_journal(
            &c.directory.join("model"),
            "runs.ndjson",
            &[json!({"v":1,"run":"owner-run",
            "role":"supervisor","model":"legacy-fixture","trigger_kind":"decision","started_ms":at_open,
            "duration_ms":10,"outcome":"wait","tokens_in":1,"tokens_out":1,"cost_estimate_inr":null,
            "tool_calls":0,"tool_failures":0,"instruments":[],"bytes":0,"mode":"paper"})],
        );
        write_journal(
            &root,
            "turns.ndjson",
            &[manual, open_turn.clone(), near_close.clone()],
        );
        write_journal(
            &root,
            "plans.ndjson",
            &[open_receipt.clone(), close_receipt.clone()],
        );
        let shared = json!({"date":c.session,"lanes":{"decision":{
            "calls":6,"cost_usd":0.375,"input":6,"output":3,"cache_read":0,"cache_write":0
        }}});
        fs::write(root.join("usage.json"), shared.to_string()).unwrap();

        let unchanged_db = fs::read(&db_path).unwrap();
        let complete = report(&c);
        assert_eq!(complete["paper_close"]["closed"], true);
        assert_eq!(complete["decision"]["open"]["validated_turns"], 1);
        assert_eq!(complete["decision"]["pre_close"]["validated_turns"], 1);
        assert_eq!(complete["decision"]["plans"]["actions"]["waits"], 2);
        assert_eq!(complete["decision"]["journaled_usage"]["calls"], 4);
        assert_eq!(complete["decision_usage_ledger"]["settled"]["calls"], 6);
        assert_eq!(
            complete["legacy_owner"]["sealed_book_plans"]["actions"]["waits"],
            1
        );
        assert_eq!(
            complete["legacy_owner"]["decision_run_index"]["indexed_runs"],
            1
        );
        assert!(
            complete["legacy_owner"]["decision_run_index"]["configured_estimate_inr"].is_null()
        );
        assert_eq!(complete["continuous_session_proven"], false);
        assert_eq!(fs::read(db_path).unwrap(), unchanged_db);
        assert!(!c.directory.join("paper.sqlite-wal").exists());
        assert!(!c.directory.join("paper.sqlite-shm").exists());

        let mut turn = near_close;
        let mut receipt = close_receipt;
        let rejected = json!({"status":"validated","plan":{"actions":[{}]},"verdicts":[{"index":0,"action":null,"units":0,"reason":"action_schema"}]});
        turn["plan"] = rejected.clone();
        receipt["record"]["attempt"] = rejected;
        write_journal(&root, "turns.ndjson", &[open_turn, turn]);
        write_journal(&root, "plans.ndjson", &[open_receipt, receipt]);
        let report = report(&c);
        assert_eq!(report["decision"]["pre_close"]["validated_turns"], 0);
        assert_eq!(report["decision"]["plans"]["actions"]["rejected"], 1);
        assert_eq!(report["decision"]["unmatched_dispatches"], 0);
    }
}
