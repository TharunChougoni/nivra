//! Research requests between the pi lanes. They live in their own queue file,
//! `model/agent/requests.sqlite`, so a pi lane and a harness lane never claim
//! or answer each other's requests.
use super::triggers::ResearchTriggers;
use crate::runtime::harness::LaneRequests;
use crate::runtime::lane_queue::{
    self, LaneMessage, MessageBody, MessageKind, MessageState, TransitionReason,
};
use crate::runtime::{Config, Result, digest};
use rusqlite::{Connection, TransactionBehavior};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Research answers delivered per decision turn; the rest wait for the next.
const MAX_UPDATES: i64 = 4;
const MAX_ANSWER_CHARS: usize = 16_000;

pub(crate) struct Requests {
    file: PathBuf,
    /// Where research answers are stored, relative paths under `model/`.
    model: PathBuf,
    session: String,
    ttl_ms: i64,
    max_open: usize,
}

impl Requests {
    pub(crate) fn new(c: &Config, settings: &ResearchTriggers) -> Self {
        let model = c.directory.join("model");
        Self {
            file: model.join("agent").join("requests.sqlite"),
            model,
            session: c.session.to_string(),
            ttl_ms: settings.request_ttl_ms,
            max_open: settings.max_open_requests,
        }
    }

    fn open(&self) -> Result<Connection> {
        lane_queue::open_file(&self.file)
    }

    fn find(&self, id: &str) -> Result<Option<LaneMessage>> {
        Ok(lane_queue::load(&self.open()?, id)?.filter(|message| message.session == self.session))
    }

    /// Requests still queued for the research lane, oldest first.
    pub(crate) fn queued(&self, now: i64, limit: usize) -> Result<Vec<LaneMessage>> {
        let mut db = self.open()?;
        lane_queue::expire(&mut db, &self.session, now)?;
        lane_queue::list(
            &db,
            &self.session,
            &[MessageKind::ResearchRequest],
            MessageState::Queued,
            now,
            limit,
        )
    }

    /// Answers not yet shown to a decision turn.
    pub(crate) fn updates(&self, now: i64) -> Result<Vec<LaneMessage>> {
        lane_queue::completed_research_updates(&self.open()?, &self.session, now, MAX_UPDATES)
    }

    pub(crate) fn mark_delivered(&self, ids: &[String], turn_id: &str, now: i64) -> Result<()> {
        let mut db = self.open()?;
        for id in ids {
            lane_queue::transition(
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

    /// Move a queued request to running for one research turn. `false` when
    /// it was already claimed or cancelled.
    pub(crate) fn claim(&self, id: &str, turn_id: &str, now: i64) -> Result<bool> {
        lane_queue::transition(
            &mut self.open()?,
            id,
            MessageState::Queued,
            MessageState::Running,
            TransitionReason::Claimed,
            Some(turn_id),
            now,
        )
    }

    /// Fail a running request. Its turn ended without an answer; the request
    /// is never run again, and the decision lane may ask once more.
    pub(crate) fn fail(&self, id: &str, reason: TransitionReason, now: i64) -> Result<bool> {
        lane_queue::transition(
            &mut self.open()?,
            id,
            MessageState::Running,
            MessageState::Failed,
            reason,
            None,
            now,
        )
    }

    /// Requests left running by a turn that no longer runs: its process is
    /// gone, or it is this process, whose passes run one turn at a time and
    /// call this before the next. Turn ids carry the process id.
    pub(crate) fn recover(&self, now: i64) -> Result<usize> {
        let running = lane_queue::list(
            &self.open()?,
            &self.session,
            &[MessageKind::ResearchRequest],
            MessageState::Running,
            now,
            usize::MAX,
        )?;
        let mut failed = 0;
        for message in running {
            let owner = message
                .turn_id
                .as_deref()
                .and_then(|id| id.split('-').nth(2))
                .and_then(|pid| pid.parse::<u32>().ok());
            let stale = owner.is_none_or(|pid| {
                pid == std::process::id() || !Path::new(&format!("/proc/{pid}")).exists()
            });
            if stale && self.fail(&message.id, TransitionReason::Interrupted, now)? {
                failed += 1;
            }
        }
        Ok(failed)
    }

    /// Publish an answer only while this turn still owns the running request.
    /// The write lock covers the artifact and both queue changes. A failed
    /// transaction can leave an orphan artifact, which is never delivered.
    pub(crate) fn answer(
        &self,
        request: &LaneMessage,
        turn_id: &str,
        text: &str,
        usage: &Value,
        now: i64,
    ) -> Result<()> {
        let mut db = self.open()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = lane_queue::load(&tx, &request.id)?
            .filter(|message| {
                message.session == self.session
                    && message.kind == MessageKind::ResearchRequest
                    && message.state == MessageState::Running
                    && message.turn_id.as_deref() == Some(turn_id)
            })
            .ok_or("agent_research_request_not_owned")?;
        let MessageBody::ResearchRequest { question, .. } = &current.body else {
            return Err("agent_research_request_invalid".into());
        };
        let artifact = format!("agent/research/{}.json", current.id);
        let body = MessageBody::ResearchResult {
            request_id: Some(current.id.clone()),
            research_id: turn_id.to_owned(),
            summary: super::lanes::summary(text),
            artifact_ref: artifact.clone(),
            output_sha256: digest(text.as_bytes()),
        };
        let path = self.model.join(&artifact);
        super::lanes::private_dir(path.parent().ok_or("agent_research_path")?)?;
        let redacted = crate::runtime::transcript::redact(&json!(text));
        crate::runtime::rollover::atomic(
            &path,
            &json!({"request_id":current.id,"question":question,"turn_id":turn_id,
                "text":redacted,"usage":usage}),
        )?;
        lane_queue::post_in_transaction(
            &tx,
            &self.session,
            &current.id,
            &body,
            now,
            self.ttl_ms,
            self.max_open,
        )?;
        if !lane_queue::transition_in_transaction(
            &tx,
            &current.id,
            MessageState::Running,
            MessageState::Done,
            TransitionReason::Completed,
            Some(turn_id),
            now,
        )? {
            return Err("agent_research_request_not_owned".into());
        }
        tx.commit()?;
        Ok(())
    }

    fn view(message: &LaneMessage) -> Value {
        json!({"request_id":message.id,"kind":"research_request","state":message.state.as_str(),
            "reason":message.reason.map(TransitionReason::as_str),"turn_id":message.turn_id,
            "updated_ms":message.updated_ms,"expires_ms":message.expires_ms})
    }
}

fn read_answer(model: &Path, request_id: &str) -> Result<Option<Value>> {
    let path = model
        .join("agent")
        .join("research")
        .join(format!("{request_id}.json"));
    let bytes = match crate::read_bounded(&path, 4_000_000) {
        Ok(bytes) => bytes,
        Err(_) if !path.exists() => return Ok(None),
        Err(_) => return Err("agent_research_answer_unreadable".into()),
    };
    let mut answer: Value = serde_json::from_slice(&bytes)?;
    if let Some(text) = answer["text"].as_str() {
        let bounded: String = text.chars().take(MAX_ANSWER_CHARS).collect();
        answer["truncated"] = json!(bounded.len() < text.len());
        answer["text"] = json!(bounded);
    }
    Ok(Some(answer))
}

impl LaneRequests for Requests {
    fn post(&self, turn_id: &str, question: &str, refs: Vec<String>, now: i64) -> Result<Value> {
        if crate::runtime::transcript::redact(&json!(question)).as_str() != Some(question)
            || refs.iter().any(|reference| {
                crate::runtime::transcript::redact(&json!(reference)).as_str()
                    != Some(reference.as_str())
            })
        {
            return Err("research_request_secret_refused".into());
        }
        let body = MessageBody::ResearchRequest {
            question: question.to_owned(),
            refs,
        };
        let key = digest(format!("{turn_id}\n{question}").as_bytes());
        let mut db = self.open()?;
        lane_queue::expire(&mut db, &self.session, now)?;
        let posted = lane_queue::post(
            &mut db,
            &self.session,
            &key,
            &body,
            now,
            self.ttl_ms,
            self.max_open,
        )?;
        Ok(json!({
            "request_id":posted.message.id,
            "state":posted.message.state.as_str(),
            "expires_ms":posted.message.expires_ms,
            "deduped":posted.deduped,
            "note":"Paper research only. The answer arrives in a later decision brief's research_updates, or read it with research_result once done. Nothing here authorizes a trade."
        }))
    }

    fn status(&self, id: &str) -> Result<Option<Value>> {
        Ok(self
            .find(id)?
            .filter(|message| message.kind == MessageKind::ResearchRequest)
            .map(|message| Self::view(&message)))
    }

    fn result(&self, id: &str) -> Result<Option<Value>> {
        let Some(message) = self
            .find(id)?
            .filter(|message| message.kind == MessageKind::ResearchRequest)
        else {
            return Ok(None);
        };
        let answer = if message.state == MessageState::Done {
            read_answer(&self.model, &message.id)?
        } else {
            None
        };
        Ok(Some(
            json!({"request":Self::view(&message),"answer":answer,"execution_authorized":false}),
        ))
    }

    fn cancel(&self, id: &str, now: i64) -> Result<Option<Value>> {
        let Some(message) = self
            .find(id)?
            .filter(|message| message.kind == MessageKind::ResearchRequest)
        else {
            return Ok(None);
        };
        if message.state == MessageState::Queued {
            lane_queue::transition(
                &mut self.open()?,
                id,
                MessageState::Queued,
                MessageState::Cancelled,
                TransitionReason::Cancelled,
                None,
                now,
            )?;
        }
        self.status(id)
    }
}
