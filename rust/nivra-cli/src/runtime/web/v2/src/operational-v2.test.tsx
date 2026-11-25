import { test } from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { JSDOM } from "jsdom";
import { OperationalPanel } from "./Operational";
import { ageLabel } from "./operational";
import type {
  AlertCounts,
  AlertTransportStatus,
  CalendarCoverage,
  ControllerLiveness,
  HealthResponse,
  OperationalHealth,
  PortfolioResponse,
  SummaryResponse,
} from "./types";

const now = Date.now();
const envelope = {
  v: 1,
  mode: "paper",
  paper_only: true,
  broker: { data: "fixture-broker" },
  account: "private-fixture",
  session: "2026-10-29",
  as_of_ms: 1_700_000_000_000,
};

function summary(
  operational?: OperationalHealth | null,
  state: SummaryResponse["state"] = {
    stopped: false,
    halted: false,
    closed: false,
    health: "ok",
  },
): SummaryResponse {
  return {
    ...envelope,
    capital: "5000",
    cash: "4900",
    equity: "5000",
    state,
    ...(operational ? { operational_health: operational } : {}),
  };
}

function portfolio(
  overrides: Partial<PortfolioResponse> = {},
): PortfolioResponse {
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
    ...overrides,
  };
}

function health(
  liveness?: ControllerLiveness,
  operational?: OperationalHealth | null,
): HealthResponse {
  return {
    ...envelope,
    ...(liveness ? { controller_liveness: liveness } : {}),
    ...(operational ? { operational_health: operational } : {}),
  };
}

function calendar(overrides: Record<string, unknown> = {}): CalendarCoverage {
  return {
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
    ...overrides,
  } as CalendarCoverage;
}

function counts(overrides: Record<string, unknown> = {}): AlertCounts {
  return {
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
    ...overrides,
  } as AlertCounts;
}

function alert(overrides: Record<string, unknown> = {}): AlertTransportStatus {
  return {
    status: "available",
    store: "alert_outbox",
    counts_scope: "host_alert_outbox_not_account_filtered",
    scope: {
      account: "private-fixture",
      session: "2026-10-29",
      store_scope: "host_alert_outbox",
    },
    counts: counts(),
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
    ...overrides,
  } as AlertTransportStatus;
}

function operational(
  overrides: Record<string, unknown> = {},
): OperationalHealth {
  return {
    version: 2,
    captured_at_ms: now - 5_000,
    calendar: calendar(),
    alert_transport: alert(),
    ...overrides,
  } as OperationalHealth;
}

function render(node: React.ReactElement) {
  const dom = new JSDOM(renderToStaticMarkup(node));
  const body = dom.window.document.body;
  return {
    text: body.textContent || "",
    fact(label: string) {
      const dt = [...body.querySelectorAll("dt")].find(
        (element) => element.textContent === label,
      );
      return dt?.nextElementSibling?.textContent ?? null;
    },
  };
}

function panel(options: {
  operational?: OperationalHealth | null;
  state?: SummaryResponse["state"];
  portfolio?: PortfolioResponse;
  liveness?: ControllerLiveness;
}) {
  return render(
    <OperationalPanel
      summary={summary(options.operational, options.state)}
      portfolio={options.portfolio ?? portfolio()}
      health={health(options.liveness, options.operational)}
    />,
  );
}

test("alert outbox 64-row scope is separate from the retained 3600 s / 20-row window", () => {
  const view = panel({ operational: operational() });
  assert.equal(view.fact("Queued now"), "2");
  assert.ok(view.fact("Window")?.startsWith("3600 s"), "window");
  assert.equal(view.text.includes("private-fixture"), false);

  const unavailable = panel({
    operational: operational({
      alert_transport: alert({ counts: null, retained_window: null }),
    }),
  });
  assert.ok(unavailable.text.includes("Counts unavailable"));
  assert.equal(unavailable.text.includes("0 alerts"), false);
});

test("present last accepted/failure timestamps say retained window", () => {
  const view = panel({ operational: operational() });
  const accepted = view.fact("Last accepted");
  const failure = view.fact("Last failure");
  assert.ok(accepted?.includes("Nov"), String(accepted));
  assert.ok(failure?.includes("Nov"), String(failure));
  assert.ok(failure?.includes("Rate limited"), String(failure));
});

test("last completed zero, malformed and future values never become 1970 or recent", () => {
  const zero = panel({
    operational: operational({ captured_at_ms: 0 }),
    liveness: {
      status: "unknown",
      last_completed_at_ms: 0,
      age_ms: null,
      max_age_ms: 62_000,
      expected_deadline_ms: null,
      loop_generation: 1,
    },
  });
  assert.equal(zero.fact("Last loop finished"), "Not reported");
  assert.equal(zero.fact("Snapshot taken"), "Not reported");
  assert.equal(zero.text.includes("1970"), false);

  const malformed = panel({
    operational: operational(),
    liveness: {
      status: "malformed",
      last_completed_at_ms: "not-a-date" as unknown as number,
      age_ms: null,
      max_age_ms: 62_000,
      expected_deadline_ms: null,
      loop_generation: 1,
    },
  });
  assert.equal(malformed.fact("Last loop finished"), "Not reported");
  assert.equal(malformed.text.includes("1970"), false);

  const future = panel({
    operational: operational({ captured_at_ms: 0 }),
    liveness: {
      status: "future",
      last_completed_at_ms: now + 60_000,
      age_ms: -60_000,
      max_age_ms: 62_000,
      expected_deadline_ms: null,
      loop_generation: 1,
    },
  });
  assert.ok(future.text.includes("ahead of this browser"));
  assert.equal(future.text.includes("Recent capture"), false);
  assert.equal(future.fact("Loop age"), "Not trusted (future clock)");
});

test("a newer summary loop time replaces a held health read and ages locally", () => {
  const liveness: ControllerLiveness = {
    status: "healthy",
    last_completed_at_ms: envelope.as_of_ms - 60_000,
    age_ms: 250,
    max_age_ms: 62_000,
    expected_deadline_ms: null,
    loop_generation: 7,
  };
  const view = panel({ operational: operational(), liveness });
  assert.notEqual(view.fact("Loop age"), ageLabel(250));
  assert.equal(
    view.fact("Last loop finished"),
    panel({
      operational: operational(),
      liveness: { ...liveness, last_completed_at_ms: envelope.as_of_ms },
    }).fact("Last loop finished"),
  );
});

test("book state uses portfolio-first projectedStatus and never shows a false open", () => {
  const stopped = panel({
    operational: operational(),
    state: { stopped: false, halted: false, closed: false, health: "ok" },
    portfolio: portfolio({ stopped: true }),
  });
  assert.equal(stopped.fact("Book state"), "Stopped");
  assert.equal(stopped.text.includes("Open snapshot"), false);

  const halted = panel({
    operational: operational(),
    portfolio: portfolio({ halted: true }),
  });
  assert.equal(halted.fact("Book state"), "Halted");

  const closed = panel({
    operational: operational(),
    portfolio: portfolio({ closed: true }),
  });
  assert.equal(closed.fact("Book state"), "Closed");

  const summaryOnly = panel({
    operational: operational(),
    state: { stopped: true, halted: false, closed: false, health: "ok" },
    portfolio: portfolio({
      stopped: undefined,
      halted: undefined,
      closed: undefined,
    }),
  });
  assert.equal(summaryOnly.fact("Book state"), "Stopped");
});

test("unsupported and versionless operational health are not interpreted", () => {
  const unsupported = operational({
    // version 1 is the retired shape
    version: 1,
    captured_at_ms: now - 5_000,
  });
  const view = panel({
    operational: unsupported,
    liveness: {
      status: "healthy",
      last_completed_at_ms: now - 5_000,
      age_ms: 5_000,
      max_age_ms: 62_000,
      expected_deadline_ms: now + 2_000,
      loop_generation: 1,
    },
  });
  assert.ok(view.text.includes("format 1 is not supported"));
  assert.ok(view.text.includes("Values are hidden"));
  assert.equal(view.fact("Snapshot taken"), "Not shown");
  assert.equal(view.text.includes("Recent capture"), false);
  assert.equal(view.text.includes("Idle"), false);
  assert.equal(view.text.includes("(verified)"), false);

  const versionless = panel({
    operational: {} as OperationalHealth,
  });
  assert.ok(versionless.text.includes("format is not reported"));
  assert.ok(versionless.text.includes("Values are hidden"));
  assert.equal(versionless.text.includes("older API"), false);
  assert.equal(versionless.text.includes("Recent capture"), false);
});

test("calendar source is verified only for the known NSE source", () => {
  const known = panel({ operational: operational() });
  assert.ok(known.text.includes("NSE/CMTR/71775 (verified)"));

  const invalid = panel({
    operational: operational({
      calendar: calendar({ source: "NSE/EVIL/1", covered: true }),
    }),
  });
  assert.equal(invalid.text.includes("(verified)"), false);
  assert.equal(invalid.text.includes("NSE/EVIL/1"), false);
  assert.ok(invalid.text.includes("Not verified"));

  const outside = panel({
    operational: operational({
      calendar: calendar({
        current_operational_date: "2027-01-01",
        covered: false,
        requested_session: "2027-01-04",
        requested_session_covered: false,
        days_remaining: -4,
        expired: true,
        expiring: true,
        expiry_warning_days: 30,
      }),
    }),
  });
  assert.ok(outside.text.includes("No 2027 data"));
  assert.ok(outside.text.includes("outside verified coverage"));
});
