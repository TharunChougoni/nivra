use super::records::date;
use super::*;
use chrono::NaiveDate;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holdout {
    pub finalist: i64,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub state: String,
    pub verdict: Option<String>,
    pub evidence: Option<String>,
}

impl Store {
    pub fn holdout(&self, campaign: i64) -> Result<Option<Holdout>> {
        Ok(self.db.query_row(
            "SELECT c.finalist_experiment_id,c.holdout_from,c.holdout_to,c.holdout_state,h.verdict,h.report_blob
             FROM campaign c LEFT JOIN holdout_report h ON h.campaign_id=c.id
             WHERE c.id=?1 AND c.finalist_experiment_id IS NOT NULL",
            [campaign],
            |r| Ok(Holdout { finalist: r.get(0)?, from: date(r.get(1)?)?,
                to: date(r.get(2)?)?, state: r.get(3)?, verdict: r.get(4)?, evidence: r.get(5)? }),
        ).optional()?)
    }

    pub fn freeze_finalist(
        &mut self,
        campaign: i64,
        finalist: i64,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<bool> {
        if from > to {
            return Err(Error("research_holdout_window_invalid"));
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE campaign SET finalist_experiment_id=?2,holdout_from=?3,holdout_to=?4,holdout_state='reserved'
             WHERE id=?1 AND holdout_state='unopened' AND EXISTS (
                 SELECT 1 FROM experiment e WHERE e.id=?2 AND e.campaign_id=?1
                   AND e.stage='bars' AND e.status='scored' AND e.verdict='keep'
             ) AND NOT EXISTS (
                 SELECT 1 FROM campaign other WHERE other.segment=campaign.segment
                   AND other.holdout_from<=?4 AND other.holdout_to>=?3
             )",
            params![campaign, finalist, from.to_string(), to.to_string()],
        )? == 1;
        tx.commit()?;
        Ok(changed)
    }

    /// Commit the one-shot read before accessing any underlying bars. If a
    /// process dies after this point, a new process cannot open it again.
    pub fn open_holdout_once(&mut self, campaign: i64) -> Result<bool> {
        Ok(self.db.execute(
            "UPDATE campaign SET holdout_state='opened' WHERE id=?1 AND holdout_state='reserved'",
            [campaign],
        )? == 1)
    }

    pub fn finish_holdout(
        &mut self,
        campaign: i64,
        verdict: &str,
        report: Option<&str>,
    ) -> Result<bool> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute(
            "UPDATE campaign SET holdout_state='retired' WHERE id=?1 AND holdout_state='opened'",
            [campaign],
        )? != 1
        {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO holdout_report(campaign_id,verdict,report_blob) VALUES(?1,?2,?3)",
            params![campaign, verdict, report],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn accepted_quote_child(
        &self,
        source: &str,
        params_sha: &str,
    ) -> Result<Option<Experiment>> {
        Ok(self.db.query_row(&format!(
            "{EXPERIMENT} WHERE stage='quote_replay' AND status='scored' AND verdict='keep'
             AND parent_id IN (SELECT id FROM experiment WHERE stage='bars' AND source_blob=?1 AND params_blob=?2)
             ORDER BY id DESC LIMIT 1"),
            params![source, params_sha], experiment_row).optional()?)
    }

    pub fn freeze_quote_draft(
        &mut self,
        idea: i64,
        source: &str,
        params_sha: &str,
        now: i64,
    ) -> Result<Option<i64>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO quote_draft(idea_id,source_blob,params_blob,frozen_at_ms) VALUES(?1,?2,?3,?4)",
            params![idea, source, params_sha, now],
        )?;
        let frozen = tx.query_row(
            "SELECT frozen_at_ms FROM quote_draft WHERE idea_id=?1 AND source_blob=?2 AND params_blob=?3",
            params![idea, source, params_sha],
            |r| r.get(0),
        ).optional()?;
        tx.commit()?;
        Ok(frozen)
    }

    pub fn replay_exposed(&self, from: NaiveDate, to: NaiveDate) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM replay_exposure WHERE from_day<=?2 AND to_day>=?1)",
            params![from.to_string(), to.to_string()],
            |r| r.get(0),
        )?)
    }

    pub fn record_replay_exposure(&mut self, from: NaiveDate, to: NaiveDate) -> Result<()> {
        self.db.execute(
            "INSERT OR IGNORE INTO replay_exposure(from_day,to_day) VALUES(?1,?2)",
            params![from.to_string(), to.to_string()],
        )?;
        Ok(())
    }

    /// Ordinary research can read older dates, but reserved confirmation
    /// inputs remain sealed (including across campaign aliases).
    pub fn holdout_overlaps(&self, from: NaiveDate, to: NaiveDate) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM campaign WHERE holdout_from<=?2 AND holdout_to>=?1)",
            params![from.to_string(), to.to_string()],
            |r| r.get(0),
        )?)
    }
}
