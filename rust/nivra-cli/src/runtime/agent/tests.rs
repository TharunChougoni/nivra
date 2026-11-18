use super::check::FixtureBackend;
use super::lanes::{LaneRunner, TurnContext};
use super::protocol::TurnStatus;
use super::tools::{TOOLS, lane_definitions};
use super::*;
use crate::runtime::lane_queue::{self, MessageKind, MessageState};
use crate::runtime::web_tools::{HttpResponse, Transport};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::Instant;

/// The agent's side of the pipe, scripted per test. No process, no socket.
struct Peer {
    reader: BufReader<std::io::PipeReader>,
    writer: std::io::PipeWriter,
    calls: usize,
}

impl Peer {
    fn send(&mut self, mut message: Value) {
        message["v"] = json!(1);
        writeln!(self.writer, "{message}").unwrap();
    }
    fn recv(&mut self) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    fn turn(&mut self) -> (String, Value) {
        let turn = self.recv();
        assert_eq!(turn["type"], "turn");
        (turn["id"].as_str().unwrap().to_owned(), turn)
    }
    fn call(&mut self, turn: &str, name: &str, args: Value) -> Value {
        self.calls += 1;
        let call_id = format!("call-{}", self.calls);
        self.send(
            json!({"type":"tool_call","turn_id":turn,"call_id":call_id,"name":name,"args":args}),
        );
        let result = self.recv();
        assert_eq!(result["call_id"], call_id.as_str());
        result
    }
    fn message(&mut self, turn: &str, cost_usd: f64) {
        self.send(json!({"type":"event","turn_id":turn,"event":{"kind":"message","stop_reason":"toolUse","usage":{"input":100,"output":10,"cost_usd":cost_usd}}}));
    }
    fn finish(&mut self, turn: &str, status: &str, text: &str, calls: u64, cost_usd: f64) {
        self.send(json!({"type":"turn_result","id":turn,"status":status,"output":{"text":text},
            "usage":{"input":100*calls,"output":10*calls,"cache_read":50,"cache_write":0,"cost_usd":cost_usd,"calls":calls},
            "model":"scripted","provider":"fake","session_file":null,"prompt_sha256":"0".repeat(64)}));
    }
}

fn connect<T: Send + 'static>(
    script: impl FnOnce(&mut Peer) -> T + Send + 'static,
) -> (AgentClient, JoinHandle<T>) {
    let (from_rust, to_agent) = std::io::pipe().unwrap();
    let (from_agent, to_rust) = std::io::pipe().unwrap();
    let handle = std::thread::spawn(move || {
        let mut peer = Peer {
            reader: BufReader::new(from_rust),
            writer: to_rust,
            calls: 0,
        };
        peer.send(json!({"type":"ready","pi_version":"test","node":"test","pid":1}));
        script(&mut peer)
    });
    let client = AgentClient::connect(from_agent, to_agent, None, Instant::now()).unwrap();
    (client, handle)
}

/// Serves canned public responses so the web tools run without a socket.
struct FakeWeb;
impl Transport for FakeWeb {
    fn get(
        &mut self,
        url: &str,
        _headers: &[(&str, String)],
        _timeout_ms: u64,
        _max_bytes: u64,
    ) -> std::result::Result<HttpResponse, String> {
        let (content_type, body) = if url.starts_with("https://search.example.test/") {
            ("application/json", r#"{"results":[]}"#)
        } else if url.starts_with("https://news.example.test/feed") {
            (
                "application/rss+xml",
                "<rss><channel><item><title>Results</title><link>https://news.example.test/a</link></item></channel></rss>",
            )
        } else {
            ("text/html", "<html><body>Quarterly results</body></html>")
        };
        Ok(HttpResponse {
            status: 200,
            content_type: Some(content_type.into()),
            bytes: body.as_bytes().to_vec(),
        })
    }
}

fn lane_settings(daily_calls: u64, daily_cost_usd: f64) -> LaneSettings {
    LaneSettings {
        model: ModelSpec {
            provider: "openai-codex".into(),
            id: "gpt-6.1-sol".into(),
            thinking: None,
        },
        daily_calls,
        daily_cost_usd,
        turn_timeout_ms: 10_000,
        max_tool_calls: 40,
    }
}

fn settings(daily_calls: u64, daily_cost_usd: f64) -> AgentSettings {
    AgentSettings {
        node: "/usr/bin/node".into(),
        entry: "/opt/nivra/agent/dist/src/main.js".into(),
        agent_dir: "/var/lib/nivra/pi".into(),
        prompts: None,
        lanes: Lane::ALL
            .into_iter()
            .map(|lane| (lane, lane_settings(daily_calls, daily_cost_usd)))
            .collect(),
    }
}

/// Every reviewed capability on, so every bridged tool is published.
fn all_tools(dir: &std::path::Path) -> (Config, planner::PlannerConfig) {
    use crate::runtime::harness::research::{
        CandidateComputeSettings, CandidateDataset, CandidateDatasetKind, ResearchSettings,
    };
    use crate::runtime::owner_tasks::{
        OwnerResearchQuoteCaptureSettings, OwnerResearchSettings, OwnerTasksSettings,
        ReviewSettings,
    };
    let (c, mut cfg) = crate::runtime::decision_owner::owner_fixture_config(dir);
    cfg.owner_tasks = Some(OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: ReviewSettings::default(),
        research: Some(OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: Some(OwnerResearchQuoteCaptureSettings {
                enabled: true,
                max_rows: 64,
            }),
            observation_capture: None,
            lane: Some(Default::default()),
        }),
    });
    cfg.workspace = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
    cfg.web_tools = Some(
        serde_json::from_value(json!({"enabled":true,"provider":"searxng",
            "search_url":"https://search.example.test/search","news_feeds":["https://news.example.test/feed"],
            "min_interval_ms":0}))
        .unwrap(),
    );
    cfg.research_tools = Some(ResearchSettings {
        queue: dir.join("research.sqlite"),
        stage_root: dir.join("research-stage"),
        task_ttl_ms: 3_600_000,
        termination_grace_ms: 5_000,
        library_max_candidates: None,
        admission_limits: Some(nivra_store::research_jobs::AdmissionLimits {
            max_trials_per_input: 40,
            max_session_trials: 100,
        }),
        library_max_bytes: None,
        candidate_compute: Some(CandidateComputeSettings {
            enabled: true,
            abi: "scorer_replay_v1".into(),
            sandbox: nivra_sandbox::ScorerSandboxConfig {
                wall_ms: 2_000,
                ..Default::default()
            },
            replay: nivra_cli::scorer_compare::ScorerReplaySettings {
                max_snapshots: 10,
                min_valid_snapshots: 1,
                horizons_minutes: vec![5],
                top_n: 2,
                max_age_ms: 2_000,
                lookback_samples: 20,
                ..Default::default()
            },
            max_source_bytes: 262_144,
            max_params_bytes: 65_536,
            max_result_bytes: 1_048_576,
            wall_millis: 120_000,
            limits: nivra_research::worker::ResourceLimits {
                cpu_millis: 120_000,
                tasks: 1,
                workspace_bytes: 1_048_576,
                file_bytes: 65_536,
            },
            helper_path: None,
            unit_tasks: 16,
            unit_memory_bytes: 512 * 1024 * 1024,
            unit_open_files: 128,
            unit_cpu_millis: 0,
            unit_wall_millis: 0,
            datasets: vec![CandidateDataset {
                dataset_id: "current_session_features_v1".into(),
                kind: CandidateDatasetKind::CurrentSessionFeatures,
            }],
            execution: None,
            observation: None,
        }),
        registrations: Vec::new(),
    });
    // The turn guard refuses a policy that differs from the reviewed file.
    std::fs::write(
        c.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&cfg).unwrap(),
    )
    .unwrap();
    (c, cfg)
}

fn instrument(key: &str) -> Value {
    json!({"key":key,"symbol":key.rsplit('|').next().unwrap(),"series":"EQ","tick":"0.05"})
}

fn context(cash: &str) -> TurnContext {
    let bundle = json!({
        "session":"2026-10-16",
        "allowlist":[instrument("NSE_EQ|A"),instrument("NSE_EQ|B")],
        "discovery":{"prospects":[{"instrument_key":"NSE_EQ|A","symbol":"A","sample_change_bps":12.5,"bid":"100","ask":"100.05","bid_units":10,"ask_units":12,"quote_age_ms":100,"screening_age_ms":200,"observed_ms":123,"screening_reason":"move"}],"market_median_bps":5.0},
        "historical_features":[{"instrument":instrument("NSE_EQ|A"),"history_as_of":"2026-10-15","history_current":true,"return_20d":0.1,"liquidity":"100000"}],
        "cash":cash,"positions":{},
        "book_state":{"halted":false,"stopped":false,"closed":false},
        "decision_history":{"luna":null,"grok":[],"advisory_memory":null},
        "trigger":{"kind":"schedule","reason":"test","observed_ms":123}
    });
    let mut portfolio = crate::runtime::tests::test_portfolio();
    portfolio.cash = nivra_core::finance::Amount::parse(cash).unwrap();
    TurnContext {
        instruments: serde_json::from_value(bundle["allowlist"].clone()).unwrap(),
        bundle,
        backend: Box::new(FixtureBackend(Some(
            json!({"quotes":[],"execution_authorized":false}),
        ))),
        portfolio,
        web: Box::new(FakeWeb),
    }
}

fn args(name: &str) -> Value {
    let task = json!({"task_id":"a".repeat(64),"revision":1});
    match name {
        "evidence" => json!({"evidence_id":"portfolio"}),
        "snapshot" | "features" | "intraday" => json!({"instrument_keys":["NSE_EQ|A"]}),
        "prospects" => json!({"limit":1,"sort":null}),
        "rank" => json!({"sort_by":null,"order":null,"limit":1,"min_liquidity":null}),
        "portfolio" | "session" => json!({}),
        "decisions" => json!({"limit":1}),
        "analysis" => {
            json!({"task_key":"gap-fade","goal":"Does the gap fade?","analysis_id":"none","dataset_id":"none"})
        }
        "plan" => json!({"actions":[],"stance":"hold","notes":"no edge"}),
        "research" => json!({"question":"Does the gap fade survive costs?","refs":null}),
        "research_status" | "research_result" | "research_cancel" | "backtest_status"
        | "backtest_result" | "backtest_cancel" => task,
        "quotes" => json!({"instrument_keys":["NSE_EQ|A"],"from_ms":0,"to_ms":1}),
        "run" => json!({"script":"scan.py","input":null,"input_path":null}),
        "backtest" => {
            json!({"task_key":"cand-1","hypothesis":"momentum","source":"def score(f):\n    return 0\n",
            "params":"{}","dataset_id":"current_session_features_v1","training_from_ms":0,"training_to_ms":1,
            "selection_from_ms":2,"selection_to_ms":3,"horizon_minutes":5,"parent_task_id":null,"parent_ref":null})
        }
        "list" => json!({"path":null}),
        "read" => json!({"path":"notes/brief.md"}),
        "write" => json!({"path":"notes/brief.md","text":"gap fade notes"}),
        "install" => json!({"name":"scan.py","text":"print(1)"}),
        "search" => json!({"query":"RELIANCE results"}),
        "fetch" => json!({"url":"https://news.example.test/a"}),
        "news" => json!({"query":null}),
        _ => panic!("no fixture args for {name}"),
    }
}

/// Failure modes: a lane allowlisted tool missing from what pi registers, a
/// pi name that no Rust executor serves, a tool leaking into another lane,
/// and a bridge that answers without reaching the existing executor (or
/// bypasses its validation).
#[test]
fn every_lane_tool_round_trips_through_the_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = all_tools(dir.path());
    let settings = settings(100, 10.0);
    let clock = || 1_760_600_000_000_i64;
    // Each tool's observable answer from its existing Rust executor. The
    // refusals are today's executor policy, not the bridge: owner research
    // tasks need a durable owner claim that pi turns do not hold yet, and the
    // research binding refuses workspace run/install and the web tools.
    let expected = |lane: Lane, name: &str| -> std::result::Result<(), &'static str> {
        match (lane, name) {
            (_, "intraday") => Err("decision_evidence_unavailable"),
            (_, "analysis" | "research_status" | "research_result" | "research_cancel") => {
                Err("harness_owner_research_scope_invalid")
            }
            (_, "run" | "install") => Err("workspace_research_scope_invalid"),
            (_, "quotes") => Err("scorer_snapshots_unavailable"),
            (_, "backtest_status" | "backtest_result" | "backtest_cancel") => {
                Err("research_queue_unavailable")
            }
            (Lane::Research, "search" | "fetch" | "news") => Err("harness_web_tools_scope_invalid"),
            _ => Ok(()),
        }
    };
    let workspace = cfg.workspace.as_ref().unwrap();
    crate::runtime::workspace::execute(
        &c.directory,
        workspace,
        crate::runtime::workspace::parse_tool("workspace_write", args("write")).unwrap(),
    )
    .unwrap();
    let mut mismatches = Vec::new();
    for lane in Lane::ALL {
        let published: Vec<String> = lane_definitions(&cfg, lane)
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        let allowlist: Vec<String> = TOOLS
            .iter()
            .filter(|tool| tool.lanes.contains(&lane))
            .map(|tool| tool.name.to_owned())
            .collect();
        assert_eq!(
            published, allowlist,
            "{lane:?} registers its whole allowlist"
        );
        let (mut client, peer) = connect(|peer| {
            let (id, turn) = peer.turn();
            let mut replies = Vec::new();
            for tool in turn["tools"].as_array().unwrap() {
                let name = tool["name"].as_str().unwrap();
                replies.push((name.to_owned(), peer.call(&id, name, args(name))));
            }
            // A tool from outside the lane is refused before any executor.
            let foreign = if turn["lane"] == "decision" {
                "write"
            } else {
                "plan"
            };
            replies.push(("foreign".into(), peer.call(&id, foreign, json!({}))));
            peer.finish(&id, "ok", "done", 1, 0.0);
            replies
        });
        let mut context = context("5000");
        let mut runner = LaneRunner::new(&mut client, &settings, &c, &cfg, &clock);
        let turn = runner
            .turn(lane, json!({"kind":"test"}), json!({}), &mut context)
            .unwrap();
        assert_eq!(turn.outcome.status, TurnStatus::Ok);
        for (name, reply) in peer.join().unwrap() {
            if name == "foreign" {
                assert_eq!(reply["error"], "agent_tool_not_in_lane");
                continue;
            }
            let observed = match reply["ok"].as_bool() {
                Some(true) => Ok(()),
                _ => Err(reply["error"].as_str().unwrap_or_default().to_owned()),
            };
            if observed != expected(lane, &name).map_err(str::to_owned) {
                mismatches.push(format!("{lane:?} {name}: {observed:?}"));
            }
        }
        assert_eq!(turn.plan.is_some(), lane == Lane::Decision);
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

/// Failure modes: a research request that never reaches a research turn, an
/// answer that never reaches the next decision brief, an answer delivered
/// twice, and a plan that skips the existing Rust validation.
#[test]
fn request_research_reaches_a_research_turn_and_the_next_decision_brief() {
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = all_tools(dir.path());
    let settings = settings(100, 10.0);
    let clock = || 1_760_600_000_000_i64;
    let question = "Does the gap fade survive costs?";
    let (mut client, peer) = connect(move |peer| {
        let (decision, brief) = peer.turn();
        assert_eq!(brief["brief"]["research_updates"], json!([]));
        let queued = peer.call(&decision, "research", args("research"));
        let refused = peer.call(&decision, "plan", json!({"actions":[],"stance":"x"}));
        let unlisted = peer.call(&decision, "plan", json!({"actions":[{"instrument_key":"NSE_EQ|Z","side":"buy","rupees":"100","units":null,"max_price":"10","rationale":"x","exit_intent":{"target":null,"stop":null,"time":null}}],"stance":"x","notes":"y"}));
        let plan = peer.call(&decision, "plan", json!({"actions":[{"instrument_key":"NSE_EQ|A","side":"buy","rupees":"1000","units":null,"max_price":"100.10","rationale":"momentum","exit_intent":{"target":null,"stop":null,"time":null}}],"stance":"selective","notes":"asked research"}));
        peer.finish(&decision, "ok", "asked research", 4, 0.03);
        let (research, brief) = peer.turn();
        assert_eq!(brief["lane"], "research");
        assert_eq!(brief["brief"]["question"], question);
        assert!(
            brief["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|tool| tool["name"] != "research")
        );
        peer.finish(&research, "ok", "Gap fades only before costs.", 2, 0.02);
        let (next, brief) = peer.turn();
        let updates = brief["brief"]["research_updates"].clone();
        peer.finish(&next, "ok", "holding", 1, 0.01);
        (queued, refused, unlisted, plan, updates)
    });
    let mut context = context("5000");
    let mut runner = LaneRunner::new(&mut client, &settings, &c, &cfg, &clock);
    let first = runner
        .decision(json!({"kind":"schedule"}), json!("open"), &mut context)
        .unwrap();
    let research = runner.research_requests(&mut context).unwrap();
    let next = runner
        .decision(json!({"kind":"fill"}), json!("after"), &mut context)
        .unwrap();
    let (queued, refused, unlisted, plan, updates) = peer.join().unwrap();

    assert_eq!(queued["output"]["state"], "queued");
    assert_eq!(refused["error"], "planner_json_invalid");
    assert_eq!(
        unlisted["output"]["verdicts"][0]["reason"],
        "action_allowlist"
    );
    assert_eq!(plan["output"]["verdicts"][0]["reason"], "accepted");
    assert_eq!(first.plan.unwrap()["verdicts"][0]["units"], 9);
    assert_eq!(research.len(), 1);
    assert_eq!(updates.as_array().unwrap().len(), 1);
    assert_eq!(updates[0]["kind"], "research_result");
    assert_eq!(updates[0]["request_id"], queued["output"]["request_id"]);
    assert_eq!(updates[0]["summary"], "Gap fades only before costs.");
    assert_eq!(next.outcome.status, TurnStatus::Ok);
    let db = lane_queue::open(&c.directory).unwrap();
    let request = lane_queue::load(&db, queued["output"]["request_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(request.state, MessageState::Done);
    assert!(lane_queue::trading_updates(&c, clock()).unwrap().is_empty());
    let artifact = c
        .directory
        .join("model")
        .join(match &updates[0]["artifact_ref"] {
            Value::String(path) => path,
            other => panic!("artifact_ref {other}"),
        });
    assert!(
        std::fs::read_to_string(artifact)
            .unwrap()
            .contains("Gap fades")
    );
    let spent: Value =
        serde_json::from_slice(&std::fs::read(c.directory.join("model/agent/usage.json")).unwrap())
            .unwrap();
    assert_eq!(spent["lanes"]["decision"]["calls"], 5);
    assert_eq!(spent["lanes"]["research"]["calls"], 2);
}

/// Failure modes: a turn that keeps spending after pi reports the lane's
/// daily allowance is gone, a next turn that still calls the model, a
/// research turn that blocks a waiting decision, and an agent crash that
/// hangs the controller.
#[test]
fn budgets_preemption_and_agent_exit_stop_turns() {
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = all_tools(dir.path());
    let settings = settings(10, 1.0);
    assert!(settings.validate().is_ok());
    let mut unbounded = settings.clone();
    unbounded
        .lanes
        .get_mut(&Lane::Research)
        .unwrap()
        .daily_calls = 0;
    assert!(
        unbounded.validate().is_err(),
        "a lane needs a positive call budget"
    );
    // Without the block, policy bytes stay what deployed books hashed.
    assert!(serde_json::to_value(&cfg).unwrap().get("agent").is_none());
    let clock = || 1_760_600_000_000_i64;
    let (mut client, peer) = connect(|peer| {
        let (id, _) = peer.turn();
        peer.message(&id, 0.6);
        peer.message(&id, 0.6);
        let abort = peer.recv();
        peer.finish(&id, "aborted", "", 2, 1.2);
        let (research, _) = peer.turn();
        peer.message(&research, 0.1);
        let preempted = peer.recv();
        peer.finish(&research, "aborted", "", 1, 0.1);
        (abort, preempted)
    });
    let mut context = context("5000");
    let mut runner = LaneRunner::new(&mut client, &settings, &c, &cfg, &clock);
    let spent = runner
        .turn(Lane::Decision, json!({}), json!({}), &mut context)
        .unwrap();
    assert_eq!(spent.outcome.status, TurnStatus::Aborted);
    let refused = runner
        .turn(Lane::Decision, json!({}), json!({}), &mut context)
        .err()
        .unwrap();
    assert_eq!(refused.to_string(), "agent_budget_exhausted");
    lane_queue::post_request(
        &c,
        &cfg,
        "decision-1",
        "Is the gap real?",
        Vec::new(),
        clock(),
    )
    .unwrap();
    // A decision trigger arrives while the research turn runs.
    let preempt = runner.preempt.clone();
    let waiting = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        preempt.store(true, Ordering::Release);
    });
    let research = runner.research_requests(&mut context).unwrap();
    waiting.join().unwrap();
    let (abort, preempted) = peer.join().unwrap();
    assert_eq!(abort["type"], "abort");
    assert_eq!(abort["reason"], "agent_budget_exhausted");
    assert_eq!(preempted["reason"], "agent_preempted");
    assert_eq!(research[0].outcome.status, TurnStatus::Aborted);
    let db = lane_queue::open(&c.directory).unwrap();
    let failed = lane_queue::list(
        &db,
        &c.session.to_string(),
        &[MessageKind::ResearchRequest],
        MessageState::Failed,
        clock(),
        4,
    )
    .unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(
        failed[0].reason,
        Some(lane_queue::TransitionReason::Interrupted)
    );
    drop(runner);
    // An agent that dies mid-turn ends the turn with an error, not a hang,
    // and what pi already reported stays charged.
    let (mut client, peer) = connect(|peer| {
        let (id, _) = peer.turn();
        peer.message(&id, 0.25);
    });
    let mut runner = LaneRunner::new(&mut client, &settings, &c, &cfg, &clock);
    let exited = runner
        .turn(Lane::Review, json!({}), json!({}), &mut context)
        .unwrap();
    peer.join().unwrap();
    assert_eq!(exited.outcome.status, TurnStatus::Error);
    assert_eq!(exited.outcome.error.as_deref(), Some("agent_exited"));
    let spent: Value =
        serde_json::from_slice(&std::fs::read(c.directory.join("model/agent/usage.json")).unwrap())
            .unwrap();
    assert_eq!(spent["lanes"]["review"]["cost_usd"], 0.25);
}
