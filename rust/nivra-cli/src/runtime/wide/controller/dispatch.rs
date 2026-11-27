//! Deadlines, session phases, and reply dispatch.
use super::*;

impl Controller {
    pub(super) fn time_until_due(&self) -> Duration {
        let now = millis();
        let maintenance =
            Duration::from_millis(self.next_maintenance.saturating_sub(now).max(0) as u64);
        if self.active {
            self.trade_wait(now)
                .map_or(maintenance, |d| maintenance.min(d))
        } else {
            maintenance
        }
    }

    /// Trade on a completion or deadline. Run dashboard and session work on
    /// their own cadence even while the screening feed is busy.
    pub(super) fn tick(&mut self, events: &Events) -> Result<Flow> {
        let now = millis();
        if self.stopping.load(Ordering::Relaxed) {
            return Ok(Flow::Stopped { at: now });
        }
        let maintenance = now >= self.next_maintenance;
        let prepared = events.contains(ControllerEvent::Preparation);
        let trading_due = self.active
            && (events.has_trading_work() || self.trade_wait(now).is_some_and(|d| d.is_zero()));
        if !maintenance && !prepared && !trading_due {
            return Ok(Flow::Continue);
        }
        let t = Now {
            now,
            day: local(now).date_naive(),
            m: minute(now),
        };
        if !maintenance {
            if self.active {
                self.trade_session(t, self.history_admitted(), events, false)?;
            }
            if prepared {
                self.prepare_universe(t)?;
                self.prepare_options(t)?;
            }
            return Ok(Flow::Continue);
        }

        self.active = self.session_active(t)?;
        let active = self.active;
        self.track_feed_activity(now, active);
        if active {
            self.trade_session(t, self.history_admitted(), events, true)?;
        }
        self.record_archive_gaps()?;
        self.shadow_scorer.poll();
        let session_end_ms = session_end_ms(&self.config);
        let research_context_unavailable = self.event_config.is_some() && session_end_ms.is_none();
        if research_context_unavailable {
            self.research_tasks_status = serde_json::json!({
                "status":"unavailable",
                "reason":"controller_session_end_unavailable",
                "action":"none"
            });
        }
        self.prepare_universe(t)?;
        self.prepare_options(t)?;
        self.prepare_history(t)?;
        self.check_readiness(t);
        let mut status = if self.universe.is_some() {
            "sleeping_until_configured_session"
        } else {
            "sleeping_waiting_for_d1_universe"
        }
        .to_owned();
        let history_admitted = self.history_admitted();
        if let Some(closed) = self.close_session(t)? {
            status = closed;
        }
        self.owner_research(now, research_context_unavailable);
        let agent_now = millis();
        self.agent_tick(
            Now {
                now: agent_now,
                day: local(agent_now).date_naive(),
                m: minute(agent_now),
            },
            active && history_admitted && self.universe.is_some() && self.read_failure.is_none(),
            history_admitted && self.universe.is_some() && self.read_failure.is_none(),
        );
        if t.day > self.config.session {
            match self.rollover(t)? {
                Advance::Adopted => return Ok(Flow::Continue),
                Advance::Idle(idle) => status = idle.to_owned(),
            }
        }
        if self.p.stopped {
            status = "stopped_operator_review_required".to_owned();
        }
        self.emit_transitions(t, active)?;
        self.note_health(now);
        let interval_ms = self.publish(
            t,
            status,
            active,
            history_admitted,
            research_context_unavailable,
        )?;
        self.next_maintenance = [
            millis().saturating_add(interval_ms as i64),
            session_minute_ms(&self.config, self.config.history_preparation_start_minute)
                .unwrap_or(0),
            session_minute_ms(&self.config, self.config.execution.session_start_minute)
                .unwrap_or(0),
            session_end_ms.unwrap_or(0),
        ]
        .into_iter()
        .filter(|deadline| *deadline > now)
        .min()
        .expect("maintenance interval positive");
        Ok(Flow::Continue)
    }
}
