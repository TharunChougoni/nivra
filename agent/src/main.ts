#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { ModelRuntime, VERSION } from "@earendil-works/pi-coding-agent";
import { LaneAgent } from "./agent.js";
import { type FakeScript, installFake } from "./fake.js";
import { type Outbound, PROTOCOL_VERSION, parseInbound } from "./protocol.js";

// stdout carries only protocol lines; library logging goes to stderr.
console.log = console.error;
console.info = console.error;
console.warn = console.error;

const { values } = parseArgs({
  options: {
    "agent-dir": { type: "string" },
    prompts: { type: "string" },
    fake: { type: "string" },
  },
});
const agentDir = values["agent-dir"];
if (!agentDir) {
  process.stderr.write("usage: nivra-agent --agent-dir <dir> [--prompts <dir>] [--fake <script.json>]\n");
  process.exit(2);
}

const send = (message: Outbound) => {
  process.stdout.write(`${JSON.stringify(message)}\n`);
};

// auth.json (subscription OAuth, API keys) and models.json (custom providers)
// live in the application's own agent directory; nothing else is read.
const runtime = await ModelRuntime.create({
  authPath: join(agentDir, "auth.json"),
  modelsPath: join(agentDir, "models.json"),
  allowModelNetwork: false,
  refreshOnCreate: false,
});
const beforeTurn = values.fake ? installFake(runtime, JSON.parse(readFileSync(values.fake, "utf8")) as FakeScript) : undefined;
const agent = new LaneAgent({
  runtime,
  promptsDir: values.prompts ?? join(dirname(fileURLToPath(import.meta.url)), "..", "..", "prompts"),
  send,
  ...(beforeTurn ? { beforeTurn } : {}),
});

send({ v: PROTOCOL_VERSION, type: "ready", pi_version: VERSION, node: process.version, pid: process.pid });

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  if (line.trim() === "") continue;
  try {
    const message = parseInbound(line);
    agent.handle(message);
    if (message.type === "shutdown") break;
  } catch (error) {
    send({ v: PROTOCOL_VERSION, type: "error", error: error instanceof Error ? error.message : String(error) });
  }
}
agent.dispose();
process.exit(0);
