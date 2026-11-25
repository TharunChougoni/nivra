//! Bounded execution and usage views; original incident handling stays intact.
use super::{Connection, Result};

pub(super) fn execution_projection(db: &Connection) -> Result<Vec<serde_json::Value>> {
    Ok(db.prepare("SELECT a.decision,a.idx,a.verdict,r.reason FROM planner_action a LEFT JOIN planner_refusal r ON r.decision=a.decision AND r.idx=a.idx ORDER BY a.rowid DESC LIMIT 200")?
        .query_map([],|r|Ok(serde_json::json!({"decision_id":r.get::<_,String>(0)?,"index":r.get::<_,i64>(1)?,"verdict":r.get::<_,String>(2)?,"reason":r.get::<_,Option<String>>(3)?})))?.collect::<std::result::Result<Vec<_>,_>>()?)
}
