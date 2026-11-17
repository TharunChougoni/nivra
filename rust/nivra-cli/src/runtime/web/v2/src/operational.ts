import { count, duration, numeric } from "./data";
import type {
  ControllerLiveness,
  OperationalHealth,
  SupervisorOperationalStatus,
} from "./types";

// The server projects only bounded, static reason codes. The client keeps its
// own whitelist so an unexpected value can never become visible operator text.
const SUPERVISOR_REASONS = new Set([
  "model_response_incomplete",
  "supervisor_budget_exhausted",
  "supervisor_call_failed",
  "supervisor_call_timeout",
  "supervisor_cancelled",
  "supervisor_compaction_invalid",
  "supervisor_config_revision_changed",
  "supervisor_context_exhausted",
  "supervisor_context_exhausted_backoff",
  "supervisor_context_exhausted_no_history",
  "supervisor_disabled",
  "supervisor_handoff_pending",
  "supervisor_lifecycle_receipt_conflict",
  "supervisor_model_response_incomplete",
  "supervisor_notes_unavailable",
  "supervisor_parked_budget",
  "supervisor_pending_at_handoff",
  "supervisor_prompt_revision_changed",
  "supervisor_session_log_failed",
  "supervisor_settings_invalid",
  "supervisor_state_corrupt",
  "supervisor_state_unreadable",
  "supervisor_state_unwritable",
  "supervisor_usage_contended",
  "supervisor_worker_disconnected",
]);
const COMPACTION_REASONS = new Set([
  ...SUPERVISOR_REASONS,
  "session_notes_changed",
  "supervisor_action_receipt_failed",
  "supervisor_harness_citation_ids_invalid",
  "supervisor_harness_final_tools_invalid",
  "supervisor_harness_owner_tools_invalid",
  "supervisor_harness_request_invalid",
  "supervisor_harness_tool_definitions_changed",
  "supervisor_harness_tool_invalid",
  "supervisor_instruction_store_failed",
  "supervisor_lifecycle_failed",
  "supervisor_note_expiry_failed",
  "supervisor_output_invalid",
  "supervisor_session_note_mark_failed",
  "supervisor_session_note_marks_invalid",
]);
const INCOMPLETE_REASONS = new Set([
  "max_output_tokens",
  "content_filter",
  "other",
]);
const PENDING_KINDS = new Set([
  "session_open",
  "resume",
  "change",
  "pre_close",
  "post_fill",
  "failure_streak",
  "decision",
  "review",
  "compaction",
  "handoff",
  "aside",
  "idle",
]);
const SUPERVISOR_STATUS: Record<string, string> = {
  idle: "Idle",
  pending: "Pending",
  parked: "Parked",
  unknown: "Unknown",
  disabled: "Disabled",
};
const SUPERVISOR_STATE: Record<string, string> = {
  present: "Current session state present",
  missing: "Current session state missing",
  other_session: "State belongs to another session",
  malformed: "Malformed state",
  unreadable: "Unreadable state",
  not_configured: "Not configured",
};
const SUPERVISOR_CONFIG: Record<string, string> = {
  configured: "Configured",
  unavailable: "Config unavailable",
  not_configured: "Not configured",
};
const SUPERVISOR_CONFIG_REASONS: Record<string, string> = {
  planner_config_unavailable: "Planner config unavailable",
  planner_usage_unavailable: "Planner usage unavailable",
  planner_usage_unavailable_recovery_required:
    "Planner usage unavailable (recovery required)",
  planner_authority_invalid: "Planner authority invalid",
  planner_event_configuration_changed: "Planner event configuration changed",
  planner_integrity_failure: "Planner integrity failure",
};
const PENDING_SOURCES: Record<string, string> = {
  missing: "Session log missing",
  no_pending: "No pending turn",
  pending: "Pending turn present",
  corrupt: "Session log corrupt",
  incomplete: "Session log incomplete",
  unreadable: "Session log unreadable",
  oversize: "Session log oversized",
  non_regular: "Session log is not a regular file",
  symlink: "Session log is a symlink",
  not_checked: "Not checked",
};
const AUTHORITATIVE_PENDING_SOURCES = new Set(["missing", "no_pending"]);
const VERIFIED_CALENDAR_SOURCES = new Set(["NSE/CMTR/71775"]);
const LIVENESS_STATUS: Record<string, string> = {
  healthy: "Healthy",
  unknown: "Unknown",
  stale: "Stale",
  future: "Future clock",
  malformed: "Malformed",
};
const ALERT_STORE_STATUS: Record<string, string> = {
  available: "Available",
  missing: "Missing",
  malformed: "Malformed",
  unreadable: "Unreadable",
  disabled: "Disabled",
};
const FAILURE_CATEGORIES: Record<string, string> = {
  rate_limited: "Rate limited",
  receiver_rejected: "Receiver rejected",
  receiver_error: "Receiver error",
  informational: "Informational response",
  redirect_or_unexpected: "Redirect or unexpected response",
  unknown: "Unknown category",
};
function sentence(value: string) {
  const text = value.replaceAll("_", " ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}
export function metric(value: unknown): string {
  return numeric(value) === null ? "Not reported" : count(value);
}
export function durationOrUnknown(value: unknown): string {
  return numeric(value) === null ? "Not reported" : duration(value);
}
export function ageLabel(value: unknown): string {
  const age = numeric(value);
  if (age === null) return "Not reported";
  if (age < 0) return "Not trusted (future clock)";
  return duration(age);
}
export function secondsLabel(value: unknown): string {
  const seconds = numeric(value);
  return seconds === null ? "Not reported" : `${seconds} s`;
}
export function bytesLabel(value: unknown): string {
  const bytes = numeric(value);
  return bytes === null ? "Not reported" : `${count(bytes)} bytes`;
}
export function yesNo(value: unknown): string {
  return value === true ? "Yes" : value === false ? "No" : "Not reported";
}
export function knownReason(value: unknown, known?: boolean): string {
  if (
    known === false ||
    typeof value !== "string" ||
    !SUPERVISOR_REASONS.has(value)
  )
    return "Unknown reason";
  return sentence(value);
}
export function compactionReason(value: unknown, known?: boolean): string {
  if (
    known === false ||
    typeof value !== "string" ||
    !COMPACTION_REASONS.has(value)
  )
    return "Unknown reason";
  return sentence(value);
}
export function incompleteReason(value: unknown): string {
  if (typeof value !== "string" || !INCOMPLETE_REASONS.has(value))
    return "Unknown reason";
  return sentence(value);
}
export function pendingKind(value: unknown): string {
  if (typeof value !== "string" || !PENDING_KINDS.has(value))
    return "Unknown kind";
  return sentence(value);
}
export function failureCategory(value: unknown): string {
  return typeof value === "string" && FAILURE_CATEGORIES[value]
    ? FAILURE_CATEGORIES[value]
    : "Unknown category";
}
export function configLabel(value: unknown): string {
  return typeof value === "string"
    ? SUPERVISOR_CONFIG[value] || "Unknown config source"
    : "Unknown config source";
}
export function configReason(value: unknown): string {
  return typeof value === "string"
    ? SUPERVISOR_CONFIG_REASONS[value] || "Unknown config failure"
    : "Unknown config failure";
}
export function configScopeLabel(value: unknown): string {
  return value === "loaded_policy_or_static_failure_code"
    ? "Loaded policy or static failure code"
    : "Not reported";
}
export function pendingSourceLabel(value: unknown): string {
  return typeof value === "string"
    ? PENDING_SOURCES[value] || "Unknown pending source"
    : "Unknown pending source";
}
export function pendingScopeLabel(value: unknown): string {
  return value === "checked_bounded_session_log"
    ? "Checked bounded session log"
    : "Not reported";
}
export function isAuthoritativePendingSource(value: unknown): boolean {
  return typeof value === "string" && AUTHORITATIVE_PENDING_SOURCES.has(value);
}
export function pendingAbsenceAuthoritative(
  supervisor?: SupervisorOperationalStatus | null,
): boolean {
  return (
    supervisor?.state === "present" &&
    supervisor?.config === "configured" &&
    isAuthoritativePendingSource(supervisor?.pending_source)
  );
}
export function supervisorStatus(
  value: unknown,
  state?: unknown,
  config?: unknown,
  pendingSource?: unknown,
): string {
  if (typeof value !== "string") return "Unknown";
  if (value === "disabled")
    return config === "unavailable" ? "Unknown" : "Disabled";
  if (config === "unavailable") return "Unknown";
  if (state !== "present" || config !== "configured") return "Unknown";
  if (value === "idle" && !isAuthoritativePendingSource(pendingSource))
    return "Unknown";
  if (value === "pending" && pendingSource !== "pending") return "Unknown";
  if (
    value === "parked" &&
    !isAuthoritativePendingSource(pendingSource) &&
    pendingSource !== "pending"
  )
    return "Unknown";
  return SUPERVISOR_STATUS[value] || "Unknown";
}
export function supervisorState(value: unknown): string {
  return typeof value === "string"
    ? SUPERVISOR_STATE[value] || "Unknown source"
    : "Unknown source";
}
export function livenessLabel(value: unknown): string {
  return typeof value === "string"
    ? LIVENESS_STATUS[value] || "Not reported"
    : "Not reported";
}
export function alertStoreLabel(value: unknown): string {
  return typeof value === "string"
    ? ALERT_STORE_STATUS[value] || "Not reported"
    : "Not reported";
}
export function coverageLabel(value: unknown): string {
  return value === true
    ? "Covered"
    : value === false
      ? "Outside coverage"
      : "Not reported";
}
export function daysRemaining(value: unknown): string {
  const days = numeric(value);
  if (days === null) return "Not reported";
  if (days < 0) return `${count(Math.abs(days))} days past coverage`;
  if (days === 0) return "Expires today";
  return `${count(days)} days`;
}
export function exchangeDate(value: unknown): string {
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(value))
    return "Not reported";
  const [year, month, day] = value.split("-").map(Number);
  const date = new Date(Date.UTC(year, month - 1, day));
  if (
    date.getUTCFullYear() !== year ||
    date.getUTCMonth() !== month - 1 ||
    date.getUTCDate() !== day
  )
    return "Not reported";
  return `${value} IST`;
}
export function calendarOutsideLabel(
  calendar?: {
    covered?: boolean | null;
    requested_session?: string | null;
    requested_session_covered?: boolean | null;
    current_operational_date?: string | null;
  } | null,
): string {
  const source =
    calendar?.covered === false
      ? calendar.current_operational_date
      : calendar?.requested_session_covered === false
        ? calendar.requested_session
        : calendar?.requested_session || calendar?.current_operational_date;
  const year =
    typeof source === "string" && /^\d{4}/.test(source)
      ? source.slice(0, 4)
      : null;
  return year ? `No ${year} data` : "No calendar data";
}
export function isVerifiedCalendarSource(value: unknown): boolean {
  return typeof value === "string" && VERIFIED_CALENDAR_SOURCES.has(value);
}
export function calendarSource(value: unknown): string {
  if (isVerifiedCalendarSource(value)) return value as string;
  return value == null ? "Not reported" : "Not verified";
}
export function countsScopeLabel(value: unknown): string {
  return value === "host_alert_outbox_not_account_filtered"
    ? "Host alert outbox; not filtered per account"
    : "Not reported";
}
export function outboxScopeLabel(value: unknown): string {
  return value === "current_host_outbox_not_delivery_window"
    ? "Current host outbox; not the retained delivery window"
    : "Not reported";
}
export function outboxRowScopeLabel(value: unknown): string {
  return value === "current_outbox_rows_not_retained_delivery_window"
    ? "Current host outbox rows"
    : "Not reported";
}
export function outboxDeadScopeLabel(value: unknown): string {
  return value === "current_outbox_quarantined_rows_not_delivery_window"
    ? "Current host outbox quarantined rows"
    : "Not reported";
}
export function retainedWindowScopeLabel(value: unknown): string {
  return value === "retained_rows_within_window_lower_bound_not_lifetime"
    ? "Retained delivery window; lower bound, not lifetime"
    : "Not reported";
}
export function timestampScopeLabel(value: unknown): string {
  return value === "retained_window_only_not_lifetime"
    ? "Retained window only; not lifetime"
    : "Not reported";
}
export type FreshnessState = "unknown" | "future" | "recent" | "aging" | "old";
export interface Freshness {
  state: FreshnessState;
  age_ms: number | null;
  label: string;
  detail: string;
}
export function validTimestampMs(value: unknown): number | null {
  const ms = numeric(value);
  if (
    ms === null ||
    !Number.isFinite(ms) ||
    !Number.isInteger(ms) ||
    ms <= 0 ||
    ms > 8_640_000_000_000_000
  )
    return null;
  return ms;
}
export function freshness(captured: unknown, now: unknown): Freshness {
  const at = validTimestampMs(captured);
  const current = validTimestampMs(now);
  if (at === null || current === null)
    return {
      state: "unknown",
      age_ms: null,
      label: "Not reported",
      detail: "No published capture timestamp.",
    };
  const age = current - at;
  if (age < 0)
    return {
      state: "future",
      age_ms: age,
      label: "Clock anomaly",
      detail:
        "Published capture is later than this browser clock; age is not trusted.",
    };
  const state: FreshnessState =
    age <= 60_000 ? "recent" : age <= 360_000 ? "aging" : "old";
  return {
    state,
    age_ms: age,
    label:
      state === "recent"
        ? "Recent capture"
        : state === "aging"
          ? "Aging capture"
          : "Old capture",
    detail: `${duration(age)} before this render; server as-of unchanged.`,
  };
}
export function operationalHealthState(
  value?: unknown,
): "missing" | "unsupported" | "present" {
  if (value === null || value === undefined) return "missing";
  if (typeof value !== "object" || Array.isArray(value)) return "unsupported";
  if ((value as { version?: unknown }).version !== 1) return "unsupported";
  return "present";
}
export function operationalHealthPresence(
  summary?: { operational_health?: unknown } | null,
  health?: { operational_health?: unknown } | null,
): unknown {
  return summary?.operational_health ?? health?.operational_health;
}
export function operationalHealthFrom(
  summary?: { operational_health?: OperationalHealth | null } | null,
  health?: { operational_health?: OperationalHealth | null } | null,
): OperationalHealth | null {
  const value = operationalHealthPresence(summary, health);
  return operationalHealthState(value) === "present"
    ? (value as OperationalHealth)
    : null;
}
export function controllerLivenessFrom(
  health?: { controller_liveness?: ControllerLiveness | null } | null,
): ControllerLiveness | null {
  const value = health?.controller_liveness;
  return value && typeof value === "object" ? value : null;
}
export function lastCompletedAt(
  liveness?: ControllerLiveness | null,
  operational?: OperationalHealth | null,
  interpret = true,
): number | null {
  if (
    liveness &&
    Object.prototype.hasOwnProperty.call(liveness, "last_completed_at_ms")
  )
    return validTimestampMs(liveness.last_completed_at_ms);
  return interpret ? validTimestampMs(operational?.captured_at_ms) : null;
}
