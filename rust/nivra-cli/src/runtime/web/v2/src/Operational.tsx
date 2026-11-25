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
} from "./types";
import { amount, count, datetime, projectedStatus } from "./data";
import { plain } from "./labels";
import { Empty, Facts, KpiStrip, Notice, Panel, Status, Table } from "./ui";
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
  qualificationLabel,
  replayAxes,
  researchReason,
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
} from "./research";
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
    <Panel title="Controller">
      <Facts
        items={[
          ["Status", livenessLabel(liveness?.status)],
          ["Last loop finished", datetime(completed)],
          [
            "Loop age",
            completedFuture
              ? "Not trusted (future clock)"
              : ageLabel(liveness?.age_ms),
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
function ObservationPanel({ summary }: { summary?: SummaryResponse | null }) {
  const tasks = summary?.research_tasks;
  const observation = tasks?.observation;
  const state = observationState(observation);
  if (state === "missing") {
    return (
      <Panel title="Observation">
        <Empty title="No observation data in this snapshot" />
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
      <Panel title="Observation">
        <Notice error>
          {observation &&
          typeof observation === "object" &&
          !Array.isArray(observation)
            ? `Observation format ${versionText} is not supported. Values are hidden.`
            : "Observation data is malformed. Values are hidden."}
        </Notice>
      </Panel>
    );
  }
  if (state === "disabled") {
    return (
      <Panel title="Observation">
        <Notice>Observation is disabled.</Notice>
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
  const partial =
    trials?.status === "partial" || trials?.counts_lower_bound === true;
  const trialCountRows: [string, string][] = [
    ["Tested", trialMainCount(trials, "global_tested_trial_count")],
    ["Admitted", trialMainCount(trials, "admitted_trial_count")],
    ["Executed", trialMainCount(trials, "executed_trial_count")],
    ["Finished", trialMainCount(trials, "terminal_trial_count")],
    ["Unknown exposure", trialMainCount(trials, "unknown_exposure_count")],
    ["Sessions", trialMainCount(trials, "distinct_sessions")],
  ];
  const trialCounts: [string, string][] = trialCountRows.every(
    ([, v]) => v === "Not reported",
  )
    ? [["Counts", "Not reported"]]
    : trialCountRows;
  return (
    <Panel title="Observation">
      {value.status === "unavailable" && (
        <Notice error>
          Observation unavailable:{" "}
          {researchReason((value as { reason?: unknown }).reason, true)}.
        </Notice>
      )}
      {future && (
        <Notice error>
          Source time is ahead of this browser's clock. Age is not trusted.
        </Notice>
      )}
      <div className="cols-2">
        <Facts
          items={[
            ["Status", observationStatusLabel(value.status)],
            ["Source", observationSourceLabel(value.source)],
            ["Pending", metric(queue?.pending)],
            ["Running", metric(queue?.running)],
            ["Unknown state", metric(queue?.unknown)],
            ["Cancel requested", metric(queue?.cancel_requested)],
            ["Succeeded", metric(queue?.succeeded)],
            ["Rows scanned", metric(queue?.total)],
            ["Oldest item", observationAge(queue?.oldest_age_ms)],
            [
              "Next deadline",
              queue?.next_deadline_ms == null
                ? "Not reported"
                : datetime(validTimestampMs(queue.next_deadline_ms)),
            ],
            [
              "Time to deadline",
              queue?.deadline_remaining_ms == null
                ? "Not reported"
                : durationOrUnknown(queue.deadline_remaining_ms),
            ],
            ["Queue cut off", yesNo(queue?.truncated)],
            [
              "Source refs",
              observationRefsStatusLabel(value.observation_refs_status),
            ],
            ["Refs scanned", metric(value.observation_refs_scanned)],
            ...(value.observation_refs_source_error_reason == null
              ? []
              : [
                  [
                    "Refs error",
                    researchReason(
                      value.observation_refs_source_error_reason,
                      true,
                    ),
                  ] as [string, string],
                ]),
          ]}
        />
        <Facts
          items={[
            ["Recorded", metric(lifecycle?.recorded)],
            ["Matured", metric(lifecycle?.matured)],
            ["List cut off", yesNo(lifecycle?.truncated)],
            ["As of", asOf === null ? "Not reported" : datetime(asOf)],
            [
              "Age",
              future
                ? "Not trusted (future clock)"
                : ageLabel(asOf === null ? null : Date.now() - asOf),
            ],
            ["Cohort key", observationCohortKey(value.cohort)],
            ["Cohort match", cohortStatus(value.cohort)],
            ["Scope", observationEvaluationScope(value.measurement)],
          ]}
        />
      </div>
      {queue?.truncated === true && (
        <Notice>Queue list is cut off. Counts are not a full total.</Notice>
      )}
      <Panel title="Milestones">
        {rows.length ? (
          <Table
            hideEmpty
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
                label: "Recorded state",
                render: (row) =>
                  lifecycleStateLabel(row.original_recorded_state),
              },
              {
                key: "effective",
                label: "Current state",
                render: (row) => lifecycleStateLabel(row.effective_state),
              },
              {
                key: "scientific",
                label: "Scientific state",
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
                label: "From session",
                render: (row) => safeSession(row.origin_session),
              },
              {
                key: "recording",
                label: "Recorded in",
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
                label: "Data gaps",
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
                label: "Original result",
                render: (row) =>
                  originalMeasurementStatusLabel(
                    row.original_measurement_status,
                    row.original_measurement_reason,
                  ),
              },
              {
                key: "source-ref",
                label: "Source",
                render: (row) =>
                  sourceRefStatusLabel(row.source_observation_ref_status),
              },
              {
                key: "measurement",
                label: "Receipt",
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
          <Empty title="No milestones" />
        )}
        {milestone.truncatedByDisplay && (
          <Notice>
            Showing {milestone.shown} of {milestone.total} milestones.
          </Notice>
        )}
        {milestone.sourceTruncated && (
          <Notice>Milestone list is cut off. Total unknown.</Notice>
        )}
      </Panel>
      <Panel title="Trials">
        <Facts
          items={[
            ["Status", trialStatusLabel(trials?.status)],
            ["Count basis", trialLowerBound(trials)],
            ["Gaps", trialGapReason(trials)],
            ...trialCounts,
            ["Family key", trialFamilyKeyNote(trials)],
            ["Trial reset", trialReset(trials)],
          ]}
        />
        {partial && <Notice>Counts are a lower bound, not a total.</Notice>}
      </Panel>
      <Panel title="Capture">
        <Facts
          items={[
            ["Status", captureStatusLabel(capture?.status)],
            ["Coverage", captureCoverageLabel(capture?.coverage)],
            [
              "Eligibility",
              captureEligibilityLabel(capture?.current_eligibility),
            ],
            [
              "Eligible to try",
              captureEligibleForAttempt(capture?.eligible_for_attempt),
            ],
            ["Reason", captureReason(capture)],
            ["Budget", captureBudget(capture)],
            ["Limits", captureOperationalCaps(capture)],
            ["Provider called", yesNo(capture?.provider_called)],
            ["State written", yesNo(capture?.state_written)],
            ["Last capture", observationAge(capture?.last_capture_age_ms)],
          ]}
        />
        {capture?.status === "unavailable" || capture?.status === "unknown" ? (
          <Notice error>
            Capture is {captureStatusLabel(capture?.status).toLowerCase()}:{" "}
            {captureReason(capture)}.
          </Notice>
        ) : null}
        {capture?.enabled === false && (
          <Notice>Observation capture is disabled.</Notice>
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
      <Panel title="Virtual episodes">
        <Empty title="No episode results" />
      </Panel>
    );
  }
  return (
    <Panel title="Virtual episodes">
      <Notice>Virtual episodes, not portfolio returns.</Notice>
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
                    "Result",
                    state === "unsupported" ? "Unsupported" : "Unavailable",
                  ],
                  ...(item.measurement_status
                    ? ([
                        [
                          "Checked",
                          measurementStatusLabel(item.measurement_status),
                        ],
                      ] as [string, string][])
                    : []),
                  ...(item.original_measurement_status
                    ? ([["Original", measurementOriginalStatus(item)]] as [
                        string,
                        string,
                      ][])
                    : []),
                  ["Reason", reason],
                ]}
              />
              <Notice error>
                {state === "unsupported"
                  ? `${reason}. Financial fields are hidden.`
                  : `Result unavailable: ${reason}.`}
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
            "Result",
            measurementStatusLabel(item.measurement_status),
          ]);
        if (item.original_measurement_status)
          statusFacts.push(["Original", measurementOriginalStatus(item)]);
        return (
          <div
            key={`${safeHash(item.candidate_id)}-${index}`}
            className="cols-2"
          >
            <Facts
              items={[
                ...statusFacts,
                ["Episode", safeHash(item.candidate_id)],
                ["Scope", measurementScope(measurement)],
                ["Episodes", measurementEpisodeCount(measurement)],
                ["P&L", pnl.value],
                ["P&L note", pnl.reason || "Not applicable"],
                [
                  "Research capital",
                  capital.assumption && capital.value
                    ? `${capital.value} (assumed, not account funds)`
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
                  "Entry quote",
                  continuation
                    ? continuationQuoteHash(measurement, "entry")
                    : safeHash(episode?.entry_quote_sha256),
                ],
                [
                  "Exit quote",
                  continuation
                    ? continuationQuoteHash(measurement, "exit")
                    : safeHash(episode?.exit_quote_sha256),
                ],
                ["Episode hash", measurementHash(measurement)],
                [
                  "Receipt hash",
                  continuation
                    ? continuationReceiptHash(measurement)
                    : "Not applicable",
                ],
                ["Context hash", liveContextHash(measurement)],
                ["Cohort match", cohortStatus(measurement.cohort)],
                [
                  "Trial debt",
                  measurement.trials?.status === "partial" ||
                  measurement.trials?.counts_lower_bound === true
                    ? `${trialStatusLabel(measurement.trials?.status)} · ${trialLowerBound(measurement.trials)}`
                    : trialStatusLabel(measurement.trials?.status),
                ],
                ["Family key", trialFamilyKeyNote(measurement.trials)],
                ["Qualification", qualificationLabel(measurement)],
              ]}
            />
            <Facts
              items={[
                ["Scoring status", scoring.status],
                ["Score", scoring.score],
                ["Scoring qualified", scoring.qualified],
                ["Replay status", replay.status],
                ["Looked back", replay.retrospective],
                ["Looked forward", replay.prospective],
                ["No look-ahead", replay.noLookahead],
                ["Holdout", replay.holdout],
                ["Edge", replay.edge],
                ["Promotion", replay.promotion],
              ]}
            />
            {continuation && (
              <div className="span-all">
                <Panel title="Continuation detail">
                  <Facts
                    items={[
                      ["Continuation", continuationId(measurement)],
                      ["State", continuationStateLabel(measurement.state)],
                      [
                        "Source session",
                        safeSession(measurement.source_session),
                      ],
                      [
                        "Recorded in session",
                        safeSession(measurement.current_scope?.current_session),
                      ],
                      [
                        "Entry quote hash",
                        continuationQuoteHash(measurement, "entry"),
                      ],
                      [
                        "Exit quote hash",
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
                        "Entry quote (source time)",
                        continuationQuoteClock(measurement, "entry", "source"),
                      ],
                      [
                        "Entry quote (received)",
                        continuationQuoteClock(measurement, "entry", "receipt"),
                      ],
                      [
                        "Exit quote (source time)",
                        continuationQuoteClock(measurement, "exit", "source"),
                      ],
                      [
                        "Exit quote (received)",
                        continuationQuoteClock(measurement, "exit", "receipt"),
                      ],
                      [
                        "Exit due",
                        datetime(validTimestampMs(measurement.exit_due_ms)),
                      ],
                      [
                        "Exit deadline",
                        datetime(
                          validTimestampMs(measurement.exit_deadline_ms),
                        ),
                      ],
                      [
                        "Exited",
                        datetime(validTimestampMs(measurement.exit_at_ms)),
                      ],
                      [
                        "Processed",
                        datetime(validTimestampMs(measurement.processed_at_ms)),
                      ],
                      [
                        "Recorded",
                        datetime(validTimestampMs(measurement.recorded_at_ms)),
                      ],
                    ]}
                  />
                </Panel>
              </div>
            )}
            <div className="span-all">
              {!pnl.available && (
                <Notice error>P&L unavailable: {pnl.reason}. Not zero.</Notice>
              )}
              {(episode?.entry === false ||
                (continuation && measurement.entry === false)) && (
                <Notice>No entry was recorded. This is not a zero loss.</Notice>
              )}
              <details className="evidence-fold">
                <summary>Episode times</summary>
                <Facts items={timeline} />
              </details>
            </div>
          </div>
        );
      })}
      {window.truncatedByDisplay && (
        <Notice>
          Showing {window.shown} of {window.total} episodes.
        </Notice>
      )}
      {window.sourceTruncated && (
        <Notice>Episode list is cut off. Total unknown.</Notice>
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
      <Panel title="Across sessions">
        <Empty title="No continued episodes" />
      </Panel>
    );
  }
  const rows = all.slice(0, 10);
  return (
    <Panel title="Across sessions">
      <Table
        hideEmpty
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
            label: "From session",
            render: (row) => safeSession(row.origin_session),
          },
          {
            key: "original",
            label: "Recorded state",
            render: (row) => lifecycleStateLabel(row.original_recorded_state),
          },
          {
            key: "effective",
            label: "Current state",
            render: (row) => lifecycleStateLabel(row.effective_state),
          },
          {
            key: "scientific",
            label: "Scientific state",
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
            label: "Recorded in",
            render: (row) => safeSession(row.continuation_recorded_in_session),
          },
          {
            key: "measurement-state",
            label: "Measurement",
            render: (row) => measurementStatusLabel(row.measurement_status),
          },
          {
            key: "original-measurement",
            label: "Original result",
            render: (row) =>
              originalMeasurementStatusLabel(
                row.original_measurement_status,
                row.original_measurement_reason,
              ),
          },
          {
            key: "measurement",
            label: "Receipt",
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
          Showing {rows.length} of {all.length} continued milestones.
        </Notice>
      )}
      {lifecycle?.truncated === true && (
        <Notice>Milestone list is cut off. Total unknown.</Notice>
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
          now={now}
          interpret={interpret}
        />
      </div>
      <ResearchPanel summary={summary} />
      <ObservationPanel summary={summary} />
      <CrossSessionPanel summary={summary} />
      <MeasurementPanel summary={summary} />
      <CalendarPanel calendar={operational?.calendar} interpret={interpret} />
      <AlertPanel alert={operational?.alert_transport} interpret={interpret} />
    </Panel>
  );
}
