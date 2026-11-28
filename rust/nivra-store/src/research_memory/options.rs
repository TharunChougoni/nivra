use super::{Result, Store};
use chrono::NaiveDate;
use rusqlite::{OptionalExtension, params};
use serde_json::Value;

pub struct OptionStage {
    pub session: NaiveDate,
    pub checked_ms: i64,
    pub proposed_ms: Option<i64>,
    pub cohort_digest: Option<String>,
    pub body: Value,
}

impl Store {
    pub fn option_stage(&self, session: NaiveDate) -> Result<Option<OptionStage>> {
        self.db
            .query_row(
                "SELECT checked_ms,proposed_ms,cohort_digest,body FROM option_research_stage WHERE session=?1",
                [session.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get::<_, String>(3)?)),
            )
            .optional()?
            .map(|(checked_ms, proposed_ms, cohort_digest, body)| {
                Ok(OptionStage {
                    session,
                    checked_ms,
                    proposed_ms,
                    cohort_digest,
                    body: serde_json::from_str(&body)?,
                })
            })
            .transpose()
    }

    pub fn latest_option_stage(&self) -> Result<Option<OptionStage>> {
        self.db
            .query_row(
                "SELECT session FROM option_research_stage ORDER BY session DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|session| {
                self.option_stage(
                    session
                        .parse()
                        .map_err(|_| super::Error("research_store_record_invalid"))?,
                )
            })
            .transpose()
            .map(Option::flatten)
    }

    pub fn record_option_stage(&self, stage: &OptionStage) -> Result<()> {
        self.db.execute(
            "INSERT INTO option_research_stage(session,checked_ms,cohort_digest,body)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(session) DO UPDATE SET checked_ms=excluded.checked_ms,
                cohort_digest=excluded.cohort_digest, body=excluded.body
             WHERE option_research_stage.cohort_digest IS NULL",
            params![
                stage.session.to_string(),
                stage.checked_ms,
                stage.cohort_digest,
                stage.body.to_string()
            ],
        )?;
        Ok(())
    }

    pub fn mark_option_proposed(&self, session: NaiveDate, now: i64) -> Result<()> {
        self.db.execute(
            "UPDATE option_research_stage SET proposed_ms=?2 WHERE session=?1 AND proposed_ms IS NULL",
            params![session.to_string(), now],
        )?;
        Ok(())
    }

    /// Reserve a ranking and hypothesis before revealing any comparison. A
    /// second request for the same ranking returns the first report.
    pub fn option_hypothesis(
        &self,
        session: NaiveDate,
        ranking: &str,
        hypothesis: &str,
    ) -> Result<Option<Value>> {
        let session = session.to_string();
        self.db.execute(
            "INSERT INTO option_research_comparison(session,ranking,hypothesis)
             VALUES (?1,?2,?3) ON CONFLICT DO NOTHING",
            params![session, ranking, hypothesis],
        )?;
        let body: Option<String> = self.db.query_row(
            "SELECT body FROM option_research_comparison WHERE session=?1 AND ranking=?2",
            params![session, ranking],
            |row| row.get(0),
        )?;
        body.map(|body| Ok(serde_json::from_str(&body)?))
            .transpose()
    }

    pub fn latest_option_comparison(&self, session: NaiveDate) -> Result<Option<Value>> {
        self.db
            .query_row(
                "SELECT body FROM option_research_comparison
             WHERE session=?1 AND body IS NOT NULL
             ORDER BY json_extract(body, '$.recorded_ms') DESC, ranking LIMIT 1",
                [session.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|body| Ok(serde_json::from_str(&body)?))
            .transpose()
    }

    pub fn finish_option_comparison(
        &self,
        session: NaiveDate,
        ranking: &str,
        body: &Value,
    ) -> Result<Value> {
        let session = session.to_string();
        self.db.execute(
            "UPDATE option_research_comparison SET body=?3
             WHERE session=?1 AND ranking=?2 AND body IS NULL",
            params![session, ranking, body.to_string()],
        )?;
        let body: String = self.db.query_row(
            "SELECT body FROM option_research_comparison WHERE session=?1 AND ranking=?2",
            params![session, ranking],
            |row| row.get(0),
        )?;
        Ok(serde_json::from_str(&body)?)
    }
}
