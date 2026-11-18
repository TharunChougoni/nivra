// JSONL protocol between the Rust controller and this agent. One JSON object per
// line. The Rust side (rust/nivra-cli/src/runtime/agent/protocol.rs) mirrors it.

export const PROTOCOL_VERSION = 1;

export type Lane = "decision" | "research" | "review";
export const LANES: readonly Lane[] = ["decision", "research", "review"];

export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export type JsonObject = { [key: string]: JsonValue };

export interface ToolSpec {
  name: string;
  description: string;
  parameters: JsonObject;
}

export type Thinking = "off" | "minimal" | "low" | "medium" | "high" | "xhigh";

export interface ModelSpec {
  provider: string;
  id: string;
  thinking?: Thinking;
}

export interface TurnRequest {
  type: "turn";
  id: string;
  lane: Lane;
  /** The lane's pi session: one per session date, stored under `dir`. */
  session: { date: string; dir: string };
  model: ModelSpec;
  trigger: JsonObject;
  brief: JsonValue;
  tools: ToolSpec[];
}

export interface ToolResultMessage {
  type: "tool_result";
  turn_id: string;
  call_id: string;
  ok: boolean;
  output?: JsonValue;
  error?: string;
}

export interface AbortMessage {
  type: "abort";
  turn_id: string;
  reason: string;
}

export interface ShutdownMessage {
  type: "shutdown";
}

export type Inbound = TurnRequest | ToolResultMessage | AbortMessage | ShutdownMessage;

export interface Usage {
  input: number;
  output: number;
  cache_read: number;
  cache_write: number;
  cost_usd: number;
  /** Model requests made, including compaction summaries. */
  calls: number;
}

export type TurnStatus = "ok" | "error" | "aborted";

export type Outbound =
  | { v: number; type: "ready"; pi_version: string; node: string; pid: number }
  | { v: number; type: "tool_call"; turn_id: string; call_id: string; name: string; args: JsonValue }
  | { v: number; type: "event"; turn_id: string; event: JsonObject }
  | {
      v: number;
      type: "turn_result";
      id: string;
      status: TurnStatus;
      error?: string;
      output: { text: string };
      usage: Usage;
      model: string;
      provider: string;
      session_file: string | null;
      prompt_sha256: string;
    }
  | { v: number; type: "error"; error: string };

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function str(value: unknown, field: string): string {
  if (typeof value !== "string" || value.length === 0) throw new Error(`protocol_field_invalid:${field}`);
  return value;
}

function tools(value: unknown): ToolSpec[] {
  if (!Array.isArray(value)) throw new Error("protocol_field_invalid:tools");
  const names = new Set<string>();
  return value.map((tool) => {
    if (!isObject(tool) || !isObject(tool.parameters)) throw new Error("protocol_field_invalid:tools");
    const name = str(tool.name, "tools.name");
    if (names.has(name)) throw new Error(`protocol_tool_duplicate:${name}`);
    names.add(name);
    return { name, description: str(tool.description, "tools.description"), parameters: tool.parameters as JsonObject };
  });
}

/** Parse one inbound line. Unknown types and versions are refused, never guessed. */
export function parseInbound(line: string): Inbound {
  const value: unknown = JSON.parse(line);
  if (!isObject(value)) throw new Error("protocol_message_invalid");
  if (value.v !== PROTOCOL_VERSION) throw new Error("protocol_version_unsupported");
  switch (value.type) {
    case "turn": {
      const lane = value.lane;
      if (typeof lane !== "string" || !LANES.includes(lane as Lane)) throw new Error("protocol_field_invalid:lane");
      const session = value.session;
      const model = value.model;
      if (!isObject(session) || !isObject(model) || !isObject(value.trigger)) throw new Error("protocol_message_invalid");
      const thinking = model.thinking;
      if (thinking !== undefined && !["off", "minimal", "low", "medium", "high", "xhigh"].includes(String(thinking))) {
        throw new Error("protocol_field_invalid:model.thinking");
      }
      return {
        type: "turn",
        id: str(value.id, "id"),
        lane: lane as Lane,
        session: { date: str(session.date, "session.date"), dir: str(session.dir, "session.dir") },
        model: {
          provider: str(model.provider, "model.provider"),
          id: str(model.id, "model.id"),
          ...(thinking === undefined ? {} : { thinking: thinking as Thinking }),
        },
        trigger: value.trigger as JsonObject,
        brief: (value.brief ?? null) as JsonValue,
        tools: tools(value.tools),
      };
    }
    case "tool_result":
      if (typeof value.ok !== "boolean") throw new Error("protocol_field_invalid:ok");
      return {
        type: "tool_result",
        turn_id: str(value.turn_id, "turn_id"),
        call_id: str(value.call_id, "call_id"),
        ok: value.ok,
        output: (value.output ?? null) as JsonValue,
        ...(typeof value.error === "string" ? { error: value.error } : {}),
      };
    case "abort":
      return { type: "abort", turn_id: str(value.turn_id, "turn_id"), reason: str(value.reason, "reason") };
    case "shutdown":
      return { type: "shutdown" };
    default:
      throw new Error("protocol_type_unknown");
  }
}
