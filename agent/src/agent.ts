import { createHash } from "node:crypto";
import { mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { Model } from "@earendil-works/pi-ai";
import {
  type AgentSession,
  type AgentSessionEvent,
  createAgentSession,
  createExtensionRuntime,
  type ModelRuntime,
  type ResourceLoader,
  SessionManager,
  SettingsManager,
  type ToolDefinition,
} from "@earendil-works/pi-coding-agent";
import {
  type Inbound,
  type JsonObject,
  type JsonValue,
  type Lane,
  type Outbound,
  PROTOCOL_VERSION,
  type ToolSpec,
  type TurnRequest,
  type TurnStatus,
  type Usage,
} from "./protocol.js";

export interface AgentOptions {
  runtime: ModelRuntime;
  promptsDir: string;
  send: (message: Outbound) => void;
  /** Test and fake-provider hook, called before a turn prompts the model. */
  beforeTurn?: (turn: TurnRequest) => void;
}

interface LaneSession {
  date: string;
  dir: string;
  toolsKey: string;
  session: AgentSession;
}

interface PendingCall {
  resolve: (output: JsonValue) => void;
  reject: (error: Error) => void;
}

interface ActiveTurn {
  request: TurnRequest;
  usage: Usage;
  callIds: Set<string>;
  pending: Map<string, PendingCall>;
  abortReason?: string;
}

function emptyUsage(): Usage {
  return { input: 0, output: 0, cache_read: 0, cache_write: 0, cost_usd: 0, calls: 0 };
}

function addUsage(total: Usage, usage: { input: number; output: number; cacheRead: number; cacheWrite: number; cost: { total: number } }): void {
  total.input += usage.input;
  total.output += usage.output;
  total.cache_read += usage.cacheRead;
  total.cache_write += usage.cacheWrite;
  total.cost_usd += usage.cost.total;
  total.calls += 1;
}

function toolsKey(tools: ToolSpec[]): string {
  return createHash("sha256").update(JSON.stringify(tools)).digest("hex");
}

/** Rust accepts call ids of [A-Za-z0-9_-]{1,128}; provider ids are mapped into that shape. */
function bridgeCallId(raw: string, seen: Set<string>): string {
  const base = raw.replace(/[^A-Za-z0-9_-]/g, "_").slice(0, 120) || "call";
  let id = base;
  for (let n = 2; seen.has(id); n++) id = `${base}-${n}`;
  seen.add(id);
  return id;
}

/** Stable facts first and the trigger last, so a cached prefix survives the most turns. */
function briefText(turn: TurnRequest): string {
  const brief = typeof turn.brief === "string" ? turn.brief : JSON.stringify(turn.brief, null, 1);
  return `Brief:\n${brief}\n\nTrigger: ${JSON.stringify(turn.trigger)}`;
}

function usageFields(usage: { input: number; output: number; cacheRead: number; cacheWrite: number; cost: { total: number } }): JsonObject {
  return { input: usage.input, output: usage.output, cache_read: usage.cacheRead, cache_write: usage.cacheWrite, cost_usd: usage.cost.total };
}

function resourceLoader(systemPrompt: string): ResourceLoader {
  return {
    getExtensions: () => ({ extensions: [], errors: [], runtime: createExtensionRuntime() }),
    getSkills: () => ({ skills: [], diagnostics: [] }),
    getPrompts: () => ({ prompts: [], diagnostics: [] }),
    getThemes: () => ({ themes: [], diagnostics: [] }),
    getAgentsFiles: () => ({ agentsFiles: [] }),
    getSystemPrompt: () => systemPrompt,
    getSystemPromptSource: () => undefined,
    getAppendSystemPrompt: () => [],
    getAppendSystemPromptSources: () => [],
    extendResources: () => {},
    reload: async () => {},
  };
}

/**
 * Runs one lane turn at a time on pi. Every tool is a Rust tool: the agent only
 * forwards calls and returns Rust's answer. pi's built-in tools are never enabled.
 */
export class LaneAgent {
  private readonly sessions = new Map<Lane, LaneSession>();
  private readonly prompts = new Map<Lane, { text: string; sha256: string }>();
  private active: ActiveTurn | undefined;

  constructor(private readonly options: AgentOptions) {}

  handle(message: Inbound): void {
    switch (message.type) {
      case "turn":
        void this.runTurn(message);
        return;
      case "tool_result": {
        const pending = this.active?.request.id === message.turn_id ? this.active.pending.get(message.call_id) : undefined;
        if (!pending) {
          this.send({ v: PROTOCOL_VERSION, type: "error", error: `agent_tool_result_unknown:${message.call_id}` });
          return;
        }
        this.active?.pending.delete(message.call_id);
        if (message.ok) pending.resolve(message.output ?? null);
        else pending.reject(new Error(message.error ?? "tool_failed"));
        return;
      }
      case "abort":
        if (this.active?.request.id === message.turn_id) {
          this.active.abortReason = message.reason;
          for (const pending of this.active.pending.values()) pending.reject(new Error(`aborted:${message.reason}`));
          this.active.pending.clear();
          void this.sessions.get(this.active.request.lane)?.session.abort();
        }
        return;
      case "shutdown":
        this.dispose();
        return;
    }
  }

  dispose(): void {
    for (const lane of this.sessions.values()) lane.session.dispose();
    this.sessions.clear();
  }

  /** Session file of a lane, for tests and diagnostics. */
  sessionFile(lane: Lane): string | undefined {
    return this.sessions.get(lane)?.session.sessionFile;
  }

  activeToolNames(lane: Lane): string[] {
    return this.sessions.get(lane)?.session.getActiveToolNames() ?? [];
  }

  private send(message: Outbound): void {
    this.options.send(message);
  }

  private prompt(lane: Lane): { text: string; sha256: string } {
    let prompt = this.prompts.get(lane);
    if (!prompt) {
      const text = readFileSync(join(this.options.promptsDir, `${lane}.md`), "utf8");
      prompt = { text, sha256: createHash("sha256").update(text).digest("hex") };
      this.prompts.set(lane, prompt);
    }
    return prompt;
  }

  private bridgeTool(spec: ToolSpec): ToolDefinition {
    return {
      name: spec.name,
      label: spec.name,
      description: spec.description,
      // Rust publishes JSON Schema; TypeBox schemas are JSON Schema, so pi validates it as is.
      parameters: spec.parameters as unknown as ToolDefinition["parameters"],
      executionMode: "sequential",
      execute: async (toolCallId, params) => {
        const turn = this.active;
        if (!turn) throw new Error("agent_turn_missing");
        const callId = bridgeCallId(toolCallId, turn.callIds);
        const output = await new Promise<JsonValue>((resolve, reject) => {
          turn.pending.set(callId, { resolve, reject });
          this.send({
            v: PROTOCOL_VERSION,
            type: "tool_call",
            turn_id: turn.request.id,
            call_id: callId,
            name: spec.name,
            args: (params ?? {}) as JsonValue,
          });
        });
        return { content: [{ type: "text", text: JSON.stringify(output) }], details: undefined };
      },
    };
  }

  private async laneSession(turn: TurnRequest, model: Model<string>): Promise<AgentSession> {
    const key = toolsKey(turn.tools);
    const current = this.sessions.get(turn.lane);
    if (current && current.date === turn.session.date && current.dir === turn.session.dir && current.toolsKey === key) {
      return current.session;
    }
    // A new date starts a new session; a changed tool set reopens the same file.
    const file = current && current.date === turn.session.date && current.dir === turn.session.dir ? current.session.sessionFile : undefined;
    current?.session.dispose();
    this.sessions.delete(turn.lane);
    mkdirSync(turn.session.dir, { recursive: true, mode: 0o700 });
    const sessionManager = file
      ? SessionManager.open(file, turn.session.dir, turn.session.dir)
      : SessionManager.continueRecent(turn.session.dir, turn.session.dir);
    const { session } = await createAgentSession({
      cwd: turn.session.dir,
      agentDir: turn.session.dir,
      model,
      thinkingLevel: turn.model.thinking ?? "medium",
      modelRuntime: this.options.runtime,
      resourceLoader: resourceLoader(this.prompt(turn.lane).text),
      tools: turn.tools.map((tool) => tool.name),
      customTools: turn.tools.map((tool) => this.bridgeTool(tool)),
      sessionManager,
      settingsManager: SettingsManager.inMemory({ compaction: { enabled: true }, retry: { enabled: true, maxRetries: 2 } }),
    });
    this.sessions.set(turn.lane, { date: turn.session.date, dir: turn.session.dir, toolsKey: key, session });
    return session;
  }

  private event(turn: ActiveTurn, event: AgentSessionEvent): void {
    const emit = (fields: JsonObject) => this.send({ v: PROTOCOL_VERSION, type: "event", turn_id: turn.request.id, event: fields });
    switch (event.type) {
      case "message_end": {
        const message = event.message;
        if (message.role !== "assistant") return;
        addUsage(turn.usage, message.usage);
        const text = message.content.flatMap((part) => (part.type === "text" ? [part.text] : [])).join("");
        const toolCalls = message.content.flatMap((part) => (part.type === "toolCall" ? [{ id: part.id, name: part.name }] : []));
        emit({
          kind: "message",
          stop_reason: message.stopReason,
          text,
          tool_calls: toolCalls,
          provider: message.provider,
          model: message.model,
          usage: { ...usageFields(message.usage), reasoning: message.usage.reasoning ?? null },
          error: message.errorMessage ?? null,
        });
        return;
      }
      case "tool_execution_start":
        emit({ kind: "tool_start", pi_call_id: event.toolCallId, name: event.toolName, args: (event.args ?? null) as JsonValue });
        return;
      case "tool_execution_end":
        emit({ kind: "tool_end", pi_call_id: event.toolCallId, name: event.toolName, is_error: event.isError, duration_ms: event.durationMs ?? null });
        return;
      case "compaction_start":
        emit({ kind: "compaction_start", reason: event.reason });
        return;
      case "compaction_end":
        if (event.result?.usage) addUsage(turn.usage, event.result.usage);
        emit({
          kind: "compaction_end",
          reason: event.reason,
          aborted: event.aborted,
          will_retry: event.willRetry,
          tokens_before: event.result?.tokensBefore ?? null,
          tokens_after: event.result?.estimatedTokensAfter ?? null,
          // Rust charges this to the lane even if the agent dies before turn_result.
          usage: event.result?.usage ? usageFields(event.result.usage) : null,
          error: event.errorMessage ?? null,
        });
        return;
      case "auto_retry_start":
        emit({ kind: "retry_start", attempt: event.attempt, max_attempts: event.maxAttempts, delay_ms: event.delayMs, error: event.errorMessage });
        return;
      case "auto_retry_end":
        emit({ kind: "retry_end", success: event.success, attempt: event.attempt, error: event.finalError ?? null });
        return;
      default:
        return;
    }
  }

  private async runTurn(request: TurnRequest): Promise<void> {
    const prompt = (() => {
      try {
        return this.prompt(request.lane);
      } catch {
        return undefined;
      }
    })();
    const result = (status: TurnStatus, text: string, usage: Usage, error?: string, session?: AgentSession) =>
      this.send({
        v: PROTOCOL_VERSION,
        type: "turn_result",
        id: request.id,
        status,
        ...(error === undefined ? {} : { error }),
        output: { text },
        usage,
        model: request.model.id,
        provider: request.model.provider,
        session_file: session?.sessionFile ?? null,
        prompt_sha256: prompt?.sha256 ?? "",
      });
    if (this.active) {
      result("error", "", emptyUsage(), "agent_turn_busy");
      return;
    }
    if (!prompt) {
      result("error", "", emptyUsage(), "agent_prompt_missing");
      return;
    }
    const model = this.options.runtime.getModel(request.model.provider, request.model.id);
    if (!model) {
      result("error", "", emptyUsage(), "agent_model_unknown");
      return;
    }
    const turn: ActiveTurn = { request, usage: emptyUsage(), callIds: new Set(), pending: new Map() };
    this.active = turn;
    let session: AgentSession | undefined;
    let unsubscribe = () => {};
    try {
      session = await this.laneSession(request, model);
      if (session.model?.provider !== model.provider || session.model?.id !== model.id) await session.setModel(model);
      if (request.model.thinking && session.thinkingLevel !== request.model.thinking) session.setThinkingLevel(request.model.thinking);
      // An abort that arrived while the session was being opened has no
      // running prompt to cancel; honour it before any model call.
      if (turn.abortReason !== undefined) {
        result("aborted", "", turn.usage, turn.abortReason, session);
        return;
      }
      const active = session;
      unsubscribe = active.subscribe((event) => this.event(turn, event));
      this.options.beforeTurn?.(request);
      await active.prompt(briefText(request));
      const last = active.messages.findLast((message) => message.role === "assistant");
      const text = active.getLastAssistantText() ?? "";
      if (turn.abortReason !== undefined || (last?.role === "assistant" && last.stopReason === "aborted")) {
        result("aborted", text, turn.usage, turn.abortReason ?? "aborted", active);
      } else if (last?.role === "assistant" && last.stopReason === "error") {
        result("error", text, turn.usage, last.errorMessage ?? "provider_error", active);
      } else {
        result("ok", text, turn.usage, undefined, active);
      }
    } catch (error) {
      result("error", "", turn.usage, error instanceof Error ? error.message : String(error), session);
    } finally {
      unsubscribe();
      this.active = undefined;
    }
  }
}
