import { test } from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { JSDOM } from "jsdom";
import { OperationalPanel } from "./Operational";
import type {
  HealthResponse,
  OperationalHealth,
  PortfolioResponse,
  SummaryResponse,
} from "./types";

const envelope = {
  v: 1,
  mode: "paper",
  paper_only: true,
  broker: { data: "fixture-broker" },
  account: "private-fixture",
  session: "2026-10-29",
  as_of_ms: 1_700_000_000_000,
};
const now = Date.now();

function summary(operational?: OperationalHealth | null): SummaryResponse {
  return {
    ...envelope,
    capital: "5000",
    cash: "4900",
    equity: "5000",
    status: "open",
    model_status: "model-led",
    entry_status: "ready",
    state: { stopped: false, halted: false, closed: false, health: "ok" },
    readiness: { status: "ready" },
    research_library: {
      status: "ready",
      items: [
        { candidate_id: "fixture-candidate", state: "analysis_cancelled" },
      ],
      truncated: false,
      activation_authorized: false,
    },
    ...(operational ? { operational_health: operational } : {}),
  };
}
function portfolio(): PortfolioResponse {
  return {
    ...envelope,
    capital: "5000",
    cash: "4900",
    equity: "5000",
    positions: [],
    fills: [],
    stopped: false,
    halted: false,
    closed: false,
  };
}
function health(
  liveness?: HealthResponse["controller_liveness"],
  operational?: OperationalHealth | null,
): HealthResponse {
  return {
    ...envelope,
    ...(liveness ? { controller_liveness: liveness } : {}),
    ...(operational ? { operational_health: operational } : {}),
  };
}
function text(node: React.ReactElement) {
  const dom = new JSDOM(renderToStaticMarkup(node));
  return dom.window.document.body.textContent || "";
}
function happyOperational(): OperationalHealth {
  return {
    version: 2,
    captured_at_ms: now - 5_000,
    calendar: {
      source: "NSE/CMTR/71775",
      covered_from: "2026-01-01",
      covered_to: "2026-12-31",
      current_operational_date: "2026-10-29",
      covered: true,
      requested_session: "2026-10-29",
      requested_session_covered: true,
      historical_session: "2026-10-28",
      historical_session_covered: true,
      days_remaining: 63,
      expiring: false,
      expired: false,
      expiry_warning_days: 30,
      expiry_warning_basis: "operator_chosen_default_not_exchange_guarantee",
      update_prerequisite: "verified_exchange_calendar_update_required",
    },
    alert_transport: {
      status: "available",
      store: "alert_outbox",
      counts_scope: "host_alert_outbox_not_account_filtered",
      scope: {
        account: "private-fixture",
        session: "2026-10-29",
        store_scope: "host_alert_outbox",
      },
      counts: {
        queued: 2,
        retrying: 1,
        transport_accepted: 3,
        transport_accepted_scope: "retained_window_only_not_lifetime",
        dead: 0,
        dead_definition: "quarantined_outbox_rows",
        dropped: null,
        in_flight_or_transport_error: 1,
        in_flight_or_transport_error_definition:
          "status_zero_or_null_in_flight_or_transport_error_not_confirmed_failure",
        outbox_scope: "current_host_outbox_not_delivery_window",
        outbox_max_rows: 64,
        queued_scope: "current_outbox_rows_not_retained_delivery_window",
        retrying_scope: "current_outbox_rows_not_retained_delivery_window",
        dead_scope: "current_outbox_quarantined_rows_not_delivery_window",
        in_flight_or_transport_error_scope: "retained_window_only_not_lifetime",
      },
      retained_window: {
        start_ms: now - 3_600_000,
        end_ms: now,
        scope: "retained_rows_within_window_lower_bound_not_lifetime",
        max_retained_rows: 20,
        retained_delivery_rows: 4,
        rows_in_window: 4,
        truncation: "unknown_cap_or_lazy_pruning",
        clock_anomaly: false,
      },
      last_accepted_at_ms: now - 10_000,
      last_accepted_scope: "retained_window_only_not_lifetime",
      last_failure_at_ms: now - 20_000,
      last_failure_category: "rate_limited",
      last_failure_scope: "retained_window_only_not_lifetime",
      failure_marker_present: true,
      failure_marker_definition: "marker_presence_not_attempt_outcome",
      retention_window_seconds: 3600,
      timestamp_normalization:
        "delivery_at_unix_seconds_checked_mul_1000_unknown_on_overflow",
      dropped_source: "not_recorded",
      ack_meaning: "sender_http_2xx_receipt_not_human_read",
      delivery_guarantee: false,
    },
  } as OperationalHealth;
}

test("operational panel separates paper finance, liveness, research, calendar and alerts", () => {
  const operational = happyOperational();
  const body = text(
    <OperationalPanel
      summary={summary(operational)}
      portfolio={portfolio()}
      health={health(
        {
          status: "healthy",
          last_completed_at_ms: operational.captured_at_ms,
          age_ms: 5_000,
          max_age_ms: 62_000,
          expected_deadline_ms: operational.captured_at_ms! + 2_000,
          loop_generation: 42,
        },
        operational,
      )}
    />,
  );
  for (const label of [
    "Paper financial status",
    "Controller liveness",
    "Research & advisory",
    "Calendar coverage",
    "Alert transport",
  ])
    assert.ok(body.includes(label), label);
  assert.ok(body.includes("5,000"));
  assert.ok(body.includes("NSE/CMTR/71775"));
  assert.ok(body.includes("2026-12-31 IST"));
  assert.ok(body.includes("Covered"));
  assert.ok(body.includes("63 days"));
  assert.ok(body.includes("3600 s"));
  assert.ok(body.includes("20 rows max"));
  assert.ok(body.includes("Sender HTTP 2xx"));
  assert.ok(body.includes("not a confirmed failure"));
  assert.ok(body.includes("Not recorded (unknown, not zero)"));
  assert.ok(body.includes("Marker presence only"));
  assert.ok(body.includes("No delivery guarantee"));
  assert.ok(
    body.includes(
      "A stale last completed loop is not evidence that the controller is hung.",
    ),
  );
});

test("operational panel keeps corrupt state, unknown reasons and null counts honest", () => {
  const operational: OperationalHealth = {
    version: 2,
    captured_at_ms: now - 10_000,
    calendar: {
      source: "raw source with spaces",
      covered_from: null,
      covered_to: "2026-12-31",
      current_operational_date: "2027-01-01",
      covered: false,
      requested_session: "2027-01-04",
      requested_session_covered: false,
      historical_session: "2026-12-31",
      historical_session_covered: true,
      days_remaining: -4,
      expiring: false,
      expired: true,
      expiry_warning_days: 30,
      expiry_warning_basis: "raw basis",
      update_prerequisite: "raw prerequisite",
    },
    alert_transport: {
      status: "malformed",
      store: "raw store",
      counts_scope: "raw scope",
      scope: {
        account: "private-fixture",
        session: "2026-10-29",
        store_scope: "raw store scope",
      },
      counts: null,
      retained_window: {
        start_ms: now - 3_600_000,
        end_ms: now,
        max_retained_rows: 20,
        retained_delivery_rows: 0,
        rows_in_window: 0,
        clock_anomaly: true,
      },
      last_accepted_at_ms: null,
      last_failure_at_ms: null,
      last_failure_category: "raw failure text",
      failure_marker_present: true,
      failure_marker_definition: "raw marker",
      retention_window_seconds: 3600,
      timestamp_normalization: "raw normalization",
      dropped_source: "raw dropped",
      ack_meaning: "raw ack",
      delivery_guarantee: false,
    },
  } as OperationalHealth;
  const body = text(
    <OperationalPanel
      summary={summary(operational)}
      portfolio={portfolio()}
      health={health(undefined, operational)}
    />,
  );
  assert.ok(body.includes("Unknown"));
  assert.ok(body.includes("Not reported"));
  assert.ok(body.includes("No 2027 data"));
  assert.ok(body.includes("4 days past coverage"));
  assert.ok(body.includes("Counts unavailable"));
  assert.ok(body.includes("not treated as zero"));
  assert.ok(body.includes("Alert transport"));
  assert.ok(body.includes("Marker presence only"));
  assert.ok(body.includes("Not recorded in retained window"));
  assert.ok(body.includes("Clock anomaly: future-dated delivery rows"));
  assert.equal(body.includes("private-fixture"), false);
  assert.equal(body.includes("raw source with spaces"), false);
  assert.equal(body.includes("raw failure text"), false);
});

test("operational panel shows disabled alert transport and expiring calendar warnings", () => {
  const operational: OperationalHealth = {
    version: 2,
    captured_at_ms: now - 5_000,
    calendar: {
      source: "NSE/CMTR/71775",
      covered_from: "2026-01-01",
      covered_to: "2026-12-31",
      current_operational_date: "2026-12-20",
      covered: true,
      requested_session: "2026-12-20",
      requested_session_covered: true,
      historical_session: "2026-12-19",
      historical_session_covered: true,
      days_remaining: 11,
      expiring: true,
      expired: false,
      expiry_warning_days: 30,
      expiry_warning_basis: "operator_chosen_default_not_exchange_guarantee",
      update_prerequisite: "verified_exchange_calendar_update_required",
    },
    alert_transport: {
      status: "disabled",
      store: "alert_outbox",
      counts_scope: "host_alert_outbox_not_account_filtered",
      scope: { account: "private-fixture", session: "2026-10-29" },
      counts: null,
      retained_window: null,
      last_accepted_at_ms: null,
      last_failure_at_ms: null,
      last_failure_category: null,
      failure_marker_present: false,
      retention_window_seconds: 3600,
      delivery_guarantee: false,
    },
  } as OperationalHealth;
  const body = text(
    <OperationalPanel
      summary={summary(operational)}
      portfolio={portfolio()}
      health={health(undefined, operational)}
    />,
  );
  assert.ok(body.includes("Disabled"));
  assert.ok(body.includes("Expiring"));
  assert.ok(body.includes("30-day operator default warning"));
  assert.ok(body.includes("11 days"));
  assert.ok(body.includes("Counts unavailable"));
});

test("operational panel degrades honestly when an older snapshot omits the projection", () => {
  const body = text(
    <OperationalPanel
      summary={summary()}
      portfolio={portfolio()}
      health={health()}
    />,
  );
  assert.ok(body.includes("not present in this snapshot"));
  assert.ok(body.includes("5,000"));
  assert.ok(body.includes("Paper financial status"));
  assert.ok(body.includes("Not reported"));
  assert.equal(body.includes("undefined"), false);
  assert.equal(body.includes("0 alerts"), false);
});

test("operational panel keeps the published capture when rerendered", () => {
  const operational = happyOperational();
  const props = {
    summary: summary(operational),
    portfolio: portfolio(),
    health: health(undefined, operational),
  };
  const first = text(<OperationalPanel {...props} />);
  const second = text(<OperationalPanel {...props} />);
  const stamp =
    new Intl.DateTimeFormat("en-IN", {
      timeZone: "Asia/Kolkata",
      day: "2-digit",
      month: "short",
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    }).format(operational.captured_at_ms!) + " IST";
  assert.ok(first.includes(stamp));
  assert.ok(second.includes(stamp));
  assert.equal(first.includes("Published capture"), true);
  assert.equal(second.includes("Published capture"), true);
});
