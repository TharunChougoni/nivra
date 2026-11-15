//! Durable typed messages between the trading and research lanes (#239).
//!
//! This is not chat. The trading lane posts a bounded `ResearchRequest`; the
//! research lane claims it for one turn and answers with a `ResearchResult`
//! that carries a short summary and an artifact reference, never a trace.
//! Messages live in the experiment ledger beside the candidate lifecycle, are
//! idempotent per (session, kind, key), expire, and change state only through
//! compare-and-set transitions that leave one audit event each.
use super::owner_tasks::ResearchLaneSettings;
use super::*;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Appended to the experiment ledger schema; created on open like its tables.
pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS lane_messages(
  id TEXT PRIMARY KEY,
  session TEXT NOT NULL,
  kind TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  state TEXT NOT NULL,
  body TEXT NOT NULL,
  created_ms INTEGER NOT NULL,
  expires_ms INTEGER NOT NULL,
  updated_ms INTEGER NOT NULL,
  turn_id TEXT,
  reason TEXT,
  UNIQUE(session, kind, idempotency_key)
);
CREATE TABLE IF NOT EXISTS lane_message_events(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  message_id TEXT NOT NULL,
  from_state TEXT,
  to_state TEXT NOT NULL,
  reason TEXT NOT NULL,
  turn_id TEXT,
  at_ms INTEGER NOT NULL
);";

/// Trading results delivered per decision turn; the rest wait for the next.
pub(crate) const MAX_TRADING_UPDATES: usize = 4;
const MAX_QUESTION_CHARS: usize = 1_000;
const MAX_SUMMARY_CHARS: usize = 600;
const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2_000);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessageKind {
    /// Trading → research.
    ResearchRequest,
    /// Research → trading.
    ResearchResult,
    /// Research → trading: a candidate lifecycle change worth a look.
    CandidateUpdate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MessageState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// Why a message moved. Recorded on the message and on its audit event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TransitionReason {
    Posted,
    Claimed,
    Completed,
    Delivered,
    TurnFailed,
    Interrupted,
    Expired,
    SessionEnded,
}

macro_rules! text_enum {
    ($ty:ty { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $ty {
            pub(crate) const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
            fn parse(raw: &str) -> Result<Self> {
                match raw { $($text => Ok(Self::$variant),)+ _ => Err("lane_message_invalid".into()) }
            }
        }
    };
}
text_enum!(MessageKind { ResearchRequest => "research_request", ResearchResult => "research_result", CandidateUpdate => "candidate_update" });
text_enum!(MessageState { Queued => "queued", Running => "running", Done => "done", Failed => "failed", Cancelled => "cancelled" });
text_enum!(TransitionReason {
    Posted => "posted", Claimed => "claimed", Completed => "completed", Delivered => "delivered",
    TurnFailed => "turn_failed", Interrupted => "interrupted", Expired => "expired", SessionEnded => "session_ended",
});

impl MessageState {
    /// The only legal moves. Requests run in a research turn; results and
    /// updates are delivered to a trading turn; anything queued can expire.
    pub(crate) const fn can_move(kind: MessageKind, from: Self, to: Self) -> bool {
        matches!(
            (kind, from, to),
            (MessageKind::ResearchRequest, Self::Queued, Self::Running)
                | (
                    MessageKind::ResearchRequest,
                    Self::Running,
                    Self::Done | Self::Failed
                )
                | (
                    MessageKind::ResearchResult | MessageKind::CandidateUpdate,
                    Self::Queued,
                    Self::Done
                )
                | (_, Self::Queued, Self::Cancelled)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum MessageBody {
    ResearchRequest {
        question: String,
        #[serde(default)]
        refs: Vec<String>,
    },
    ResearchResult {
        request_id: Option<String>,
        research_id: String,
        summary: String,
        artifact_ref: String,
        output_sha256: String,
    },
    CandidateUpdate {
        candidate_id: String,
        state: String,
        summary: String,
    },
}

fn bounded_text(value: &str, max_chars: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= max_chars
        && !value
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
        && super::transcript::redact(&json!(value)).as_str() == Some(value)
}

fn bounded_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-:./".contains(&b))
}

impl MessageBody {
    pub(crate) const fn kind(&self) -> MessageKind {
        match self {
            Self::ResearchRequest { .. } => MessageKind::ResearchRequest,
            Self::ResearchResult { .. } => MessageKind::ResearchResult,
            Self::CandidateUpdate { .. } => MessageKind::CandidateUpdate,
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        let ok = match self {
            Self::ResearchRequest { question, refs } => {
                bounded_text(question, MAX_QUESTION_CHARS)
                    && refs.len() <= 8
                    && refs.iter().all(|r| bounded_id(r))
            }
            Self::ResearchResult {
                request_id,
                research_id,
                summary,
                artifact_ref,
                output_sha256,
            } => {
                request_id.as_deref().is_none_or(bounded_id)
                    && bounded_id(research_id)
                    && bounded_text(summary, MAX_SUMMARY_CHARS)
                    && bounded_id(artifact_ref)
                    && super::experiment_ledger::valid_hash(output_sha256)
            }
            Self::CandidateUpdate {
                candidate_id,
                state,
                summary,
            } => {
                bounded_id(candidate_id)
                    && bounded_id(state)
                    && bounded_text(summary, MAX_SUMMARY_CHARS)
            }
        };
        if ok {
            Ok(())
        } else {
            Err("lane_message_invalid".into())
        }
    }
    /// Hash of the canonical body; a research turn binds a request by it.
    pub(crate) fn sha256(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(self)?))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct LaneMessage {
    pub id: String,
    pub session: String,
    pub kind: MessageKind,
    pub state: MessageState,
    pub idempotency_key: String,
    pub body: MessageBody,
    pub created_ms: i64,
    pub expires_ms: i64,
    pub updated_ms: i64,
    pub turn_id: Option<String>,
    pub reason: Option<TransitionReason>,
}

#[derive(Debug)]
pub(crate) struct Posted {
    pub message: LaneMessage,
    /// The same key and body were already posted; nothing new was written.
    pub deduped: bool,
}

pub(crate) fn message_id(session: &str, kind: MessageKind, key: &str) -> String {
    digest(format!("lane-message-v1\n{session}\n{}\n{key}", kind.as_str()).as_bytes())
}

pub(crate) fn open(directory: &std::path::Path) -> Result<Connection> {
    let db = super::experiment_ledger::open_directory(directory)?;
    db.busy_timeout(BUSY_TIMEOUT)?;
    Ok(db)
}

const COLUMNS: &str =
    "id,session,kind,state,idempotency_key,body,created_ms,expires_ms,updated_ms,turn_id,reason";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<[Option<String>; 8]> {
    Ok([
        Some(row.get(0)?),
        Some(row.get(1)?),
        Some(row.get(2)?),
        Some(row.get(3)?),
        Some(row.get(4)?),
        Some(row.get(5)?),
        row.get(9)?,
        row.get(10)?,
    ])
}

fn decode(raw: [Option<String>; 8], times: [i64; 3]) -> Result<LaneMessage> {
    let [id, session, kind, state, key, body, turn_id, reason] = raw;
    let kind = MessageKind::parse(kind.as_deref().unwrap_or_default())?;
    let body: MessageBody = serde_json::from_str(body.as_deref().unwrap_or_default())
        .map_err(|_| "lane_message_invalid")?;
    if body.kind() != kind {
        return Err("lane_message_invalid".into());
    }
    Ok(LaneMessage {
        id: id.unwrap_or_default(),
        session: session.unwrap_or_default(),
        kind,
        state: MessageState::parse(state.as_deref().unwrap_or_default())?,
        idempotency_key: key.unwrap_or_default(),
        body,
        created_ms: times[0],
        expires_ms: times[1],
        updated_ms: times[2],
        turn_id,
        reason: reason.as_deref().map(TransitionReason::parse).transpose()?,
    })
}

fn query(db: &Connection, filter: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<LaneMessage>> {
    let mut statement = db.prepare(&format!("SELECT {COLUMNS} FROM lane_messages {filter}"))?;
    let rows = statement.query_map(args, |r| {
        Ok((row(r)?, [r.get::<_, i64>(6)?, r.get(7)?, r.get(8)?]))
    })?;
    let mut messages = Vec::new();
    for item in rows {
        let (raw, times) = item?;
        messages.push(decode(raw, times)?);
    }
    Ok(messages)
}

pub(crate) fn load(db: &Connection, id: &str) -> Result<Option<LaneMessage>> {
    Ok(query(db, "WHERE id=?1", &[&id])?.into_iter().next())
}

fn event(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    from: Option<MessageState>,
    to: MessageState,
    reason: TransitionReason,
    turn_id: Option<&str>,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO lane_message_events(message_id,from_state,to_state,reason,turn_id,at_ms) VALUES(?,?,?,?,?,?)",
        params![id, from.map(MessageState::as_str), to.as_str(), reason.as_str(), turn_id, now],
    )?;
    Ok(())
}

/// Post one message. The same key with the same body returns the original
/// message without a write; the same key with another body is refused. Open
/// requests (queued or running) are capped so trading cannot flood research.
pub(crate) fn post(
    db: &mut Connection,
    session: &str,
    key: &str,
    body: &MessageBody,
    now: i64,
    ttl_ms: i64,
    max_open_requests: usize,
) -> Result<Posted> {
    body.validate()?;
    if session.is_empty() || !bounded_id(key) || ttl_ms <= 0 || now < 0 {
        return Err("lane_message_invalid".into());
    }
    let kind = body.kind();
    let id = message_id(session, kind, key);
    let encoded = serde_json::to_string(body)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT body FROM lane_messages WHERE session=?1 AND kind=?2 AND idempotency_key=?3",
            params![session, kind.as_str(), key],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        drop(tx);
        if existing != encoded {
            return Err("lane_message_conflict".into());
        }
        let message = load(db, &id)?.ok_or("lane_message_invalid")?;
        return Ok(Posted {
            message,
            deduped: true,
        });
    }
    if kind == MessageKind::ResearchRequest {
        let open: i64 = tx.query_row(
            "SELECT count(*) FROM lane_messages WHERE session=?1 AND kind=?2 AND (state='running' OR (state='queued' AND expires_ms>?3))",
            params![session, kind.as_str(), now],
            |r| r.get(0),
        )?;
        if open as usize >= max_open_requests {
            return Err("lane_request_capacity".into());
        }
    }
    let expires = now.saturating_add(ttl_ms);
    tx.execute(
        "INSERT INTO lane_messages(id,session,kind,idempotency_key,state,body,created_ms,expires_ms,updated_ms,turn_id,reason) VALUES(?1,?2,?3,?4,'queued',?5,?6,?7,?6,NULL,'posted')",
        params![id, session, kind.as_str(), key, encoded, now, expires],
    )?;
    event(
        &tx,
        &id,
        None,
        MessageState::Queued,
        TransitionReason::Posted,
        None,
        now,
    )?;
    tx.commit()?;
    let message = load(db, &id)?.ok_or("lane_message_invalid")?;
    Ok(Posted {
        message,
        deduped: false,
    })
}

/// Compare-and-set one transition. `Ok(false)` means another writer moved the
/// message first; nothing is written. An illegal move is an error.
pub(crate) fn transition(
    db: &mut Connection,
    id: &str,
    from: MessageState,
    to: MessageState,
    reason: TransitionReason,
    turn_id: Option<&str>,
    now: i64,
) -> Result<bool> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let kind: Option<String> = tx
        .query_row("SELECT kind FROM lane_messages WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    let kind = MessageKind::parse(&kind.ok_or("lane_message_missing")?)?;
    if !MessageState::can_move(kind, from, to) {
        return Err("lane_message_transition_invalid".into());
    }
    let changed = tx.execute(
        "UPDATE lane_messages SET state=?1,updated_ms=?2,turn_id=COALESCE(?3,turn_id),reason=?4 WHERE id=?5 AND state=?6",
        params![to.as_str(), now, turn_id, reason.as_str(), id, from.as_str()],
    )?;
    if changed != 1 {
        return Ok(false);
    }
    event(&tx, id, Some(from), to, reason, turn_id, now)?;
    tx.commit()?;
    Ok(true)
}

/// Cancel queued messages that expired or belong to another session. Running
/// requests are settled by their turn or by its crash recovery, never here.
pub(crate) fn expire(db: &mut Connection, session: &str, now: i64) -> Result<usize> {
    let stale = query(
        db,
        "WHERE state='queued' AND (expires_ms<=?1 OR session<>?2) ORDER BY created_ms,id",
        &[&now, &session],
    )?;
    let mut cancelled = 0;
    for message in stale {
        let reason = if message.session == session {
            TransitionReason::Expired
        } else {
            TransitionReason::SessionEnded
        };
        if transition(
            db,
            &message.id,
            MessageState::Queued,
            MessageState::Cancelled,
            reason,
            None,
            now,
        )? {
            cancelled += 1;
        }
    }
    Ok(cancelled)
}

pub(crate) fn list(
    db: &Connection,
    session: &str,
    kinds: &[MessageKind],
    state: MessageState,
    now: i64,
    limit: usize,
) -> Result<Vec<LaneMessage>> {
    let mut messages = query(
        db,
        "WHERE session=?1 AND state=?2 AND (state<>'queued' OR expires_ms>?3) ORDER BY created_ms,id",
        &[&session, &state.as_str(), &now],
    )?;
    messages.retain(|message| kinds.contains(&message.kind));
    messages.truncate(limit);
    Ok(messages)
}

pub(crate) fn lane_settings(cfg: &planner::PlannerConfig) -> Option<&ResearchLaneSettings> {
    cfg.owner_tasks
        .as_ref()
        .and_then(|settings| settings.research.as_ref())
        .filter(|research| research.enabled)
        .and_then(|research| research.lane.as_ref())
}

/// Trading tool: post one bounded research request for the research lane. The
/// key binds the decision turn and the question, so a retried tool call never
/// posts twice.
pub(crate) fn post_request(
    c: &Config,
    cfg: &planner::PlannerConfig,
    turn_id: &str,
    question: &str,
    refs: Vec<String>,
    now: i64,
) -> Result<Value> {
    let lane = lane_settings(cfg).ok_or("research_lane_disabled")?;
    let body = MessageBody::ResearchRequest {
        question: question.to_owned(),
        refs,
    };
    let key = digest(format!("{turn_id}\n{question}").as_bytes());
    let mut db = open(&c.directory)?;
    let session = c.session.to_string();
    expire(&mut db, &session, now)?;
    let posted = post(
        &mut db,
        &session,
        &key,
        &body,
        now,
        lane.request_ttl_ms,
        lane.max_open_requests,
    )?;
    Ok(json!({
        "request_id":posted.message.id,
        "state":posted.message.state.as_str(),
        "expires_ms":posted.message.expires_ms,
        "deduped":posted.deduped,
        "note":"Advisory paper research only. The research lane answers in a later turn's research_updates; nothing here authorizes a trade."
    }))
}

/// Undelivered research results for one trading turn: summary and refs only.
pub(crate) fn trading_updates(c: &Config, now: i64) -> Result<Vec<LaneMessage>> {
    let db = open(&c.directory)?;
    list(
        &db,
        &c.session.to_string(),
        &[MessageKind::ResearchResult, MessageKind::CandidateUpdate],
        MessageState::Queued,
        now,
        MAX_TRADING_UPDATES,
    )
}

pub(crate) fn update_fact(message: &LaneMessage) -> Value {
    let mut fact = serde_json::to_value(&message.body).unwrap_or(Value::Null);
    fact["message_id"] = json!(message.id);
    fact["posted_ms"] = json!(message.created_ms);
    fact
}

/// Mark updates delivered once the trading turn that rendered them is durable.
/// A crash before this re-delivers the same message ids once (at-least-once).
pub(crate) fn mark_delivered(c: &Config, ids: &[String], turn_id: &str, now: i64) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut db = open(&c.directory)?;
    for id in ids {
        transition(
            &mut db,
            id,
            MessageState::Queued,
            MessageState::Done,
            TransitionReason::Delivered,
            Some(turn_id),
            now,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(question: &str) -> MessageBody {
        MessageBody::ResearchRequest {
            question: question.into(),
            refs: vec!["candidate:abc".into()],
        }
    }

    fn events(db: &Connection, id: &str) -> Vec<(Option<String>, String, String)> {
        let mut statement = db
            .prepare("SELECT from_state,to_state,reason FROM lane_message_events WHERE message_id=?1 ORDER BY id")
            .unwrap();
        statement
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    }

    /// Failure modes: duplicate trading requests (dedupe), a reused key with a
    /// new body, a trading lane flooding research, two writers racing one
    /// claim, illegal state moves, stale requests outliving their session, and
    /// unbounded or unredacted bodies reaching a prompt.
    #[test]
    fn lane_messages_are_typed_idempotent_bounded_and_compare_and_set() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = open(dir.path()).unwrap();
        let day = "2026-11-16";

        let first = post(
            &mut db,
            day,
            "k1",
            &request("why did X gap?"),
            1_000,
            60_000,
            2,
        )
        .unwrap();
        assert!(!first.deduped);
        assert_eq!(first.message.state, MessageState::Queued);
        assert_eq!(first.message.expires_ms, 61_000);
        let again = post(
            &mut db,
            day,
            "k1",
            &request("why did X gap?"),
            2_000,
            60_000,
            2,
        )
        .unwrap();
        assert!(again.deduped);
        assert_eq!(again.message, first.message);
        let conflict = post(&mut db, day, "k1", &request("other"), 2_000, 60_000, 2).unwrap_err();
        assert_eq!(conflict.to_string(), "lane_message_conflict");
        post(&mut db, day, "k2", &request("second"), 2_000, 60_000, 2).unwrap();
        let full = post(&mut db, day, "k3", &request("third"), 2_000, 60_000, 2).unwrap_err();
        assert_eq!(full.to_string(), "lane_request_capacity");
        for bad in ["", "   ", &"x".repeat(1_001), "bell\u{7}"] {
            let error = post(&mut db, day, "k9", &request(bad), 2_000, 60_000, 8).unwrap_err();
            assert_eq!(error.to_string(), "lane_message_invalid", "{bad:?}");
        }

        let id = first.message.id.clone();
        let claim = |db: &mut Connection| {
            transition(
                db,
                &id,
                MessageState::Queued,
                MessageState::Running,
                TransitionReason::Claimed,
                Some("research-2026-11-16-1"),
                3_000,
            )
        };
        assert!(claim(&mut db).unwrap());
        assert!(
            !claim(&mut db).unwrap(),
            "a lost compare-and-set writes nothing"
        );
        let illegal = transition(
            &mut db,
            &id,
            MessageState::Running,
            MessageState::Queued,
            TransitionReason::Claimed,
            None,
            3_000,
        )
        .unwrap_err();
        assert_eq!(illegal.to_string(), "lane_message_transition_invalid");
        assert!(
            transition(
                &mut db,
                &id,
                MessageState::Running,
                MessageState::Failed,
                TransitionReason::Interrupted,
                None,
                4_000,
            )
            .unwrap()
        );
        let failed = load(&db, &id).unwrap().unwrap();
        assert_eq!(failed.state, MessageState::Failed);
        assert_eq!(failed.turn_id.as_deref(), Some("research-2026-11-16-1"));
        assert_eq!(failed.reason, Some(TransitionReason::Interrupted));
        assert_eq!(
            events(&db, &id),
            vec![
                (None, "queued".into(), "posted".into()),
                (Some("queued".into()), "running".into(), "claimed".into()),
                (
                    Some("running".into()),
                    "failed".into(),
                    "interrupted".into()
                ),
            ]
        );
        // A terminal request is never re-served, even when trading re-posts it.
        let replay = post(
            &mut db,
            day,
            "k1",
            &request("why did X gap?"),
            5_000,
            60_000,
            8,
        )
        .unwrap();
        assert!(replay.deduped);
        assert_eq!(replay.message.state, MessageState::Failed);

        // Expiry cancels only queued messages: past TTL, or left by another
        // session. Running work is settled by its own turn, never here.
        let old = post(
            &mut db,
            "2026-11-13",
            "k1",
            &request("stale day"),
            1_000,
            600_000,
            8,
        )
        .unwrap();
        assert_eq!(expire(&mut db, day, 62_000).unwrap(), 2);
        let k2 = load(&db, &message_id(day, MessageKind::ResearchRequest, "k2"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (k2.state, k2.reason),
            (MessageState::Cancelled, Some(TransitionReason::Expired))
        );
        let old = load(&db, &old.message.id).unwrap().unwrap();
        assert_eq!(
            (old.state, old.reason),
            (
                MessageState::Cancelled,
                Some(TransitionReason::SessionEnded)
            )
        );
        assert_eq!(load(&db, &id).unwrap().unwrap().state, MessageState::Failed);

        // Results are delivered once and carry a summary and refs only.
        let result = MessageBody::ResearchResult {
            request_id: Some(id.clone()),
            research_id: "r".repeat(64),
            summary: "Gap was an index rebalance.".into(),
            artifact_ref: format!("model/research/{}.json", "t".repeat(64)),
            output_sha256: "a".repeat(64),
        };
        let posted = post(&mut db, day, &"r".repeat(64), &result, 70_000, 600_000, 1).unwrap();
        let resend = transition(
            &mut db,
            &posted.message.id,
            MessageState::Queued,
            MessageState::Running,
            TransitionReason::Claimed,
            None,
            70_001,
        )
        .unwrap_err();
        assert_eq!(resend.to_string(), "lane_message_transition_invalid");
        let mut leaky = result.clone();
        if let MessageBody::ResearchResult { summary, .. } = &mut leaky {
            *summary = "x".repeat(601);
        }
        assert!(leaky.validate().is_err());
    }
}
