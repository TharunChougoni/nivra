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
  "/api/v1/health": {
    ...envelope,
    live_enabled: false,
    execution_authority: false,
    readiness: { status: "ready", reason: "controller_ready" },
    activity_attribution: {
      session: "2026-10-29",
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
        await until(() => document.querySelector('[role="tab"]'));
        await until(() =>
          requests.some((url) =>
            url.startsWith("/api/v1/agent/transcript?lane="),
          ),
        );
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
        await until(() => document.body.textContent?.includes("Lot size"));
        await until(() => document.body.textContent?.includes("CATALOG"));
        assert.ok(document.querySelector('[title="fixture-broker-hash"]'));
        const future = [
          ...document.querySelectorAll<HTMLButtonElement>(".row-link"),
        ].find((button) => button.textContent?.includes("FIXTURE FUTURE"));
        assert.ok(future);
        future.click();
        await until(() =>
          document.body.textContent?.includes("F&O contract chain"),
        );
        await until(() => document.body.textContent?.includes("FIXTURE CALL"));
        assert.ok(document.body.textContent?.includes("Lot size 25"));
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
          document.body.textContent?.includes("1 strategy behind"),
        );
        assert.ok(document.body.textContent?.includes("Champion"));
        assert.ok(document.querySelector('[title="spec-hash"]'));
        assert.ok(document.body.textContent?.includes("scan-1"));
        const library = [
          ...document.querySelectorAll<HTMLButtonElement>("button"),
        ].find((button) => button.textContent === "Library");
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
        assert.ok(document.querySelector('[title="fixture-source-hash"]'));
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
        assert.ok(modes.includes("Paper"));
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
          document.body.textContent?.includes("Net after cost"),
        );
        assert.ok(document.body.textContent?.includes("Activity"));
        assert.ok(document.body.textContent?.includes("Net after cost"));
        assert.ok(document.body.textContent?.includes("13.00"));
        assert.ok(document.body.textContent?.includes("Live trading"));
        assert.ok(document.body.textContent?.includes("Off"));
        assert.ok(document.body.textContent?.includes("Order broker"));
        assert.ok(document.body.textContent?.includes("Read only"));
        const bar = document.querySelector(".statusbar")?.textContent || "";
        assert.ok(bar.includes("fixture-upstox"));
        assert.ok(bar.includes("Paper"));
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
    await until(() => document.body.textContent?.includes("Status checks"));
    await until(() =>
      document.body.textContent?.includes("Accepted by receiver"),
    );
    for (const label of [
      "Paper finances",
      "Controller",
      "Research",
      "Calendar",
      "Alert delivery",
    ])
      assert.ok(document.body.textContent?.includes(label), label);
    assert.ok(document.body.textContent?.includes("5,000"));
    assert.ok(document.body.textContent?.includes("Accepted by receiver"));
    assert.ok(document.body.textContent?.includes("Not recorded"));
    const capturedText = () =>
      [...document.querySelectorAll("dt")].find(
        (dt) => dt.textContent === "Snapshot taken",
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
      document.body.textContent?.includes("Some data is unavailable"),
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
      document.body.textContent?.includes("Some data is unavailable"),
    );
    assert.ok(document.body.textContent?.includes("portfolio not reported"));
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
type Json = Record<string, unknown>;
const turn = (lane: string, n: number, extra: Json = {}) => ({
  turn_id: `${lane}-${n}`,
  lane,
  at_ms: envelope.as_of_ms - 1000 * n,
  status: "ok",
  model: `${lane}-model`,
  elapsed_ms: 900,
  usage: { input: 100, output: 20, cache_read: 5, cost_usd: 0.02, calls: 1 },
  trigger: { kind: "price_move", reason: "move over 30 bps", instruments: [] },
  ...extra,
});
const exchange = (turn: Json, extra: Json = {}) => ({
  session: "01a210cf-7c56-7079-a3a5-59ca71548016",
  at_ms: turn.at_ms as number,
  ended_ms: (turn.at_ms as number) + 900,
  complete: true,
  brief: { text: 'Brief:\n{"book":{}}', bytes: 4200, truncated: true },
  items: [],
  turn,
  ...extra,
});
const gateNew = turn("gate", 1, {
  trigger: {
    kind: "price_move",
    reason: "move over 30 bps",
    instruments: ["NSE_EQ|RELIANCE"],
  },
  verdict: { escalate: true, reason: "spread tight" },
});
const gateOld = turn("gate", 2, {
  verdict: { escalate: false, reason: "no volume confirmation" },
});
const decisionNew = turn("decision", 1, {
  plan: { status: "validated", stance: "hold", actions: 2 },
});
const decisionCut = turn("decision", 2, { status: "ok" });
const transcripts: Record<string, Json[][]> = {
  // Pages are newest first; the second page answers `cursor=1`.
  gate: [
    [
      exchange(gateNew, {
        items: [
          {
            kind: "thinking",
            text: "Checking the spread.\nThen depth.",
            truncated: false,
          },
          {
            kind: "tool",
            id: "c1",
            name: "intraday",
            args: {
              text: '{"instrument":"NSE_EQ|RELIANCE"}',
              truncated: true,
            },
            result: { text: '{"bid":"100.0"}', truncated: false },
            is_error: false,
            duration_ms: 9,
          },
          { kind: "text", text: "Escalating RELIANCE.", truncated: false },
        ],
      }),
    ],
    [
      exchange(gateOld, {
        items: [{ kind: "text", text: "Declining.", truncated: false }],
      }),
    ],
  ],
  decision: [
    [
      exchange(decisionNew, {
        items: [{ kind: "text", text: "Holding the book.", truncated: false }],
      }),
      exchange(decisionCut, { complete: false, ended_ms: null }),
    ],
  ],
  research: [[]],
};
async function openLanes(failing = "review") {
  const dom = new JSDOM('<div id="root"></div>', {
    url: "http://localhost/#/lanes",
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
  ] as const)
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
  const fetch = fetchFixture({
    "/api/v1/health": () =>
      json({
        ...envelope,
        readiness: {
          models: {
            roles: [
              {
                role: "decision",
                model: "primary-model",
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
        turns: [gateNew, decisionNew],
        total: 2,
        usage: {
          lanes: {
            decision: { input: 900, output: 300, cost_usd: 0.31, calls: 2 },
          },
        },
        execution_authorized: false,
      }),
    "/api/v1/agent/transcript": (url) => {
      const q = new URLSearchParams(url.split("?")[1]),
        lane = q.get("lane") ?? "",
        pages = transcripts[lane] ?? [[]],
        page = q.get("cursor") ? 1 : 0;
      if (lane === failing)
        return Promise.resolve(
          new Response(
            JSON.stringify({ error: "agent_transcript_unavailable" }),
            {
              status: 503,
            },
          ),
        );
      return json({
        v: 1,
        lane,
        sessions: [
          {
            id: "01a210cf-7c56-7079-a3a5-59ca71548016",
            started_ms: envelope.as_of_ms - 60_000,
            model: `${lane}-model`,
            thinking: "low",
          },
        ],
        exchanges: pages[page] ?? [],
        total: 2,
        next: page === 0 && pages.length > 1 ? "1" : null,
      });
    },
  });
  const app = createController(dom.window, { connect: false, fetch });
  const root = createRoot(document.getElementById("root")!);
  root.render(
    <Context.Provider value={app}>
      <App />
    </Context.Provider>,
  );
  await app.authenticate("fixture-only");
  const text = () => document.body.textContent || "",
    tab = (name: string) =>
      [...document.querySelectorAll<HTMLElement>('[role="tab"]')].find((t) =>
        t.textContent?.startsWith(name),
      )!,
    button = (label: string) =>
      [...document.querySelectorAll<HTMLElement>("button")].find((b) =>
        b.textContent?.includes(label),
      );
  return {
    text,
    tab,
    button,
    close: () => {
      app.stop();
      root.unmount();
      dom.window.close();
    },
  };
}
test("#/lanes reads each lane as a chat with expandable tools and outcome chips", async () => {
  const page = await openLanes();
  try {
    // decision is the first tab: the trigger bubble and the plan chip
    await until(() => page.text().includes("Holding the book."));
    assert.ok(page.text().includes("Price move"));
    assert.ok(page.text().includes("2 trades proposed"));
    assert.ok(page.text().includes("New session · decision-model"));
    assert.equal(page.text().includes("01a210cf"), false);
    // the stats strip sits under the thread, with spend against budget
    assert.ok(page.text().includes("$0.31 of $4.00"));
    page.tab("Gate").click();
    await until(() => page.text().includes("Escalating RELIANCE."));
    assert.ok(page.text().includes("Instruments: RELIANCE"));
    assert.ok(page.text().includes("Escalated"));
    // the written answer already says it; the gate reason is not repeated
    assert.equal(
      document
        .querySelector(".chat-chips")
        ?.textContent?.includes("spread tight"),
      false,
    );
    // a tool call is one collapsed line until opened
    const tool = page.button("Intraday bars")!;
    assert.equal(tool.getAttribute("aria-expanded"), "false");
    assert.equal(page.text().includes('"bid": "100.0"'), false);
    assert.equal(page.text().includes("Cut short"), false);
    tool.click();
    await until(() => page.text().includes('"bid": "100.0"'));
    assert.ok(page.text().includes("Cut short"));
    assert.equal(page.text().includes("Declining."), false);
    // older exchanges arrive through the cursor
    page.button("Load earlier")!.click();
    await until(() => page.text().includes("Declining."));
    assert.ok(page.text().includes("Declined"));
    assert.equal(page.button("Load earlier"), undefined);
  } finally {
    page.close();
  }
});
test("#/lanes shows plain labels, never raw code names", async () => {
  const page = await openLanes();
  try {
    await until(() => page.text().includes("Holding the book."));
    assert.doesNotMatch(page.text(), /\b[a-z]+(?:_[a-z]+)+\b/);
    page.tab("Gate").click();
    await until(() => page.text().includes("Escalating RELIANCE."));
    assert.doesNotMatch(page.text(), /\b[a-z]+(?:_[a-z]+)+\b/);
  } finally {
    page.close();
  }
});
test("#/lanes marks an interrupted exchange and keeps a failed transcript read to its lane", async () => {
  const page = await openLanes("review");
  try {
    await until(() => page.text().includes("Holding the book."));
    assert.ok(page.text().includes("Interrupted"));
    page.tab("Review").click();
    await until(() => page.text().includes("Could not load this lane"));
    assert.equal(page.text().includes("No turns yet"), false);
    page.tab("Research").click();
    await until(() => page.text().includes("No turns yet"));
    assert.ok(page.text().includes("Runs when requested"));
  } finally {
    page.close();
  }
});
test("#/lanes side by side shows two lanes and the right one can change", async () => {
  const page = await openLanes();
  try {
    await until(() => page.text().includes("Holding the book."));
    assert.equal(document.querySelectorAll("section.lane-col").length, 1);
    page.button("Side by side")!.click();
    await until(() => page.text().includes("Escalating RELIANCE."));
    const lanes = () =>
      [...document.querySelectorAll("section.lane-col")].map((c) =>
        c.getAttribute("aria-label"),
      );
    assert.deepEqual(lanes(), ["decision lane", "gate lane"]);
    const select = document.querySelector<HTMLSelectElement>(
      'select[aria-label="Right lane"]',
    )!;
    select.value = "research";
    select.dispatchEvent(new window.Event("change", { bubbles: true }));
    await until(() => lanes()[1] === "research lane");
    await until(() => page.text().includes("No turns yet"));
  } finally {
    page.close();
  }
});
test("instrument chart range picker loads archive history and shows plain unavailable state", async () => {
  const dom = new JSDOM('<div id="root"></div>', {
    url: "http://localhost/",
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
  const requests: string[] = [];
  let unavailableMode = false;
  const fetch = fetchFixture({
    "/api/v1/series": (url) => {
      const q = new URLSearchParams(url.split("?")[1] || "");
      const kind = q.get("kind") || "receipt-minute-bar";
      if (kind === "archive-minute-bar" || kind === "archive-daily-bar") {
        if (unavailableMode) {
          return json({
            ...envelope,
            kind,
            unavailable: true,
            reason: "archive_not_configured",
            points: [],
          });
        }
        const from = Number(q.get("from"));
        return json({
          ...envelope,
          kind,
          instrument_key: q.get("instrument"),
          unavailable: false,
          source: "market_archive",
          truncated: false,
          next: null,
          points: [
            {
              at: from + 33_300_000,
              open: "100",
              high: "102",
              low: "99.5",
              close: "101.25",
              tick_count: 3,
              price_kind: "receipt_price",
              bar_kind: "archive_quote_minute",
              certified_ohlcv: false,
              volume: null,
            },
            {
              at: from + 4 * 86_400_000 + 33_300_000,
              open: "101.25",
              high: "104",
              low: "101",
              close: "103.50",
              tick_count: 5,
              price_kind: "receipt_price",
              bar_kind: "archive_quote_minute",
              certified_ohlcv: false,
              volume: null,
            },
          ],
          coverage: {
            from_ms: from + 33_300_000,
            to_ms: from + 4 * 86_400_000 + 33_300_000,
            interval_ms: 60_000,
            observed: 2,
            expected: 2,
            gap_count: 0,
            gaps: [],
          },
        });
      }
      return json({
        ...envelope,
        kind,
        points: [],
        coverage: null,
        unavailable: false,
      });
    },
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
    await until(() =>
      [...document.querySelectorAll<HTMLElement>('[role="tab"]')].find((t) =>
        t.textContent?.includes("Instrument price"),
      ),
    );
    const priceTab = [
      ...document.querySelectorAll<HTMLElement>('[role="tab"]'),
    ].find((t) => t.textContent?.includes("Instrument price"))!;
    priceTab.dispatchEvent(
      new dom.window.MouseEvent("mousedown", { bubbles: true, button: 0 }),
    );
    priceTab.click();
    await until(() =>
      document.querySelector<HTMLSelectElement>(
        'select[aria-label="History range"]',
      ),
    );
    const rangeSelect = document.querySelector<HTMLSelectElement>(
      'select[aria-label="History range"]',
    )!;
    rangeSelect.value = "5d";
    rangeSelect.dispatchEvent(
      new dom.window.Event("change", { bubbles: true }),
    );
    await until(() =>
      requests.some((u) => u.includes("kind=archive-minute-bar")),
    );
    await until(() => document.body.textContent?.includes("103.50"));
    assert.ok(document.body.textContent?.includes("Market archive"));
    assert.doesNotMatch(
      document.body.textContent || "",
      /\b(archive_not_configured|archive_minute_bar|archive_quote_minute)\b/,
    );

    unavailableMode = true;
    rangeSelect.value = "30d";
    rangeSelect.dispatchEvent(
      new dom.window.Event("change", { bubbles: true }),
    );
    await until(() =>
      requests.some((u) => u.includes("kind=archive-daily-bar")),
    );
    await until(() =>
      document.body.textContent?.includes("Market archive is not configured"),
    );
    assert.ok(document.body.textContent?.includes("Archive unavailable"));
  } finally {
    app.stop();
    root.unmount();
    dom.window.close();
  }
});

test("overview shows watched held position option, hides live attribution in archived sessions, and surfaces series errors", async () => {
  const dom = new JSDOM('<div id="root"></div>', {
    url: "http://localhost/",
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
  let seriesFail = true;
  const fetch = fetchFixture({
    "/api/v1/portfolio": () =>
      json({
        ...portfolio,
        positions: [
          {
            instrument: {
              key: "NSE_EQ|HELDONLY",
              symbol: "HELDONLY",
              kind: "cash",
            },
            units: "5",
            entry: "200",
            mark: "210",
            decision_id: "d1",
            marked_at: envelope.as_of_ms,
          },
        ],
      }),
    "/api/v1/series": () => {
      if (seriesFail)
        return new Response(JSON.stringify({ error: "series_unavailable" }), {
          status: 503,
        });
      return json({ ...envelope, points: [], unavailable: false });
    },
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
    // 1. Live equity series failure is shown instead of "No equity checkpoints recorded yet."
    await until(() => app.getSnapshot().series.equityState === "error");
    await until(() => document.body.textContent?.includes("Could not load"));

    // 2. Watching a held position absent from scanner.items keeps its option in Chart instrument select.
    seriesFail = false;
    app.watch("NSE_EQ|HELDONLY");
    const priceTab = [
      ...document.querySelectorAll<HTMLElement>('[role="tab"]'),
    ].find((t) => t.textContent?.includes("Instrument price"))!;
    priceTab.dispatchEvent(
      new dom.window.MouseEvent("mousedown", { bubbles: true, button: 0 }),
    );
    priceTab.click();
    await until(() =>
      document.querySelector<HTMLSelectElement>(
        'select[aria-label="Chart instrument"]',
      ),
    );
    const instSelect = document.querySelector<HTMLSelectElement>(
      'select[aria-label="Chart instrument"]',
    )!;
    assert.equal(instSelect.value, "NSE_EQ|HELDONLY");
    assert.equal(instSelect.selectedOptions[0]?.textContent, "HELDONLY");

    // 3. Selecting an archived session does not show today's live Realized attribution figure.
    await until(() => document.body.textContent?.includes("₹12.50"));
    const sessionSelect = document.querySelector<HTMLSelectElement>(
      'select[aria-label="Selected session"]',
    )!;
    const archivedOption = [...sessionSelect.options].find((o) =>
      o.value.includes("2026-10-28"),
    )!;
    sessionSelect.value = archivedOption.value;
    sessionSelect.dispatchEvent(
      new dom.window.Event("change", { bubbles: true }),
    );
    await until(() => document.body.textContent?.includes("Archived session"));
    assert.equal(
      document.body.textContent?.includes("₹12.50"),
      false,
      "archived session overview must not show today's live realized P&L",
    );
  } finally {
    app.stop();
    root.unmount();
    dom.window.close();
  }
});
