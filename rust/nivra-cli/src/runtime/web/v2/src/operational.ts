import { count, duration, numeric } from "./data";
import type { ControllerLiveness, OperationalHealth } from "./types";

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
const VERIFIED_CALENDAR_SOURCES = new Set(["NSE/CMTR/71775"]);
const LIVENESS_STATUS: Record<string, string> = {
  healthy: "Healthy",
  unknown: "Unknown",
  stale: "Stale",
  future: "Future clock",
  malformed: "Malformed",
};
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
export function yesNo(value: unknown): string {
  return value === true ? "Yes" : value === false ? "No" : "Not reported";
}
export function failureCategory(value: unknown): string {
  return typeof value === "string" && FAILURE_CATEGORIES[value]
    ? FAILURE_CATEGORIES[value]
    : "Unknown category";
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
      detail: "Not reported",
    };
  const age = current - at;
  if (age < 0)
    return {
      state: "future",
      age_ms: age,
      label: "Clock anomaly",
      detail: "Not trusted (future clock)",
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
    detail: `${duration(age)} old`,
  };
}
export function operationalHealthState(
  value?: unknown,
): "missing" | "unsupported" | "present" {
  if (value === null || value === undefined) return "missing";
  if (typeof value !== "object" || Array.isArray(value)) return "unsupported";
  if ((value as { version?: unknown }).version !== 2) return "unsupported";
  return "present";
}
export function operationalHealthPresence(
  summary?: { operational_health?: unknown } | null,
  health?: { operational_health?: unknown } | null,
): unknown {
  return summary?.operational_health ?? health?.operational_health;
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
