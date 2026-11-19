import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { LaneAgent } from "../src/agent.js";
import { FAKE_FALLBACK_MODEL, FAKE_MODEL, FAKE_PROVIDER, type FakeScript, installFake } from "../src/fake.js";
import { fallbackReason } from "../src/fallback.js";
import { type JsonObject, type Outbound, PROTOCOL_VERSION, parseInbound, type ToolSpec, type TurnRequest } from "../src/protocol.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const prompts = join(root, "prompts");

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
async function harness(script: FakeScript, answer: (name: string, args: unknown) => { ok: boolean; output?: unknown; error?: string }) {
  const dir = mkdtempSync(join(tmpdir(), "nivra-agent-test-"));
  const runtime = await ModelRuntime.create({ authPath: join(dir, "auth.json"), modelsPath: null, allowModelNetwork: false, refreshOnCreate: false });
  const beforeTurn = installFake(runtime, script);
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
    if (message.type === "turn_result") done.get(message.id)?.(message);
  };
  agent = new LaneAgent({ runtime, promptsDir: prompts, send, beforeTurn });
  const run = (request: TurnRequest) =>
    new Promise<Extract<Outbound, { type: "turn_result" }>>((resolve) => {
      done.set(request.id, (message) => resolve(message as Extract<Outbound, { type: "turn_result" }>));
      agent.handle(request);
    });
  return { dir, agent, sent, run };
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
