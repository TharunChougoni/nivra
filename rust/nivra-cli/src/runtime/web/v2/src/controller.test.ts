import type { SummaryResponse } from "./types";
import { mock, test } from "node:test";
import assert from "node:assert/strict";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createController, WATCHDOG_MS } from "./controller";
import { conditionOf } from "./model";
import { StateBanner } from "./StateBanner";
import { JSDOM } from "jsdom";
const json = (v: unknown) =>
  new Response(JSON.stringify(v), {
    headers: { "Content-Type": "application/json" },
  });
const wait = async (fn: () => unknown) => {
  for (let i = 0; i < 200; i++) {
    if (fn()) return;
    await new Promise((r) => setTimeout(r, 5));
  }
  throw Error("timed out");
};
const projections = (
  summary: SummaryResponse,
  extra: Record<string, unknown> = {},
): Record<string, unknown> => {
  const body = {
    v: 1,
    mode: "paper",
    account: "test",
    session: "2026-10-29",
    as_of_ms: 1000,
    capital: "5000",
    ...summary,
  };
  return {
    "/api/v1/summary": body,
    "/api/v1/portfolio": {
      ...body,
      cash: "5000",
      equity: "5000",
      positions: [],
      recorded_fees: "0",
    },
    "/api/v1/fills": { ...body, total: 0, items: [], next: null },
    "/api/v1/plans": {
      ...body,
      planner: { status: "ready", decision_id: null, pending_events: 0 },
      actions: [],
      execution: [],
    },
    "/api/v1/scanner": {
      ...body,
      segment: "prospects",
      total: 0,
      items: [],
      next: null,
      coverage: { fresh: 0, stale: 0, unseen: 0, universe: 0 },
      screen_reasons: {},
    },
    "/api/v1/sessions": { ...body, total: 0, items: [], next: null },
    ...extra,
  };
};
test("lock discards pending financial responses and clears chart/session evidence", async () => {
  const dom = new JSDOM("", { url: "http://localhost" });
  let complete: ((response: Response) => void) | undefined;
  let cleared = 0;
  const app = createController(dom.window, {
    connect: false,
    cache: {
      get: async () => null,
      put: async () => true,
      clear: async () => {
        cleared++;
      },
      close: () => {},
    },
    fetch: async (url) => {
      if (url === "/api/v1/meta") return json({ mode: "paper" });
      if (url === "/api/v1/summary")
        return new Promise((resolve) => {
          complete = resolve;
        });
      return json({
        account: "private-fixture",
        portfolio: { cash: "5000" },
      });
    },
  });
  await app.authenticate("fixture-token");
  await wait(() => complete);
  app.watch("TEST|1");
  app.selectSession("broker|account|date");
  app.lock();
  assert.ok(complete);
  complete(json({ account: "private-fixture", portfolio: { cash: "5000" } }));
  await new Promise((r) => setTimeout(r, 10));
  const snapshot = app.getSnapshot();
  assert.equal(snapshot.summary, null);
  assert.equal(snapshot.snapshotState, "idle");
  assert.equal(snapshot.session, "");
  assert.deepEqual(snapshot.points, []);
  assert.deepEqual(snapshot.series.price, []);
  assert.equal(cleared, 1);
  assert.equal(dom.window.sessionStorage.getItem("paperToken"), null);
  app.stop();
  dom.window.close();
});
test("v1 projection reads coalesce and selected chart observations preserve original sample time", async () => {
  const dom = new JSDOM("", { url: "http://localhost" });
  let summaryReads = 0;
  const requests: string[] = [];
  const app = createController(dom.window, {
    connect: false,
    fetch: async (url) => {
      requests.push(url);
      if (url === "/api/v1/meta")
        return json({ mode: "paper", broker: { data: "fixture" } });
      if (url.startsWith("/api/v1/series"))
        return json({ points: [], coverage: null });
      if (url === "/api/v1/summary") {
        summaryReads++;
        return json({
          account: "test",
          session: "2026-10-29",
          as_of_ms: summaryReads * 1000,
          capital: "5000",
          status: "open",
          state: { stopped: false, halted: false, closed: false },
        });
      }
      if (url.startsWith("/api/v1/scanner"))
        return json({
          items: [
            {
              instrument_key: "TEST|1",
              latest_screening_price: String(100 + summaryReads),
              sampled_price_ms: summaryReads * 1000 - 100,
              observed_ms: summaryReads * 1000,
            },
          ],
          coverage: { fresh: 1, universe: 1 },
        });
      return json(projections({})[url.split("?")[0]] || {});
    },
  });
  await app.authenticate("fixture");
  await wait(() => app.getSnapshot().scannerState === "ready");
  assert.equal(app.getSnapshot().snapshotState, "ready");
  app.watch("TEST|1");
  await app.refresh();
  assert.deepEqual(
    app.getSnapshot().points.map((v) => v.at),
    [900, 1900],
  );
  assert.equal(app.getSnapshot().equityPoints.length, 2);
  assert.equal(
    requests.some((url) => String(url).startsWith("/api/status")),
    false,
  );
  assert.equal(app.getSnapshot().snapshotState, "ready");
  assert.deepEqual(app.getSnapshot().issues, []);
  app.stop();
  dom.window.close();
});
test("a failed projection clears its data without reusing a prior financial value", async () => {
  const dom = new JSDOM("", { url: "http://localhost" });
  let failPortfolio = false;
  const app = createController(dom.window, {
    connect: false,
    fetch: async (url) => {
      if (url === "/api/v1/meta") return json({ mode: "paper" });
      if (url === "/api/v1/portfolio" && failPortfolio)
        throw Error("portfolio_unavailable");
      if (url === "/api/v1/summary")
        return json({
          account: "test",
          session: "2026-10-29",
          as_of_ms: 1000,
          capital: "5000",
          state: { stopped: false, halted: false, closed: false },
        });
      return json(projections({})[url.split("?")[0]] || {});
    },
  });
  await app.authenticate("fixture");
  await wait(() => app.getSnapshot().portfolioState === "ready");
  assert.equal(app.getSnapshot().portfolio?.cash, "5000");
  assert.equal(app.getSnapshot().equityPoints.length, 1);
  failPortfolio = true;
  await app.refresh();
  const snapshot = app.getSnapshot();
  assert.equal(snapshot.portfolio, null);
  assert.equal(snapshot.portfolioState, "error");
  assert.equal(snapshot.summaryState, "ready");
  assert.equal(snapshot.snapshotState, "unavailable");
  assert.deepEqual(snapshot.equityPoints, []);
  assert.deepEqual(
    snapshot.issues.map((v) => v.source),
    ["portfolio"],
  );
  app.stop();
  dom.window.close();
});
test("a failed scanner projection clears tab-local points while preserving the summary", async () => {
  const dom = new JSDOM("", { url: "http://localhost" });
  let failScanner = false;
  const app = createController(dom.window, {
    connect: false,
    fetch: async (url) => {
      if (url === "/api/v1/meta") return json({ mode: "paper" });
      if (url.startsWith("/api/v1/scanner") && failScanner)
        throw Error("scanner_unavailable");
      if (url === "/api/v1/summary")
        return json({
          account: "test",
          session: "2026-10-29",
          as_of_ms: 1000,
          capital: "5000",
          state: { stopped: false, halted: false, closed: false },
        });
      if (url.startsWith("/api/v1/scanner"))
        return json({
          items: [
            {
              instrument_key: "TEST|1",
              latest_screening_price: "100",
              sampled_price_ms: 900,
              observed_ms: 1000,
            },
          ],
          coverage: { fresh: 1, universe: 1 },
        });
      return json(projections({})[url.split("?")[0]] || {});
    },
  });
  await app.authenticate("fixture");
  await wait(
    () =>
      app.getSnapshot().scannerState === "ready" &&
      app.getSnapshot().points.length === 1,
  );
  app.watch("TEST|1");
  failScanner = true;
  await app.refresh();
  const snapshot = app.getSnapshot();
  assert.equal(snapshot.scanner, null);
  assert.equal(snapshot.scannerState, "error");
  assert.equal(snapshot.summaryState, "ready");
  assert.deepEqual(snapshot.points, []);
  assert.deepEqual(
    snapshot.issues.map((v) => v.source),
    ["scanner"],
  );
  app.stop();
  dom.window.close();
});
test("cached series evidence is marked stale and survives a failed series read", async () => {
  const dom = new JSDOM("", { url: "http://localhost" });
  let cacheReads = 0;
  const cached = {
    body: {
      points: [
        {
          at: 5000,
          close: "101",
          tick_count: 2,
          price_kind: "receipt_price",
          certified_ohlcv: false,
        },
      ],
      coverage: { gaps: [] },
      watermark_ms: 5000,
      source: "receipt_derived_feature_store",
    },
  };
  const app = createController(dom.window, {
    connect: false,
    cache: {
      get: async (key) => {
        cacheReads++;
        return key.includes("receipt-minute-bar") ? cached : null;
      },
      put: async () => true,
      clear: async () => {},
      close: () => {},
    },
    fetch: async (url) => {
      if (url === "/api/v1/meta")
        return json({ mode: "paper", broker: { data: "fixture" } });
      if (url.startsWith("/api/v1/series")) throw Error("series_unavailable");
      if (url === "/api/v1/summary")
        return json({
          account: "test",
          session: "2026-10-29",
          as_of_ms: 10_000,
          capital: "5000",
          state: { stopped: false, halted: false, closed: false },
        });
      if (url.startsWith("/api/v1/scanner"))
        return json({
          items: [{ instrument_key: "TEST|1" }],
          coverage: { fresh: 1, universe: 1 },
        });
      return json(projections({})[url.split("?")[0]] || {});
    },
  });
  await app.authenticate("fixture");
  await wait(() => app.getSnapshot().series.price.length === 1);
  const series = app.getSnapshot().series;
  assert.equal(series.price[0].at, 5000);
  assert.equal(series.price[0].value, 101);
  assert.equal(series.priceCached, true);
  assert.equal(series.priceState, "cached");
  assert.ok(cacheReads >= 1);
  app.lock();
  app.stop();
  dom.window.close();
});

test("a pending equity read cannot publish into the next paper session", async () => {
  const dom = new JSDOM("", { url: "http://localhost" });
  let session = "2026-10-29";
  let finishOldEquity: ((response: Response) => void) | undefined;
  const app = createController(dom.window, {
    connect: false,
    fetch: async (url) => {
      if (url === "/api/v1/meta") return json({ mode: "paper" });
      if (url.includes("kind=account-equity")) {
        if (session === "2026-10-29")
          return new Promise((resolve) => {
            finishOldEquity = resolve;
          });
        return json({ points: [{ at: 2000, equity: "5000" }] });
      }
      return json(
        projections({ session, as_of_ms: 2000 })[url.split("?")[0]] || {},
      );
    },
  });
  await app.authenticate("fixture");
  await wait(() => finishOldEquity);
  session = "2026-10-30";
  await app.refresh();
  await wait(() => app.getSnapshot().series.equityState === "ready");
  assert.ok(finishOldEquity);
  finishOldEquity(json({ points: [{ at: 1000, equity: "1" }] }));
  await new Promise<void>((resolve) => setImmediate(resolve));
  assert.deepEqual(
    app.getSnapshot().series.equity.map(({ at, value }) => [at, value]),
    [[2000, 5000]],
  );
  app.stop();
  dom.window.close();
});
// A fetch fake whose summary carries `revisions` and whose event feed is a
// queue of poll batches (the SSE endpoint answers 503, so the stream polls).
function revisionFixture() {
  const requests: string[] = [];
  const queue: unknown[][] = [];
  let revisions: Record<string, string> | undefined = {
    portfolio: "p1",
    fills: "f1",
    plans: "l1",
    scanner: "s1",
    sessions: "n1",
    agent: "a1",
  };
  const fetch = async (url: string) => {
    if (url.startsWith("/api/v1/events")) {
      if (!url.includes("poll=1")) return new Response(null, { status: 503 });
      return json({ events: queue.shift() ?? [] });
    }
    const path = url.split("?")[0];
    requests.push(path);
    if (path === "/api/v1/meta") return json({ mode: "paper" });
    if (path === "/api/v1/agent") {
      // The server accepts limit 1..=200 and answers 400 otherwise (ops.rs).
      const limit = new URL(url, "http://x").searchParams.get("limit");
      if (limit !== null && !(Number(limit) >= 1 && Number(limit) <= 200))
        return new Response(null, { status: 400 });
      return json({ turns: [], total: 0, usage: { lanes: {} } });
    }
    if (path === "/api/v1/series") return json({ points: [], coverage: null });
    const all = projections({});
    const body = all[path] as Record<string, unknown>;
    return json(path === "/api/v1/summary" ? { ...body, revisions } : body);
  };
  return {
    requests,
    fetch,
    emit: (events: unknown[], next?: Record<string, string>) => {
      if (next) revisions = next;
      queue.push(events);
    },
    set: (next: Record<string, string> | undefined) => (revisions = next),
    get revisions() {
      return revisions;
    },
  };
}
const projectionReads = (requests: string[]) =>
  requests.filter((p) =>
    [
      "/api/v1/portfolio",
      "/api/v1/fills",
      "/api/v1/plans",
      "/api/v1/scanner",
      "/api/v1/sessions",
      "/api/v1/agent",
    ].includes(p),
  );
test("summary_changed refetches only the projections whose revision changed", async () => {
  const dom = new JSDOM("", {
    url: "http://localhost",
    pretendToBeVisual: true,
  });
  const fx = revisionFixture();
  const app = createController(dom.window, { fetch: fx.fetch });
  await app.authenticate("fixture");
  await wait(() => app.getSnapshot().snapshotState === "ready");
  await wait(() => app.getSnapshot().agentState === "ready");
  assert.equal(projectionReads(fx.requests).length, 6);
  // Only fills changed: summary and fills are read; nothing else is.
  fx.requests.length = 0;
  fx.emit(
    [
      {
        seq: 1,
        kind: "summary_changed",
        revisions: { ...fx.revisions, fills: "f2" },
      },
    ],
    {
      ...fx.revisions!,
      fills: "f2",
    },
  );
  await wait(() => fx.requests.includes("/api/v1/fills"));
  await wait(() => app.getSnapshot().snapshotState === "ready");
  assert.deepEqual(projectionReads(fx.requests), ["/api/v1/fills"]);
  assert.ok(fx.requests.includes("/api/v1/summary"));
  // The agent revision changes: the lane journal reloads, projections do not.
  fx.requests.length = 0;
  fx.emit([{ seq: 2, kind: "summary_changed" }], {
    ...fx.revisions!,
    agent: "a2",
  });
  await wait(() => fx.requests.includes("/api/v1/agent"));
  assert.deepEqual(projectionReads(fx.requests), ["/api/v1/agent"]);
  app.stop();
  dom.window.close();
});
test("a summary_changed naming held revisions reads nothing; Refresh still reads everything", async () => {
  const dom = new JSDOM("", {
    url: "http://localhost",
    pretendToBeVisual: true,
  });
  const fx = revisionFixture();
  const app = createController(dom.window, { fetch: fx.fetch });
  await app.authenticate("fixture");
  await wait(() => app.getSnapshot().agentState === "ready");
  fx.requests.length = 0;
  fx.emit([{ seq: 1, kind: "summary_changed", revisions: fx.revisions }]);
  await wait(() => app.getSnapshot().events.length > 0);
  await new Promise((r) => setTimeout(r, 120));
  assert.deepEqual(
    fx.requests.filter((p) => p !== "/api/v1/events"),
    [],
  );
  await app.refresh();
  assert.equal(projectionReads(fx.requests).length, 6);
  app.stop();
  dom.window.close();
});
test("without revisions every summary_changed refetches every projection", async () => {
  const dom = new JSDOM("", {
    url: "http://localhost",
    pretendToBeVisual: true,
  });
  const fx = revisionFixture();
  fx.set(undefined);
  const app = createController(dom.window, { fetch: fx.fetch });
  await app.authenticate("fixture");
  await wait(() => app.getSnapshot().agentState === "ready");
  fx.requests.length = 0;
  fx.emit([{ seq: 1, kind: "summary_changed" }]);
  await wait(() => projectionReads(fx.requests).length === 6);
  app.stop();
  dom.window.close();
});
test("run_ended reloads the lane journal without touching projections", async () => {
  const dom = new JSDOM("", {
    url: "http://localhost",
    pretendToBeVisual: true,
  });
  const fx = revisionFixture();
  const app = createController(dom.window, { fetch: fx.fetch });
  await app.authenticate("fixture");
  await wait(() => app.getSnapshot().agentState === "ready");
  fx.requests.length = 0;
  fx.emit([{ seq: 1, kind: "run_ended", run: "r1" }]);
  await wait(() => fx.requests.includes("/api/v1/agent"));
  assert.deepEqual(projectionReads(fx.requests), ["/api/v1/agent"]);
  app.stop();
  dom.window.close();
});
test("a stalled controller shows as stalled within the watchdog bound while SSE heartbeats continue", async () => {
  // Fake the interval clock only; the SSE reader and `wait` keep real timeouts.
  mock.timers.enable({ apis: ["setInterval", "Date"], now: 100_000 });
  const dom = new JSDOM("", {
    url: "http://localhost",
    pretendToBeVisual: true,
  });
  let push: (text: string) => void = () => {};
  let status = "healthy";
  const health: string[] = [];
  const all = projections({});
  const fetch = async (url: string) => {
    if (url === "/api/v1/events") {
      const body = new ReadableStream<Uint8Array>({
        start(c) {
          push = (text) => c.enqueue(new TextEncoder().encode(text));
        },
      });
      return new Response(body, {
        headers: { "Content-Type": "text/event-stream" },
      });
    }
    const path = url.split("?")[0];
    if (path === "/api/v1/meta") return json({ mode: "paper" });
    if (path === "/api/v1/health") {
      health.push(status);
      return json({
        controller_liveness: {
          status,
          last_completed_at_ms: 1000,
          max_age_ms: 62_000,
        },
      });
    }
    if (path === "/api/v1/agent") return json({ turns: [], total: 0 });
    if (path === "/api/v1/series") return json({ points: [], coverage: null });
    return json(all[path]);
  };
  const app = createController(dom.window, { fetch });
  try {
    await app.authenticate("fixture");
    await wait(() => app.getSnapshot().snapshotState === "ready");
    await wait(() => app.getSnapshot().connection === "live");
    // The loop stops: no more summary_changed, only heartbeat comments.
    status = "stale";
    for (let t = 0; t < WATCHDOG_MS - 5_000; t += 5_000) {
      push(": heartbeat\n\n");
      mock.timers.tick(5_000);
    }
    await new Promise((r) => setTimeout(r, 20));
    assert.deepEqual(health, []);
    push(": heartbeat\n\n");
    mock.timers.tick(5_000);
    await wait(() => app.getSnapshot().stalled !== null);
    const condition = conditionOf(app.getSnapshot());
    assert.deepEqual(condition, { kind: "stalled", lastUpdateMs: 1000 });
    const banner = renderToStaticMarkup(
      createElement(StateBanner, {
        condition,
        onRefresh: () => {},
        refreshing: false,
      }),
    );
    assert.match(banner, /The controller has stopped updating/);
    assert.match(banner, /Its last update was at 01 Jan, 05:30 IST/);
    // A summary announcement means the loop published again.
    push(`event: summary_changed\ndata: {"seq":1}\n\n`);
    await wait(() => app.getSnapshot().stalled === null);
  } finally {
    app.stop();
    dom.window.close();
    mock.timers.reset();
  }
});

test("controller retries series reads after initial unavailable or error responses", async () => {
  const dom = new JSDOM("", { url: "http://localhost" });
  let phase: "startup" | "live" = "startup";
  const fetch = async (url: string) => {
    const path = url.split("?")[0];
    if (path === "/api/v1/meta") return json({ mode: "paper" });
    if (path === "/api/v1/series") {
      if (phase === "startup") {
        return json({
          unavailable: true,
          reason: "series_session_not_started",
          points: [],
        });
      }
      if (url.includes("kind=account-equity")) {
        return json({
          points: [{ at: 2000, equity: "5100", cash: "5100" }],
          watermark_ms: 2000,
          unavailable: false,
        });
      }
      return json({
        points: [{ at: 2000, close: "105" }],
        watermark_ms: 2000,
        unavailable: false,
      });
    }
    if (path === "/api/v1/scanner") {
      return json({
        items: [{ instrument_key: "TEST|1", symbol: "TEST" }],
        coverage: { fresh: 1, universe: 1 },
      });
    }
    return json(
      projections({
        status: "open",
        as_of_ms: phase === "startup" ? 1000 : 2000,
      })[path] || {},
    );
  };
  const app = createController(dom.window, { connect: false, fetch });
  try {
    await app.authenticate("fixture");
    await wait(() => app.getSnapshot().series.equityState === "unavailable");
    assert.equal(app.getSnapshot().series.priceState, "unavailable");
    phase = "live";
    await app.refresh();
    await wait(() => app.getSnapshot().series.equityState === "ready");
    await wait(() => app.getSnapshot().series.priceState === "ready");
    assert.equal(app.getSnapshot().series.price[0]?.value, 105);
    assert.equal(app.getSnapshot().series.equity[0]?.value, 5100);
  } finally {
    app.stop();
    dom.window.close();
  }
});

test("API ETag cache is bounded to prevent unbounded browser memory growth", async () => {
  const { createApi } = await import("./api");
  const seenIfNoneMatch: (string | null)[] = [];
  const api = createApi({
    getToken: () => "tok",
    fetchImpl: async (url, init) => {
      const hdrs = new Headers(init?.headers);
      seenIfNoneMatch.push(hdrs.get("If-None-Match"));
      const q = new URL(url, "http://x").searchParams.get("q") || "0";
      return new Response(JSON.stringify({ items: [], q }), {
        status: 200,
        headers: { "Content-Type": "application/json", ETag: `"etag-${q}"` },
      });
    },
  });
  for (let i = 0; i < 80; i++) {
    await api.instruments({ q: `sym-${i}` });
  }
  seenIfNoneMatch.length = 0;
  // Oldest entry (sym-0) should have been evicted; most recent (sym-79) should remain cached.
  await api.instruments({ q: "sym-0" });
  await api.instruments({ q: "sym-79" });
  assert.equal(seenIfNoneMatch[0], null);
  assert.equal(seenIfNoneMatch[1], '"etag-sym-79"');
});
