import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { compactionSettings, LaneAgent } from "../src/agent.js";
import { FAKE_FALLBACK_MODEL, FAKE_MODEL, FAKE_PROVIDER, type FakeScript, installFake } from "../src/fake.js";
import { fallbackReason } from "../src/fallback.js";
import { type JsonObject, type Outbound, PROTOCOL_VERSION, parseInbound, type ToolSpec, type TurnRequest } from "../src/protocol.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const prompts = join(root, "prompts");
const defaultSkills = join(root, "skills");

const tool = (name: string, properties: JsonObject = {}): ToolSpec => ({
  name,
  description: `${name} from Rust`,
  parameters: { type: "object", properties, required: Object.keys(properties), additionalProperties: false },
});

const decisionTools = [
  tool("portfolio"),
  tool("prospects", { limit: { type: ["integer", "null"] }, sort: { type: ["string", "null"] } }),
  tool("research", { question: { type: "string" }, refs: { type: ["array", "null"], items: { type: "string" } } }),
  tool("plan", { actions: { type: "array" }, stance: { type: "string" }, notes: { type: "string" } }),
];

function turn(id: string, lane: TurnRequest["lane"], dir: string, date = "2026-11-18", tools = decisionTools): TurnRequest {
  return {
    type: "turn",
    id,
    lane,
    session: { date, dir: join(dir, date, lane) },
    model: { provider: FAKE_PROVIDER, id: FAKE_MODEL, thinking: "off" },
    trigger: { kind: "schedule", reason: "open" },
    brief: { facts: "fixture" },
    tools,
  };
}

/** A scripted Rust peer: answers every tool call and collects the rest. */
async function harness(
  script: FakeScript,
  answer: (name: string, args: unknown) => { ok: boolean; output?: unknown; error?: string },
  options: { skillsDir?: string; workspaceSkillsDir?: string } = {},
) {
  const dir = mkdtempSync(join(tmpdir(), "nivra-agent-test-"));
  const runtime = await ModelRuntime.create({ authPath: join(dir, "auth.json"), modelsPath: null, allowModelNetwork: false, refreshOnCreate: false });
  const { beforeTurn, beforeReview } = installFake(runtime, script);
  const sent: Outbound[] = [];
  const done = new Map<string, (message: Outbound) => void>();
  let agent: LaneAgent;
  const send = (message: Outbound) => {
    sent.push(message);
    if (message.type === "tool_call") {
      const reply = answer(message.name, message.args);
      setImmediate(() =>
        agent.handle(parseInbound(JSON.stringify({ v: PROTOCOL_VERSION, type: "tool_result", turn_id: message.turn_id, call_id: message.call_id, ...reply }))),
      );
    }
    if (message.type === "turn_result" || message.type === "review_strategy_result") done.get(message.id)?.(message);
  };
  agent = new LaneAgent({ runtime, promptsDir: prompts, send, beforeTurn, beforeReview, ...options });
  const run = (request: TurnRequest) =>
    new Promise<Extract<Outbound, { type: "turn_result" }>>((resolve) => {
      done.set(request.id, (message) => resolve(message as Extract<Outbound, { type: "turn_result" }>));
      agent.handle(request);
    });
  const review = (source: string, source_sha256: string, id = "rev-1") =>
    new Promise<Extract<Outbound, { type: "review_strategy_result" }>>((resolve) => {
      done.set(id, (message) => resolve(message as Extract<Outbound, { type: "review_strategy_result" }>));
      agent.handle(
        parseInbound(
          JSON.stringify({
            v: PROTOCOL_VERSION,
            type: "review_strategy",
            id,
            model: { provider: FAKE_PROVIDER, id: FAKE_MODEL, thinking: "off" },
            fallback: { provider: FAKE_PROVIDER, id: FAKE_FALLBACK_MODEL, thinking: "off" },
            source,
            source_sha256,
          }),
        ),
      );
    });
  return { dir, agent, sent, run, review };
}

test("a decision turn bridges every Rust tool call and reports usage, model and events", async () => {
  const script: FakeScript = {
    turns: [
      {
        lane: "decision",
        steps: [
          { tool_calls: [{ name: "portfolio", args: {} }, { name: "prospects", args: { limit: 5, sort: null } }] },
          { tool_calls: [{ name: "research", args: { question: "Does the gap fade survive costs?", refs: null } }] },
          { tool_calls: [{ name: "plan", args: { actions: [], stance: "hold", notes: "no edge" } }] },
          { text: "Held: no edge after costs." },
        ],
      },
    ],
  };
  const calls: string[] = [];
  const { dir, agent, sent, run } = await harness(script, (name) => {
    calls.push(name);
    return name === "research" ? { ok: true, output: { request_id: "r1", state: "queued" } } : { ok: true, output: { tool: name } };
  });
  const result = await run(turn("t1", "decision", dir));
  assert.equal(result.status, "ok", result.error);
  assert.deepEqual(calls, ["portfolio", "prospects", "research", "plan"]);
  assert.equal(result.output.text, "Held: no edge after costs.");
  assert.equal(result.provider, FAKE_PROVIDER);
  assert.equal(result.model, FAKE_MODEL);
  assert.equal(result.usage.calls, 4);
  assert.ok(result.usage.output > 0, "usage reported");
  assert.match(result.prompt_sha256, /^[0-9a-f]{64}$/);
  // Only the tools Rust published are active: pi's read/bash/edit/write never are.
  assert.deepEqual(agent.activeToolNames("decision").sort(), ["plan", "portfolio", "prospects", "research"]);
  const kinds = sent.flatMap((message) => (message.type === "event" ? [String(message.event.kind)] : []));
  assert.equal(kinds.filter((kind) => kind === "tool_start").length, 4);
  assert.equal(kinds.filter((kind) => kind === "tool_end").length, 4);
  assert.equal(kinds.filter((kind) => kind === "message").length, 4);
  const toolCall = sent.find((message) => message.type === "tool_call");
  assert.ok(toolCall && toolCall.type === "tool_call" && /^[A-Za-z0-9_-]{1,128}$/.test(toolCall.call_id));
  const file = result.session_file;
  assert.ok(file && readFileSync(file, "utf8").includes("Does the gap fade survive costs?"), "pi session file persisted");
  agent.dispose();
});

test("a refused tool call reaches the model as an error and the turn continues", async () => {
  const script: FakeScript = {
    turns: [{ lane: "decision", steps: [{ tool_calls: [{ name: "plan", args: { actions: [{}], stance: "x", notes: "y" } }] }, { text: "plan refused" }] }],
  };
  const { dir, agent, sent, run } = await harness(script, () => ({ ok: false, error: "planner_schema_invalid" }));
  const result = await run(turn("t1", "decision", dir));
  assert.equal(result.status, "ok");
  const end = sent.find((message) => message.type === "event" && message.event.kind === "tool_end");
  assert.ok(end && end.type === "event" && end.event.is_error === true);
  agent.dispose();
});

test("one session per lane per date, reused across turns and replaced on a new date", async () => {
  const script: FakeScript = {
    turns: [
      { lane: "research", steps: [{ text: "first" }] },
      { lane: "research", steps: [{ text: "second" }] },
      { lane: "research", steps: [{ text: "next day" }] },
    ],
  };
  const { dir, agent, run } = await harness(script, () => ({ ok: true, output: {} }));
  const first = await run(turn("r1", "research", dir, "2026-11-18", [tool("portfolio")]));
  const second = await run(turn("r2", "research", dir, "2026-11-18", [tool("portfolio")]));
  const third = await run(turn("r3", "research", dir, "2026-11-19", [tool("portfolio")]));
  assert.equal(first.session_file, second.session_file);
  assert.notEqual(third.session_file, first.session_file);
  assert.ok(readFileSync(second.session_file!, "utf8").includes("first"), "second turn continues the first turn's context");
  agent.dispose();
});

test("a new session date starts its own session even when the lane directory is shared", async () => {
  const script: FakeScript = {
    turns: [
      { lane: "research", steps: [{ text: "earlier-session-marker" }] },
      { lane: "research", steps: [{ text: "next day" }] },
    ],
  };
  const { dir, agent, run } = await harness(script, () => ({ ok: true, output: {} }));
  const laneDir = join(dir, "shared", "research");
  const request = (id: string, date: string) => ({ ...turn(id, "research", dir, date, [tool("portfolio")]), session: { date, dir: laneDir } });
  const first = await run(request("r1", "2026-11-18"));
  const second = await run(request("r2", "2026-11-19"));
  assert.ok(first.session_file && second.session_file);
  assert.notEqual(second.session_file, first.session_file, "a new date starts a new session file");
  assert.ok(readFileSync(first.session_file, "utf8").includes("earlier-session-marker"));
  assert.ok(!readFileSync(second.session_file, "utf8").includes("earlier-session-marker"), "a new date does not continue the previous date's context");
  agent.dispose();
});

test("an unknown model is refused without a model call", async () => {
  const { dir, agent, run } = await harness({ turns: [] }, () => ({ ok: true }));
  const request = turn("t1", "decision", dir);
  request.model = { provider: "openai-codex", id: "no-such-model" };
  const result = await run(request);
  assert.equal(result.status, "error");
  assert.equal(result.error, "agent_model_unknown");
  assert.equal(result.usage.calls, 0);
  agent.dispose();
});

test("an abort that arrives while the session opens stops the turn before any model call", async () => {
  const script: FakeScript = { turns: [{ lane: "decision", steps: [{ text: "should never run" }] }] };
  const { dir, agent, sent } = await harness(script, () => ({ ok: true, output: {} }));
  const request = turn("t1", "decision", dir);
  const done = new Promise<Extract<Outbound, { type: "turn_result" }>>((resolve) => {
    const poll = () => {
      const result = sent.find((message) => message.type === "turn_result");
      if (result) resolve(result as Extract<Outbound, { type: "turn_result" }>);
      else setTimeout(poll, 5);
    };
    poll();
  });
  agent.handle(request);
  agent.handle({ type: "abort", turn_id: "t1", reason: "agent_preempted" });
  const result = await done;
  assert.equal(result.status, "aborted");
  assert.equal(result.error, "agent_preempted");
  assert.equal(result.usage.calls, 0);
  assert.ok(!sent.some((message) => message.type === "event" && message.event.kind === "message"), "no model step ran");
  agent.dispose();
});

test("only quota, auth and pre-output provider rejections qualify for the fallback", () => {
  const cases: [string, boolean, string | undefined][] = [
    ["You have hit your ChatGPT usage limit (plus plan). Try again in ~90 min.", true, "quota"],
    ["insufficient_quota: You exceeded your current quota", true, "quota"],
    ["429 Too Many Requests: rate limit exceeded", true, "quota"],
    ["401 Unauthorized", true, "auth"],
    ["403 Forbidden: usage_not_included", true, "auth"],
    ["No API key found for provider mistral", false, "auth"],
    ["400 The model is not available on your plan", true, "rejected"],
    ["400 The model is not available on your plan", false, undefined],
    ["503 Service Unavailable", true, undefined],
    ["Request timed out", true, undefined],
    ["fetch failed", true, undefined],
    ["This model's maximum context length is 128000 tokens", true, undefined],
  ];
  for (const [error, answered, reason] of cases) assert.equal(fallbackReason(error, answered), reason, error);
});

test("a primary quota failure before any output reruns the turn once on the fallback from the same context", async () => {
  const script: FakeScript = {
    turns: [
      { lane: "decision", steps: [{ text: "earlier turn" }] },
      {
        lane: "decision",
        steps: [
          { error: "insufficient_quota: You exceeded your current quota" },
          { tool_calls: [{ name: "plan", args: { actions: [], stance: "hold", notes: "fallback" } }] },
          { text: "Held on the fallback." },
        ],
      },
    ],
  };
  const { dir, agent, sent, run } = await harness(script, (name) => ({ ok: true, output: { tool: name } }));
  await run(turn("t0", "decision", dir));
  const request = { ...turn("t1", "decision", dir), fallback: { provider: FAKE_PROVIDER, id: FAKE_FALLBACK_MODEL, thinking: "high" as const } };
  const result = await run(request);
  assert.equal(result.status, "ok", result.error);
  assert.equal(result.model, FAKE_FALLBACK_MODEL);
  assert.deepEqual(result.fallback, {
    from: { provider: FAKE_PROVIDER, id: FAKE_MODEL },
    to: { provider: FAKE_PROVIDER, id: FAKE_FALLBACK_MODEL },
    reason: "quota",
    error: "insufficient_quota: You exceeded your current quota",
  });
  const turnLines = sent.filter((message) => "turn_id" in message && message.turn_id === "t1");
  const switched = turnLines.findIndex((message) => message.type === "event" && message.event.kind === "fallback");
  const firstCall = turnLines.findIndex((message) => message.type === "tool_call");
  assert.ok(switched >= 0 && switched < firstCall, "the switch is reported before any tool call");
  const answered = turnLines.flatMap((message) => (message.type === "event" && message.event.kind === "message" ? [message.event.model] : []));
  assert.deepEqual(answered, [FAKE_MODEL, FAKE_FALLBACK_MODEL, FAKE_FALLBACK_MODEL]);
  assert.equal(result.usage.calls, 3);
  const context = agent.contextMessages("decision");
  assert.equal(context.filter((message) => message.role === "user").length, 2, "the earlier turn plus one copy of this brief");
  assert.ok(!context.some((message) => message.role === "assistant" && message.stopReason === "error"), "the failed attempt left the context");
  assert.ok(readFileSync(result.session_file!, "utf8").includes("insufficient_quota"), "the failed attempt stays in the session file");
  agent.dispose();
});

test("no fallback once a tool ran in the turn", async () => {
  const script: FakeScript = {
    turns: [{ lane: "decision", steps: [{ tool_calls: [{ name: "portfolio", args: {} }] }, { error: "insufficient_quota: You exceeded your current quota" }] }],
  };
  const { dir, agent, sent, run } = await harness(script, (name) => ({ ok: true, output: { tool: name } }));
  const request = { ...turn("t1", "decision", dir), fallback: { provider: FAKE_PROVIDER, id: FAKE_FALLBACK_MODEL } };
  const result = await run(request);
  assert.equal(result.status, "error");
  assert.match(result.error ?? "", /insufficient_quota/);
  assert.equal(result.model, FAKE_MODEL);
  assert.equal(result.fallback, undefined);
  assert.ok(!sent.some((message) => message.type === "event" && message.event.kind === "fallback"));
  agent.dispose();
});

test("the stdio service speaks the protocol end to end", async () => {
  const dir = mkdtempSync(join(tmpdir(), "nivra-agent-wire-"));
  const scriptFile = join(dir, "script.json");
  writeFileSync(scriptFile, JSON.stringify({ turns: [{ lane: "review", steps: [{ tool_calls: [{ name: "portfolio", args: {} }] }, { text: "memory" }] }] }));
  const child = spawn(process.execPath, [join(root, "dist", "src", "main.js"), "--agent-dir", dir, "--fake", scriptFile], { stdio: ["pipe", "pipe", "inherit"] });
  const lines = createInterface({ input: child.stdout });
  const messages: Outbound[] = [];
  const write = (value: object) => child.stdin.write(`${JSON.stringify({ v: PROTOCOL_VERSION, ...value })}\n`);
  for await (const line of lines) {
    const message = JSON.parse(line) as Outbound;
    messages.push(message);
    if (message.type === "ready") {
      const request: Omit<TurnRequest, "type"> = turn("w1", "review", dir, "2026-11-18", [tool("portfolio")]);
      write({ type: "turn", ...request });
    }
    if (message.type === "tool_call") write({ type: "tool_result", turn_id: message.turn_id, call_id: message.call_id, ok: true, output: { cash: "5000" } });
    if (message.type === "turn_result") {
      write({ type: "shutdown" });
      break;
    }
  }
  const result = messages.at(-1);
  assert.ok(result && result.type === "turn_result" && result.status === "ok" && result.output.text === "memory");
  assert.equal(messages[0]?.type, "ready");
  await new Promise((resolve) => child.on("exit", resolve));
});

test("the check mode reports catalog and credential presence without a turn", async () => {
  const dir = mkdtempSync(join(tmpdir(), "nivra-agent-check-"));
  writeFileSync(join(dir, "auth.json"), JSON.stringify({ mistral: { type: "api_key", key: "test-value-not-a-key" } }), { mode: 0o600 });
  const models = [
    { provider: "mistral", id: "deepseek-v4-1-flash-1017" },
    { provider: "openai-codex", id: "no-such-model" },
  ];
  const child = spawn(process.execPath, [join(root, "dist", "src", "main.js"), "--agent-dir", dir, "--check", JSON.stringify(models)], {
    stdio: ["ignore", "pipe", "inherit"],
    env: { PATH: process.env.PATH ?? "", HOME: dir },
  });
  let out = "";
  child.stdout.on("data", (chunk) => (out += chunk));
  const code = await new Promise((resolve) => child.on("exit", resolve));
  assert.equal(code, 0);
  assert.ok(!out.includes("test-value-not-a-key"), "no credential value is printed");
  assert.deepEqual(JSON.parse(out), {
    v: PROTOCOL_VERSION,
    type: "check",
    models: [
      { provider: "mistral", id: "deepseek-v4-1-flash-1017", known: true, auth: true },
      { provider: "openai-codex", id: "no-such-model", known: false, auth: false },
    ],
  });
});

test("a lane compaction threshold becomes a per-model reserve for the primary and fallback", () => {
  const model = (provider: string, id: string, contextWindow: number) => ({ provider, id, contextWindow }) as unknown as Parameters<typeof compactionSettings>[1][number];
  assert.deepEqual(compactionSettings(undefined, [model("mistral", "flash", 1_048_576)]), { enabled: true });
  const settings = compactionSettings(300_000, [model("mistral", "flash", 1_048_576), model("openai-codex", "sol", 500_000), model("mistral", "small", 128_000)]);
  assert.deepEqual(settings.modelOverrides, {
    "mistral/flash": { reserveTokens: 748_576, keepRecentTokens: 60_000 },
    "openai-codex/sol": { reserveTokens: 200_000, keepRecentTokens: 60_000 },
    // A window smaller than the threshold keeps pi's own reserve.
    "mistral/small": { reserveTokens: 16_384, keepRecentTokens: 60_000 },
  });
});

test("skills load from default repo skills and the workspace skills dir Rust names, and read resolves both", async () => {
  const defaultAbiPath = join(defaultSkills, "strategy-abi", "SKILL.md");
  const bookDir = mkdtempSync(join(tmpdir(), "nivra-agent-skills-book-"));
  // The research workspace lives outside the dated book, under the sessions root.
  const workspaceSkills = join(bookDir, "research", "workspace", "skills");
  mkdirSync(join(workspaceSkills, "custom-gap-fade"), { recursive: true });
  const workspaceSkillPath = join(workspaceSkills, "custom-gap-fade", "SKILL.md");
  writeFileSync(workspaceSkillPath, "---\nname: custom-gap-fade\ndescription: Procedure for testing gap-fade setups.\n---\n\n# Gap Fade\nTest on 3+ dates.\n");

  const script: FakeScript = {
    turns: [
      {
        lane: "research",
        steps: [
          {
            tool_calls: [
              { name: "read", args: { path: defaultAbiPath } },
              { name: "read", args: { path: workspaceSkillPath } },
            ],
          },
          { text: "loaded both skills" },
        ],
      },
      {
        lane: "research",
        steps: [{ text: "reloaded after workspace write" }],
      },
    ],
  };
  const rustReadPaths: string[] = [];
  const { agent, sent, run } = await harness(
    script,
    (name, args) => {
      if (name === "read") rustReadPaths.push((args as { path: string }).path);
      return { ok: true, output: { path: (args as { path: string }).path, text: "workspace skill text" } };
    },
    { skillsDir: defaultSkills },
  );

  const sessionDir = join(bookDir, "model", "agent", "pi", "research");
  const researchTools = [tool("read", { path: { type: "string" } }), tool("portfolio")];
  const r1 = await run({ ...turn("s1", "research", bookDir, "2026-11-26", researchTools), session: { date: "2026-11-26", dir: sessionDir, skills_dir: workspaceSkills } });
  assert.equal(r1.status, "ok", r1.error);
  assert.deepEqual(agent.skillDiagnostics("research"), []);
  const names = agent.loadedSkills("research").map((s) => s.name).sort();
  assert.deepEqual(names, ["autoresearch", "backtest-methodology", "custom-gap-fade", "regime-check", "strategy-abi", "strategy-library", "strategy-review"]);

  const sysPrompt = agent.systemPrompt("research") ?? "";
  assert.ok(sysPrompt.includes("<available_skills>"));
  assert.ok(sysPrompt.includes("strategy-abi"));
  assert.ok(sysPrompt.includes("custom-gap-fade"));

  // Default skill read is handled locally without hitting Rust workspace_read;
  // workspace skill absolute location is rewritten to workspace-relative skills/... for Rust.
  assert.deepEqual(rustReadPaths, ["skills/custom-gap-fade/SKILL.md"]);
  assert.ok(!sent.some((m) => m.type === "event" && m.event.kind === "skill_diagnostic"));

  // Adding a new workspace skill mid-session reloads on the next turn.
  mkdirSync(join(workspaceSkills, "post-close-sweep"), { recursive: true });
  writeFileSync(join(workspaceSkills, "post-close-sweep", "SKILL.md"), "---\nname: post-close-sweep\ndescription: Sweep discipline.\n---\n");
  const r2 = await run({ ...turn("s2", "research", bookDir, "2026-11-26", researchTools), session: { date: "2026-11-26", dir: sessionDir, skills_dir: workspaceSkills } });
  assert.equal(r2.status, "ok", r2.error);
  assert.ok(agent.loadedSkills("research").some((s) => s.name === "post-close-sweep"));
  assert.ok((agent.systemPrompt("research") ?? "").includes("post-close-sweep"));
  agent.dispose();
});

test("malformed and colliding skills produce diagnostics without failing the turn", async () => {
  const dir = mkdtempSync(join(tmpdir(), "nivra-agent-bad-skills-"));
  const workspaceSkills = join(dir, "workspace", "skills");
  // Malformed YAML frontmatter + missing description
  mkdirSync(join(workspaceSkills, "broken-yaml"), { recursive: true });
  writeFileSync(join(workspaceSkills, "broken-yaml", "SKILL.md"), "---\nname: [unterminated\n---\n");
  mkdirSync(join(workspaceSkills, "missing-desc"), { recursive: true });
  writeFileSync(join(workspaceSkills, "missing-desc", "SKILL.md"), "---\nname: missing-desc\n---\nNo description.\n");
  // Collides with committed default skill name
  mkdirSync(join(workspaceSkills, "strategy-abi"), { recursive: true });
  writeFileSync(join(workspaceSkills, "strategy-abi", "SKILL.md"), "---\nname: strategy-abi\ndescription: Shadowed workspace copy.\n---\n");

  const script: FakeScript = {
    turns: [{ lane: "review", steps: [{ text: "review completed despite broken skills" }] }],
  };
  const { agent, sent, run } = await harness(script, () => ({ ok: true, output: {} }), {
    skillsDir: defaultSkills,
    workspaceSkillsDir: workspaceSkills,
  });

  const result = await run(turn("d1", "review", dir, "2026-11-26", [tool("read", { path: { type: "string" } })]));
  assert.equal(result.status, "ok", result.error);
  assert.equal(result.output.text, "review completed despite broken skills");

  const diagnostics = agent.skillDiagnostics("review");
  assert.ok(diagnostics.length >= 3, `expected diagnostics for broken/missing/colliding skills, got ${diagnostics.length}`);
  assert.ok(diagnostics.some((d) => d.path?.includes("broken-yaml") && d.type === "warning"));
  assert.ok(diagnostics.some((d) => d.path?.includes("missing-desc") && d.message.includes("description is required")));
  assert.ok(diagnostics.some((d) => d.type === "collision" && d.collision?.name === "strategy-abi"));

  const emitted = sent.filter((m): m is Extract<Outbound, { type: "event" }> => m.type === "event" && m.event.kind === "skill_diagnostic");
  assert.equal(emitted.length, diagnostics.length);
  // Default skills still loaded cleanly.
  assert.equal(agent.loadedSkills("review").length, 6);
  agent.dispose();
});

test("review_strategy returns structured verdicts, supports fallback, and fails closed on invalid output", async () => {
  const script: FakeScript = {
    turns: [],
    reviews: [
      { text: '{"verdict":"safe","reasons":[]}' },
      { text: '{"verdict":"unsafe","reasons":["attempts socket connect"]}' },
      { error: "429 quota exceeded" },
      { text: '{"verdict":"safe","reasons":[]}' },
      { text: "not json at all" },
    ],
  };
  const { agent, review } = await harness(script, () => ({ ok: true, output: {} }));

  const safe = await review("def on_observation(obs):\n    return {'actions': []}\n", "sha-safe", "r1");
  assert.equal(safe.status, "ok");
  assert.equal(safe.verdict, "safe");
  assert.deepEqual(safe.reasons, []);

  const unsafe = await review("import socket", "sha-unsafe", "r2");
  assert.equal(unsafe.status, "ok");
  assert.equal(unsafe.verdict, "unsafe");
  assert.deepEqual(unsafe.reasons, ["attempts socket connect"]);

  const viaFallback = await review("def on_observation(obs):\n    return {'actions': []}\n", "sha-fb", "r3");
  assert.equal(viaFallback.status, "ok");
  assert.equal(viaFallback.verdict, "safe");
  assert.equal(viaFallback.model, FAKE_FALLBACK_MODEL);

  const malformed = await review("def on_observation(obs):\n    return {'actions': []}\n", "sha-bad", "r4");
  assert.equal(malformed.status, "ok");
  assert.equal(malformed.verdict, "unsafe");
  assert.deepEqual(malformed.reasons, ["strategy_review_output_invalid"]);

  agent.dispose();
});
