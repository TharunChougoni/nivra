import type {
  Fetch,
  RunResponse,
  SummaryResponse,
  PortfolioResponse,
  ApprovalsResponse,
  ScannerResponse,
  InstrumentCatalogResponse,
  ContractChainResponse,
  FillsResponse,
  PlansResponse,
  SessionsResponse,
  HealthResponse,
  AlertsResponse,
  SessionResponse,
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
const approvals = {
  ...envelope,
  models: {
    roles: [
      {
        role: "digest",
        model: "luna",
        status: "ready",
        remaining_current: 10,
      },
      {
        role: "orchestrator",
        model: "shared",
        status: "ready",
        remaining_current: 8,
      },
    ],
  },
  identities: [
    {
      identity: "digest",
      role: {
        role: "digest",
        model: "luna",
        status: "ready",
        remaining_current: 10,
      },
    },
    {
      identity: "orchestrator",
      role: {
        role: "orchestrator",
        model: "shared",
        status: "ready",
        remaining_current: 8,
      },
    },
  ],
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
  | ApprovalsResponse
  | ScannerResponse
  | InstrumentCatalogResponse
  | ContractChainResponse
  | FillsResponse
  | PlansResponse
  | SessionsResponse
  | HealthResponse
  | AlertsResponse
  | SessionResponse
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
  "/api/v1/session": {
    v: 1,
    session: "2026-10-29",
    notes: {
      version: 2,
      notes: [
        {
          id: "note-1",
          text: "Recorded operator note from the session log.",
          state: "done",
          keep: true,
          revision: 3,
          created_at_ms: envelope.as_of_ms - 2000,
          updated_at_ms: envelope.as_of_ms - 1000,
          updated_by: "operator",
        },
      ],
    },
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
        run: "harness-supervisor-fixture",
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
  "/api/v1/approvals": approvals,
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
    if (path === "/api/v1/runs") return json({ runs: [], next: null });
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
test("client renders eight sections, two identities, archived books and private lock state", async () => {
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
      document.querySelector(".metric-strip")?.textContent?.includes("5,000"),
    );
    assert.equal(
      document.querySelectorAll(".workspace-sidebar nav a").length,
      8,
    );
    assert.equal(
      document.querySelector('.workspace-sidebar nav a[href="#/chat"]'),
      null,
    );
    assert.equal(
      [...document.querySelectorAll(".workspace-sidebar nav a")].some((v) =>
        v.textContent?.includes("Chat"),
      ),
      false,
    );
    assert.equal(document.querySelectorAll(".model-identity").length, 2);
    assert.equal(
      requests.some((url) => url.startsWith("/api/status")),
      false,
    );
    for (const section of [
      "models",
      "portfolio",
      "sessions",
      "scanner",
      "strategies",
      "backtest",
      "health",
    ]) {
      location.hash = "#/" + section;
      await until(
        () =>
          document
            .querySelector(`.workspace-sidebar a[href="#/${section}"]`)
            ?.getAttribute("aria-current") === "page",
      );
      if (section === "models") {
        await until(() =>
          document.querySelector('[aria-label="Model identity"]'),
        );
        assert.deepEqual(
          [...document.querySelectorAll(".model-identity strong")].map(
            (v) => v.textContent,
          ),
          ["Digest", "Orchestrator"],
        );
        assert.deepEqual(
          [
            ...document.querySelectorAll(
              '[aria-label="Model identity"] button',
            ),
          ].map((v) => v.textContent),
          ["All identities", "Digest", "Orchestrator"],
        );
        const orchestrator = [
          ...document.querySelectorAll<HTMLButtonElement>(
            '[aria-label="Model identity"] button',
          ),
        ].find((v) => v.textContent === "Orchestrator");
        assert.ok(orchestrator);
        orchestrator.dispatchEvent(
          new dom.window.MouseEvent("mousedown", {
            bubbles: true,
            cancelable: true,
            button: 0,
          }),
        );
        orchestrator.click();
        await until(() =>
          requests.some((url) => url.includes("identity=orchestrator")),
        );
        await until(() =>
          document.body.textContent?.includes("Web tool activity"),
        );
        assert.ok(document.body.textContent?.includes("nifty bank"));
        assert.ok(document.body.textContent?.includes("42 ms"));
        assert.ok(requests.some((url) => url.startsWith("/api/v1/tools?")));
      }
      if (section === "scanner") {
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
        await until(() =>
          document.body.textContent?.includes(
            "Recorded operator note from the session log.",
          ),
        );
        const notes = [
          ...document.querySelectorAll<HTMLElement>(".surface"),
        ].find(
          (v) =>
            v.querySelector("h2")?.textContent === "Recorded operator notes",
        );
        assert.ok(notes);
        assert.equal(
          notes.querySelectorAll("button, form, input, textarea").length,
          0,
        );
        assert.equal(
          requests.some((url) => url.startsWith("/api/v1/session?date=")),
          true,
        );
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
        assert.ok(
          document
            .querySelector(".broker-banner")
            ?.textContent?.includes("fixture-upstox"),
        );
        // The paper ledger is the explicit order authority, not "Not reported".
        assert.match(
          document.querySelector(".broker-banner")?.textContent || "",
          /Orders\s+paper/,
        );
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
      document.querySelector(".metric-strip")?.textContent?.includes("4,990"),
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
test("archived model text stays inert and full tool evidence is fetched only when expanded", async () => {
  const dom = new JSDOM('<div id="root"></div>', {
    url: "http://localhost/#/runs/harness-digest-100-normal",
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
  const { readFileSync } = await import("node:fs");
  const fixture = JSON.parse(
    readFileSync(new URL("../fixtures/accepted.json", import.meta.url), "utf8"),
  ) as RunResponse;
  const assistant = fixture.events?.find((v) => v.kind === "assistant_text");
  assert.ok(assistant);
  assistant.text = '<img src=x onerror="window.injected=true"> **Recorded**';
  const reads: string[] = [];
  const fetch = fetchFixture({
    "/api/v1/runs/harness-digest-100-normal": () => json(fixture),
  });
  const app = createController(dom.window, {
    connect: false,
    fetch: async (url) => {
      reads.push(url);
      if (url.includes("/blobs/")) return new Response("original full result");
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
    await until(() => document.querySelector(".model-output"));
    assert.ok(document.querySelector(".prose")?.textContent?.includes("<img"));
    assert.equal(document.querySelector(".prose img"), null);
    assert.equal(Reflect.get(dom.window, "injected"), undefined);
    assert.equal(
      reads.some((v) => v.includes("/blobs/")),
      false,
    );
    const fold = [...document.querySelectorAll("details")].find((v) =>
      v.querySelector("summary")?.textContent?.includes("Tool result"),
    );
    assert.ok(fold);
    fold.open = true;
    fold.dispatchEvent(new dom.window.Event("toggle"));
    await until(() =>
      document.body.textContent?.includes("original full result"),
    );
    assert.equal(reads.filter((v) => v.includes("/blobs/")).length, 1);
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
    version: 1,
    captured_at_ms: captured,
    supervisor: {
      status: "idle",
      config: "configured",
      config_reason: null,
      config_scope: "loaded_policy_or_static_failure_code",
      state: "present",
      enabled: true,
      budget: {
        configured_daily_calls: 20,
        charged_calls: 7,
        parked_at_calls: null,
        exhausted: false,
      },
      backoff: {
        configured_ms: 30_000,
        retry_limit: 2,
        enforced: true,
        active: false,
        earliest_retry_at_ms: null,
      },
      last_completed_run: {
        at_ms: captured - 60_000,
        age_ms: 60_000,
        runs: 3,
      },
      pending_source: "no_pending",
      pending_scope: "checked_bounded_session_log",
      pending: null,
    },
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
    await until(() =>
      document.body.textContent?.includes("Current session state present"),
    );
    for (const label of [
      "Paper financial status",
      "Controller liveness",
      "Research & advisory",
      "Supervisor",
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
