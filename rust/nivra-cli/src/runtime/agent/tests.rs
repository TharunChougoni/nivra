use super::brief::{self, Brief, Facts, SessionFacts};
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
pub(super) struct Peer {
    reader: BufReader<std::io::PipeReader>,
    writer: std::io::PipeWriter,
    calls: usize,
    /// The model the last turn request named; a real agent reports it back.
    model: Value,
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
    pub(super) fn turn(&mut self) -> (String, Value) {
        let turn = self.recv();
        assert_eq!(turn["type"], "turn");
        self.model = turn["model"].clone();
        (turn["id"].as_str().unwrap().to_owned(), turn)
    }
    pub(super) fn call(&mut self, turn: &str, name: &str, args: Value) -> Value {
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
    pub(super) fn finish(
        &mut self,
        turn: &str,
        status: &str,
        text: &str,
        calls: u64,
        cost_usd: f64,
    ) {
        self.send(json!({"type":"turn_result","id":turn,"status":status,"output":{"text":text},
            "usage":{"input":100*calls,"output":10*calls,"cache_read":50,"cache_write":0,"cost_usd":cost_usd,"calls":calls},
            "model":self.model["id"],"provider":self.model["provider"],"session_file":null,"prompt_sha256":"0".repeat(64)}));
    }
}

pub(super) fn connect<T: Send + 'static>(
    script: impl FnOnce(&mut Peer) -> T + Send + 'static,
) -> (AgentClient, JoinHandle<T>) {
    let (from_rust, to_agent) = std::io::pipe().unwrap();
    let (from_agent, to_rust) = std::io::pipe().unwrap();
    let handle = std::thread::spawn(move || {
        let mut peer = Peer {
            reader: BufReader::new(from_rust),
            writer: to_rust,
            calls: 0,
            model: Value::Null,
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
        fallback: None,
        daily_calls,
        daily_cost_usd,
        turn_timeout_ms: 10_000,
        max_tool_calls: 40,
        compact_at_tokens: None,
    }
}

pub(super) fn settings(daily_calls: u64, daily_cost_usd: f64) -> AgentSettings {
    AgentSettings {
        node: "/usr/bin/node".into(),
        entry: "/opt/nivra/agent/dist/src/main.js".into(),
        agent_dir: "/var/lib/nivra/pi".into(),
        prompts: None,
        mode: AgentMode::Shadow,
        pass_env: Vec::new(),
        triggers: None,
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
    let (c, mut cfg) = crate::runtime::decision::decision_fixture_config(dir);
    let fragment: Value = serde_json::from_str(include_str!(
        "../../../../../packaging/rust/decision-evidence.fragment.json"
    ))
    .unwrap();
    cfg.decision_evidence =
        Some(serde_json::from_value(fragment["decision_evidence"].clone()).unwrap());
    let install_hash = crate::runtime::digest(b"print(1)");
    cfg.workspace = Some(
        serde_json::from_value(json!({
            "enabled":true,
            "install_allowlist":[{"name":"scan.py","sha256":install_hash}]
        }))
        .unwrap(),
    );
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
        quote_capture: Some(crate::runtime::harness::research::QuoteCaptureSettings {
            enabled: true,
            max_rows: 64,
        }),
        candidate_compute: Some(CandidateComputeSettings {
            enabled: true,
            sandbox: nivra_sandbox::SandboxConfig {
                wall_ms: 2_000,
                ..Default::default()
            },
            max_source_bytes: 262_144,
            max_params_bytes: 65_536,
            wall_millis: 120_000,
            unit_tasks: 16,
            unit_memory_bytes: 512 * 1024 * 1024,
            unit_cpu_millis: 0,
            unit_wall_millis: 0,
            datasets: vec![CandidateDataset {
                dataset_id: "current_session_features_v1".into(),
                kind: CandidateDatasetKind::CurrentSessionFeatures,
            }],
            strategy: Some(
                crate::runtime::harness::research::CandidateStrategySettings {
                    enabled: true,
                    abi: "strategy_actions_v1".into(),
                    max_quote_events: 100,
                    max_observations: 1_000,
                    max_tape_bytes: 1_048_576,
                    max_wall_ms: 20_000,
                    max_archive_window_days:
                        crate::runtime::harness::research::default_archive_window_days(),
                    recent_archive_sessions: 3,
                },
            ),
        }),
        registrations: Vec::new(),
    });
    cfg.strategy_execution = Some(planner::StrategyExecution {
        enabled: true,
        ..Default::default()
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
        backend: Box::new(FixtureBackend {
            snapshot: Some(json!({"quotes":[],"execution_authorized":false})),
            intraday: Some(json!({"instruments":[]})),
        }),
        portfolio,
        web: Box::new(FakeWeb),
    }
}

pub(super) fn fixture_agent(client: AgentClient) -> Agent {
    let mut client = Some(client);
    Agent::new(Box::new(move || {
        client.take().ok_or_else(|| "fixture_agent_exited".into())
    }))
}

fn fixture_brief(context: &TurnContext, now_ms: i64) -> Brief {
    Brief::build(
        &Facts {
            session: SessionFacts {
                date: "2026-10-16".into(),
                phase: triggers::Phase::Open,
                minute: 600,
                now_ms,
            },
            portfolio: &context.portfolio,
            ranking: &[],
            memory: None,
            execution: "shadow_never_executed",
            regime: None,
        },
        brief::budget(Lane::Decision, &Default::default(), 100, 10.0, (0.0, 5)),
        Value::Null,
        &[],
    )
}

fn read_brief(turn: &Value) -> Value {
    serde_json::from_str(turn["brief"].as_str().unwrap()).unwrap()
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
        "strategy_select" => json!({"strategy_id":null,"reason":"stand down in fixture"}),
        "verdict" => json!({"escalate":false,"reason":"one tick, no depth","prospects":[]}),
        "research" => {
            json!({"question":"Does the gap fade survive costs?","refs":["2026-11-20","NSE_EQ|INE0ECP01024"]})
        }
        "research_status" | "research_result" | "research_cancel" | "backtest_status"
        | "backtest_result" | "backtest_cancel" => task,
        "quotes" => json!({"instrument_keys":["NSE_EQ|A"],"from_ms":0,"to_ms":1}),
        "run" => json!({"script":"scan.py","input":null,"input_path":null}),
        "backtest" => {
            json!({"task_key":"cand-1","hypothesis":"momentum","source":"def score(f):\n    return 0\n",
            "params":"{}","dataset_id":"current_session_features_v1","training_from_ms":0,"training_to_ms":1,
            "selection_from_ms":2,"selection_to_ms":3})
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
    // `nivra agent-check` loads this fixture at run time only, so a config
    // shape change would otherwise break it without failing any test.
    let fixture: Value = serde_json::from_slice(
        &std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/agent-check.json"
        ))
        .unwrap(),
    )
    .unwrap();
    serde_json::from_value::<planner::PlannerConfig>(fixture["planner"].clone())
        .expect("agent-check fixture planner matches the current planner shape");
    serde_json::from_value::<Config>(fixture["paper"].clone())
        .expect("agent-check fixture paper config matches the current config shape");
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = all_tools(dir.path());
    let settings = settings(100, 10.0);
    let clock = || 1_760_600_000_000_i64;
    // Without registered jobs or captured quotes the executors return their
    // resource-specific errors. Lane authority and schema validation run first.
    let expected = |name: &str| -> std::result::Result<(), &'static str> {
        match name {
            "analysis" => Err("research_unregistered_alias"),
            "research_status" | "research_result" | "research_cancel" | "backtest_status"
            | "backtest_result" | "backtest_cancel" => Err("research_queue_unavailable"),
            "run" => Err("workspace_run_not_allowlisted"),
            "quotes" => Err("scorer_snapshots_unavailable"),
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
        let published: Vec<String> = lane_definitions(&cfg, lane, true)
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
        let (client, peer) = connect(|peer| {
            let (id, turn) = peer.turn();
            let mut replies = Vec::new();
            for tool in turn["tools"].as_array().unwrap() {
                let name = tool["name"].as_str().unwrap();
                replies.push((name.to_owned(), peer.call(&id, name, args(name))));
            }
            // A tool from outside the lane is refused before any executor.
            let foreign = match turn["lane"].as_str().unwrap() {
                "decision" => "write",
                "review" => "run",
                _ => "plan",
            };
            replies.push(("foreign".into(), peer.call(&id, foreign, json!({}))));
            peer.finish(&id, "ok", "done", 1, 0.0);
            replies
        });
        let mut context = context("5000");
        let mut agent = fixture_agent(client);
        let mut runner = LaneRunner::new(&mut agent, &settings, &c, &cfg, &clock);
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
            if observed != expected(&name).map_err(str::to_owned) {
                mismatches.push(format!("{lane:?} {name}: {observed:?}"));
            }
        }
        assert_eq!(turn.plan.is_some(), lane == Lane::Decision);
        assert_eq!(turn.verdict.is_some(), lane == Lane::Gate);
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    // Without the decision-evidence policy every intraday call would fail, and
    // without `strategy_execution` no `strategy_select` tool is offered.
    let mut bare = cfg.clone();
    bare.decision_evidence = None;
    bare.strategy_execution = None;
    for lane in Lane::ALL {
        assert!(
            lane_definitions(&bare, lane, true)
                .unwrap()
                .iter()
                .all(|tool| tool["name"] != "intraday" && tool["name"] != "strategy_select")
        );
    }
}

#[test]
fn research_lane_writes_and_runs_a_scores_only_ranker() {
    use crate::runtime::workspace::{self, WorkspaceOp, WorkspaceScope};

    let dir = tempfile::tempdir().unwrap();
    let (c, mut cfg) = all_tools(dir.path());
    cfg.workspace.as_mut().unwrap().network = true;
    std::fs::write(
        c.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&cfg).unwrap(),
    )
    .unwrap();
    let workspace = cfg.workspace.as_ref().unwrap();
    let script = "scripts/net_edge.py";
    let root = workspace::ensure(&c.directory, workspace).unwrap();
    let canary = c.directory.join("ranker-fixture.py");
    std::fs::write(&canary, "print('{}')\n").unwrap();

    let host_net = std::fs::read_link("/proc/self/ns/net").unwrap();
    let host_net = host_net.to_str().unwrap();
    let source = format!(
        r#"import json, os, resource, sys

assert os.readlink("/proc/self/ns/net") != {host_net:?}
assert resource.getrlimit(resource.RLIMIT_AS)[0] <= 256 * 1024 * 1024
with open(sys.argv[1]) as data:
    features = json.load(data)
print(json.dumps({{key: row["market_relative_bps"] - row["spread_bps"]
                  for key, row in features.items()}}))
"#
    );
    for scope in [WorkspaceScope::Decision, WorkspaceScope::Research] {
        assert_eq!(
            workspace::execute_scoped(
                &c.directory,
                workspace,
                scope,
                WorkspaceOp::Write {
                    path: script.into(),
                    text: source.clone(),
                },
            )
            .unwrap_err()
            .to_string(),
            "workspace_write_refused"
        );
    }
    let features = json!({
        "NSE_EQ|A": {"market_relative_bps": 30, "spread_bps": 2.5},
        "NSE_EQ|B": {"market_relative_bps": 10, "spread_bps": 5.5}
    });
    let run = WorkspaceOp::Run {
        script: script.into(),
        input: Some(features.clone()),
        input_path: None,
    };
    let script_dir = root.join("scripts");
    let (client, peer) = connect(move |peer| {
        let (id, turn) = peer.turn();
        assert_eq!(turn["lane"], "research");
        let write = |peer: &mut Peer, path, text: &str| {
            peer.call(&id, "write", json!({"path":path,"text":text}))
        };
        let run = |peer: &mut Peer, script, input: Value, input_path: Value| {
            peer.call(
                &id,
                "run",
                json!({"script":script,"input":input,"input_path":input_path}),
            )
        };
        let written = write(peer, script, &source);
        assert_eq!(written["ok"], true, "{written}");
        assert_eq!(written["output"]["execution_authorized"], false);
        std::fs::write(script_dir.join("net_edge.params.json"), "{}").unwrap();
        let input = json!(features.to_string());
        let scored = run(peer, script, input.clone(), Value::Null);
        assert_eq!(scored["ok"], true, "{scored}");
        assert_eq!(
            scored["output"]["scores"],
            json!({"NSE_EQ|A":27.5,"NSE_EQ|B":4.5})
        );
        assert_eq!(
            scored["output"]["source_sha256"],
            written["output"]["sha256"]
        );
        assert_eq!(scored["output"]["execution_authorized"], false);
        assert_eq!(scored["output"]["evidence_only"], true);

        for path in ["scripts/sub/rank.py", "scripts/rank.txt"] {
            assert_eq!(
                write(peer, path, &source)["error"],
                "workspace_write_refused"
            );
        }
        assert_eq!(
            write(peer, "scripts/../tools/rank.py", &source)["error"],
            "workspace_path_escape"
        );
        assert_eq!(
            run(peer, script, Value::Null, json!("notes/features.json"))["error"],
            "workspace_run_ranker_input_required"
        );
        for invalid in [
            json!("{}"),
            json!("{\"x\":3}"),
            json!("{\"x\":{},\"x\":{}}"),
        ] {
            assert_eq!(
                run(peer, script, invalid, Value::Null)["error"],
                "workspace_run_input_invalid"
            );
        }
        std::os::unix::fs::symlink(&canary, script_dir.join("linked.py")).unwrap();
        assert_eq!(
            run(peer, "scripts/linked.py", input.clone(), Value::Null)["error"],
            "workspace_symlink"
        );
        assert_eq!(
            write(peer, "scripts/linked.py", &source)["error"],
            "workspace_symlink"
        );
        let foreign = "scripts/foreign.py";
        assert_eq!(write(peer, foreign, "print('{\"outside\":1}')")["ok"], true);
        assert_eq!(
            run(peer, foreign, input, Value::Null)["error"],
            "workspace_run_output_invalid"
        );
        peer.finish(&id, "ok", "Ranked both instruments.", 1, 0.0);
    });
    let clock = || 1_760_600_000_000_i64;
    let settings = settings(100, 10.0);
    let mut agent = fixture_agent(client);
    let mut runner = LaneRunner::new(&mut agent, &settings, &c, &cfg, &clock);
    let turn = runner
        .turn(
            Lane::Research,
            json!({"kind":"test"}),
            json!({}),
            &mut context("5000"),
        )
        .unwrap();
    assert_eq!(turn.outcome.status, TurnStatus::Ok);
    peer.join().unwrap();
    let mut workspace = cfg.workspace.unwrap();
    workspace.run_allowlist.push(script.into());
    for (scope, expected) in [
        (WorkspaceScope::Decision, "workspace_run_not_allowlisted"),
        (WorkspaceScope::Research, "workspace_research_scope_invalid"),
    ] {
        assert_eq!(
            workspace::execute_scoped(&c.directory, &workspace, scope, run.clone())
                .unwrap_err()
                .to_string(),
            expected
        );
    }
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
    let (client, peer) = connect(move |peer| {
        let (decision, brief) = peer.turn();
        assert_eq!(read_brief(&brief)["research_updates"], json!([]));
        let queued = peer.call(&decision, "research", args("research"));
        let refused = peer.call(&decision, "plan", json!({"actions":[],"stance":"x"}));
        let unlisted = peer.call(&decision, "plan", json!({"actions":[{"instrument_key":"NSE_EQ|Z","side":"buy","units":null,"max_price":"10","rationale":"x","exit_intent":{"target":null,"stop":null,"time":null}}],"stance":"x","notes":"y"}));
        let plan = peer.call(&decision, "plan", json!({"actions":[{"instrument_key":"NSE_EQ|A","side":"buy","units":9,"max_price":"100.10","rationale":"momentum","exit_intent":{"target":null,"stop":"95","time":null}}],"stance":"selective","notes":"asked research"}));
        peer.finish(&decision, "ok", "asked research", 4, 0.03);
        let (research, brief) = peer.turn();
        assert_eq!(brief["lane"], "research");
        assert_eq!(read_brief(&brief)["task"]["question"], question);
        assert!(
            brief["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|tool| tool["name"] != "research")
        );
        peer.finish(&research, "ok", "Gap fades only before costs.", 2, 0.02);
        let (next, brief) = peer.turn();
        let updates = read_brief(&brief)["research_updates"].clone();
        assert_eq!(peer.call(&next, "plan", args("plan"))["ok"], true);
        assert_eq!(
            peer.call(&next, "plan", json!({"actions":[]}))["error"],
            "planner_json_invalid"
        );
        peer.finish(&next, "ok", "holding", 1, 0.01);
        (queued, refused, unlisted, plan, updates)
    });
    let mut context = context("5000");
    let costs = crate::runtime::tests::test_costs();
    context.portfolio.fee_schedule = Some(
        nivra_core::finance::FeeSchedule::new(costs.clone())
            .with_segment(nivra_core::market::Segment::NseFo, costs.clone()),
    );
    let mut agent = fixture_agent(client);
    let mut runner = LaneRunner::new(&mut agent, &settings, &c, &cfg, &clock);
    let brief = fixture_brief(&context, clock());
    let book = serde_json::to_value(&brief).unwrap()["book"].clone();
    assert_eq!(book["paper_costs"], serde_json::to_value(&costs).unwrap());
    assert_eq!(
        book["paper_costs_by_segment"]["NSE_FO"],
        serde_json::to_value(&costs).unwrap()
    );
    let first = runner
        .decision(json!({"kind":"schedule"}), brief.clone(), &mut context)
        .unwrap();
    let research = runner
        .research_requests(&json!({"kind":"request"}), &brief, &mut context)
        .unwrap();
    let next = runner
        .decision(json!({"kind":"fill"}), brief.clone(), &mut context)
        .unwrap();
    let (queued, refused, unlisted, plan, updates) = peer.join().unwrap();

    assert_eq!(queued["output"]["state"], "queued");
    assert_eq!(refused["error"], "planner_json_invalid");
    assert_eq!(
        unlisted["output"]["verdicts"][0]["reason"],
        "action_allowlist"
    );
    assert_eq!(plan["output"]["verdicts"][0]["reason"], "accepted");
    let tools::PlanAttempt::Validated { verdicts, .. } = first.plan.unwrap() else {
        panic!("the final plan should validate");
    };
    assert_eq!(verdicts[0]["units"], 9);
    assert_eq!(research.len(), 1);
    assert_eq!(updates.as_array().unwrap().len(), 1);
    assert_eq!(updates[0]["kind"], "research_result");
    assert_eq!(updates[0]["request_id"], queued["output"]["request_id"]);
    assert_eq!(updates[0]["summary"], "Gap fades only before costs.");
    assert_eq!(next.outcome.status, TurnStatus::Ok);
    assert!(matches!(
        next.plan,
        Some(tools::PlanAttempt::Refused { .. })
    ));
    let db = lane_queue::open_file(&c.directory.join("model/agent/requests.sqlite")).unwrap();
    let request = lane_queue::load(&db, queued["output"]["request_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(request.state, MessageState::Done);
    assert!(runner.requests.updates(clock()).unwrap().is_empty());
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

#[test]
fn research_claim_checks_expiry_at_the_claim_clock() {
    use crate::runtime::harness::LaneRequests;

    for (claim_at, accepted) in [(199, true), (200, false)] {
        let dir = tempfile::tempdir().unwrap();
        let (c, _) = all_tools(dir.path());
        let requests = requests::Requests::new(
            &c,
            &triggers::ResearchTriggers {
                request_ttl_ms: 100,
                ..Default::default()
            },
        );
        let posted = requests
            .post("decision-1", "Check evidence", Vec::new(), 100)
            .unwrap();
        let id = posted["request_id"].as_str().unwrap();
        assert_eq!(requests.queued(199, 4).unwrap()[0].id, id);
        assert_eq!(
            requests.claim(id, "research-1-1-1", claim_at).unwrap(),
            accepted
        );
        let status = requests.status(id).unwrap().unwrap();
        if accepted {
            assert_eq!(status["state"], "running");
            assert_eq!(status["turn_id"], "research-1-1-1");
        } else {
            assert_eq!(status["state"], "cancelled");
            assert_eq!(status["reason"], "expired");
            assert!(status["turn_id"].is_null());
        }
        assert!(requests.result(id).unwrap().unwrap()["answer"].is_null());
        assert!(requests.updates(claim_at).unwrap().is_empty());
    }
}

#[test]
fn research_publication_failure_rolls_back_and_preserves_the_answer() {
    use crate::runtime::harness::LaneRequests;

    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = all_tools(dir.path());
    let settings = settings(100, 10.0);
    let clock = || 1_760_600_000_000_i64;
    let (client, peer) = connect(|peer| {
        let (research, _) = peer.turn();
        peer.finish(&research, "ok", "Verified fixture answer", 1, 0.01);
    });
    let mut context = context("5000");
    let mut agent = fixture_agent(client);
    let mut runner = LaneRunner::new(&mut agent, &settings, &c, &cfg, &clock);
    let posted = runner
        .requests
        .post("decision-1", "Check evidence", Vec::new(), clock())
        .unwrap();
    let id = posted["request_id"].as_str().unwrap();
    let mut db = lane_queue::open_file(&c.directory.join("model/agent/requests.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER fail_completion BEFORE UPDATE OF state ON lane_messages
         WHEN NEW.state='done'
         BEGIN SELECT RAISE(ABORT,'injected private database detail'); END;",
    )
    .unwrap();
    let turns = runner
        .research_requests(
            &json!({"kind":"request"}),
            &fixture_brief(&context, clock()),
            &mut context,
        )
        .unwrap();
    peer.join().unwrap();

    assert_eq!(turns.len(), 1);
    assert_ne!(turns[0].outcome.status, TurnStatus::Ok);
    assert_eq!(
        turns[0].outcome.error.as_deref(),
        Some("agent_research_answer_failed")
    );
    let result = runner.requests.result(id).unwrap().unwrap();
    assert_eq!(result["request"]["state"], "failed");
    assert_eq!(result["request"]["reason"], "turn_failed");
    assert!(result["answer"].is_null());
    assert!(runner.requests.updates(clock()).unwrap().is_empty());
    let published: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM lane_messages WHERE kind='research_result'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(published, 0);
    let artifact = c
        .directory
        .join("model/agent/research")
        .join(format!("{id}.json"));
    let answer: Value = serde_json::from_slice(&std::fs::read(artifact).unwrap()).unwrap();
    assert_eq!(answer["request_id"], id);
    assert_eq!(answer["text"], "Verified fixture answer");
    assert_eq!(answer["turn_id"], turns[0].outcome.id);
    let journal = std::fs::read_to_string(c.directory.join("model/agent/turns.ndjson")).unwrap();
    let receipt: Value = serde_json::from_str(journal.lines().last().unwrap()).unwrap();
    assert_eq!(receipt["status"], "error");
    assert_eq!(receipt["error"], "agent_research_answer_failed");

    // A pre-fix journal can already contain the result of a failed request.
    let body = lane_queue::MessageBody::ResearchResult {
        request_id: Some(id.to_owned()),
        research_id: turns[0].outcome.id.clone(),
        summary: "Legacy answer".into(),
        artifact_ref: format!("agent/research/{id}.json"),
        output_sha256: crate::runtime::digest(b"Legacy answer"),
    };
    lane_queue::post(&mut db, &c.session.to_string(), id, &body, clock(), 100, 4).unwrap();
    assert!(runner.requests.updates(clock()).unwrap().is_empty());
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
    let clock = || 1_760_600_000_000_i64;
    let (client, peer) = connect(|peer| {
        let (id, _) = peer.turn();
        peer.message(&id, 0.6);
        peer.message(&id, 0.6);
        let abort = peer.recv();
        peer.finish(&id, "ok", "", 2, 1.2);
        let (research, _) = peer.turn();
        peer.message(&research, 0.1);
        let preempted = peer.recv();
        peer.finish(&research, "aborted", "", 1, 0.1);
        (abort, preempted)
    });
    let mut context = context("5000");
    let mut agent = fixture_agent(client);
    let mut runner = LaneRunner::new(&mut agent, &settings, &c, &cfg, &clock);
    let spent = runner
        .turn(Lane::Decision, json!({}), json!({}), &mut context)
        .unwrap();
    assert_eq!(spent.outcome.status, TurnStatus::Aborted);
    let refused = runner
        .turn(Lane::Decision, json!({}), json!({}), &mut context)
        .err()
        .unwrap();
    assert_eq!(refused.to_string(), "agent_budget_exhausted");
    use crate::runtime::harness::LaneRequests;
    // A credential-shaped request is refused; diagnostics keep where the
    // guard matched and never the matched text.
    let blocked = runner
        .requests
        .post(
            "decision-1",
            "Use Bearer abc9 now",
            vec!["ok".into()],
            clock(),
        )
        .unwrap_err();
    assert_eq!(blocked.to_string(), "research_request_secret_refused");
    let guard = std::fs::read_to_string(c.directory.join("diagnostics.ndjson")).unwrap();
    assert!(!guard.contains("abc9"));
    assert_eq!(
        serde_json::from_str::<Value>(guard.trim_end()).unwrap(),
        json!({"at_ms":clock(),"event":"secret_guard","site":"research_request",
            "action":"blocked","turn_id":"decision-1","count":1,
            "matches":[{"class":"bearer_prefix","field":"$.question","length":19,"offset":4}]})
    );
    runner
        .requests
        .post("decision-1", "Is the gap real?", Vec::new(), clock())
        .unwrap();
    let brief = fixture_brief(&context, clock());
    // A decision trigger arrives while the research turn runs.
    let preempt = runner.preempt.clone();
    let waiting = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        preempt.store(true, Ordering::Release);
    });
    let research = runner
        .research_requests(&json!({"kind":"request"}), &brief, &mut context)
        .unwrap();
    waiting.join().unwrap();
    let (abort, preempted) = peer.join().unwrap();
    assert_eq!(abort["type"], "abort");
    assert_eq!(abort["reason"], "agent_budget_exhausted");
    assert_eq!(preempted["reason"], "agent_preempted");
    assert_eq!(research[0].outcome.status, TurnStatus::Aborted);
    let db = lane_queue::open_file(&c.directory.join("model/agent/requests.sqlite")).unwrap();
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
    // The session's diagnostics tie the preempted turn to the request's
    // durable queue outcome.
    let research_id = &research[0].outcome.id;
    assert_eq!(failed[0].turn_id.as_ref(), Some(research_id));
    let diagnostics = std::fs::read_to_string(c.directory.join("diagnostics.ndjson")).unwrap();
    let preemption: Value = serde_json::from_str(
        diagnostics
            .lines()
            .find(|line| line.contains("research_request_failed"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        preemption,
        json!({"at_ms":clock(),"event":"research_request_failed","turn_id":research_id,
            "request_id":failed[0].id,"turn_error":"agent_preempted",
            "queue_state":"failed","queue_reason":"interrupted"})
    );
    drop(runner);
    // A slow tool call must not block turn_timeout_ms or emit a stale tool_result after abort.
    struct SlowHost;
    impl super::client::ToolHost for SlowHost {
        fn call(
            &mut self,
            _call_id: &str,
            _name: &str,
            _args: &Value,
        ) -> std::result::Result<Value, String> {
            std::thread::sleep(std::time::Duration::from_millis(250));
            Ok(json!({"ok":true}))
        }
    }
    let (mut slow_client, slow_peer) = connect(|peer| {
        let (id, _) = peer.turn();
        peer.send(
            json!({"type":"tool_call","turn_id":id,"call_id":"slow-1","name":"quotes","args":{}}),
        );
        let abort = peer.recv();
        assert_eq!(abort["type"], "abort");
        assert_eq!(abort["reason"], "agent_turn_timeout");
        peer.finish(&id, "aborted", "", 0, 0.0);
        abort
    });
    let mut slow_req = super::client::TurnRequest {
        id: "research-slow-timeout".into(),
        lane: Lane::Research,
        session: super::protocol::SessionRef {
            date: c.session.to_string(),
            dir: c.directory.display().to_string(),
        },
        model: settings.lane(Lane::Research).unwrap().model.clone(),
        fallback: None,
        trigger: json!({"kind":"agenda"}),
        brief: json!({}),
        tools: Vec::new(),
        compact_at_tokens: None,
    };
    slow_req.tools.clear();
    let slow_outcome = slow_client
        .run_turn(
            &slow_req,
            &mut SlowHost,
            &super::protocol::Usage::default(),
            super::client::Allowance {
                calls: 10,
                cost_usd: 1.0,
                max_tool_calls: 10,
                timeout: std::time::Duration::from_millis(40),
            },
            &|| false,
        )
        .unwrap();
    slow_peer.join().unwrap();
    assert_eq!(slow_outcome.status, TurnStatus::Aborted);
    assert_eq!(slow_outcome.error.as_deref(), Some("agent_turn_timeout"));
    assert_eq!(slow_outcome.tool_calls.len(), 1);
    let _ = slow_client.shutdown();
    // An agent that dies mid-turn ends the turn with an error, not a hang,
    // and what pi already reported stays charged.
    let (client, peer) = connect(|peer| {
        let (id, _) = peer.turn();
        peer.message(&id, 0.25);
    });
    let mut agent = fixture_agent(client);
    let mut runner = LaneRunner::new(&mut agent, &settings, &c, &cfg, &clock);
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

#[test]
fn a_fallback_switch_is_journalled_and_charged_but_refused_after_a_served_tool() {
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = all_tools(dir.path());
    let mut settings = settings(10, 1.0);
    let fallback = ModelSpec {
        provider: "mistral".into(),
        id: "deepseek-v4-1-flash-1017".into(),
        thinking: Some("high".into()),
    };
    let decision = settings.lanes.get_mut(&Lane::Decision).unwrap();
    decision.fallback = Some(decision.model.clone());
    assert!(
        settings.validate().is_err(),
        "a fallback must differ from the primary"
    );
    let mut unknown_level = fallback.clone();
    unknown_level.thinking = Some("max".into());
    settings.lanes.get_mut(&Lane::Decision).unwrap().fallback = Some(unknown_level);
    assert!(
        settings.validate().is_err(),
        "pi's agent accepts no other level"
    );
    settings.lanes.get_mut(&Lane::Decision).unwrap().fallback = Some(fallback.clone());
    settings.validate().unwrap();
    let switch = json!({"kind":"fallback","from":{"provider":"openai-codex","id":"gpt-6.1-sol"},
        "to":{"provider":"mistral","id":"deepseek-v4-1-flash-1017"},"reason":"quota","error":"usage limit"});
    let clock = || 1_760_600_000_000_i64;
    let (client, peer) = connect(move |peer| {
        let (id, turn) = peer.turn();
        peer.send(json!({"type":"event","turn_id":id,"event":switch}));
        peer.model = turn["fallback"].clone();
        peer.message(&id, 0.3);
        peer.send(json!({"type":"turn_result","id":id,"status":"ok","output":{"text":"held"},
            "usage":{"input":100,"output":10,"cache_read":0,"cache_write":0,"cost_usd":0.3,"calls":1},
            "model":"deepseek-v4-1-flash-1017","provider":"mistral","session_file":null,
            "prompt_sha256":"0".repeat(64),"fallback":switch}));
        let (id, _) = peer.turn();
        peer.call(&id, "portfolio", json!({}));
        peer.send(json!({"type":"event","turn_id":id,"event":switch}));
        let refused = peer.recv();
        peer.finish(&id, "aborted", "", 1, 0.0);
        let (id, _) = peer.turn();
        peer.model = json!({"provider":"mistral","id":"deepseek-v4-1-flash-1017"});
        peer.finish(&id, "ok", "unannounced", 1, 0.0);
        (turn, refused)
    });
    let mut context = context("5000");
    let mut agent = fixture_agent(client);
    let mut runner = LaneRunner::new(&mut agent, &settings, &c, &cfg, &clock);
    let switched = runner
        .turn(Lane::Decision, json!({}), json!({}), &mut context)
        .unwrap();
    let after_tool = runner
        .turn(Lane::Decision, json!({}), json!({}), &mut context)
        .unwrap();
    let unannounced = runner
        .turn(Lane::Decision, json!({}), json!({}), &mut context)
        .unwrap();
    let (request, refused) = peer.join().unwrap();
    assert_eq!(request["fallback"], json!(fallback));
    assert_eq!(switched.outcome.status, TurnStatus::Ok);
    assert_eq!(switched.outcome.model, "deepseek-v4-1-flash-1017");
    let journal = std::fs::read_to_string(c.directory.join("model/agent/turns.ndjson")).unwrap();
    let first: Value = serde_json::from_str(journal.lines().next().unwrap()).unwrap();
    assert_eq!(first["fallback"]["reason"], "quota");
    assert_eq!(first["fallback"]["to"]["provider"], "mistral");
    let spent: Value =
        serde_json::from_slice(&std::fs::read(c.directory.join("model/agent/usage.json")).unwrap())
            .unwrap();
    assert_eq!(spent["lanes"]["decision"]["cost_usd"], 0.3);
    assert_eq!(refused["type"], "abort");
    assert_eq!(refused["reason"], "agent_fallback_refused");
    assert_eq!(after_tool.outcome.status, TurnStatus::Aborted);
    assert!(after_tool.outcome.fallback.is_none());
    assert_eq!(unannounced.outcome.status, TurnStatus::Error);
    assert_eq!(
        unannounced.outcome.error.as_deref(),
        Some("agent_fallback_unreported")
    );
}

#[test]
fn next_decision_receives_only_the_sealed_predecessor_review() {
    let dir = tempfile::tempdir().unwrap();
    let (mut prior, cfg) = crate::runtime::decision::decision_fixture_config(dir.path());
    let original = dir.path().join("sessions").join(prior.session.to_string());
    std::fs::create_dir_all(&original).unwrap();
    prior.directory = original.clone();
    let mut successor = prior.clone();
    successor.session = "2026-10-19".parse().unwrap();
    successor.previous_session = prior.session;
    successor.directory = successor.sessions_root.as_ref().unwrap().join("2026-10-19");

    for marker in [lanes::REVIEW_MARKER, lanes::AFTER_CLOSE_MARKER] {
        assert!(lanes::claim(&prior, marker).unwrap());
        assert!(!lanes::claim(&prior, marker).unwrap());
    }

    let settings = settings(10, 1.0);
    let clock = || 1_760_600_000_000_i64;
    let memory_text = "Wait for confirmed depth after the open.";
    let (client, peer) = connect(move |peer| {
        let (id, request) = peer.turn();
        assert_eq!(request["lane"], "review");
        peer.finish(&id, "ok", memory_text, 1, 0.01);
    });
    let mut agent = fixture_agent(client);
    let mut context = context("5000");
    let brief = fixture_brief(&context, clock());
    let review = LaneRunner::new(&mut agent, &settings, &prior, &cfg, &clock)
        .review(json!({"kind":"seal"}), &brief, &mut context)
        .unwrap();
    peer.join().unwrap();

    assert!(lanes::memory(&successor, Lane::Decision).is_none());
    let mut report = json!({"session":prior.session,"account":prior.account,"broker":"upstox",
        "paper_only":true,"live_enabled":false,"portfolio":{"closed":true}});
    let path = original.join("report.json");
    std::fs::write(&path, report.to_string()).unwrap();

    let turn_id = review.outcome.id;
    let expected = json!({"session":prior.session,"turn_id":turn_id,"text":memory_text});
    let (client, peer) = connect(move |peer| {
        let (id, request) = peer.turn();
        assert_eq!(request["lane"], "decision");
        assert_eq!(read_brief(&request)["memory"], expected);
        peer.finish(&id, "ok", "Using the prior review", 1, 0.01);
    });
    let mut agent = fixture_agent(client);
    let mut first_brief = brief;
    first_brief.session.date = successor.session.to_string();
    first_brief.memory = lanes::memory(&successor, Lane::Decision);
    LaneRunner::new(&mut agent, &settings, &successor, &cfg, &clock)
        .decision(json!({"kind":"schedule"}), first_brief, &mut context)
        .unwrap();
    peer.join().unwrap();

    report["account"] = json!("different-paper-book");
    std::fs::write(&path, report.to_string()).unwrap();
    assert!(lanes::memory(&successor, Lane::Decision).is_none());

    report["account"] = json!(prior.account);
    std::fs::write(&path, report.to_string()).unwrap();
    successor.previous_session = "2026-10-15".parse().unwrap();
    assert!(lanes::memory(&successor, Lane::Decision).is_none());
}

#[test]
fn pi_paper_config_refuses_retired_model_keys() {
    let mut agent = settings(10, 1.0);
    agent.mode = AgentMode::Paper;
    let mut cfg = json!({"enabled":true,"agent":agent});
    let parse = |cfg: &Value| planner::PlannerConfig::parse(cfg.to_string().as_bytes());
    assert!(parse(&cfg).unwrap().uses_pi_paper());

    cfg["digest"] = json!({"enabled":true,"model_id":"retired","daily_call_budget":1});
    assert_eq!(
        parse(&cfg).err().unwrap().to_string(),
        "planner_config_invalid"
    );
}

#[test]
fn decision_lane_supervises_strategies_and_refuses_cash_buys_while_active() {
    use crate::runtime::strategy_library::{Author, Library, acceptance};
    use nivra_cli::canonical_json::bytes as artifact_bytes;
    use nivra_core::execution::{ExitIntent, Instrument, Position};
    use nivra_core::finance::{FeeSchedule, Price};
    use nivra_core::market::{InstrumentKind, OptionCatalog, OptionContract, Segment};
    use nivra_core::time::ExchangeDate;

    let dir = tempfile::tempdir().unwrap();
    let (c, mut cfg) = all_tools(dir.path());
    cfg.strategy_execution = Some(planner::StrategyExecution {
        enabled: true,
        ..Default::default()
    });
    std::fs::write(
        c.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&cfg).unwrap(),
    )
    .unwrap();
    let library = Library::at(crate::runtime::strategy_library::root(&c).unwrap());
    let register = |n: u64, at: i64| {
        let source = b"def on_observation(obs):\n    return {'actions': []}\n";
        let params = json!({"n": n});
        let report = artifact_bytes(&json!({
            "engine_sha256": nivra_core::replay::engine_fingerprint(),
            "adapter_sha256": crate::runtime::strategy::replay::adapter_sha256(),
            "parameters": {"version": 1, "top_n": 20, "params": params},
        }))
        .unwrap();
        let source_sha256 = crate::runtime::digest(source);
        let accepted = acceptance::Accepted {
            report: &report,
            report_sha256: crate::runtime::digest(&report),
            source,
            source_sha256: source_sha256.clone(),
            params,
            summary: json!({"trades": 14, "net_pnl": "62.50", "worst_drawdown": "35.00"}),
            tape_dates: vec!["2026-10-15".into()],
            thresholds: acceptance::Thresholds::default(),
        };
        let review = crate::runtime::strategy_library::review::EntryReview {
            verdict: "safe".into(),
            model: "fixture".into(),
            source_sha256,
            runner_sha256: crate::runtime::strategy::runner_sha256(),
            policy_sha256: crate::runtime::strategy_library::review::review_policy_sha256(),
            reasons: Vec::new(),
            reviewed_at_ms: at,
        };
        let author = Author {
            lane: "research".into(),
            model: Some("deepseek-v4-pro".into()),
            model_revision: None,
            turn_id: Some("research-1".into()),
            session: Some(c.session),
        };
        library
            .register(&accepted, &review, author, at)
            .unwrap()
            .entry
            .strategy_id
    };
    let s1 = register(1, 1_000);
    let s2 = register(2, 2_000);

    let db = crate::runtime::open_book(&c).unwrap();
    db.execute_batch(
        "CREATE TABLE strategy_action(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,observed_at INTEGER NOT NULL,strategy_id TEXT NOT NULL,idx INTEGER NOT NULL,instrument_key TEXT,side TEXT,units INTEGER NOT NULL,outcome TEXT NOT NULL);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO strategy_action(at,observed_at,strategy_id,idx,instrument_key,side,units,outcome) VALUES(100,100,?,0,'NSE_EQ|A','buy',1,'filled'),(200,200,?,1,'NSE_EQ|B','buy',1,'price_above_max')",
        rusqlite::params![&s2, &s2],
    )
    .unwrap();

    let mut context = context("10000");
    // Hold NSE_EQ|A so risk-reducing sell/revise_exit can be tested, and add one
    // Nifty option contract so option buys remain allowed while cash buys fail.
    let held_inst = Instrument {
        key: "NSE_EQ|A".into(),
        symbol: "A".into(),
        tick: Price::parse("0.05").unwrap(),
        series: "EQ".into(),
    };
    context.portfolio.positions.insert(
        held_inst.key.clone(),
        Position {
            instrument: held_inst.clone(),
            units: 5,
            entry: Price::parse("100").unwrap(),
            mark: Price::parse("99").unwrap(),
            marked_at: 1_760_600_000_000,
            exit_intent: Some(ExitIntent {
                stop: Some(Price::parse("95").unwrap()),
                target: Some(Price::parse("110").unwrap()),
                time: None,
            }),
            decision_id: Some(s2.clone()),
        },
    );
    let contract = OptionContract {
        key: "NSE_FO|26000CE".into(),
        symbol: "NIFTY 26000 CE 20 OCT 26".into(),
        underlying_key: "NSE_INDEX|Nifty 50".into(),
        underlying_symbol: "NIFTY".into(),
        expiry: ExchangeDate::parse("2026-10-20").unwrap(),
        strike: Price::parse("26000").unwrap(),
        option: InstrumentKind::Call,
        lot_size: 65,
        tick: Price::parse("0.05").unwrap(),
        freeze_quantity: Some(1800),
        weekly: true,
    };
    let costs = crate::runtime::tests::test_costs();
    context.portfolio.fee_schedule =
        Some(FeeSchedule::new(costs.clone()).with_segment(Segment::NseFo, costs));
    context.portfolio.options = Some(
        OptionCatalog::try_new(
            ExchangeDate::parse("2026-10-16").unwrap(),
            "NSE_INDEX|Nifty 50",
            "NIFTY",
            contract.expiry,
            Price::parse("26000").unwrap(),
            [contract.clone()],
        )
        .unwrap(),
    );
    context.instruments.push(Instrument {
        key: contract.key.clone(),
        symbol: contract.symbol.clone(),
        tick: contract.tick,
        series: "FO".into(),
    });

    let mut brief = fixture_brief(&context, 1_760_600_000_000);
    brief.strategies = brief::strategy_facts(&db, &c, &cfg);
    let facts = brief.strategies.as_ref().unwrap();
    assert_eq!(facts.active_strategy_id.as_deref(), Some(s2.as_str()));
    assert_eq!(facts.eligible.len(), 2);
    assert_eq!(facts.active_live_outcomes.get("filled"), Some(&1));
    assert_eq!(facts.active_live_outcomes.get("price_above_max"), Some(&1));

    let s1_turn = s1.clone();
    let (client, peer) = connect(move |peer| {
        let (id, _) = peer.turn();
        // Pick s1 for the current regime.
        let selected = peer.call(
            &id,
            "strategy_select",
            json!({"strategy_id": s1_turn, "reason": "up_trend fits s1"}),
        );
        // Cash equity buy must be refused while a strategy is active.
        let cash_buy = peer.call(
            &id,
            "plan",
            json!({"actions":[{"instrument_key":"NSE_EQ|B","side":"buy","units":9,"max_price":"100","rationale":"cash entry","exit_intent":{"target":"105","stop":"95","time":null}}],"stance":"active","notes":"cash buy"}),
        );
        // Risk-reducing cash sell + exit revision + Nifty option buy are allowed.
        let allowed = peer.call(
            &id,
            "plan",
            json!({"actions":[
                {"instrument_key":"NSE_EQ|A","side":"sell","units":2,"max_price":"98","rationale":"trim risk","exit_intent":{"target":null,"stop":null,"time":null}},
                {"instrument_key":"NSE_EQ|A","side":"revise_exit","units":null,"max_price":null,"rationale":"tighten stop","exit_intent":{"target":"108","stop":"97","time":null}},
                {"instrument_key":"NSE_FO|26000CE","side":"buy","units":65,"max_price":"50","rationale":"hedge/option","exit_intent":{"target":"70","stop":"35","time":null}}
            ],"stance":"supervise","notes":"risk and option"}),
        );
        peer.finish(&id, "ok", "supervised", 3, 0.01);

        // Stand down (`null`) in a second turn -> cash equity buy is allowed again.
        let (id2, _) = peer.turn();
        let stood_down = peer.call(
            &id2,
            "strategy_select",
            json!({"strategy_id": null, "reason": "no strategy fits"}),
        );
        let cash_after = peer.call(
            &id2,
            "plan",
            json!({"actions":[{"instrument_key":"NSE_EQ|B","side":"buy","units":9,"max_price":"100","rationale":"manual entry","exit_intent":{"target":"105","stop":"95","time":null}}],"stance":"selective","notes":"no active strategy"}),
        );
        peer.finish(&id2, "ok", "manual", 2, 0.01);
        (selected, cash_buy, allowed, stood_down, cash_after)
    });
    let settings = settings(100, 10.0);
    let clock = || 1_760_600_000_000_i64;
    let mut agent = fixture_agent(client);
    let mut runner = LaneRunner::new(&mut agent, &settings, &c, &cfg, &clock);
    let turn1 = runner
        .decision(
            json!({"kind":"schedule","reason":"open"}),
            brief.clone(),
            &mut context,
        )
        .unwrap();
    let turn2 = runner
        .decision(
            json!({"kind":"schedule","reason":"regime_changed"}),
            brief,
            &mut context,
        )
        .unwrap();
    let (selected, cash_buy, allowed, stood_down, cash_after) = peer.join().unwrap();

    assert_eq!(selected["output"]["active"], s1);
    assert_eq!(cash_buy["ok"], false);
    assert_eq!(cash_buy["error"], "agent_paper_strategy_owns_cash_entries");
    assert_eq!(allowed["ok"], true, "{allowed}");
    assert_eq!(turn1.outcome.status, TurnStatus::Ok);
    assert!(matches!(
        turn1.plan,
        Some(tools::PlanAttempt::Validated { .. })
    ));
    assert_eq!(stood_down["output"]["active"], Value::Null);
    assert_eq!(cash_after["ok"], true, "{cash_after}");
    assert_eq!(turn2.outcome.status, TurnStatus::Ok);
    assert_eq!(library.resolve().unwrap().active, None);
}
