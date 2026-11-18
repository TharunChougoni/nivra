use super::*;

#[cfg(test)]
pub(crate) fn owner_fixture_config(dir: &std::path::Path) -> (Config, planner::PlannerConfig) {
    let mut c = super::super::tests::config(dir);
    let mut cfg: planner::PlannerConfig = serde_json::from_value(json!({
        "enabled":true,
        "digest":{"enabled":true,"model_id":"gpt-6-luna","transport":"openai_codex",
            "tools":[],"timeout_ms":20000,"output_cap":4096,"daily_call_budget":1},
        "events":{"enabled":true,"cooldown_ms":0,"max_pending":10,
            "policy":{"price_move_bps":100,"volume_step_units":null,"gap_bps":null,
                "proximity_bps":null,"max_event_age_ms":60000,"max_news_age_ms":60000,
                "max_instruments":20,"max_identities":100}},
        "orchestrator":{"enabled":true,"model":{"model_id":"grok-4.7","transport":"direct_oauth"},"triggers":{"supervisor":{"enabled":true,"prompt":"prompts/supervisor.md"}}}
    }))
    .unwrap();
    let supervisor_prompt = dir.join("owner-supervisor.md");
    fs::write(&supervisor_prompt, "owner decision fixture").unwrap();
    cfg.orchestrator
        .as_mut()
        .unwrap()
        .triggers
        .get_mut(&super::super::orchestrator_model::Trigger::Supervisor)
        .unwrap()
        .prompt = supervisor_prompt;
    // The fresh-quote executor reloads the durable policy to refuse a stale
    // intent; point it at the exact same reviewed policy this fixture uses.
    let planner_path = dir.join("owner-planner.json");
    fs::write(&planner_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    c.planner_config = Some(planner_path);
    (c, cfg)
}
