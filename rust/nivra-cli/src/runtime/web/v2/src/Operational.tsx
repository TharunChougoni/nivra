import { useEffect, useState } from "react";
import type {
  AlertTransportStatus,
  CalendarCoverage,
  ControllerLiveness,
  HealthResponse,
  OperationalHealth,
  PortfolioResponse,
  SummaryResponse,
} from "./types";
import { amount, count, datetime, projectedStatus } from "./data";
import { plain } from "./labels";
import { Empty, Facts, KpiStrip, Notice, Panel, Status, Table } from "./ui";
import {
  ageLabel,
  alertStoreLabel,
  calendarOutsideLabel,
  calendarSource,
  controllerLivenessFrom,
  coverageLabel,
  daysRemaining,
  durationOrUnknown,
  exchangeDate,
  failureCategory,
  freshness,
  isVerifiedCalendarSource,
  lastCompletedAt,
  livenessLabel,
  metric,
  operationalHealthPresence,
  operationalHealthState,
  secondsLabel,
  validTimestampMs,
  yesNo,
} from "./operational";

function useNow() {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  return now;
}
function bookState(
  summary?: SummaryResponse | null,
  portfolio?: PortfolioResponse | null,
) {
  switch (projectedStatus(portfolio, summary)) {
    case "stopped":
      return "Stopped";
    case "halted":
      return "Halted";
    case "closed":
      return "Closed";
    case "open_snapshot":
      return "Open snapshot";
    default:
      return "Not reported";
  }
}
function PaperFinancialPanel({
  summary,
  portfolio,
}: {
  summary?: SummaryResponse | null;
  portfolio?: PortfolioResponse | null;
}) {
  return (
    <Panel title="Paper finances">
      <KpiStrip
        items={[
          ["Capital", amount(summary?.capital ?? portfolio?.capital)],
          ["Cash", amount(summary?.cash ?? portfolio?.cash)],
          ["Equity", amount(summary?.equity ?? portfolio?.equity)],
        ]}
      />
      <Facts items={[["Book state", bookState(summary, portfolio)]]} />
    </Panel>
  );
}
function ControllerLivenessPanel({
  liveness,
  operational,
  loopAt,
  now,
  interpret,
}: {
  liveness: ControllerLiveness | null;
  operational: OperationalHealth | null;
  loopAt: unknown;
  now: number;
  interpret: boolean;
}) {
  const captured = interpret
    ? validTimestampMs(operational?.captured_at_ms)
    : null;
  const fresh = freshness(captured, now);
  // Health is read again only when its content changes, so its loop time can
  // lag. The summary's loop time refreshes at least every 5 s; age it here.
  const reported = lastCompletedAt(liveness, operational, interpret),
    summaryAt = validTimestampMs(loopAt),
    newer = reported !== null && summaryAt !== null && summaryAt > reported,
    completed = newer ? summaryAt : reported;
  const completedFuture = completed !== null && completed > now;
  return (
    <Panel title="Controller">
      <Facts
        items={[
          ["Status", livenessLabel(liveness?.status)],
          ["Last loop finished", datetime(completed)],
          [
            "Loop age",
            completedFuture
              ? "Not trusted (future clock)"
              : ageLabel(
                  newer && summaryAt !== null
                    ? now - summaryAt
                    : liveness?.age_ms,
                ),
          ],
          [
            "Next loop due",
            datetime(validTimestampMs(liveness?.expected_deadline_ms)),
          ],
          ["Max loop age", durationOrUnknown(liveness?.max_age_ms)],
          ["Loop count", metric(liveness?.loop_generation)],
          ["Snapshot taken", interpret ? datetime(captured) : "Not shown"],
          ["Snapshot age", interpret ? fresh.detail : "Not shown"],
          [
            "Snapshot freshness",
            interpret ? (
              <Status
                value={
                  fresh.state === "recent"
                    ? "ready"
                    : fresh.state === "future"
                      ? "stale"
                      : "unknown"
                }
              >
                {fresh.label}
              </Status>
            ) : (
              "Not shown"
            ),
          ],
        ]}
      />
      {!interpret && (
        <Notice>Health format not supported. Values are hidden.</Notice>
      )}
      {completedFuture && (
        <Notice error>
          Last loop time is ahead of this browser's clock. Age is not trusted.
        </Notice>
      )}
      {fresh.state === "future" && (
        <Notice error>
          Snapshot time is ahead of this browser's clock. Age is unknown.
        </Notice>
      )}
    </Panel>
  );
}
function ResearchPanel({ summary }: { summary?: SummaryResponse | null }) {
  const library = summary?.research_library;
  const activation = library?.activation_authorized as unknown;
  const candidates = Array.isArray(library?.items)
    ? library.items.length
    : null;
  return (
    <Panel title="Research">
      <Facts
        items={[
          ["Library", plain(library?.status)],
          [
            "Candidates",
            candidates === null ? "Not reported" : count(candidates),
          ],
          ["Library cut off", yesNo(library?.truncated)],
          [
            "Activation",
            activation === false
              ? "Not authorized"
              : activation === true
                ? "Reported authorized; verify independently"
                : "Not reported",
          ],
          ["Model", plain(summary?.model_status)],
          ["Entries", plain(summary?.entry_status)],
          ["Readiness", plain(summary?.readiness?.status)],
        ]}
      />
    </Panel>
  );
}
function CalendarPanel({
  calendar,
  interpret,
}: {
  calendar?: CalendarCoverage | null;
  interpret: boolean;
}) {
  if (!interpret || !calendar)
    return (
      <Panel title="Calendar">
        <Empty title="Calendar unavailable" />
      </Panel>
    );
  const outside =
    calendar.covered === false || calendar.requested_session_covered === false;
  const verified = isVerifiedCalendarSource(calendar.source);
  const warningDays =
    calendar.expiry_warning_days == null
      ? null
      : metric(calendar.expiry_warning_days);
  return (
    <Panel title="Calendar">
      <Facts
        items={[
          [
            "Source",
            verified
              ? `${calendarSource(calendar.source)} (verified)`
              : calendarSource(calendar.source),
          ],
          [
            "Covered range",
            `${exchangeDate(calendar.covered_from)} to ${exchangeDate(calendar.covered_to)}`,
          ],
          [
            "Today",
            `${exchangeDate(calendar.current_operational_date)} · ${coverageLabel(calendar.covered)}`,
          ],
          [
            "Requested session",
            `${exchangeDate(calendar.requested_session)} · ${coverageLabel(calendar.requested_session_covered)}`,
          ],
          [
            "Historical session",
            `${exchangeDate(calendar.historical_session)} · ${coverageLabel(calendar.historical_session_covered)}`,
          ],
          ["Days left", daysRemaining(calendar.days_remaining)],
          [
            "Expiring",
            calendar.expiring === true
              ? warningDays === null
                ? "Yes"
                : `Yes (warns ${warningDays} days ahead)`
              : yesNo(calendar.expiring),
          ],
          ["Expired", yesNo(calendar.expired)],
        ]}
      />
      {outside && (
        <Notice error>
          {calendarOutsideLabel(calendar)}: outside{" "}
          {verified ? "verified" : "reported"} coverage for{" "}
          {calendar.covered === false ? "today" : "the requested session"}.
        </Notice>
      )}
      {calendar.expiring === true && (
        <Notice>
          Calendar coverage ends in {daysRemaining(calendar.days_remaining)}.
        </Notice>
      )}
      {calendar.expired === true && (
        <Notice error>Calendar coverage has expired. Update it.</Notice>
      )}
    </Panel>
  );
}
function AlertPanel({
  alert,
  interpret,
}: {
  alert?: AlertTransportStatus | null;
  interpret: boolean;
}) {
  if (!interpret || !alert)
    return (
      <Panel title="Alert delivery">
        <Empty title="Alert delivery unavailable" />
      </Panel>
    );
  const counts = alert.counts;
  const retained = alert.retained_window;
  const countValue = (value: unknown) =>
    counts ? metric(value) : "Counts unavailable";
  const lastAccepted = validTimestampMs(alert.last_accepted_at_ms);
  const lastFailure = validTimestampMs(alert.last_failure_at_ms);
  const windowStart = validTimestampMs(retained?.start_ms);
  const windowEnd = validTimestampMs(retained?.end_ms);
  return (
    <Panel title="Alert delivery">
      <Facts
        items={[
          ["Status", alertStoreLabel(alert.status)],
          ["Queued now", countValue(counts?.queued)],
          ["Retrying now", countValue(counts?.retrying)],
          ["Dead now", countValue(counts?.dead)],
          ["Accepted by receiver", countValue(counts?.transport_accepted)],
          [
            "Errors or in flight",
            countValue(counts?.in_flight_or_transport_error),
          ],
          [
            "Dropped",
            counts
              ? counts.dropped == null
                ? "Not recorded"
                : metric(counts.dropped)
              : "Counts unavailable",
          ],
          [
            "Window",
            retained
              ? `${secondsLabel(alert.retention_window_seconds)} · ${datetime(windowStart)} to ${datetime(windowEnd)}`
              : "Not reported",
          ],
          [
            "Last accepted",
            lastAccepted != null
              ? datetime(lastAccepted)
              : alert.last_accepted_at_ms == null
                ? "None in window"
                : "Not reported",
          ],
          [
            "Last failure",
            lastFailure != null
              ? `${datetime(lastFailure)} · ${failureCategory(alert.last_failure_category)}`
              : alert.last_failure_at_ms == null
                ? "None in window"
                : "Not reported",
          ],
          [
            "Failure marker",
            alert.failure_marker_present === true
              ? "Present"
              : alert.failure_marker_present === false
                ? "Not present"
                : "Not reported",
          ],
          [
            "Delivery guarantee",
            alert.delivery_guarantee === false
              ? "None"
              : yesNo(alert.delivery_guarantee),
          ],
        ]}
      />
      {counts == null && <Notice>Alert counts unavailable.</Notice>}
      {retained?.clock_anomaly === true && (
        <Notice error>
          Some delivery rows are dated in the future. Times and counts may be
          wrong.
        </Notice>
      )}
    </Panel>
  );
}
export function BookContinuity({
  summary,
  portfolio,
}: {
  summary?: SummaryResponse | null;
  portfolio?: PortfolioResponse | null;
}) {
  const state = bookState(summary, portfolio);
  const stopped = state === "Stopped" || state === "Halted";
  return (
    <Panel title="Paper account">
      <Facts
        className="facts-strip"
        items={[
          ["Book state", state],
          ["Capital", amount(summary?.capital ?? portfolio?.capital)],
        ]}
      />
      {stopped && (
        <Notice error>The paper book is {state.toLowerCase()}.</Notice>
      )}
    </Panel>
  );
}
export function OperationalPanel({
  summary,
  portfolio,
  health,
}: {
  summary?: SummaryResponse | null;
  portfolio?: PortfolioResponse | null;
  health?: HealthResponse | null;
}) {
  const now = useNow();
  const raw = operationalHealthPresence(summary, health);
  const version = operationalHealthState(raw);
  const interpret = version === "present";
  const operational = interpret ? (raw as OperationalHealth) : null;
  const liveness = controllerLivenessFrom(health);
  const rawObject =
    raw !== null && typeof raw === "object" && !Array.isArray(raw);
  const rawVersion = rawObject
    ? (raw as { version?: unknown }).version
    : undefined;
  const versionMissing = rawObject && rawVersion == null;
  const versionMalformed =
    rawObject && rawVersion != null && typeof rawVersion !== "number";
  const versionText =
    typeof rawVersion === "number" && Number.isFinite(rawVersion)
      ? String(rawVersion)
      : typeof rawVersion === "string" && /^\d+$/.test(rawVersion)
        ? rawVersion
        : "not reported";
  return (
    <Panel title="Status checks">
      {version === "missing" && (
        <Notice>
          Status checks are missing from this snapshot. Unknown stays unknown.
        </Notice>
      )}
      {version === "unsupported" && (
        <Notice error>
          {rawObject
            ? versionMissing
              ? "Status check format is not reported. Values are hidden."
              : versionMalformed
                ? "Status check format is malformed. Values are hidden."
                : `Status check format ${versionText} is not supported. Values are hidden.`
            : "Status checks are malformed. Values are hidden."}
        </Notice>
      )}
      <div className="cols-2">
        <PaperFinancialPanel summary={summary} portfolio={portfolio} />
        <ControllerLivenessPanel
          liveness={liveness}
          operational={operational}
          loopAt={summary?.as_of_ms}
          now={now}
          interpret={interpret}
        />
      </div>
      <ResearchPanel summary={summary} />
      <CalendarPanel calendar={operational?.calendar} interpret={interpret} />
      <AlertPanel alert={operational?.alert_transport} interpret={interpret} />
    </Panel>
  );
}
