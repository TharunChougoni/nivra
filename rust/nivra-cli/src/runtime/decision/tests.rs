use super::*;
use serde_json::json;

/// A pi paper policy with every lane, written beside the book so the
/// fresh-quote executor reloads the same reviewed policy.
pub(crate) fn decision_fixture_config(dir: &std::path::Path) -> (Config, planner::PlannerConfig) {
    let mut c = super::super::tests::config(dir);
    let lane = json!({
        "model":{"provider":"openai-codex","id":"gpt-6.1-sol"},
        "daily_calls":10,"daily_cost_usd":10.0,"turn_timeout_ms":10_000,"max_tool_calls":40
    });
    let cfg: planner::PlannerConfig = serde_json::from_value(json!({
        "enabled":true,
        "agent":{
            "mode":"paper","node":"/usr/bin/node",
            "entry":"/opt/nivra/agent/dist/src/main.js","agent_dir":"/var/lib/nivra/pi",
            "lanes":{"decision":lane,"research":lane,"review":lane}
        }
    }))
    .unwrap();
    let planner_path = dir.join("decision-planner.json");
    fs::write(&planner_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    c.planner_config = Some(planner_path);
    (c, cfg)
}
