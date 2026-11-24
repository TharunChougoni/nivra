import { useEffect, useState } from "react";
import type {
  AlertTransportStatus,
  CalendarCoverage,
  ControllerLiveness,
  HealthResponse,
  ObservationStatus,
  OperationalHealth,
  PortfolioResponse,
  SummaryResponse,
  SupervisorOperationalStatus,
} from "./types";
import { amount, count, datetime, projectedStatus, words } from "./data";
import { Empty, Facts, MetricStrip, Notice, Panel, Status, Table } from "./ui";
import {
  actualScoring,
  captureBudget,
  captureCoverageLabel,
  captureEligibilityLabel,
  captureEligibleForAttempt,
  captureOperationalCaps,
  captureReason,
  captureStatusLabel,
  cohortStatus,
  continuationId,
  continuationQuoteClock,
  continuationQuoteHash,
  continuationQuotePurpose,
  continuationReceiptHash,
  continuationRecordingSession,
  continuationSourceSession,
  continuationStateLabel,
  episodeCapital,
  episodeEntryLabel,
  episodePnl,
  exitStatusLabel,
  lifecycleStateLabel,
  liveContextHash,
  measurementEpisodeCount,
  measurementHash,
  measurementOriginalStatus,
  measurementScope,
  measurementStatusLabel,
  measurementTimeline,
  measurementWindow,
  milestoneGapReasons,
  milestoneMeasurementGapReasons,
  milestoneWindow,
  observationAge,
  observationCohortKey,
  observationEvaluationScope,
  observationRefsStatusLabel,
  observationSourceLabel,
  observationState,
  observationStatusLabel,
  originalMeasurementStatusLabel,
  ownerResearchFrom,
  ownerResearchVersionState,
  qualificationLabel,
  replayAxes,
  researchConfig,
  researchConfigReason,
  researchEvidenceId,
  researchModelCall,
  researchPendingKind,
  researchPendingSource,
  researchReason,
  researchStateLabel,
  researchWorkspaceSnapshot,
  safeHash,
  safeSession,
  scientificStateLabel,
  sourceRefStatusLabel,
  trialFamilyKeyNote,
  trialGapReason,
  trialLowerBound,
  trialMainCount,
  trialReset,
  trialStatusLabel,
  validObservationTimestamp,
  workspaceRefCount,
} from "./research";
import {
  ageLabel,
  alertStoreLabel,
  bytesLabel,
  calendarOutsideLabel,
  calendarSource,
  compactionReason,
  configLabel,
  configReason,
  configScopeLabel,
  controllerLivenessFrom,
  countsScopeLabel,
  coverageLabel,
  daysRemaining,
  durationOrUnknown,
  exchangeDate,
  failureCategory,
  freshness,
  incompleteReason,
  isVerifiedCalendarSource,
  knownReason,
  lastCompletedAt,
  livenessLabel,
  metric,
  operationalHealthPresence,
  operationalHealthState,
  outboxDeadScopeLabel,
  outboxRowScopeLabel,
  outboxScopeLabel,
  pendingAbsenceAuthoritative,
  pendingKind,
  pendingScopeLabel,
  pendingSourceLabel,
  retainedWindowScopeLabel,
  secondsLabel,
  supervisorState,
  supervisorStatus,
  timestampScopeLabel,
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
function modeLabel(summary?: SummaryResponse | null) {
  if (summary?.live_enabled === true || summary?.mode === "live") return "LIVE";
  if (summary?.mode === "paper" || summary?.paper_only === true) return "PAPER";
  return "Not reported";
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
    <Panel
      title="Paper financial status"
      note="Published paper book values. This is not a P&L, profitability or operating qualification."
    >
      <MetricStrip
        items={[
          [
            "Capital",
            amount(summary?.capital ?? portfolio?.capital),
            "Published paper allocation",
          ],
          [
            "Cash",
            amount(summary?.cash ?? portfolio?.cash),
            "Published paper cash",
          ],
          [
            "Equity",
            amount(summary?.equity ?? portfolio?.equity),
            "Published paper equity",
          ],
        ]}
      />
      <Facts
        items={[
          ["Mode", modeLabel(summary)],
          ["Paper only", yesNo(summary?.paper_only)],
          [
            "Live activation",
            summary?.live_enabled === true
              ? "Enabled"
              : summary?.live_enabled === false
                ? "Disabled"
                : "Not reported",
          ],
          ["Book state", bookState(summary, portfolio)],
          [
            "Book projection",
            "Portfolio projection first; summary state fallback",
          ],
          [
            "Financial freshness gate",
            "Original quote receipt · 2 seconds (unchanged)",
          ],
          ["Profitability claim", "None; published values only"],
        ]}
      />
    </Panel>
  );
}
function ControllerLivenessPanel({
  liveness,
  operational,
  now,
  interpret,
}: {
  liveness: ControllerLiveness | null;
  operational: OperationalHealth | null;
  now: number;
  interpret: boolean;
}) {
  const captured = interpret
    ? validTimestampMs(operational?.captured_at_ms)
    : null;
  const fresh = freshness(captured, now);
  const completed = lastCompletedAt(liveness, operational, interpret);
  const completedFuture = completed !== null && completed > now;
  return (
    <Panel
      title="Controller liveness"
      note="Uses the controller's original completed-at timestamp. Browser refresh does not renew it."
    >
      <Facts
        items={[
          ["Liveness status", livenessLabel(liveness?.status)],
          ["Last completed loop (original)", datetime(completed)],
          [
            "Reported age",
            completedFuture
              ? "Not trusted (future clock)"
              : ageLabel(liveness?.age_ms),
          ],
          [
            "Expected next deadline",
            datetime(validTimestampMs(liveness?.expected_deadline_ms)),
          ],
          ["Server max age", durationOrUnknown(liveness?.max_age_ms)],
          ["Loop generation", metric(liveness?.loop_generation)],
          [
            "Published capture",
            interpret ? datetime(captured) : "Not interpreted",
          ],
          [
            "Capture age in this browser",
            interpret ? fresh.detail : "Not interpreted (unsupported version).",
          ],
          [
            "Capture freshness",
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
              "Not interpreted"
            ),
          ],
        ]}
      />
      {!interpret && (
        <Notice>
          Operational health version is not supported; published capture and
          projection fields are not interpreted.
        </Notice>
      )}
      {completedFuture && (
        <Notice error>
          Clock anomaly: last completed timestamp is later than this browser
          clock; age and freshness are not trusted.
        </Notice>
      )}
      {fresh.state === "future" && (
        <Notice error>
          Clock anomaly: published capture is later than this browser clock. Age
          and freshness are unknown.
        </Notice>
      )}
      <p className="caption">
        A stale last completed run is not evidence that the supervisor is hung.
        Only the published liveness status is used.
      </p>
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
    <Panel
      title="Research & advisory"
      note="Research library and advisory status from the same snapshot. No promotion, new endpoint or activation control is added."
    >
      <Facts
        items={[
          ["Library status", words(library?.status)],
          [
            "Recorded candidates",
            candidates === null ? "Not reported" : count(candidates),
          ],
          ["Library truncated", yesNo(library?.truncated)],
          [
            "Activation authority",
            activation === false
              ? "Not authorized"
              : activation === true
                ? "Reported authorized; verify independently"
                : "Not reported",
          ],
          ["Model status", words(summary?.model_status)],
          ["Entry status", words(summary?.entry_status)],
          ["Readiness", words(summary?.readiness?.status)],
          ["Cross-session visualization", "See Cross-session continuity below"],
        ]}
      />
    </Panel>
  );
}
function SupervisorPanel({
  supervisor,
  interpret,
}: {
  supervisor?: SupervisorOperationalStatus | null;
  interpret: boolean;
}) {
  if (!interpret || !supervisor)
    return (
      <Panel title="Supervisor">
        <Empty title="Supervisor state unavailable">
          The snapshot did not include a readable supervisor operational
          projection. No idle, healthy or zero state is inferred.
        </Empty>
      </Panel>
    );
  const state = supervisor.state;
  const statePresent = state === "present";
  const config = supervisor.config;
  const configUnavailable = config === "unavailable";
  const configNotConfigured = config === "not_configured";
  const status = supervisorStatus(
    supervisor.status,
    state,
    config,
    supervisor.pending_source,
  );
  const pending = supervisor.pending;
  const pendingSource = supervisor.pending_source;
  const pendingSourceKnown =
    pendingSource === "missing" ||
    pendingSource === "no_pending" ||
    pendingSource === "pending";
  const pendingSourceUnavailable = !pendingSourceKnown;
  const pendingAuthoritative = pendingAbsenceAuthoritative(supervisor);
  const pendingKindValue = pending
    ? pendingKind(pending.kind)
    : pendingAuthoritative
      ? "No pending turn reported"
      : "Unknown (source not authoritative)";
  const parked = supervisor.parked;
  const budget = supervisor.budget;
  const context = supervisor.context;
  const compaction = supervisor.compaction;
  const backoff = supervisor.backoff;
  const lastRun = supervisor.last_completed_run;
  const configuredCalls = budget?.configured_daily_calls;
  const configuredCallsValue = configUnavailable
    ? configuredCalls == null
      ? "Unknown (no static fallback)"
      : `Unknown (static fallback reports ${metric(configuredCalls)})`
    : configNotConfigured
      ? "Not reported"
      : metric(configuredCalls);
  const enabledValue = configUnavailable
    ? "Unknown (static fallback)"
    : configNotConfigured
      ? "Not reported"
      : yesNo(supervisor.enabled);
  const exhaustedValue = configUnavailable
    ? "Unknown (static fallback)"
    : yesNo(budget?.exhausted);
  return (
    <Panel
      title="Supervisor"
      note="Current-session supervisor state only. Missing, other-session or malformed state is not treated as idle or healthy."
    >
      {state !== "present" && supervisor.status !== "disabled" && (
        <Notice error>
          Supervisor state is {supervisorState(state).toLowerCase()}; it is not
          treated as idle or healthy. Published status: {status}.
        </Notice>
      )}
      {configUnavailable && (
        <Notice error>
          Planner policy is configured but unavailable:{" "}
          {configReason(supervisor.config_reason)}. Static fallback caps are not
          the loaded policy; enabled state and configured daily calls are
          unknown, not a disabled policy or zero. Published status: {status}.
        </Notice>
      )}
      {configNotConfigured && (
        <Notice>
          Supervisor is not configured; this is a disabled policy, not a
          configured policy that failed to load.
        </Notice>
      )}
      {pendingSourceUnavailable && (
        <Notice error>
          Pending source is {pendingSourceLabel(pendingSource)}; absence of a
          pending turn is not established. This is not treated as idle.
        </Notice>
      )}
      {pending && pendingSourceUnavailable && (
        <Notice>
          A checked pending turn is exposed as evidence only; the unavailable
          source still makes the status non-authoritative.
        </Notice>
      )}
      {pendingSource === "pending" && !pending && (
        <Notice>
          Pending source reports a pending turn, but no pending evidence was
          exposed.
        </Notice>
      )}
      <div className="cols-2">
        <Facts
          items={[
            ["Status", status],
            ["Config source", configLabel(config)],
            [
              "Config failure",
              configUnavailable
                ? configReason(supervisor.config_reason)
                : "Not reported",
            ],
            ["Config scope", configScopeLabel(supervisor.config_scope)],
            ["State source", supervisorState(state)],
            ["Enabled", enabledValue],
            ["Pending source", pendingSourceLabel(pendingSource)],
            ["Pending scope", pendingScopeLabel(supervisor.pending_scope)],
            ["Pending kind", pendingKindValue],
            [
              "Pending started",
              pending
                ? datetime(validTimestampMs(pending.started_at_ms))
                : "Not reported",
            ],
            [
              "Pending age",
              pending ? ageLabel(pending.age_ms) : "Not reported",
            ],
            [
              "Parked reason",
              parked
                ? knownReason(parked.reason, parked.reason_known)
                : statePresent
                  ? "Not parked in current-session state"
                  : "Not reported",
            ],
            [
              "Parked at",
              parked
                ? datetime(validTimestampMs(parked.at_ms))
                : "Not reported",
            ],
            [
              "Budget at park",
              parked ? metric(parked.budget_at_park) : "Not reported",
            ],
            ["Configured daily calls", configuredCallsValue],
            ["Charged calls", metric(budget?.charged_calls)],
            ["Charged calls scope", "Provider usage projection, not turn runs"],
            ["Exhausted", exhaustedValue],
            [
              "Exhausted scope",
              "Authoritative current-session parked state only",
            ],
          ]}
        />
        <Facts
          items={[
            [
              "Context reason",
              context
                ? knownReason(context.reason, context.reason_known)
                : statePresent
                  ? "No context-exhaustion record"
                  : "Not reported",
            ],
            [
              "Incomplete reason",
              context
                ? incompleteReason(context.incomplete_reason)
                : "Not reported",
            ],
            [
              "Context request / max bytes",
              context
                ? `${bytesLabel(context.request_bytes)} / ${bytesLabel(context.max_request_bytes)}`
                : "Not reported",
            ],
            [
              "Context at",
              context
                ? datetime(validTimestampMs(context.at_ms))
                : "Not reported",
            ],
            [
              "Context retries",
              context ? metric(context.retries) : "Not reported",
            ],
            [
              "Compaction reason",
              compaction
                ? compactionReason(compaction.reason, compaction.reason_known)
                : statePresent
                  ? "No compaction-failure record"
                  : "Not reported",
            ],
            [
              "Compaction at",
              compaction
                ? datetime(validTimestampMs(compaction.at_ms))
                : "Not reported",
            ],
            ["Backoff configured", durationOrUnknown(backoff?.configured_ms)],
            ["Backoff retry limit", metric(backoff?.retry_limit)],
            ["Backoff enforced", yesNo(backoff?.enforced)],
            [
              "Backoff enforced scope",
              "Current-session context exhaustion only",
            ],
            ["Backoff active", yesNo(backoff?.active)],
            [
              "Backoff active scope",
              "Authoritative current-session state only",
            ],
            [
              "Earliest retry",
              validTimestampMs(backoff?.earliest_retry_at_ms) != null
                ? datetime(validTimestampMs(backoff?.earliest_retry_at_ms))
                : "Not provided",
            ],
            [
              "Last completed run",
              lastRun
                ? datetime(validTimestampMs(lastRun.at_ms))
                : "Not recorded in current-session state",
            ],
            [
              "Last run age",
              lastRun ? ageLabel(lastRun.age_ms) : "Not reported",
            ],
            ["Last run count", lastRun ? metric(lastRun.runs) : "Not reported"],
          ]}
        />
      </div>
      <p className="caption">
        A stale last completed run is not evidence that the supervisor is hung.
        Only the published status is used.
      </p>
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
      <Panel title="Calendar coverage">
        <Empty title="Calendar coverage unavailable">
          The snapshot did not include verified NSE calendar coverage.
        </Empty>
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
    <Panel
      title="Calendar coverage"
      note={
        verified
          ? "Verified NSE calendar source. Dates are exchange dates (IST); no holiday is guessed outside coverage."
          : "Calendar source is not verified. Dates are shown as reported but are not treated as verified; no holiday is guessed outside coverage."
      }
    >
      <Facts
        items={[
          ["Source", calendarSource(calendar.source)],
          [
            "Source verification",
            verified ? "Verified NSE calendar source" : "Not verified",
          ],
          [
            "Covered range",
            `${exchangeDate(calendar.covered_from)} to ${exchangeDate(calendar.covered_to)}`,
          ],
          [
            "Current operational date",
            exchangeDate(calendar.current_operational_date),
          ],
          ["Current coverage", coverageLabel(calendar.covered)],
          ["Requested session", exchangeDate(calendar.requested_session)],
          [
            "Requested coverage",
            coverageLabel(calendar.requested_session_covered),
          ],
          ["Historical session", exchangeDate(calendar.historical_session)],
          [
            "Historical coverage",
            coverageLabel(calendar.historical_session_covered),
          ],
          ["Days remaining", daysRemaining(calendar.days_remaining)],
          [
            "Expiring",
            calendar.expiring === true
              ? warningDays === null
                ? "Yes · operator-default warning (days not reported)"
                : `Yes · ${warningDays}-day operator default warning`
              : yesNo(calendar.expiring),
          ],
          ["Expired", yesNo(calendar.expired)],
          [
            "Warning basis",
            "Operator-chosen default; not an exchange guarantee",
          ],
          ["Update prerequisite", "Verified exchange calendar update required"],
        ]}
      />
      {outside && (
        <Notice error>
          {calendarOutsideLabel(calendar)} — outside{" "}
          {verified ? "verified NSE" : "reported"} coverage for{" "}
          {calendar.covered === false
            ? "the current operational date"
            : "the requested session"}
          ; no holiday is guessed.
        </Notice>
      )}
      {calendar.expiring === true && (
        <Notice>
          Calendar coverage expires in {daysRemaining(calendar.days_remaining)}.
          {warningDays === null
            ? " The operator-default warning is not an exchange guarantee."
            : ` The ${warningDays}-day warning is an operator default, not an exchange guarantee.`}
        </Notice>
      )}
      {calendar.expired === true && (
        <Notice error>
          Calendar coverage has expired. A verified exchange calendar update is
          required.
        </Notice>
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
      <Panel title="Alert transport">
        <Empty title="Alert transport unavailable">
          The snapshot did not include a host alert outbox projection.
        </Empty>
      </Panel>
    );
  const counts = alert.counts;
  const retained = alert.retained_window;
  const outboxCap =
    counts && counts.outbox_max_rows != null
      ? `${metric(counts.outbox_max_rows)} rows max`
      : "Not reported";
  const retainedCap =
    retained && retained.max_retained_rows != null
      ? `${metric(retained.max_retained_rows)} rows max`
      : "Not reported";
  const countValue = (value: unknown, scope: string) =>
    counts ? `${metric(value)} · ${scope}` : "Counts unavailable";
  const lastAccepted = validTimestampMs(alert.last_accepted_at_ms);
  const lastFailure = validTimestampMs(alert.last_failure_at_ms);
  return (
    <Panel
      title="Alert transport"
      note="Host alert outbox. Queued/retrying/dead are current host-outbox rows; accepted counts and delivery timestamps are the retained window. Neither is lifetime history."
    >
      <Facts
        items={[
          ["Store", "Host alert outbox"],
          ["Status", alertStoreLabel(alert.status)],
          ["Counts scope", countsScopeLabel(alert.counts_scope)],
          [
            "Outbox scope",
            counts ? outboxScopeLabel(counts.outbox_scope) : "Not reported",
          ],
          ["Outbox row cap", outboxCap],
          [
            "Queued (current outbox)",
            countValue(
              counts?.queued,
              outboxRowScopeLabel(counts?.queued_scope),
            ),
          ],
          [
            "Retrying (current outbox)",
            countValue(
              counts?.retrying,
              outboxRowScopeLabel(counts?.retrying_scope),
            ),
          ],
          [
            "Dead / quarantined (current outbox)",
            countValue(counts?.dead, outboxDeadScopeLabel(counts?.dead_scope)),
          ],
          [
            "Sender 2xx ACKs (retained window)",
            countValue(
              counts?.transport_accepted,
              timestampScopeLabel(counts?.transport_accepted_scope),
            ),
          ],
          [
            "ACK meaning",
            "Sender HTTP 2xx receipt; not a human read or delivery guarantee.",
          ],
          [
            "In-flight or transport error (retained window)",
            countValue(
              counts?.in_flight_or_transport_error,
              timestampScopeLabel(counts?.in_flight_or_transport_error_scope),
            ),
          ],
          [
            "Status 0 meaning",
            "Ambiguous in-flight or transport error; not a confirmed failure.",
          ],
          [
            "Dropped",
            counts
              ? counts.dropped == null
                ? "Not recorded (unknown, not zero)"
                : metric(counts.dropped)
              : "Counts unavailable",
          ],
          [
            "Retention window",
            alert.retention_window_seconds == null
              ? "Not reported"
              : `${secondsLabel(alert.retention_window_seconds)} retained`,
          ],
          ["Retained row cap", retainedCap],
          [
            "Retained window scope",
            retained
              ? retainedWindowScopeLabel(retained.scope)
              : "Not reported",
          ],
          [
            "Rows in retained window",
            retained ? metric(retained.rows_in_window) : "Not reported",
          ],
          [
            "Retained delivery rows",
            retained ? metric(retained.retained_delivery_rows) : "Not reported",
          ],
          [
            "Retained window start",
            retained
              ? datetime(validTimestampMs(retained.start_ms))
              : "Not reported",
          ],
          [
            "Retained window end",
            retained
              ? datetime(validTimestampMs(retained.end_ms))
              : "Not reported",
          ],
          ["Truncation", "Unknown cap or lazy pruning"],
          ["Dead definition", "Quarantined outbox rows only."],
          [
            "Last accepted",
            lastAccepted != null
              ? `${datetime(lastAccepted)} (retained window)`
              : alert.last_accepted_at_ms == null
                ? "Not recorded in retained window"
                : "Not reported (retained window)",
          ],
          [
            "Last failure",
            lastFailure != null
              ? `${datetime(lastFailure)} (retained window) · ${failureCategory(alert.last_failure_category)}`
              : alert.last_failure_at_ms == null
                ? "Not recorded in retained window"
                : "Not reported (retained window)",
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
            "Marker meaning",
            "Marker presence only; not proof of an attempt or its outcome.",
          ],
          [
            "Delivery guarantee",
            alert.delivery_guarantee === false
              ? "No delivery guarantee"
              : yesNo(alert.delivery_guarantee),
          ],
        ]}
      />
      {counts == null && (
        <Notice>
          Alert counts are unavailable for this store state; they are not
          treated as zero.
        </Notice>
      )}
      {retained?.clock_anomaly === true && (
        <Notice error>
          Clock anomaly: future-dated delivery rows are present. Timestamps and
          counts are not trusted as current.
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
    <Panel
      title="Paper book continuity"
      note="Portfolio-first book state. The continuing paper account is unchanged; no new trading risk cap, provider switch or activation control is added."
    >
      <Facts
        items={[
          ["Book state", state],
          [
            "Continuing account",
            amount(summary?.capital ?? portfolio?.capital),
          ],
          [
            "Stopped / halted books",
            "Preserved; no restart, unhalt or activation control",
          ],
          ["Actual model actions", "Separate from virtual research studies"],
          [
            "Financial freshness gate",
            "Original quote receipt · 2 seconds (unchanged)",
          ],
        ]}
      />
      {stopped && (
        <Notice error>
          The paper book is {state.toLowerCase()}. This dashboard has no
          restart, unhalt or activation control; the original stopped or halted
          book is preserved.
        </Notice>
      )}
    </Panel>
  );
}
function OwnerResearchPanel({
  summary,
  supervisor,
  interpret,
}: {
  summary?: SummaryResponse | null;
  supervisor?: SupervisorOperationalStatus | null;
  interpret: boolean;
}) {
  const raw = summary?.owner_research;
  const versionState = ownerResearchVersionState(raw);
  const supervisorResearch = interpret ? (supervisor?.research ?? null) : null;
  const research = ownerResearchFrom(summary, interpret ? supervisor : null);
  const rawVersion =
    raw && typeof raw === "object" && !Array.isArray(raw)
      ? (raw as { version?: unknown }).version
      : undefined;
  if (versionState === "unsupported") {
    return (
      <Panel
        title="Owner research"
        note="Evidence-driven owner research from the same supervisor owner, profile and context budget. This is not a second agent."
      >
        <Notice error>
          Owner research version{" "}
          {typeof rawVersion === "number" || typeof rawVersion === "string"
            ? String(rawVersion)
            : "not reported"}{" "}
          is not supported by this client; values are not interpreted.
        </Notice>
        <Empty title="Owner research projection not interpreted">
          No status, evidence count, cutoff or model-call claim is inferred from
          an unsupported version.
        </Empty>
      </Panel>
    );
  }
  if (!research) {
    return (
      <Panel
        title="Owner research"
        note="Evidence-driven owner research from the same supervisor owner, profile and context budget. This is not a second agent."
      >
        <Empty title="Owner research not present in this snapshot">
          This snapshot does not include the owner research projection. Evidence
          count, cutoff, workspace refs and model calls are not inferred as zero
          or Disabled.
        </Empty>
        <Facts
          items={[
            ["Research state", "Not reported"],
            ["Evidence count", "Not reported"],
            ["Model call", "Not reported"],
          ]}
        />
      </Panel>
    );
  }
  const config = supervisorResearch?.config;
  const configUnavailable = config === "unavailable";
  const configNotConfigured = config === "not_configured";
  const status = researchStateLabel(research.status);
  const asOf = validTimestampMs(research.as_of_ms);
  const future = asOf !== null && asOf > Date.now();
  const age = asOf === null ? null : Date.now() - asOf;
  const evidenceCount = research.evidence_count;
  const pendingSource =
    supervisorResearch?.pending_source ?? supervisor?.pending_source;
  const pending = supervisorResearch?.pending ?? supervisor?.pending;
  const workspaceCount = workspaceRefCount(research);
  const workspaceSnapshot = researchWorkspaceSnapshot(
    research.workspace_snapshot_sha256,
  );
  const awaitingEvidence =
    research.status === "awaiting_evidence" ||
    (evidenceCount !== null &&
      evidenceCount !== undefined &&
      evidenceCount === 0);
  return (
    <Panel
      title="Owner research"
      note="Evidence-driven owner research from the same supervisor owner, profile and context budget. This is not a second agent and never authorizes a model call, order or activation."
    >
      {configUnavailable && (
        <Notice error>
          Research policy is configured but unavailable:{" "}
          {researchConfigReason(supervisorResearch?.config_reason)}. This is not
          Disabled and no fallback policy is assumed.
        </Notice>
      )}
      {configNotConfigured && (
        <Notice>
          Research is not configured; this is an explicit disabled policy, not a
          configured policy that failed to load.
        </Notice>
      )}
      {future && (
        <Notice error>
          Clock anomaly: research source as-of is later than this browser clock;
          age is not trusted and the source time is not renewed.
        </Notice>
      )}
      {awaitingEvidence && (
        <Notice>
          No eligible evidence is present; no model call was made by this
          projection. No progress or completion is claimed.
        </Notice>
      )}
      <Facts
        items={[
          ["Research state", status],
          ["Research reason", researchReason(research.reason, true)],
          ["Research due", yesNo(research.research_due)],
          ["State available", yesNo(research.state_available)],
          ["Config source", researchConfig(config)],
          [
            "Config failure",
            configUnavailable
              ? researchConfigReason(supervisorResearch?.config_reason)
              : "Not reported",
          ],
          ["Evidence count", metric(evidenceCount)],
          ["Evidence id", researchEvidenceId(research.evidence_id)],
          [
            "Cutoff / task deadline",
            research.cutoff_ms == null
              ? "Not reported"
              : datetime(validTimestampMs(research.cutoff_ms)),
          ],
          [
            "Deferred / backoff until",
            research.deferred_until_ms == null
              ? "Not reported"
              : datetime(validTimestampMs(research.deferred_until_ms)),
          ],
          ["Source as of", asOf === null ? "Not reported" : datetime(asOf)],
          ["Source age", future ? "Not trusted (future clock)" : ageLabel(age)],
          ["Pending source", researchPendingSource(pendingSource)],
          ["Pending kind", researchPendingKind(pending?.kind, pendingSource)],
          [
            "Pending started",
            pending
              ? datetime(validTimestampMs(pending.started_at_ms))
              : "Not reported",
          ],
          ["Pending age", pending ? ageLabel(pending.age_ms) : "Not reported"],
          [
            "Owner",
            "Same supervisor owner, profile and context budget; no separate agent",
          ],
          ["Model call", researchModelCall(research.status, evidenceCount)],
          [
            "Workspace refs",
            workspaceCount === null ? "Not reported" : count(workspaceCount),
          ],
          ["Workspace snapshot", workspaceSnapshot],
          [
            "Provider entitlement",
            "Not reported; no provider guarantee, fallback or entitlement claim is inferred",
          ],
        ]}
      />
      <p className="caption">
        A public scalar workspace-ref count and snapshot hash are shown only
        when the captured projection supplies them. Raw workspace paths are
        never read or rendered; account payloads, scripts and prompts are never
        read either.
      </p>
    </Panel>
  );
}
function ObservationPanel({ summary }: { summary?: SummaryResponse | null }) {
  const tasks = summary?.research_tasks;
  const observation = tasks?.observation;
  const state = observationState(observation);
  if (state === "missing") {
    return (
      <Panel
        title="Prospective observation"
        note="Read-only queue, lifecycle and trial-debt projection from original ledgers. No queue action, progress estimate or aggregate return is shown."
      >
        <Empty title="Observation projection not present in this snapshot">
          This snapshot does not include the observation projection. Queue
          counts, milestones and trial debt are not inferred as zero.
        </Empty>
      </Panel>
    );
  }
  if (state === "unsupported") {
    const rawVersion =
      observation &&
      typeof observation === "object" &&
      !Array.isArray(observation)
        ? (observation as { version?: unknown }).version
        : undefined;
    const versionText =
      typeof rawVersion === "number" || typeof rawVersion === "string"
        ? String(rawVersion)
        : "not reported";
    return (
      <Panel
        title="Prospective observation"
        note="Read-only queue, lifecycle and trial-debt projection from original ledgers."
      >
        <Notice error>
          {observation &&
          typeof observation === "object" &&
          !Array.isArray(observation)
            ? `Observation projection version ${versionText} is not supported; values are not interpreted. Queue counts and trial debt are not inferred.`
            : "Observation projection is malformed or unsupported; values are not interpreted. Queue counts and trial debt are not inferred."}
        </Notice>
      </Panel>
    );
  }
  if (state === "disabled") {
    return (
      <Panel
        title="Prospective observation"
        note="Read-only queue, lifecycle and trial-debt projection from original ledgers."
      >
        <Notice>
          Observation is disabled by policy; no queue counts, milestones or
          trial debt are inferred as zero.
        </Notice>
        <Facts items={[["Status", observationStatusLabel("disabled")]]} />
      </Panel>
    );
  }
  const value = observation as ObservationStatus;
  const queue = value.queue;
  const lifecycle = value.lifecycle;
  const trials = value.trials;
  const capture = value.capture ?? tasks?.observation_capture;
  const asOf = validObservationTimestamp(value.as_of_ms);
  const future = asOf !== null && asOf > Date.now();
  const milestone = milestoneWindow(lifecycle);
  const rows = milestone.rows;
  const continued = rows.some(
    (row) => row.continued === true || row.continuation_state != null,
  );
  const partial =
    trials?.status === "partial" || trials?.counts_lower_bound === true;
  return (
    <Panel
      title="Prospective observation"
      note="Read-only queue, lifecycle and trial-debt projection from original ledgers. No queue action, progress estimate or aggregate return is shown."
    >
      {value.status === "unavailable" && (
        <Notice error>
          Observation projection is unavailable:{" "}
          {researchReason((value as { reason?: unknown }).reason, true)}. Counts
          are not inferred as zero.
        </Notice>
      )}
      {future && (
        <Notice error>
          Clock anomaly: observation source as-of is later than this browser
          clock; age is not trusted and the source time is not renewed.
        </Notice>
      )}
      <div className="cols-2">
        <Facts
          items={[
            ["Status", observationStatusLabel(value.status)],
            ["Source", observationSourceLabel(value.source)],
            ["Queue pending", metric(queue?.pending)],
            ["Queue running", metric(queue?.running)],
            ["Queue unknown", metric(queue?.unknown)],
            ["Queue cancel requested", metric(queue?.cancel_requested)],
            ["Queue succeeded", metric(queue?.succeeded)],
            ["Queue rows scanned", metric(queue?.total)],
            ["Oldest age", observationAge(queue?.oldest_age_ms)],
            [
              "Next deadline",
              queue?.next_deadline_ms == null
                ? "Not reported"
                : datetime(validTimestampMs(queue.next_deadline_ms)),
            ],
            [
              "Deadline remaining",
              queue?.deadline_remaining_ms == null
                ? "Not reported"
                : durationOrUnknown(queue.deadline_remaining_ms),
            ],
            ["Queue truncated", yesNo(queue?.truncated)],
            [
              "Native source refs",
              observationRefsStatusLabel(value.observation_refs_status),
            ],
            ["Native refs scanned", metric(value.observation_refs_scanned)],
            ["Native refs limit", metric(value.observation_refs_limit)],
            ["Native refs truncated", yesNo(value.observation_refs_truncated)],
            ["Native refs partial", yesNo(value.observation_refs_partial)],
            [
              "Native refs source error",
              value.observation_refs_source_error_reason == null
                ? "Not reported"
                : researchReason(
                    value.observation_refs_source_error_reason,
                    true,
                  ),
            ],
          ]}
        />
        <Facts
          items={[
            ["Lifecycle recorded", metric(lifecycle?.recorded)],
            ["Lifecycle matured", metric(lifecycle?.matured)],
            ["Lifecycle truncated", yesNo(lifecycle?.truncated)],
            ["Source as of", asOf === null ? "Not reported" : datetime(asOf)],
            [
              "Source age",
              future
                ? "Not trusted (future clock)"
                : ageLabel(asOf === null ? null : Date.now() - asOf),
            ],
            ["Cohort key (label only)", observationCohortKey(value.cohort)],
            ["Cohort match", cohortStatus(value.cohort)],
            ["Evaluation scope", observationEvaluationScope(value.measurement)],
            [
              "Financial profit claim",
              value.financial_profit_claim === false ? "No" : "Not reported",
            ],
            [
              "Activation authority",
              value.activation_authorized === false ? "No" : "Not reported",
            ],
          ]}
        />
      </div>
      {queue?.truncated === true && (
        <Notice>
          Queue scan was truncated; counts are a bounded scan, not a complete
          total.
        </Notice>
      )}
      <Notice>
        Not validated: cohort match is unknown or different; this projection
        does not assert a common cohort.
      </Notice>
      <Panel
        title="Lifecycle milestones"
        note="Original recorded state and the latest verified effective state are separate. Milestones are the cross-session source."
      >
        {rows.length ? (
          <Table
            rows={rows}
            getKey={(row, index) => `${safeHash(row.candidate_id)}-${index}`}
            columns={[
              {
                key: "candidate",
                label: "Candidate",
                render: (row) => safeHash(row.candidate_id),
              },
              {
                key: "original",
                label: "Original recorded",
                render: (row) =>
                  lifecycleStateLabel(row.original_recorded_state),
              },
              {
                key: "effective",
                label: "Effective",
                render: (row) => lifecycleStateLabel(row.effective_state),
              },
              {
                key: "scientific",
                label: "Effective scientific",
                render: (row) =>
                  scientificStateLabel(row.effective_scientific_state),
              },
              {
                key: "continued",
                label: "Continued",
                render: (row) => yesNo(row.continued),
              },
              {
                key: "source",
                label: "Source session",
                render: (row) => safeSession(row.origin_session),
              },
              {
                key: "recording",
                label: "Recording session",
                render: (row) =>
                  safeSession(row.continuation_recorded_in_session),
              },
              {
                key: "continuation",
                label: "Continuation",
                render: (row) => continuationStateLabel(row.continuation_state),
              },
              {
                key: "gaps",
                label: "Observation gaps",
                render: (row) =>
                  milestoneGapReasons(row).join("; ") || "Not reported",
              },
              {
                key: "measurement-gaps",
                label: "Measurement gaps",
                render: (row) =>
                  milestoneMeasurementGapReasons(row).join("; ") ||
                  "Not reported",
              },
              {
                key: "measurement-state",
                label: "Measurement",
                render: (row) => measurementStatusLabel(row.measurement_status),
              },
              {
                key: "original-measurement",
                label: "Original measurement",
                render: (row) =>
                  originalMeasurementStatusLabel(
                    row.original_measurement_status,
                    row.original_measurement_reason,
                  ),
              },
              {
                key: "source-ref",
                label: "Source ref",
                render: (row) =>
                  sourceRefStatusLabel(row.source_observation_ref_status),
              },
              {
                key: "measurement",
                label: "Continuation receipt hash",
                render: (row) => safeHash(row.measurement_sha256),
              },
              {
                key: "processed",
                label: "Processed",
                render: (row) =>
                  datetime(validTimestampMs(row.processed_at_ms)),
              },
              {
                key: "exit",
                label: "Exit",
                render: (row) => datetime(validTimestampMs(row.exit_at_ms)),
              },
            ]}
          />
        ) : (
          <Empty title="No observation milestones">
            No lifecycle milestone is present in this projection.
          </Empty>
        )}
        {milestone.truncatedByDisplay && (
          <Notice>
            Showing {milestone.shown} of {milestone.total} milestones; the view
            is bounded by the display limit. The remaining rows are not copied
            or counted.
          </Notice>
        )}
        {milestone.sourceTruncated && (
          <Notice>
            Milestone source is truncated; total count not reported. This view
            does not extrapolate a full total from the loaded rows.
          </Notice>
        )}
      </Panel>
      {continued && (
        <Notice>
          Original recorded state is preserved. The effective state is the
          latest verified continuation recorded by the named session; the
          current session does not claim the financial fill. Source and
          recording sessions are recorded source clocks; the current read time
          is not substituted.
        </Notice>
      )}
      <Panel
        title="Global trial debt"
        note="Folded from original queues and lifecycle ledgers. A partial read is a lower bound, never a completed count."
      >
        <Facts
          items={[
            ["Trial debt status", trialStatusLabel(trials?.status)],
            ["Trial lower bound", trialLowerBound(trials)],
            ["Trial gaps", trialGapReason(trials)],
            [
              "Global tested trials",
              trialMainCount(trials, "global_tested_trial_count"),
            ],
            ["Admitted trials", trialMainCount(trials, "admitted_trial_count")],
            ["Executed trials", trialMainCount(trials, "executed_trial_count")],
            ["Terminal trials", trialMainCount(trials, "terminal_trial_count")],
            [
              "Unknown exposure",
              trialMainCount(trials, "unknown_exposure_count"),
            ],
            ["Distinct sessions", trialMainCount(trials, "distinct_sessions")],
            ["Family key", trialFamilyKeyNote(trials)],
            ["Multiple testing", "Unadjusted; family key is a label only"],
            ["Trial reset", trialReset(trials)],
          ]}
        />
        {partial && (
          <Notice>
            Trial debt is a lower bound ({trialLowerBound(trials)}). The known
            gap reasons above explain why the history is incomplete; main counts
            stay Not reported rather than an exact total.
          </Notice>
        )}
      </Panel>
      <Panel
        title="Observation capture"
        note="Operational capture budget only; this is not a financial risk cap."
      >
        <Facts
          items={[
            ["Capture status", captureStatusLabel(capture?.status)],
            ["Coverage", captureCoverageLabel(capture?.coverage)],
            ["Coverage basis", captureCoverageLabel(capture?.coverage_label)],
            [
              "Current eligibility",
              captureEligibilityLabel(capture?.current_eligibility),
            ],
            [
              "Eligible for attempt",
              captureEligibleForAttempt(capture?.eligible_for_attempt),
            ],
            ["Capture reason", captureReason(capture)],
            ["Capture budget", captureBudget(capture)],
            ["Resource caps", captureOperationalCaps(capture)],
            ["Provider called", yesNo(capture?.provider_called)],
            ["State written", yesNo(capture?.state_written)],
            ["Last capture age", observationAge(capture?.last_capture_age_ms)],
          ]}
        />
        {capture?.status === "unavailable" || capture?.status === "unknown" ? (
          <Notice error>
            Capture projection is {captureStatusLabel(capture?.status)}:{" "}
            {captureReason(capture)}. No capture, provider call or current
            eligibility is inferred.
          </Notice>
        ) : null}
        <Notice>
          Journal readability is not current eligibility. This projection does
          not evaluate the current book, session or universe and makes no
          provider guarantee. The caps are resource limits, not trading risk
          caps.
        </Notice>
        {capture?.enabled === false && (
          <Notice>
            Observation capture is explicitly disabled; no quote request or
            provider call is inferred.
          </Notice>
        )}
      </Panel>
    </Panel>
  );
}
function MeasurementPanel({ summary }: { summary?: SummaryResponse | null }) {
  const library = summary?.research_library;
  const window = measurementWindow(library);
  if (!window.total) {
    return (
      <Panel
        title="Virtual research episodes"
        note="Independent virtual episodes — not portfolio returns. Each episode uses its own declared research capital assumption, never account funds or actual book fills."
      >
        <Empty title="No measurement evidence">
          No per-episode observation measurement is present in this snapshot. No
          PnL, return or progress is inferred.
        </Empty>
      </Panel>
    );
  }
  return (
    <Panel
      title="Virtual research episodes"
      note="Independent virtual episodes — not portfolio returns. Each episode uses its own declared research capital assumption, never account funds or actual book fills."
    >
      <Notice>
        Independent virtual episodes — not portfolio returns. Research quotes
        are virtual archived receipts, not live tradeable market quotes.
      </Notice>
      {window.entries.map(({ item, measurement, state, reason }, index) => {
        if (state !== "present" || !measurement) {
          return (
            <div
              key={`${safeHash(item.candidate_id)}-${index}`}
              className="cols-1"
            >
              <Facts
                items={[
                  ["Candidate", safeHash(item.candidate_id)],
                  [
                    "Measurement state",
                    state === "unsupported" ? "Unsupported" : "Unavailable",
                  ],
                  ...(item.measurement_status
                    ? ([
                        [
                          "Checked status",
                          measurementStatusLabel(item.measurement_status),
                        ],
                      ] as [string, string][])
                    : []),
                  ...(item.original_measurement_status
                    ? ([
                        ["Original record", measurementOriginalStatus(item)],
                      ] as [string, string][])
                    : []),
                  ["Reason", reason],
                ]}
              />
              <Notice error>
                {state === "unsupported"
                  ? `${reason}. No financial fields are interpreted and no PnL, return or progress is inferred.`
                  : `Measurement unavailable: ${reason}. No PnL, return or progress is inferred as zero.`}
              </Notice>
            </div>
          );
        }
        const episode = measurement.episodes?.[0];
        const pnl = episodePnl(measurement);
        const capital = episodeCapital(measurement);
        const replay = replayAxes(measurement);
        const scoring = actualScoring(measurement);
        const timeline = measurementTimeline(measurement);
        const continuation =
          measurement.source_scope === "continuation_original_episode";
        const statusFacts: [string, string][] = [];
        if (item.measurement_status)
          statusFacts.push([
            "Measurement state",
            measurementStatusLabel(item.measurement_status),
          ]);
        if (item.original_measurement_status)
          statusFacts.push([
            "Original record",
            measurementOriginalStatus(item),
          ]);
        return (
          <div
            key={`${safeHash(item.candidate_id)}-${index}`}
            className="cols-2"
          >
            <Facts
              items={[
                ...statusFacts,
                ["Episode id", safeHash(item.candidate_id)],
                ["Evaluation scope", measurementScope(measurement)],
                ["Episode count", measurementEpisodeCount(measurement)],
                ["Episode PnL", pnl.value],
                ["PnL reason", pnl.reason || "Not applicable"],
                [
                  "Declared research capital",
                  capital.assumption && capital.value
                    ? `${capital.value} declared research capital assumption; not account funds`
                    : "Unavailable",
                ],
                ["Entry", episodeEntryLabel(measurement)],
                [
                  "Exit status",
                  exitStatusLabel(
                    episode?.exit_status ?? measurement.exit_status,
                  ),
                ],
                [
                  "Entry source quote (T0)",
                  continuation
                    ? continuationQuoteHash(measurement, "entry")
                    : safeHash(episode?.entry_quote_sha256),
                ],
                [
                  "Exit source quote",
                  continuation
                    ? continuationQuoteHash(measurement, "exit")
                    : safeHash(episode?.exit_quote_sha256),
                ],
                ["Immutable episode hash", measurementHash(measurement)],
                [
                  "Continuation receipt hash",
                  continuation
                    ? continuationReceiptHash(measurement)
                    : "Not applicable (episode measurement)",
                ],
                [
                  "Continuation receipt scope",
                  continuation ? measurement.source_scope : "Not applicable",
                ],
                ["Live context hash", liveContextHash(measurement)],
                ["Cohort match", cohortStatus(measurement.cohort)],
                [
                  "Trial debt",
                  measurement.trials?.status === "partial" ||
                  measurement.trials?.counts_lower_bound === true
                    ? `${trialStatusLabel(measurement.trials?.status)} · ${trialLowerBound(measurement.trials)}`
                    : trialStatusLabel(measurement.trials?.status),
                ],
                ["Family key", trialFamilyKeyNote(measurement.trials)],
                [
                  "Actual book fills",
                  "None; this is a virtual research episode",
                ],
                ["Qualification", qualificationLabel(measurement)],
              ]}
            />
            <Facts
              items={[
                ["Actual scoring status", scoring.status],
                ["Actual scoring", scoring.score],
                ["Scoring qualification", scoring.qualified],
                ["Task 8 replay status", replay.status],
                ["Task 8 report copied unchanged", replay.copied],
                ["Retrospective axes", replay.retrospective],
                ["Prospective axes", replay.prospective],
                ["No-lookahead qualification", replay.noLookahead],
                ["Holdout qualification", replay.holdout],
                ["Edge qualification", replay.edge],
                ["Promotion authority", replay.promotion],
                [
                  "Measurement scope",
                  "Immutable episode evidence; live trial debt is separate",
                ],
              ]}
            />
            {continuation && (
              <div className="span-all">
                <Panel
                  title="Continuation receipt detail"
                  note="Typed original quote refs and recorded source clocks. The continuation receipt hash is not the immutable episode projection."
                >
                  <Facts
                    items={[
                      ["Continuation id", continuationId(measurement)],
                      [
                        "Continuation state",
                        continuationStateLabel(measurement.state),
                      ],
                      [
                        "Continuation source session",
                        safeSession(measurement.source_session),
                      ],
                      [
                        "Continuation recording session",
                        safeSession(measurement.current_scope?.current_session),
                      ],
                      [
                        "Entry quote archive hash",
                        continuationQuoteHash(measurement, "entry"),
                      ],
                      [
                        "Exit quote archive hash",
                        continuationQuoteHash(measurement, "exit"),
                      ],
                      [
                        "Entry quote purpose",
                        continuationQuotePurpose(measurement, "entry"),
                      ],
                      [
                        "Exit quote purpose",
                        continuationQuotePurpose(measurement, "exit"),
                      ],
                      [
                        "Entry quote source clock",
                        continuationQuoteClock(measurement, "entry", "source"),
                      ],
                      [
                        "Entry quote receipt clock",
                        continuationQuoteClock(measurement, "entry", "receipt"),
                      ],
                      [
                        "Exit quote source clock",
                        continuationQuoteClock(measurement, "exit", "source"),
                      ],
                      [
                        "Exit quote receipt clock",
                        continuationQuoteClock(measurement, "exit", "receipt"),
                      ],
                      [
                        "Exit due source clock",
                        datetime(validTimestampMs(measurement.exit_due_ms)),
                      ],
                      [
                        "Exit deadline source clock",
                        datetime(
                          validTimestampMs(measurement.exit_deadline_ms),
                        ),
                      ],
                      [
                        "Exit source clock",
                        datetime(validTimestampMs(measurement.exit_at_ms)),
                      ],
                      [
                        "Processed source clock",
                        datetime(validTimestampMs(measurement.processed_at_ms)),
                      ],
                      [
                        "Recorded source clock",
                        datetime(validTimestampMs(measurement.recorded_at_ms)),
                      ],
                    ]}
                  />
                  <p className="caption">
                    Original exit, processing and recording clocks are separate.
                    The immutable episode projection hash is not replaced by the
                    continuation receipt hash.
                  </p>
                </Panel>
              </div>
            )}
            <div className="span-all">
              {!pnl.available && (
                <Notice error>
                  Episode PnL is unavailable: {pnl.reason}. It is not zero and
                  not green.
                </Notice>
              )}
              {(episode?.entry === false ||
                (continuation && measurement.entry === false)) && (
                <Notice>
                  No entry was recorded (source quote gap or no entry). This is
                  not a paper loss of zero.
                </Notice>
              )}
              <Notice>
                Reading the current cohort, trial debt or source context does
                not rewrite the original episode or its recorded measurement and
                does not claim the financial fill. No episode PnL is summed; no
                portfolio return, equity curve, Sharpe or profit percentage is
                computed.
              </Notice>
              <details className="evidence-fold">
                <summary>
                  Episode clock details <span>Recorded source clocks</span>
                </summary>
                <Facts items={timeline} />
                <p className="caption">
                  Authoring, target freeze, score, decision, exit and recording
                  are separate recorded source clocks; they are not one
                  fabricated timestamp.
                </p>
              </details>
              <details className="evidence-fold">
                <summary>
                  Task 8 retrospective report <span>Copied unchanged</span>
                </summary>
                <Facts
                  items={[
                    ["Replay status", replay.status],
                    ["Retrospective axes", replay.retrospective],
                    ["Prospective axes", replay.prospective],
                    ["No-lookahead qualification", replay.noLookahead],
                    ["Holdout qualification", replay.holdout],
                    ["Edge qualification", replay.edge],
                    ["Promotion authority", replay.promotion],
                  ]}
                />
                <p className="caption">
                  The old Task 8 retrospective axes are separate from the new
                  forward score. No qualification, edge or holdout claim is
                  inferred.
                </p>
              </details>
            </div>
          </div>
        );
      })}
      {window.truncatedByDisplay && (
        <Notice>
          Showing {window.shown} of {window.total} measurement entries; the view
          is bounded by the display limit. The remaining rows are not copied or
          counted.
        </Notice>
      )}
      {window.sourceTruncated && (
        <Notice>
          Measurement library is truncated by the source; total count not
          reported. This view does not extrapolate a full total from the loaded
          rows.
        </Notice>
      )}
    </Panel>
  );
}
function CrossSessionPanel({ summary }: { summary?: SummaryResponse | null }) {
  const lifecycle = summary?.research_tasks?.observation?.lifecycle;
  const all = milestoneWindow(lifecycle, Number.MAX_SAFE_INTEGER).rows.filter(
    (row) => row.continued === true || row.continuation_state != null,
  );
  if (!all.length) {
    return (
      <Panel
        title="Cross-session continuity"
        note="Original episode state and the latest verified continuation from observation milestones. No predecessor row is copied or rewritten."
      >
        <Empty title="No cross-session continuity">
          No continued original episode is present in the observation
          milestones. Research-library continued/continuation flags are not used
          as continuation evidence.
        </Empty>
      </Panel>
    );
  }
  const rows = all.slice(0, 10);
  return (
    <Panel
      title="Cross-session continuity"
      note="Original episode state and the latest verified continuation from observation milestones. No predecessor row is copied or rewritten."
    >
      <Table
        rows={rows}
        getKey={(row, index) => `${safeHash(row.candidate_id)}-${index}`}
        columns={[
          {
            key: "candidate",
            label: "Candidate",
            render: (row) => safeHash(row.candidate_id),
          },
          {
            key: "source",
            label: "Original session",
            render: (row) => safeSession(row.origin_session),
          },
          {
            key: "original",
            label: "Original recorded state",
            render: (row) => lifecycleStateLabel(row.original_recorded_state),
          },
          {
            key: "effective",
            label: "Effective state",
            render: (row) => lifecycleStateLabel(row.effective_state),
          },
          {
            key: "scientific",
            label: "Effective scientific",
            render: (row) =>
              scientificStateLabel(row.effective_scientific_state),
          },
          {
            key: "continued",
            label: "Continued",
            render: (row) => yesNo(row.continued),
          },
          {
            key: "continuation",
            label: "Continuation",
            render: (row) => continuationStateLabel(row.continuation_state),
          },
          {
            key: "recording",
            label: "Recording session",
            render: (row) => safeSession(row.continuation_recorded_in_session),
          },
          {
            key: "measurement-state",
            label: "Measurement",
            render: (row) => measurementStatusLabel(row.measurement_status),
          },
          {
            key: "original-measurement",
            label: "Original measurement",
            render: (row) =>
              originalMeasurementStatusLabel(
                row.original_measurement_status,
                row.original_measurement_reason,
              ),
          },
          {
            key: "measurement",
            label: "Continuation receipt hash",
            render: (row) => safeHash(row.measurement_sha256),
          },
          {
            key: "processed",
            label: "Processed",
            render: (row) => datetime(validTimestampMs(row.processed_at_ms)),
          },
          {
            key: "exit",
            label: "Exit",
            render: (row) => datetime(validTimestampMs(row.exit_at_ms)),
          },
        ]}
      />
      {all.length > rows.length && (
        <Notice>
          Showing {rows.length} of {all.length} continued milestones; the view
          is bounded by the display limit and does not copy the remaining rows.
        </Notice>
      )}
      {lifecycle?.truncated === true && (
        <Notice>
          Milestone source is truncated; the loaded continued milestones are not
          a complete total. This view does not extrapolate a total or claim the
          remaining rows.
        </Notice>
      )}
      <Notice>
        Milestones are the only cross-session source; research-library
        continued/continuation flags are not used. The recording session is the
        source that recorded the continuation; the current session does not
        claim the financial fill. Source and recording sessions are recorded
        source clocks, not a new read time.
      </Notice>
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
    <Panel
      title="Operational health"
      note="Read-only projection. Paper finance, controller liveness, research/advisory state, supervisor state, calendar coverage and alert transport are separate observations. Browser refresh never renews the published server capture."
    >
      {version === "missing" && (
        <Notice>
          Operational health is not present in this snapshot (older API).
          Unknown values stay unknown; no zero, healthy or complete state is
          inferred.
        </Notice>
      )}
      {version === "unsupported" && (
        <Notice error>
          {rawObject
            ? versionMissing
              ? "Operational health version is not reported; this client supports only version 1. values are not interpreted."
              : versionMalformed
                ? "Operational health version is malformed; this client supports only numeric version 1. values are not interpreted."
                : `Operational health version ${versionText} is not supported by this client; values are not interpreted.`
            : "Operational health is malformed; values are not interpreted."}
        </Notice>
      )}
      <BookContinuity summary={summary} portfolio={portfolio} />
      <div className="cols-2">
        <PaperFinancialPanel summary={summary} portfolio={portfolio} />
        <ControllerLivenessPanel
          liveness={liveness}
          operational={operational}
          now={now}
          interpret={interpret}
        />
      </div>
      <div className="cols-2">
        <ResearchPanel summary={summary} />
        <SupervisorPanel
          supervisor={operational?.supervisor}
          interpret={interpret}
        />
      </div>
      <OwnerResearchPanel
        summary={summary}
        supervisor={operational?.supervisor}
        interpret={interpret}
      />
      <ObservationPanel summary={summary} />
      <CrossSessionPanel summary={summary} />
      <MeasurementPanel summary={summary} />
      <CalendarPanel calendar={operational?.calendar} interpret={interpret} />
      <AlertPanel alert={operational?.alert_transport} interpret={interpret} />
    </Panel>
  );
}
