use super::*;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Forward evidence for a reviewed library entry. The entry may have been
/// registered by a research turn outside an autoresearch campaign.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ShadowPlan {
    pub version: u32,
    pub strategy_id: String,
    pub comparator_id: Option<String>,
    pub sessions: Vec<NaiveDate>,
    pub end_ms: i64,
    pub source_blob: String,
    pub params: Value,
    pub runner_sha256: String,
    pub engine_sha256: String,
    pub adapter_sha256: String,
    pub review_sha256: String,
    pub replay_sha256: String,
    pub inputs: Value,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowState {
    Running,
    BetweenSessions,
    Completed,
    Inconclusive,
}

impl ShadowState {
    fn code(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::BetweenSessions => "between_sessions",
            Self::Completed => "completed",
            Self::Inconclusive => "inconclusive",
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct ShadowWindow {
    pub id: i64,
    pub plan: ShadowPlan,
    pub state: ShadowState,
    pub reason: Option<String>,
    pub summary: Value,
}

impl Store {
    pub fn shadow_window(&self, candidate: Option<&str>) -> Result<Option<ShadowWindow>> {
        let raw = self
            .db
            .query_row(
                "SELECT id, plan, state, reason, summary FROM strategy_shadow_window
             WHERE (?1 IS NULL OR strategy_id=?1)
             ORDER BY (state IN ('running','between_sessions')) DESC, id DESC LIMIT 1",
                [candidate],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        raw.map(|(id, plan, state, reason, summary)| {
            Ok(ShadowWindow {
                id,
                plan: serde_json::from_str(&plan)?,
                state: serde_json::from_value(Value::String(state))?,
                reason,
                summary: serde_json::from_str(&summary)?,
            })
        })
        .transpose()
    }

    /// Admit one window and its first session in a single transaction.
    pub fn start_shadow(&mut self, plan: &ShadowPlan, session: NaiveDate) -> Result<Option<i64>> {
        let tx = self.db.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO strategy_shadow_window(strategy_id, plan, state)
             VALUES(?1, ?2, 'running')",
            params![plan.strategy_id, serde_json::to_string(plan)?],
        )?;
        if inserted == 0 {
            return Ok(None);
        }
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO strategy_shadow_session(window_id, session) VALUES(?1, ?2)",
            params![id, session.to_string()],
        )?;
        tx.commit()?;
        Ok(Some(id))
    }

    pub fn continue_shadow(&mut self, id: i64, session: NaiveDate) -> Result<()> {
        let tx = self.db.transaction()?;
        if tx.execute(
            "UPDATE strategy_shadow_window SET state='running' WHERE id=?1 AND state='between_sessions'",
            [id],
        )? != 1 {
            return Err(Error("strategy_shadow_window_not_pending"));
        }
        tx.execute(
            "INSERT INTO strategy_shadow_session(window_id, session) VALUES(?1, ?2)",
            params![id, session.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn shadow_ledger(&self, id: i64, session: NaiveDate) -> Result<Option<Vec<u8>>> {
        Ok(self
            .db
            .query_row(
                "SELECT ledger FROM strategy_shadow_session WHERE window_id=?1 AND session=?2",
                params![id, session.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Replace the checkpoint and publish its corresponding summary atomically.
    pub fn save_shadow(
        &mut self,
        id: i64,
        session: NaiveDate,
        ledger: &[u8],
        summary: &Value,
        status: (ShadowState, Option<&str>),
    ) -> Result<()> {
        let (state, reason) = status;
        let tx = self.db.transaction()?;
        if tx.execute(
            "UPDATE strategy_shadow_window SET state=?2, reason=?3, summary=?4 WHERE id=?1 AND state='running'",
            params![id, state.code(), reason, serde_json::to_string(summary)?],
        )? != 1 {
            return Err(Error("strategy_shadow_window_not_running"));
        }
        if tx.execute(
            "UPDATE strategy_shadow_session SET ledger=?3 WHERE window_id=?1 AND session=?2",
            params![id, session.to_string(), ledger],
        )? != 1
        {
            return Err(Error("strategy_shadow_session_missing"));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn interrupt_shadow(&self, id: i64, reason: &str) -> Result<()> {
        self.db.execute(
            "UPDATE strategy_shadow_window SET state='inconclusive', reason=?2
             WHERE id=?1 AND state IN ('running','between_sessions')",
            params![id, reason],
        )?;
        Ok(())
    }
}
