use super::*;

pub(crate) struct CallGuard<'a> {
    pub(crate) cancel: Option<&'a std::sync::atomic::AtomicBool>,
    pub(crate) deadline: Option<i64>,
    pub(crate) not_before: Option<i64>,
    pub(crate) configuration: Option<(&'a Config, &'a PlannerConfig)>,
    /// Actual observation wall clock. This is the only source for model-visible
    /// observation timestamps; it is never derived from an anchor plus elapsed.
    pub(crate) clock: &'a dyn Fn() -> i64,
    /// Monotonic elapsed milliseconds for duration budgets. Absent callers keep
    /// the historical wall-clock budget fallback.
    pub(crate) duration: Option<&'a dyn Fn() -> i64>,
    /// Wall time the duration clock started from. Absent means no projection.
    pub(crate) started_wall_ms: Option<i64>,
}
impl CallGuard<'_> {
    fn check_configuration(&self) -> Result<()> {
        if let Some((c, cfg)) = self.configuration
            && let Some(path) = &c.planner_config
        {
            let current = PlannerConfig::load_for_session(path, c.session)?;
            if policy_bytes(&current)? != policy_bytes(cfg)? {
                return Err("planner_event_configuration_changed".into());
            }
        }
        Ok(())
    }
    /// The actual current wall observation. Never `started + elapsed`.
    pub(crate) fn wall_ms(&self) -> i64 {
        (self.clock)()
    }
    /// A conservative admission time for deadline/not-before checks: the later
    /// of the actual wall observation and the monotonic duration projection.
    /// An absolute UTC deadline must respect a forward wall jump or a suspended
    /// host even when the monotonic reading is slow; a frozen wall must not
    /// freeze it either. This is not an observation timestamp and must never be
    /// persisted as UTC.
    pub(crate) fn projected_wall_ms(&self) -> i64 {
        let projected = match (self.started_wall_ms, self.duration) {
            (Some(start), Some(duration)) => start.saturating_add((duration)().max(0)),
            _ => (self.clock)(),
        };
        projected.max(self.wall_ms())
    }
    /// A wall backstep or negative monotonic reading refuses new calls instead
    /// of silently fabricating a later UTC observation.
    pub(crate) fn check_clock(&self) -> Result<()> {
        if self
            .started_wall_ms
            .is_some_and(|start| self.wall_ms() < start)
            || self.duration.is_some_and(|duration| (duration)() < 0)
        {
            return Err("planner_clock_anomaly".into());
        }
        Ok(())
    }
    /// Config change, clock anomaly and a passed event deadline reject a
    /// finished answer or a new call.
    pub(crate) fn check_finished(&self) -> Result<()> {
        self.check_configuration()?;
        self.check_clock()?;
        let wall = self.wall_ms();
        let projected = self.projected_wall_ms();
        // An absolute deadline is passed when either the actual wall or the
        // monotonic projection is past it. A not-before window is not entered
        // while the actual wall is still before it, even if a monotonic reading
        // claims otherwise.
        if self.deadline.is_some_and(|deadline| projected > deadline)
            || self
                .not_before
                .is_some_and(|not_before| wall < not_before || projected < not_before)
        {
            return Err("planner_event_deadline_expired".into());
        }
        Ok(())
    }
    pub(crate) fn check_cancel(&self) -> Result<()> {
        if self
            .cancel
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire))
        {
            Err("planner_cancelled_no_replay".into())
        } else {
            Ok(())
        }
    }
    pub(crate) fn check(&self) -> Result<()> {
        self.check_finished()?;
        self.check_cancel()
    }
}
pub(crate) fn preview_guard() -> CallGuard<'static> {
    CallGuard {
        cancel: None,
        deadline: None,
        not_before: None,
        configuration: None,
        clock: &millis,
        duration: None,
        started_wall_ms: None,
    }
}
