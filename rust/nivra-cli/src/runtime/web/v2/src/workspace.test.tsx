import type {
  Fetch,
  SummaryResponse,
  PortfolioResponse,
  ScannerResponse,
  InstrumentCatalogResponse,
  ContractChainResponse,
  FillsResponse,
  PlansResponse,
  SessionsResponse,
  HealthResponse,
  AlertsResponse,
  ToolsResponse,
  OperationalHealth,
} from "./types";
import { test } from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { JSDOM } from "jsdom";
import { createController } from "./controller";
const loadApp = async () => {
  const [{ default: App }, { Context }] = await Promise.all([
    import("./App"),
    import("./ui"),
  ]);
  return { App, Context };
};
const pause = () => new Promise((r) => setTimeout(r, 10));
const until = async (fn: () => unknown) => {
  for (let i = 0; i < 200; i++) {
    if (fn()) return;
    await pause();
  }
  throw Error("UI timed out");
};
const json = (value: unknown) =>
  new Response(JSON.stringify(value), {
    headers: { "Content-Type": "application/json" },
  });
const envelope = {
  v: 1,
  mode: "paper",
  paper_only: true,
  broker: { data: "fixture-broker" },
  account: "private-fixture",
  session: "2026-10-29",
  as_of_ms: 1_700_000_000_000,
};
const summary: SummaryResponse = {
  ...envelope,
  research_library: {
    status: "ready",
    activation_authorized: false,
    items: [
      {
        candidate_id: "fixture-candidate",
        session: "2026-10-29",
        state: "analysis_cancelled",
        hypothesis: "Original registered analysis",
        source_sha256: "fixture-source-hash",
        result_sha256: null,
        scientific_state: "insufficient_evidence",
      },
    ],
  },
  capital: "5000",
  cash: "5000",
  equity: "5000",
  status: "open",
  state: { stopped: false, halted: false, closed: false },
  readiness: {
    status: "ready",
    reason: "controller_ready",
    history: { status: "READY", ready_instruments: 1, instruments: 1 },
  },
  market_feed: { mode: "paper", state: "live" },
  planner: { status: "ready", decision_id: null, pending_events: 0 },
};
const portfolio = {
  ...envelope,
  capital: "5000",
  cash: "4900",
  equity: "5000",
  positions: [
    {
      instrument: { key: "TEST|1", symbol: "TEST", kind: "index" },
      units: "1",
      entry: "90",
      mark: "100",
      decision_id: "d1",
      marked_at: envelope.as_of_ms,
    },
    {
      instrument: { key: "OPT|1", symbol: "OPT", kind: "call" },
      units: "1",
      entry: "10",
      mark: "10",
      decision_id: "d2",
      marked_at: envelope.as_of_ms,
    },
  ],
  fills_count: 1,
  recorded_fees: "1",
  stopped: false,
  halted: false,
  closed: false,
};
const scanner = {
  ...envelope,
  segment: "prospects",
  total: 1,
  items: [
    {
      instrument_key: "TEST|1",
      symbol: "TEST",
      latest_screening_price: "100",
      sampled_price_ms: 1_700_000_000_000,
      observed_ms: 1_700_000_000_100,
      sample_count: 2,
      screening_reason: "fresh",
      strategy_spec_id: "champion",
      strategy_spec_version: 2,
      strategy_spec_hash: "spec-hash",
      strategy_scan_id: "scan-1",
      strategy_score: 0.75,
    },
  ],
  coverage: {
    fresh: 1,
    stale: 0,
    unseen: 0,
    eligible: 1,
    presented: 1,
    universe: 1,
  },
  screen_reasons: { fresh: 1 },
};
const splits: Record<
  string,
  | SummaryResponse
  | PortfolioResponse
  | ScannerResponse
  | InstrumentCatalogResponse
  | ContractChainResponse
  | FillsResponse
  | PlansResponse
  | SessionsResponse
  | HealthResponse
  | AlertsResponse
  | ToolsResponse
> = {
  "/api/v1/summary": summary,
  "/api/v1/portfolio": portfolio,
  "/api/v1/fills": {
    ...envelope,
    total: 1,
    items: [
      {
        instrument: { key: "TEST|1", symbol: "TEST", kind: "index" },
        buy: true,
        units: "1",
        price: "90",
        expected_price: "90",
        fee: "1",
        decision_id: "d1",
        at: envelope.as_of_ms - 1000,
      },
    ],
    next: null,
  },
  "/api/v1/plans": {
    ...envelope,
    planner: { status: "ready", decision_id: "d1", pending_events: 0 },
    actions: [
      {
        index: 0,
        action: { instrument_key: "TEST|1", side: "buy" },
        units: "1",
        reason: "accepted",
      },
    ],
    execution: [{ decision_id: "d1", index: 0, verdict: "filled" }],
  },
  "/api/v1/scanner": scanner,
  "/api/v1/instruments": {
    ...envelope,
    source: "session_nse_bod",
    coverage_kind: "contract_metadata_only",
    segments: { NSE_EQ: 1, NSE_FO: 1 },
    kinds: { cash: 1, future: 1 },
    rows_seen: 2,
    rejected_rows: 0,
    segment: "NSE_EQ",
    series: "EQ",
    file_date: "2026-10-28",
    received_at_ms: envelope.as_of_ms,
    source_hashes: {
      nse_cm_security_sha256: "fixture-security-hash",
      upstox_nse_bod_sha256: "fixture-broker-hash",
      upstox_suspended_bod_sha256: "fixture-suspended-hash",
    },
    total: 2,
    items: [
      {
        instrument_key: "NSE_EQ|INE00U801010",
        symbol: "CATALOG",
        kind: "cash",
        lot_size: 1,
        segment: "NSE_EQ",
        series: "EQ",
        tick: "0.05",
        tick_unit: "INR",
      },
      {
        instrument_key: "NSE_FO|SYNTHETIC-F",
        symbol: "FIXTURE FUTURE",
        segment: "NSE_FO",
        kind: "future",
        lot_size: 25,
        underlying_key: "NSE_EQ|SYNTHETIC-U",
        expiry_ms: 1800000000000,
        provider_tick: "5",
        cash_execution_verified: false,
      },
    ],
    next: null,
  },
  "/api/v1/instruments/chain": {
    ...envelope,
    source: "session_nse_bod",
    received_at_ms: envelope.as_of_ms,
    total: 1,
    items: [
      {
        strike: "100",
        call: {
          instrument_key: "NSE_FO|SYNTHETIC-C",
          symbol: "FIXTURE CALL",
          segment: "NSE_FO",
          kind: "call",
          lot_size: 25,
        },
        put: null,
      },
    ],
    futures: [],
    futures_total: 0,
    unavailable_strikes: 0,
    next: null,
    display_only: true,
  },
  "/api/v1/sessions": {
    ...envelope,
    total: 3,
    items: [
      {
        id: {
          broker: "fixture-broker",
          account: "private-fixture",
          date: "2026-10-28",
        },
        paper_only: true,
        live_enabled: false,
        status: "archived_snapshot",
        capital: "5000",
        portfolio: {
          cash: "4990",
          equity: "4990",
          positions: {},
          fills: [],
          stopped: false,
          halted: false,
          closed: true,
        },
        stale: true,
        provenance: "declared_report_hash_checked",
      },
      {
        id: {
          broker: "fixture-broker",
          account: "private-fixture",
          date: "2026-10-27",
        },
        status: "archived_snapshot",
        capital: "5000",
        stale: true,
      },
    ],
    next: null,
  },
  "/api/v1/tools": {
    v: 1,
    session: "2026-10-29",
    total: 1,
    calls: [
      {
        v: 1,
        at_ms: envelope.as_of_ms,
        session: "2026-10-29",
        run: "harness-decision-fixture",
        turn: 0,
        call: "call_one",
        tool: "web_search",
        provider: "searxng",
        query: "nifty bank",
        url: null,
        status: "ok",
        reason: null,
        latency_ms: 42,
        bytes: 512,
        results: 3,
        execution_authorized: false,
      },
    ],
    next: null,
    execution_authorized: false,
  },
  "/api/v1/health": {
    ...envelope,
    live_enabled: false,
    execution_authority: false,
    readiness: { status: "ready", reason: "controller_ready" },
    activity_attribution: {
      session: "2026-10-29",
      conversion: {
        candidates_evaluated: 12,
        decisions_considered: 3,
        actions_proposed: 2,
        actions_validated: 1,
        fills_executed: 1,
        overall_conversion_rate: 1 / 12,
      },
      financials: {
        realized_pnl: "12.50",
        marked_pnl: "2.00",
        total_fees: "1.00",
        total_slippage: "0.50",
        net_after_cost_pnl: "13.00",
      },
      turnover: { notional_traded: "1000", total_cost_drag_bps: 15 },
      drawdown: {
        current_drawdown: "0",
        max_drawdown: "5.00",
        max_drawdown_pct: 0.01,
      },
      latency: { p50_ms: 500, p95_ms: 1000 },
      experiment_attributions: [
        {
          source_hash: "fixture-source-hash",
          spec_id: "fixture-spec",
          decisions_count: 2,
          fills_count: 1,
          net_after_cost_pnl: "13.00",
          trial_status: "reviewed",
        },
      ],
    },
    history_preparation: {
      status: "READY",
      ready_instruments: 1,
      instruments: 1,
      target_session: "2026-10-29",
    },
    market_feed: { mode: "paper", state: "live" },
    alert_count: 0,
  },
  "/api/v1/alerts": { ...envelope, total: 0, items: [], next: null },
};
function fetchFixture(
  extra: Record<string, (url: string) => Response | Promise<Response>> = {},
): Fetch {
  return async (url: string) => {
    const path = String(url).split("?")[0];
    if (extra[path]) return extra[path](url);
    if (url === "/api/v1/meta")
      return json({
        mode: "paper",
        paper_only: true,
        account: "private-fixture",
        broker: { data: "fixture-upstox", orders: "paper" },
      });
    if (path.startsWith("/api/v1/scanner"))
      return json({
        ...scanner,
        segment:
          new URLSearchParams(String(url).split("?")[1] || "").get("segment") ||
          scanner.segment,
        ...(path === "/api/v1/scanner" &&
        String(url).includes("segment=screens")
          ? { items: [{ reason: "fresh", total: 1 }] }
          : {}),
      });
    if (splits[path]) return json(splits[path]);
    return json({});
  };
}
test("client renders every section, archived books and private lock state", async () => {
  const dom = new JSDOM('<div id="root"></div>', {
    url: "http://localhost/",
    pretendToBeVisual: true,
  });
  for (const key of [
    "window",
    "document",
    "Node",
    "HTMLElement",
    "Element",
    "Event",
    "CustomEvent",
    "MutationObserver",
    "getComputedStyle",
    "location",
    "localStorage",
    "NodeFilter",
  ])
    Object.defineProperty(globalThis, key, {
      value:
        key === "getComputedStyle"
          ? dom.window.getComputedStyle.bind(dom.window)
          : dom.window[key],
      configurable: true,
      writable: true,
    });
  globalThis.addEventListener = dom.window.addEventListener.bind(dom.window);
  globalThis.removeEventListener = dom.window.removeEventListener.bind(
    dom.window,
  );
  const { App, Context } = await loadApp();
  const { createRoot } = await import("react-dom/client");
  const requests: string[] = [];
  const fetch = fetchFixture();
  const app = createController(dom.window, {
    connect: false,
    fetch: async (url) => {
      requests.push(String(url));
      return fetch(url);
    },
  });
  const root = createRoot(document.getElementById("root")!);
  root.render(
    <Context.Provider value={app}>
      <App />
    </Context.Provider>,
  );
  try {
    await until(() => document.querySelector("#token"));
    await app.authenticate("fixture-only");
    await until(() =>
      document.querySelector(".kpis")?.textContent?.includes("5,000"),
    );
    assert.equal(document.querySelectorAll(".nav a").length, 9);
    assert.equal(document.querySelector('.nav a[href="#/chat"]'), null);
    assert.equal(
      [...document.querySelectorAll(".nav a")].some((v) =>
        v.textContent?.includes("Chat"),
      ),
      false,
    );
    assert.equal(
      requests.some((url) => url.startsWith("/api/status")),
      false,
    );
    for (const section of [
      "lanes",
      "portfolio",
      "sessions",
      "scanner",
      "instruments",
      "strategies",
      "backtest",
      "health",
    ]) {
      location.hash = "#/" + section;
      await until(
        () =>
          document
            .querySelector(`.nav a[href="#/${section}"]`)
            ?.getAttribute("aria-current") === "page",
      );
      if (section === "lanes") {
        await until(() =>
          document.body.textContent?.includes("Web tool activity"),
        );
        await until(() => document.body.textContent?.includes("nifty bank"));
        assert.ok(document.body.textContent?.includes("42 ms"));
        assert.ok(requests.some((url) => url.startsWith("/api/v1/tools?")));
        assert.equal(
          requests.some((url) =>
            /\/api\/v1\/(runs|approvals|session)(\?|\/|$)/.test(url),
          ),
          false,
        );
      }
      if (section === "scanner") {
        await until(() => document.querySelector(".row-link"));
        // The metadata catalog moved to its own page.
        assert.equal(
          document.body.textContent?.includes("Instrument catalog"),
          false,
        );
      }
      if (section === "instruments") {
        await until(() =>
          document.body.textContent?.includes("Instrument catalog"),
        );
        await until(() => document.body.textContent?.includes("CATALOG"));
        assert.ok(document.body.textContent?.includes("fixture-broker-hash"));
        const future = [
          ...document.querySelectorAll<HTMLButtonElement>(".row-link"),
        ].find((button) => button.textContent?.includes("FIXTURE FUTURE"));
        assert.ok(future);
        future.click();
        await until(() =>
          document.body.textContent?.includes("F&O contract chain"),
        );
        await until(() => document.body.textContent?.includes("FIXTURE CALL"));
        assert.ok(document.body.textContent?.includes("25 units / lot"));
        assert.ok(document.body.textContent?.includes("Not captured"));
        assert.ok(
          requests.some(
            (url) =>
              url.includes("/instruments/chain?") &&
              url.includes("expiry_ms=1800000000000"),
          ),
        );
        document
          .querySelector<HTMLButtonElement>('[aria-label="Close detail"]')!
          .click();
      }
      if (section === "strategies") {
        await until(() =>
          document.body.textContent?.includes("1 recorded strategy reference"),
        );
        assert.ok(document.body.textContent?.includes("champion"));
        assert.ok(document.body.textContent?.includes("spec-hash"));
        assert.ok(document.body.textContent?.includes("scan-1"));
        const library = [
          ...document.querySelectorAll<HTMLButtonElement>("button"),
        ].find((button) => button.textContent === "Library & switches");
        assert.ok(library);
        library.dispatchEvent(
          new dom.window.MouseEvent("mousedown", {
            bubbles: true,
            cancelable: true,
            button: 0,
          }),
        );
        library.click();
        await until(() =>
          document.body.textContent?.includes("fixture-candidate"),
        );
        assert.ok(
          document.body.textContent?.includes("Original registered analysis"),
        );
        assert.ok(document.body.textContent?.includes("fixture-source-hash"));
        assert.ok(
          document.body.textContent?.includes("No automatic promotion"),
        );
      }
      if (section === "sessions") {
        await until(() => document.querySelector(".row-link"));
        const sessionTable = [...document.querySelectorAll("table")].find(
          (table) =>
            [...table.querySelectorAll("th")].some(
              (th) => th.textContent === "Mode",
            ),
        );
        assert.ok(sessionTable);
        const modeIndex = [...sessionTable.querySelectorAll("th")].findIndex(
          (th) => th.textContent === "Mode",
        );
        const modes = [...sessionTable.querySelectorAll("tbody tr")].map(
          (row) => row.children[modeIndex]?.textContent,
        );
        assert.ok(modes.includes("PAPER"));
        assert.ok(modes.includes("Not reported"));
        document.querySelector<HTMLButtonElement>(".row-link")!.click();
        await until(() => document.querySelector(".drawer"));
        assert.equal(
          requests.some((url) => url.startsWith("/api/v1/session?")),
          false,
        );
        document
          .querySelector<HTMLButtonElement>('[aria-label="Close detail"]')!
          .click();
      }
      if (section === "health") {
        await until(() =>
          document.body.textContent?.includes("fixture-source-hash"),
        );
        assert.ok(document.body.textContent?.includes("Activity attribution"));
        assert.ok(document.body.textContent?.includes("Net after cost"));
        assert.ok(document.body.textContent?.includes("13.00"));
        assert.ok(document.body.textContent?.includes("Live activation"));
        assert.ok(document.body.textContent?.includes("Disabled"));
        assert.ok(document.body.textContent?.includes("Order broker"));
        assert.ok(document.body.textContent?.includes("Display only"));
        const bar = document.querySelector(".statusbar")?.textContent || "";
        assert.ok(bar.includes("fixture-upstox"));
        assert.ok(bar.includes("PAPER"));
        // The paper ledger is the explicit order authority, not "Not reported".
        assert.match(bar, /orders\s*paper/);
      }
    }
    location.hash = "#/portfolio";
    await until(
      () => document.querySelector("h1")?.textContent === "Portfolio & fills",
    );
    await until(() =>
      document
        .querySelector('[aria-label="Instrument class"]')
        ?.textContent?.includes("Index"),
    );
    assert.ok(
      document
        .querySelector('[aria-label="Instrument class"]')!
        .textContent?.includes("Call"),
    );
    const select = document.querySelector<HTMLSelectElement>(
      '[aria-label="Selected session"]',
    );
    assert.ok(select);
    select.value = "fixture-broker|private-fixture|2026-10-28";
    select.dispatchEvent(new dom.window.Event("change", { bubbles: true }));
    await until(() =>
      document.querySelector(".kpis")?.textContent?.includes("4,990"),
    );
    app.lock();
    await until(() => document.querySelector("#token"));
    assert.equal(document.body.textContent?.includes("private-fixture"), false);
    assert.equal(document.body.textContent?.includes("4,990"), false);
  } finally {
    app.stop();
    root.unmount();
    dom.window.close();
  }
});
test("operational panel keeps the published capture and separates operational evidence", async () => {
  const dom = new JSDOM('<div id="root"></div>', {
    url: "http://localhost/",
    pretendToBeVisual: true,
  });
  for (const key of [
    "window",
    "document",
    "Node",
    "HTMLElement",
    "Element",
    "Event",
    "CustomEvent",
    "MutationObserver",
    "getComputedStyle",
    "location",
    "localStorage",
    "NodeFilter",
  ])
    Object.defineProperty(globalThis, key, {
      value:
        key === "getComputedStyle"
          ? dom.window.getComputedStyle.bind(dom.window)
          : dom.window[key],
      configurable: true,
      writable: true,
    });
  globalThis.addEventListener = dom.window.addEventListener.bind(dom.window);
  globalThis.removeEventListener = dom.window.removeEventListener.bind(
    dom.window,
  );
  const { App, Context } = await loadApp();
  const { createRoot } = await import("react-dom/client");
  const captured = envelope.as_of_ms;
  const operational: OperationalHealth = {
    version: 2,
    captured_at_ms: captured,
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
    },
    alert_transport: {
      status: "available",
      counts: {
        queued: 2,
        retrying: 1,
        transport_accepted: 3,
        dead: 0,
        dropped: null,
        in_flight_or_transport_error: 1,
        outbox_scope: "current_host_outbox_not_delivery_window",
        outbox_max_rows: 64,
        queued_scope: "current_outbox_rows_not_retained_delivery_window",
        retrying_scope: "current_outbox_rows_not_retained_delivery_window",
        dead_scope: "current_outbox_quarantined_rows_not_delivery_window",
        in_flight_or_transport_error_scope: "retained_window_only_not_lifetime",
      },
      retained_window: {
        start_ms: captured - 3_600_000,
        end_ms: captured,
        max_retained_rows: 20,
        retained_delivery_rows: 4,
        rows_in_window: 4,
        clock_anomaly: false,
      },
      last_accepted_at_ms: captured - 10_000,
      last_failure_at_ms: captured - 20_000,
      last_failure_category: "rate_limited",
      failure_marker_present: true,
      retention_window_seconds: 3600,
      delivery_guarantee: false,
    },
  } as OperationalHealth;
  const summaryWithOperational: SummaryResponse = {
    ...summary,
    operational_health: operational,
  };
  const healthWithLiveness: HealthResponse = {
    ...envelope,
    live_enabled: false,
    execution_authority: false,
    readiness: { status: "ready", reason: "controller_ready" },
    controller_liveness: {
      status: "healthy",
      last_completed_at_ms: captured,
      age_ms: 0,
      max_age_ms: 62_000,
      expected_deadline_ms: captured + 2_000,
      loop_generation: 42,
    },
    operational_health: operational,
    alert_count: 0,
  };
  const requests: string[] = [];
  const fetch = fetchFixture({
    "/api/v1/summary": () => json(summaryWithOperational),
    "/api/v1/health": () => json(healthWithLiveness),
  });
  const app = createController(dom.window, {
    connect: false,
    fetch: async (url) => {
      requests.push(String(url));
      return fetch(url);
    },
  });
  const root = createRoot(document.getElementById("root")!);
  root.render(
    <Context.Provider value={app}>
      <App />
    </Context.Provider>,
  );
  try {
    await app.authenticate("fixture-only");
    location.hash = "#/health";
    await until(() =>
      document.body.textContent?.includes("Operational health"),
    );
    await until(() => document.body.textContent?.includes("Sender HTTP 2xx"));
    for (const label of [
      "Paper financial status",
      "Controller liveness",
      "Research & advisory",
      "Calendar coverage",
      "Alert transport",
    ])
      assert.ok(document.body.textContent?.includes(label), label);
    assert.ok(document.body.textContent?.includes("5,000"));
    assert.ok(document.body.textContent?.includes("Sender HTTP 2xx"));
    assert.ok(document.body.textContent?.includes("not a confirmed failure"));
    assert.ok(
      document.body.textContent?.includes("Not recorded (unknown, not zero)"),
    );
    const capturedText = () =>
      [...document.querySelectorAll("dt")].find(
        (dt) => dt.textContent === "Published capture",
      )?.nextElementSibling?.textContent;
    const before = capturedText();
    assert.ok(before);
    const summaryReads = requests.filter(
      (url) => url === "/api/v1/summary",
    ).length;
    await app.refresh();
    await until(
      () =>
        requests.filter((url) => url === "/api/v1/summary").length >
        summaryReads,
    );
    assert.equal(capturedText(), before);
    assert.ok(document.body.textContent?.includes("5,000"));
  } finally {
    app.stop();
    root.unmount();
    dom.window.close();
  }
});

test("controller starting renders a retrying loading state instead of unavailable evidence", async () => {
  const dom = new JSDOM('<div id="root"></div>', {
    url: "http://localhost/",
    pretendToBeVisual: true,
  });
  for (const key of [
    "window",
    "document",
    "Node",
    "HTMLElement",
    "Element",
    "Event",
    "CustomEvent",
    "MutationObserver",
    "getComputedStyle",
    "location",
    "localStorage",
    "NodeFilter",
  ])
    Object.defineProperty(globalThis, key, {
      value:
        key === "getComputedStyle"
          ? dom.window.getComputedStyle.bind(dom.window)
          : dom.window[key],
      configurable: true,
      writable: true,
    });
  globalThis.addEventListener = dom.window.addEventListener.bind(dom.window);
  globalThis.removeEventListener = dom.window.removeEventListener.bind(
    dom.window,
  );
  const { App, Context } = await loadApp();
  const { createRoot } = await import("react-dom/client");
  const requests: string[] = [];
  let mode: "starting" | "ready" | "unavailable" = "starting";
  let summaryReads = 0;
  const unavailable = (reason: string) => ({
    ...envelope,
    unavailable: true,
    reason,
  });
  const fetch = fetchFixture({
    "/api/v1/summary": () => {
      if (mode === "starting" && ++summaryReads > 1) mode = "ready";
      return json(
        mode === "starting"
          ? {
              ...summary,
              status: "starting",
              state: { ...summary.state, phase: "starting" },
            }
          : summary,
      );
    },
    "/api/v1/portfolio": () =>
      json(
        mode === "ready" ? portfolio : unavailable("portfolio_not_reported"),
      ),
    "/api/v1/fills": () =>
      json(
        mode === "ready"
          ? splits["/api/v1/fills"]
          : unavailable("fills_not_reported"),
      ),
    "/api/v1/scanner": () =>
      json(mode === "ready" ? scanner : unavailable("scanner_not_reported")),
  });
  const app = createController(dom.window, {
    connect: false,
    fetch: async (url) => {
      requests.push(String(url));
      return fetch(url);
    },
  });
  const root = createRoot(document.getElementById("root")!);
  root.render(
    <Context.Provider value={app}>
      <App />
    </Context.Provider>,
  );
  const waitFor = async (fn: () => unknown) => {
    for (let i = 0; i < 400; i++) {
      if (fn()) return;
      await pause();
    }
    throw Error("UI timed out");
  };
  try {
    await app.authenticate("fixture-only");
    // Startup is a loading state, never "missing evidence".
    await waitFor(() =>
      document.body.textContent?.includes("Controller starting"),
    );
    assert.equal(
      document.body.textContent?.includes("Unavailable v1 evidence"),
      false,
    );
    // The page retries by itself and renders the evidence once it is published.
    await waitFor(() => document.body.textContent?.includes("4,900"));
    assert.equal(
      document.body.textContent?.includes("Controller starting"),
      false,
    );
    assert.equal(app.getSnapshot().snapshotState, "ready");
    assert.ok(requests.filter((url) => url === "/api/v1/summary").length >= 2);
    // A running controller whose source is genuinely unavailable keeps the
    // explicit unavailable notice.
    mode = "unavailable";
    await app.refresh();
    await waitFor(() =>
      document.body.textContent?.includes("Unavailable v1 evidence"),
    );
    assert.ok(document.body.textContent?.includes("portfolio_not_reported"));
  } finally {
    app.stop();
    root.unmount();
    dom.window.close();
  }
});
const GLOBALS = [
  "window",
  "document",
  "Node",
  "HTMLElement",
  "Element",
  "Event",
  "CustomEvent",
  "MutationObserver",
  "getComputedStyle",
  "location",
  "localStorage",
  "NodeFilter",
] as const;
test("#/lanes shows lane cards, the turn table and the turn drawer from the journal", async () => {
  const dom = new JSDOM('<div id="root"></div>', {
    url: "http://localhost/#/lanes",
    pretendToBeVisual: true,
  });
  for (const key of GLOBALS)
    Object.defineProperty(globalThis, key, {
      value:
        key === "getComputedStyle"
          ? dom.window.getComputedStyle.bind(dom.window)
          : dom.window[key],
      configurable: true,
      writable: true,
    });
  globalThis.addEventListener = dom.window.addEventListener.bind(dom.window);
  globalThis.removeEventListener = dom.window.removeEventListener.bind(
    dom.window,
  );
  const { App, Context } = await loadApp();
  const { createRoot } = await import("react-dom/client");
  const turns = [
    {
      turn_id: "gate-1",
      lane: "gate",
      at_ms: envelope.as_of_ms - 2000,
      status: "ok",
      model: "gate-model",
      elapsed_ms: 900,
      usage: {
        input: 100,
        output: 20,
        cache_read: 5,
        cost_usd: 0.02,
        calls: 1,
      },
      trigger: { kind: "price_move", instruments: ["NSE_EQ|RELIANCE"] },
      verdict: { escalate: true, reason: "spread tight" },
    },
    {
      turn_id: "decision-1",
      lane: "decision",
      at_ms: envelope.as_of_ms - 1000,
      status: "failed",
      error: "provider timeout",
      model: "primary-model",
      fallback: { to: { id: "backup-model" }, reason: "timeout" },
      elapsed_ms: 4100,
      usage: {
        input: 900,
        output: 300,
        cache_read: 50,
        cost_usd: 0.31,
        calls: 2,
      },
      tool_calls: [
        { name: "positions", ok: true },
        { name: "quote", ok: false, error: "stale receipt" },
      ],
      plan: { status: "validated", stance: "hold", actions: 0 },
    },
  ];
  const fetch = fetchFixture({
    // Only the decision lane reports a budget; gate's stays unknown.
    "/api/v1/health": () =>
      json({
        ...envelope,
        readiness: {
          models: {
            roles: [
              {
                role: "decision",
                model: "primary-model",
                provider: "router",
                status: "unknown",
                daily_call_budget: 60,
                daily_cost_usd: 4,
                current_usage: 2,
                current_cost_usd: 0.31,
              },
            ],
          },
        },
      }),
    "/api/v1/agent": () =>
      json({
        v: 1,
        turns,
        total: 2,
        triggers: { gate_escalated: 1 },
        usage: {
          lanes: {
            gate: {
              input: 100,
              output: 20,
              cache_read: 5,
              cost_usd: 0.02,
              calls: 1,
            },
            decision: {
              input: 900,
              output: 300,
              cache_read: 50,
              cost_usd: 0.31,
              calls: 2,
            },
          },
        },
        execution_authorized: false,
      }),
  });
  const app = createController(dom.window, { connect: false, fetch });
  const root = createRoot(document.getElementById("root")!);
  root.render(
    <Context.Provider value={app}>
      <App />
    </Context.Provider>,
  );
  try {
    await app.authenticate("fixture-only");
    await until(() => document.querySelectorAll(".lane-card").length === 4);
    const cards = [...document.querySelectorAll(".lane-card")].map(
      (c) => c.textContent || "",
    );
    // fixed lane order, totals from the API usage block
    assert.ok(cards[0].startsWith("decision"));
    assert.ok(cards[0].includes("$0.31"));
    assert.ok(cards[0].includes("1 · 1"), "failed and fallback counts");
    await until(() =>
      (document.querySelector(".lane-card")?.textContent || "").includes(
        "2 of 60",
      ),
    );
    const budgeted =
        document.querySelectorAll(".lane-card")[0].textContent || "",
      unbudgeted = document.querySelectorAll(".lane-card")[1].textContent || "";
    assert.ok(
      budgeted.includes("$0.31 of $4.00"),
      "spend against daily budget",
    );
    assert.ok(unbudgeted.includes("not reported"), "absent budget is not zero");
    assert.ok(cards[1].startsWith("gate"));
    // a lane with no turns keeps unknown latency instead of 0 ms
    assert.ok(cards[2].includes("— / —"));
    const rows = [...document.querySelectorAll("tbody tr")].filter((r) =>
      r.textContent?.includes("primary-model"),
    );
    assert.equal(rows.length, 1);
    assert.ok(rows[0].textContent?.includes("fallback"));
    assert.ok(rows[0].querySelector(".tool-failed"));
    rows[0].querySelector<HTMLButtonElement>(".row-link")!.click();
    await until(() => document.querySelector(".drawer"));
    const drawer = document.querySelector(".drawer")!.textContent || "";
    assert.ok(drawer.includes("provider timeout"));
    assert.ok(drawer.includes("backup-model"));
    assert.ok(drawer.includes("stale receipt"));
  } finally {
    app.stop();
    root.unmount();
    dom.window.close();
  }
});
