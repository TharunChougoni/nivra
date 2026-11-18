import { type AssistantMessage, isContextOverflow, isRetryableAssistantError } from "@earendil-works/pi-ai";
import type { FallbackReason } from "./protocol.js";

// Checked before pi's transient classifier: pi treats 429 and rate limits as
// retryable, so a limit still reported after pi's retries is exhaustion here.
const QUOTA = /usage.?limit|quota|rate.?limit|too many requests|out of budget|billing|resource.?exhausted|(?<![a-z0-9])429(?![a-z0-9])/i;
const AUTH =
  /(?<![a-z0-9])(?:401|403)(?![a-z0-9])|unauthori[sz]ed|forbidden|authenticat|api.?key|credential|entitle|usage_not_included|not included in your plan|permission|access denied|oauth|token (?:has )?(?:expired|invalid|revoked)/i;

/**
 * Why a turn that failed before any output may move to the lane's fallback
 * model, or undefined to keep the failure. `providerAnswered` is false for an
 * error raised before a provider response, such as missing credentials; only
 * quota and auth failures qualify then. Transient failures (timeouts, 5xx,
 * network) and context overflow never qualify: pi retries or compacts those.
 */
export function fallbackReason(error: string, providerAnswered: boolean): FallbackReason | undefined {
  if (QUOTA.test(error)) return "quota";
  if (AUTH.test(error)) return "auth";
  if (!providerAnswered) return undefined;
  const failed = { stopReason: "error", errorMessage: error } as AssistantMessage;
  if (isRetryableAssistantError(failed) || isContextOverflow(failed)) return undefined;
  return "rejected";
}
