use super::*;

pub(crate) struct CallGuard<'a> {
    pub(crate) book: &'a Config,
    pub(crate) policy: &'a PlannerConfig,
    pub(crate) clock: &'a (dyn Fn() -> i64 + Send + Sync),
}

impl CallGuard<'_> {
    pub(crate) fn check(&self) -> Result<()> {
        if let Some(path) = &self.book.planner_config {
            let current = PlannerConfig::load_for_session(path, self.book.session)?;
            if policy_bytes(&current)? != policy_bytes(self.policy)? {
                return Err("planner_event_configuration_changed".into());
            }
        }
        Ok(())
    }

    /// Use the actual observation time for model-visible timestamps. The
    /// worker's monotonic budget must never supply a UTC observation time.
    pub(crate) fn wall_ms(&self) -> i64 {
        (self.clock)()
    }
}
