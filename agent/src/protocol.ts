// JSONL protocol between the Rust controller and this agent. One JSON object per
// line. The Rust side (rust/nivra-cli/src/runtime/agent/protocol.rs) mirrors it.

export const PROTOCOL_VERSION = 1;

export type Lane = "decision" | "research" | "review" | "gate";
export const LANES: readonly Lane[] = ["decision", "research", "review", "gate"];

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
  /** Used only when the primary fails before any output for a reason `fallbackReason` accepts. */
  fallback?: ModelSpec;
  trigger: JsonObject;
  brief: JsonValue;
  tools: ToolSpec[];
  /** Context tokens at which pi compacts this lane's session, for every model it uses. */
  compact_at_tokens?: number;
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

export interface ReviewStrategyRequest {
  type: "review_strategy";
  id: string;
  model: ModelSpec;
  fallback?: ModelSpec;
  source: string;
  source_sha256: string;
}

export type Inbound = TurnRequest | ReviewStrategyRequest | ToolResultMessage | AbortMessage | ShutdownMessage;

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

export type FallbackReason = "quota" | "auth" | "rejected";

/** A turn moved from the lane's primary model to its fallback. */
export interface Fallback {
  from: { provider: string; id: string };
  to: { provider: string; id: string };
  reason: FallbackReason;
  /** The primary's error, bounded. */
  error: string;
}

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
      fallback?: Fallback;
    }
  | {
      v: number;
      type: "review_strategy_result";
      id: string;
      status: TurnStatus;
      error?: string;
      verdict: "safe" | "unsafe";
      reasons: string[];
      model: string;
      provider: string;
    }
  | { v: number; type: "error"; error: string };

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function str(value: unknown, field: string): string {
  if (typeof value !== "string" || value.length === 0) throw new Error(`protocol_field_invalid:${field}`);
  return value;
}

const THINKING: readonly Thinking[] = ["off", "minimal", "low", "medium", "high", "xhigh"];

function modelSpec(value: unknown, field: string): ModelSpec {
  if (!isObject(value)) throw new Error(`protocol_field_invalid:${field}`);
  const thinking = value.thinking;
  if (thinking !== undefined && !THINKING.includes(thinking as Thinking)) throw new Error(`protocol_field_invalid:${field}.thinking`);
  return {
    provider: str(value.provider, `${field}.provider`),
    id: str(value.id, `${field}.id`),
    ...(thinking === undefined ? {} : { thinking: thinking as Thinking }),
  };
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

function compactAt(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) throw new Error("protocol_field_invalid:compact_at_tokens");
  return value;
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
      if (!isObject(session) || !isObject(value.trigger)) throw new Error("protocol_message_invalid");
      return {
        type: "turn",
        id: str(value.id, "id"),
        lane: lane as Lane,
        session: { date: str(session.date, "session.date"), dir: str(session.dir, "session.dir") },
        model: modelSpec(value.model, "model"),
        ...(value.fallback == null ? {} : { fallback: modelSpec(value.fallback, "fallback") }),
        trigger: value.trigger as JsonObject,
        brief: (value.brief ?? null) as JsonValue,
        tools: tools(value.tools),
        ...(value.compact_at_tokens == null ? {} : { compact_at_tokens: compactAt(value.compact_at_tokens) }),
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
    case "review_strategy":
      return {
        type: "review_strategy",
        id: str(value.id, "id"),
        model: modelSpec(value.model, "model"),
        ...(value.fallback == null ? {} : { fallback: modelSpec(value.fallback, "fallback") }),
        source: str(value.source, "source"),
        source_sha256: str(value.source_sha256, "source_sha256"),
      };
    case "shutdown":
      return { type: "shutdown" };
    default:
      throw new Error("protocol_type_unknown");
  }
}
