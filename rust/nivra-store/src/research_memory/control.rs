//! Skill revisions, IST-day budget reservations, the coordinator lease, the
//! loop's published state and the import ledger.
use super::{DayLimits, DaySpend, Error, Lease, Result, SKILL, SkillRevision, Store, skill_row};
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;

impl Store {
    /// Stage a learned skill draft for review. A repeat digest is a no-op.
    pub fn propose_skill(
        &mut self,
        name: &str,
        bytes: &[u8],
        finding_ids: &[i64],
        now: i64,
    ) -> Result<(SkillRevision, bool)> {
        let digest = self.put_object(bytes)?;
        let parent: Option<String> = self
            .db
            .query_row(
                "SELECT digest FROM skill_revision WHERE name=?1 AND status='approved'",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        let inserted = self.db.execute(
            "INSERT OR IGNORE INTO skill_revision(name,digest,parent_digest,finding_ids,status,created_at_ms) VALUES(?1,?2,?3,?4,'proposed',?5)",
            params![name, digest, parent, serde_json::to_string(finding_ids)?, now],
        )? == 1;
        let skill = self.db.query_row(
            &format!("{SKILL} WHERE name=?1 AND digest=?2"),
            params![name, digest],
            skill_row,
        )?;
        Ok((skill, inserted))
    }

    /// Skill revisions, optionally in one status, oldest first.
    pub fn skills(&self, status: Option<&str>) -> Result<Vec<SkillRevision>> {
        let mut statement = self.db.prepare(&format!(
            "{SKILL} WHERE (?1 IS NULL OR status=?1) ORDER BY id"
        ))?;
        Ok(statement
            .query_map([status], skill_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Record a review decision. Approval retires the name's previous active
    /// revision, so one revision per name stays active.
    pub fn review_skill(
        &mut self,
        name: &str,
        digest: &str,
        approve: bool,
        reason: &str,
        reviewer: &str,
        now: i64,
    ) -> Result<SkillRevision> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM skill_revision WHERE name=?1 AND digest=?2",
                params![name, digest],
                |row| row.get(0),
            )
            .optional()?;
        if status.as_deref() != Some("proposed") {
            return Err(Error("research_skill_not_proposed"));
        }
        if approve {
            tx.execute(
                "UPDATE skill_revision SET status='retired', retired_at_ms=?2 WHERE name=?1 AND status='approved'",
                params![name, now],
            )?;
        }
        tx.execute(
            "UPDATE skill_revision SET status=?3, decision_reason=?4, reviewer=?5, active_from_ms=CASE WHEN ?3='approved' THEN ?6 END
             WHERE name=?1 AND digest=?2",
            params![
                name,
                digest,
                if approve { "approved" } else { "rejected" },
                reason,
                reviewer,
                now
            ],
        )?;
        let skill = tx.query_row(
            &format!("{SKILL} WHERE name=?1 AND digest=?2"),
            params![name, digest],
            skill_row,
        )?;
        tx.commit()?;
        Ok(skill)
    }

    /// What `day` has used. Unsettled turn reservations count at their
    /// reserved worst case.
    pub fn day_spend(&self, day: NaiveDate) -> Result<DaySpend> {
        day_spend(&self.db, day)
    }

    /// Reserve one turn's worst-case calls and cost before the provider is
    /// asked. `Ok(Err(reason))` means today's ceilings refuse it.
    pub fn reserve_turn(
        &mut self,
        key: &str,
        day: NaiveDate,
        calls: u64,
        usd_micros: i64,
        limits: &DayLimits,
        now: i64,
    ) -> Result<std::result::Result<(), &'static str>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let spent = day_spend(&tx, day)?;
        if let Some(reason) = limits.turn_refusal(&spent, calls, usd_micros) {
            return Ok(Err(reason));
        }
        tx.execute(
            "INSERT INTO reservation(key,day,kind,calls,usd_micros,settled,created_at_ms) VALUES(?1,?2,'turn',?3,?4,0,?5)",
            params![key, day.to_string(), calls as i64, usd_micros, now],
        )?;
        tx.commit()?;
        Ok(Ok(()))
    }

    /// Replace a turn reservation with its actual usage. Settling twice is a
    /// no-op, so one call is charged once.
    pub fn settle_turn(&mut self, key: &str, calls: u64, usd_micros: i64) -> Result<bool> {
        Ok(self.db.execute(
            "UPDATE reservation SET calls=?2, usd_micros=?3, settled=1 WHERE key=?1 AND kind='turn' AND settled=0",
            params![key, calls as i64, usd_micros],
        )? == 1)
    }

    /// Settle every reservation a dead turn left open at its worst case.
    pub fn settle_abandoned_turns(&mut self, except: Option<&str>) -> Result<usize> {
        Ok(self.db.execute(
            "UPDATE reservation SET settled=1 WHERE kind='turn' AND settled=0 AND key IS NOT ?1",
            [except],
        )?)
    }

    /// Take or renew the named lease. A live holder keeps it even past its
    /// expiry; a dead holder's lease is reclaimed with a new generation.
    pub fn acquire_lease(
        &mut self,
        name: &str,
        owner: &str,
        pid: u32,
        now: i64,
        ttl_ms: i64,
        alive: &dyn Fn(u32) -> bool,
    ) -> Result<Option<Lease>> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(String, u32, i64, i64)> = tx
            .query_row(
                "SELECT owner,pid,generation,expires_at_ms FROM lease WHERE name=?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let generation = match current {
            None => 1,
            Some((held, _, generation, _)) if held == owner => generation,
            Some((_, held_pid, generation, expires))
                if held_pid == 0 || (now >= expires && held_pid != pid && !alive(held_pid)) =>
            {
                generation + 1
            }
            Some(_) => return Ok(None),
        };
        let expires_at_ms = now.saturating_add(ttl_ms);
        tx.execute(
            "INSERT INTO lease(name,owner,pid,generation,expires_at_ms) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(name) DO UPDATE SET owner=excluded.owner, pid=excluded.pid, generation=excluded.generation, expires_at_ms=excluded.expires_at_ms",
            params![name, owner, pid, generation, expires_at_ms],
        )?;
        tx.commit()?;
        Ok(Some(Lease {
            owner: owner.to_owned(),
            generation,
            expires_at_ms,
        }))
    }

    /// Give the lease back between steps so the other entry point can take
    /// it. The next holder gets a new generation.
    pub fn release_lease(&mut self, name: &str, owner: &str) -> Result<()> {
        self.db.execute(
            "UPDATE lease SET expires_at_ms=0, pid=0 WHERE name=?1 AND owner=?2",
            params![name, owner],
        )?;
        Ok(())
    }

    pub(super) fn generation_current(&self, generation: i64) -> Result<bool> {
        let current: Option<i64> = self
            .db
            .query_row(
                "SELECT generation FROM lease WHERE name='coordinator'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(current.is_none_or(|current| current == generation))
    }

    /// The loop's last published state, for the dashboard.
    pub fn loop_state(&self) -> Result<Option<Value>> {
        let body: Option<String> = self
            .db
            .query_row("SELECT body FROM loop_state WHERE id=1", [], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(body.map(|text| serde_json::from_str(&text)).transpose()?)
    }

    pub fn set_loop_state(&mut self, body: &Value) -> Result<()> {
        self.db.execute(
            "INSERT INTO loop_state(id,body) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
            [serde_json::to_string(body)?],
        )?;
        Ok(())
    }

    /// Record that `sha256` was imported to `destination`; `false` when it
    /// already was.
    pub fn record_import(
        &mut self,
        sha256: &str,
        destination: &str,
        origin: &str,
        now: i64,
    ) -> Result<bool> {
        Ok(self.db.execute(
            "INSERT OR IGNORE INTO imported(sha256,destination,origin,at_ms) VALUES(?1,?2,?3,?4)",
            params![sha256, destination, origin, now],
        )? == 1)
    }

    pub fn imported(&self, sha256: &str) -> Result<bool> {
        Ok(self
            .db
            .query_row(
                "SELECT 1 FROM imported WHERE sha256=?1 LIMIT 1",
                [sha256],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
}

pub(super) fn day_spend(db: &Connection, day: NaiveDate) -> Result<DaySpend> {
    Ok(db.query_row(
        "SELECT coalesce(sum(kind='variant'),0), coalesce(sum(kind='turn'),0), coalesce(sum(calls),0), coalesce(sum(usd_micros),0)
         FROM reservation WHERE day=?1",
        [day.to_string()],
        |row| {
            Ok(DaySpend {
                variants: row.get(0)?,
                turns: row.get(1)?,
                calls: row.get::<_, i64>(2)? as u64,
                usd_micros: row.get(3)?,
            })
        },
    )?)
}
