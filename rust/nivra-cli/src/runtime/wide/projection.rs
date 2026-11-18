//! Bounded execution and usage views; original incident handling stays intact.
#[cfg(test)]
use super::{Config, Portfolio, planner, planner_incident};
use super::{Connection, Result};

pub(super) fn execution_projection(db: &Connection) -> Result<Vec<serde_json::Value>> {
    Ok(db.prepare("SELECT decision,idx,verdict FROM planner_action ORDER BY rowid DESC LIMIT 200")?
        .query_map([],|r|Ok(serde_json::json!({"decision_id":r.get::<_,String>(0)?,"index":r.get::<_,i64>(1)?,"verdict":r.get::<_,String>(2)?})))?.collect::<std::result::Result<Vec<_>,_>>()?)
}
#[cfg(test)]
pub(super) fn usage_projection(
    db: &mut Connection,
    c: &Config,
    p: &mut Portfolio,
    now: i64,
) -> Result<serde_json::Value> {
    match planner::usage(c) {
        Ok(usage) => Ok(usage),
        Err(error)
            if matches!(
                error.to_string().as_str(),
                "planner_usage_busy_no_retry" | "planner_usage_lock_timeout"
            ) =>
        {
            Ok(serde_json::json!({"status":"busy_retry_next_observation"}))
        }
        Err(_) => {
            planner_incident(db, c, p, now)?;
            Ok(serde_json::Value::Null)
        }
    }
}
