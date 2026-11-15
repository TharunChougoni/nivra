import { test } from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { JSDOM } from "jsdom";
import { OperationalPanel } from "./Operational";
import type {
  AlertCounts,
  AlertTransportStatus,
  CalendarCoverage,
  ControllerLiveness,
  HealthResponse,
  OperationalHealth,
  PortfolioResponse,
  SummaryResponse,
  SupervisorOperationalStatus,
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

function supervisor(
  overrides: Record<string, unknown> = {},
): SupervisorOperationalStatus {
  return {
    status: "idle",
    config: "configured",
    config_reason: null,
    config_scope: "loaded_policy_or_static_failure_code",
    state: "present",
    enabled: true,
    parked: null,
    budget: {
      configured_daily_calls: 20,
      charged_calls: 7,
      charged_calls_scope: "provider_usage_projection_not_turn_runs",
      parked_at_calls: null,
      exhausted: false,
      exhausted_scope: "authoritative_current_session_parked_state_only",
    },
    context: null,
    compaction: null,
    backoff: {
      configured_ms: 30_000,
      retry_limit: 2,
      enforced: true,
      enforced_scope: "current_session_context_exhaustion_only",
      active: false,
      active_scope: "authoritative_current_session_state_only",
      earliest_retry_at_ms: null,
    },
    last_completed_run: null,
    pending_source: "no_pending",
    pending_scope: "checked_bounded_session_log",
    pending: null,
    ...overrides,
  } as SupervisorOperationalStatus;
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
    version: 1,
    captured_at_ms: now - 5_000,
    supervisor: supervisor(),
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

const PENDING_SOURCES = [
  "missing",
  "no_pending",
  "pending",
  "corrupt",
  "incomplete",
  "unreadable",
  "oversize",
  "non_regular",
  "symlink",
  "not_checked",
];
const AUTHORITATIVE_PENDING = new Set(["missing", "no_pending"]);

test("pending source matrix: only authoritative absence reads as no pending turn", () => {
  for (const source of PENDING_SOURCES) {
    const pending =
      source === "pending"
        ? { kind: "post_fill", started_at_ms: now - 5_000, age_ms: 5_000 }
        : null;
    const view = panel({
      operational: operational({
        supervisor: supervisor({
          status: source === "pending" ? "pending" : "idle",
          pending_source: source,
          pending,
        }),
      }),
    });
    if (AUTHORITATIVE_PENDING.has(source)) {
      assert.ok(
        view.text.includes("No pending turn reported"),
        `authoritative ${source}`,
      );
      assert.equal(view.text.includes("Unknown pending source"), false, source);
    } else {
      assert.equal(
        view.text.includes("No pending turn reported"),
        false,
        `unavailable ${source} must not claim no pending turn`,
      );
      assert.equal(
        view.text.includes("Idle"),
        false,
        `unavailable ${source} must not read as Idle`,
      );
    }
    if (source === "pending") {
      assert.ok(view.text.includes("Post fill"), "pending evidence");
      assert.ok(view.text.includes("Pending turn present"), "pending source");
    }
    if (source === "corrupt") {
      assert.ok(view.text.includes("Session log corrupt"), "corrupt source");
      assert.ok(view.text.includes("not treated as idle"), "corrupt notice");
    }
    if (source === "not_checked") {
      assert.ok(view.text.includes("Not checked"), "not checked source");
    }
  }
});

test("pending evidence survives an unavailable source without masking it", () => {
  const view = panel({
    operational: operational({
      supervisor: supervisor({
        status: "unknown",
        pending_source: "corrupt",
        pending: {
          kind: "post_fill",
          started_at_ms: now - 5_000,
          age_ms: 5_000,
        },
      }),
    }),
  });
  assert.ok(view.text.includes("Post fill"));
  assert.ok(view.text.includes("Session log corrupt"));
  assert.ok(view.text.includes("evidence"));
  assert.equal(view.text.includes("No pending turn reported"), false);
  assert.equal(view.text.includes("Idle"), false);
});

test("pending source unknown text is never echoed", () => {
  const view = panel({
    operational: operational({
      supervisor: supervisor({
        pending_source: "provider raw text /tmp/private-token",
      }),
    }),
  });
  assert.ok(view.text.includes("Unknown pending source"));
  assert.equal(view.text.includes("provider raw text"), false);
  assert.equal(view.text.includes("/tmp/private-token"), false);
});

test("idle needs present state, configured policy and an authoritative pending source", () => {
  const idle = panel({ operational: operational() });
  assert.equal(idle.fact("Status"), "Idle");
  assert.equal(idle.fact("Pending kind"), "No pending turn reported");

  const missingState = panel({
    operational: operational({
      supervisor: supervisor({
        state: "missing",
        pending_source: "no_pending",
      }),
    }),
  });
  assert.equal(missingState.fact("Status"), "Unknown");
  assert.equal(missingState.text.includes("No pending turn reported"), false);

  const unavailableConfig = panel({
    operational: operational({
      supervisor: supervisor({
        config: "unavailable",
        config_reason: "planner_usage_unavailable",
        pending_source: "no_pending",
      }),
    }),
  });
  assert.equal(unavailableConfig.fact("Status"), "Unknown");
  assert.equal(
    unavailableConfig.text.includes("No pending turn reported"),
    false,
  );
});

test("config unavailable is distinct from a genuinely unconfigured supervisor", () => {
  const unavailable = panel({
    operational: operational({
      supervisor: supervisor({
        status: "unknown",
        config: "unavailable",
        config_reason: "planner_usage_unavailable",
        enabled: false,
        budget: {
          configured_daily_calls: 0,
          charged_calls: 0,
          parked_at_calls: null,
          exhausted: null,
        },
      }),
    }),
  });
  assert.equal(unavailable.fact("Config source"), "Config unavailable");
  assert.equal(unavailable.fact("Config failure"), "Planner usage unavailable");
  assert.equal(unavailable.fact("Enabled"), "Unknown (static fallback)");
  assert.equal(
    unavailable.fact("Configured daily calls"),
    "Unknown (static fallback reports 0)",
  );
  assert.equal(unavailable.text.includes("EnabledNo"), false);
  assert.equal(unavailable.text.includes("Configured daily calls0"), false);
  assert.ok(unavailable.text.includes("not a disabled policy"));
  assert.ok(unavailable.text.includes("not zero"));

  const unconfigured = panel({
    operational: operational({
      supervisor: supervisor({
        status: "disabled",
        config: "not_configured",
        config_reason: null,
        state: "not_configured",
        enabled: null,
        budget: {
          configured_daily_calls: null,
          charged_calls: null,
          parked_at_calls: null,
          exhausted: null,
        },
        pending_source: "not_checked",
      }),
    }),
  });
  assert.equal(unconfigured.fact("Config source"), "Not configured");
  assert.equal(unconfigured.fact("Status"), "Disabled");
  assert.equal(unconfigured.fact("Pending source"), "Not checked");
  assert.equal(unconfigured.text.includes("Config unavailable"), false);
});

test("all six static config failure codes are whitelisted and raw text is dropped", () => {
  const reasons: [string, string][] = [
    ["planner_config_unavailable", "Planner config unavailable"],
    ["planner_usage_unavailable", "Planner usage unavailable"],
    [
      "planner_usage_unavailable_recovery_required",
      "Planner usage unavailable (recovery required)",
    ],
    ["planner_authority_invalid", "Planner authority invalid"],
    [
      "planner_event_configuration_changed",
      "Planner event configuration changed",
    ],
    ["planner_integrity_failure", "Planner integrity failure"],
  ];
  for (const [reason, label] of reasons) {
    const view = panel({
      operational: operational({
        supervisor: supervisor({
          config: "unavailable",
          config_reason: reason,
        }),
      }),
    });
    assert.equal(view.fact("Config failure"), label, reason);
  }
  const unknown = panel({
    operational: operational({
      supervisor: supervisor({
        config: "unavailable",
        config_reason: "provider raw error /tmp/private-token",
      }),
    }),
  });
  assert.equal(unknown.fact("Config failure"), "Unknown config failure");
  assert.equal(unknown.text.includes("provider raw error"), false);
  assert.equal(unknown.text.includes("/tmp/private-token"), false);
});

test("alert outbox 64-row scope is separate from the retained 3600 s / 20-row window", () => {
  const view = panel({ operational: operational() });
  assert.ok(view.text.includes("64 rows max"), "outbox cap");
  assert.ok(view.text.includes("20 rows max"), "retained cap");
  assert.ok(view.text.includes("Current host outbox"), "outbox scope");
  assert.ok(view.text.includes("Retained delivery window"), "retained scope");
  assert.ok(view.text.includes("lower bound"));
  assert.ok(view.text.includes("not lifetime"));
  assert.ok(view.text.includes("Current host outbox rows"));
  assert.ok(view.text.includes("Retained window only; not lifetime"));
  assert.ok(view.text.includes("3600 s"));
  assert.ok(view.text.includes("(retained window)"));
  assert.equal(view.text.includes("private-fixture"), false);

  const missingCap = panel({
    operational: operational({
      alert_transport: alert({ counts: counts({ outbox_max_rows: null }) }),
    }),
  });
  assert.equal(missingCap.text.includes("64 rows max"), false);
  assert.equal(missingCap.fact("Outbox row cap"), "Not reported");

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
  assert.ok(accepted?.includes("(retained window)"), String(accepted));
  assert.ok(failure?.includes("(retained window)"), String(failure));
  assert.ok(failure?.includes("Rate limited"), String(failure));
  assert.equal(view.text.includes("lifetime"), true);
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
  assert.equal(zero.fact("Last completed loop (original)"), "Not reported");
  assert.equal(zero.fact("Published capture"), "Not reported");
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
  assert.equal(
    malformed.fact("Last completed loop (original)"),
    "Not reported",
  );
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
  assert.ok(future.text.includes("Clock anomaly"));
  assert.equal(future.text.includes("Recent capture"), false);
  assert.equal(future.fact("Reported age"), "Not trusted (future clock)");
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
    version: 2,
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
  assert.ok(view.text.includes("version 2 is not supported"));
  assert.ok(view.text.includes("values are not interpreted"));
  assert.equal(view.fact("Published capture"), "Not interpreted");
  assert.equal(view.text.includes("Recent capture"), false);
  assert.equal(view.text.includes("Idle"), false);
  assert.equal(view.text.includes("Verified NSE calendar source"), false);
  assert.equal(view.text.includes("64 rows max"), false);

  const versionless = panel({
    operational: {} as OperationalHealth,
  });
  assert.ok(versionless.text.includes("version is not reported"));
  assert.ok(versionless.text.includes("values are not interpreted"));
  assert.equal(versionless.text.includes("older API"), false);
  assert.equal(versionless.text.includes("Recent capture"), false);
});

test("calendar source is verified only for the known NSE source", () => {
  const known = panel({ operational: operational() });
  assert.ok(known.text.includes("Verified NSE calendar source"));
  assert.ok(known.text.includes("NSE/CMTR/71775"));

  const invalid = panel({
    operational: operational({
      calendar: calendar({ source: "NSE/EVIL/1", covered: true }),
    }),
  });
  assert.equal(invalid.text.includes("Verified NSE calendar source"), false);
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
  assert.ok(outside.text.includes("no holiday is guessed"));
  assert.ok(outside.text.includes("30-day operator default warning"));
});

test("raw config and pending scope text is never rendered", () => {
  const view = panel({
    operational: operational({
      supervisor: supervisor({
        config_scope: "raw config scope /tmp/private-token",
        pending_scope: "raw pending scope /tmp/private-token",
      }),
    }),
  });
  assert.equal(view.text.includes("raw config scope"), false);
  assert.equal(view.text.includes("raw pending scope"), false);
  assert.equal(view.text.includes("/tmp/private-token"), false);
  assert.equal(view.fact("Config scope"), "Not reported");
  assert.equal(view.fact("Pending scope"), "Not reported");
});
