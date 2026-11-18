import { type AssistantMessage, fauxAssistantMessage, fauxProvider, fauxToolCall } from "@earendil-works/pi-ai";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import type { JsonObject, Lane, TurnRequest } from "./protocol.js";

/** One scripted model response: tool calls, or a final text answer. */
export type FakeStep = { tool_calls: { name: string; args: JsonObject }[] } | { text: string };

/** Turns are consumed in order per lane. Offline proof only; never a production provider. */
export interface FakeScript {
  turns: { lane: Lane; steps: FakeStep[] }[];
}

export const FAKE_PROVIDER = "fake";
export const FAKE_MODEL = "scripted";

function message(step: FakeStep): AssistantMessage {
  if ("text" in step) return fauxAssistantMessage(step.text);
  return fauxAssistantMessage(
    step.tool_calls.map((call) => fauxToolCall(call.name, call.args)),
    { stopReason: "toolUse" },
  );
}

/** Registers the scripted provider and returns the per-turn hook that loads its responses. */
export function installFake(runtime: ModelRuntime, script: FakeScript): (turn: TurnRequest) => void {
  const faux = fauxProvider({
    provider: FAKE_PROVIDER,
    models: [{ id: FAKE_MODEL, cost: { input: 1, output: 4, cacheRead: 0.1, cacheWrite: 1 } }],
  });
  runtime.registerNativeProvider(faux.provider);
  const remaining = [...script.turns];
  return (turn) => {
    const index = remaining.findIndex((scripted) => scripted.lane === turn.lane);
    const steps = index < 0 ? [{ text: "fake script has no turn for this lane" }] : remaining.splice(index, 1)[0]!.steps;
    faux.setResponses(steps.map(message));
  };
}
