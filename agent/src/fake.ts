import { type AssistantMessage, fauxAssistantMessage, fauxProvider, fauxToolCall } from "@earendil-works/pi-ai";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import type { JsonObject, Lane, ReviewStrategyRequest, TurnRequest } from "./protocol.js";

/** One scripted model response: tool calls, a final text answer, or a provider error. */
export type FakeStep = { tool_calls: { name: string; args: JsonObject }[] } | { text: string } | { error: string };

/** Turns are consumed in order per lane. Offline proof only; never a production provider. */
export interface FakeScript {
  turns: { lane: Lane; steps: FakeStep[] }[];
  reviews?: FakeStep[];
}

export const FAKE_PROVIDER = "fake";
export const FAKE_MODEL = "scripted";
/** A second scripted model, priced differently, for fallback checks. */
export const FAKE_FALLBACK_MODEL = "scripted-fallback";

function message(step: FakeStep): AssistantMessage {
  if ("text" in step) return fauxAssistantMessage(step.text);
  if ("error" in step) return fauxAssistantMessage([], { stopReason: "error", errorMessage: step.error });
  return fauxAssistantMessage(
    step.tool_calls.map((call) => fauxToolCall(call.name, call.args)),
    { stopReason: "toolUse" },
  );
}

/** Registers the scripted provider and returns per-turn and per-review hooks that load its responses. */
export function installFake(
  runtime: ModelRuntime,
  script: FakeScript,
): {
  beforeTurn: (turn: TurnRequest) => void;
  beforeReview: (request: ReviewStrategyRequest) => void;
} {
  const faux = fauxProvider({
    provider: FAKE_PROVIDER,
    models: [
      { id: FAKE_MODEL, cost: { input: 1, output: 4, cacheRead: 0.1, cacheWrite: 1 } },
      { id: FAKE_FALLBACK_MODEL, cost: { input: 2, output: 8, cacheRead: 0.2, cacheWrite: 2 } },
    ],
  });
  runtime.registerNativeProvider(faux.provider);
  const remaining = [...script.turns];
  const remainingReviews = [...(script.reviews ?? [])];
  return {
    beforeTurn: (turn) => {
      const index = remaining.findIndex((scripted) => scripted.lane === turn.lane);
      const steps = index < 0 ? [{ text: "fake script has no turn for this lane" }] : remaining.splice(index, 1)[0]!.steps;
      faux.setResponses(steps.map(message));
    },
    beforeReview: () => {
      const step = remainingReviews.shift() ?? { text: JSON.stringify({ verdict: "safe", reasons: [] }) };
      faux.setResponses([message(step)]);
    },
  };
}
