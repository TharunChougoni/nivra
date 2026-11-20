//! Durable typed messages between the trading and research lanes (#239).
//!
//! This is not chat. The trading lane posts a bounded `ResearchRequest`; the
//! research lane claims it for one turn and answers with a `ResearchResult`
//! that carries a short summary and an artifact reference, never a trace.
//! Messages live in the experiment ledger beside the candidate lifecycle, are
//! idempotent per (session, kind, key), expire, and change state only through
//! compare-and-set transitions that leave one audit event each.
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
    Cancelled,
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
    Cancelled => "cancelled",
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

/// A research reference: an id, a date or an instrument key such as
/// `NSE_EQ|INE002A01018`.
fn bounded_ref(value: &str) -> bool {
    bounded_id(&value.replace('|', "_"))
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
            // The model reads this error; name the field it must fix.
            Self::ResearchRequest { question, refs } => {
                if !bounded_text(question, MAX_QUESTION_CHARS) {
                    return Err("lane_message_question_invalid".into());
                }
                if refs.len() > 8 || !refs.iter().all(|r| bounded_ref(r)) {
                    return Err("lane_message_refs_invalid".into());
                }
                true
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

/// A queue in its own file, for a lane set that must not share messages with
/// the harness lanes.
pub(crate) fn open_file(file: &std::path::Path) -> Result<Connection> {
    let db = super::experiment_ledger::open_file(file)?;
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
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let posted = post_in_transaction(&tx, session, key, body, now, ttl_ms, max_open_requests)?;
    tx.commit()?;
    Ok(posted)
}

/// Shared posting logic for changes that publish a result and settle its request.
pub(crate) fn post_in_transaction(
    tx: &rusqlite::Transaction<'_>,
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
    let existing: Option<String> = tx
        .query_row(
            "SELECT body FROM lane_messages WHERE session=?1 AND kind=?2 AND idempotency_key=?3",
            params![session, kind.as_str(), key],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != encoded {
            return Err("lane_message_conflict".into());
        }
        let message = load(tx, &id)?.ok_or("lane_message_invalid")?;
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
        tx,
        &id,
        None,
        MessageState::Queued,
        TransitionReason::Posted,
        None,
        now,
    )?;
    let message = load(tx, &id)?.ok_or("lane_message_invalid")?;
    Ok(Posted {
        message,
        deduped: false,
    })
}

/// Compare-and-set one transition. A claim at or after its deadline records
/// expiration and returns `Ok(false)`. Other failed comparisons write nothing.
/// An illegal move is an error.
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
    let changed = transition_in_transaction(&tx, id, from, to, reason, turn_id, now)?;
    tx.commit()?;
    Ok(changed)
}

pub(crate) fn transition_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    from: MessageState,
    to: MessageState,
    reason: TransitionReason,
    turn_id: Option<&str>,
    now: i64,
) -> Result<bool> {
    let message = load(tx, id)?.ok_or("lane_message_missing")?;
    let kind = message.kind;
    if !MessageState::can_move(kind, from, to) {
        return Err("lane_message_transition_invalid".into());
    }
    let expired = to == MessageState::Running && message.expires_ms <= now;
    let (to, reason, turn_id) = if expired {
        (MessageState::Cancelled, TransitionReason::Expired, None)
    } else {
        (to, reason, turn_id)
    };
    let changed = tx.execute(
        "UPDATE lane_messages SET state=?1,updated_ms=?2,turn_id=COALESCE(?3,turn_id),reason=?4 WHERE id=?5 AND state=?6",
        params![to.as_str(), now, turn_id, reason.as_str(), id, from.as_str()],
    )?;
    if changed != 1 {
        return Ok(false);
    }
    event(tx, id, Some(from), to, reason, turn_id, now)?;
    Ok(!expired)
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

/// Older journals can contain a posted answer whose request never completed.
pub(crate) fn completed_research_updates(
    db: &Connection,
    session: &str,
    now: i64,
    limit: i64,
) -> Result<Vec<LaneMessage>> {
    query(
        db,
        "AS result WHERE result.session=?1 AND result.kind='research_result'
         AND result.state='queued' AND result.expires_ms>?2
         AND EXISTS (SELECT 1 FROM lane_messages AS request
             WHERE request.id=result.idempotency_key AND request.session=result.session
             AND request.kind='research_request' AND request.state='done'
             AND request.id=json_extract(result.body,'$.request_id')
             AND request.turn_id=json_extract(result.body,'$.research_id'))
         ORDER BY result.created_ms,result.id LIMIT ?3",
        &[&session, &now, &limit],
    )
}

pub(crate) fn update_fact(message: &LaneMessage) -> Value {
    let mut fact = serde_json::to_value(&message.body).unwrap_or(Value::Null);
    fact["message_id"] = json!(message.id);
    fact["posted_ms"] = json!(message.created_ms);
    fact
}

#[cfg(test)]
mod tests {}
