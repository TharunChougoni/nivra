import { after, test } from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { JSDOM } from "jsdom";
import { createRoot } from "react-dom/client";
import Chart from "./Chart";
import { LivePriceChart } from "./LivePriceChart";
import { TimeChart } from "./charts/TimeChart";
import { Context } from "./ui";
import { createController } from "./controller";

const pause = () => new Promise((r) => setTimeout(r, 15));
const until = async (fn: () => unknown) => {
  for (let i = 0; i < 150; i++) {
    if (fn()) return;
    await pause();
  }
  throw Error("timed out");
};

let sharedEnv: {
  dom: JSDOM;
  drawnText: string[];
  reset: () => HTMLElement;
} | null = null;

after(() => {
  sharedEnv?.dom.window.close();
  sharedEnv = null;
});

function setupChartDom() {
  if (sharedEnv) {
    return {
      dom: sharedEnv.dom,
      drawnText: sharedEnv.drawnText,
      container: sharedEnv.reset(),
    };
  }
  const dom = new JSDOM('<div id="root" style="width:700px"></div>', {
    url: "http://localhost/",
    pretendToBeVisual: true,
  });
  const drawnText: string[] = [];
  const win = dom.window;
  for (const key of [
    "window",
    "document",
    "Node",
    "HTMLElement",
    "Element",
    "Event",
    "MouseEvent",
    "CustomEvent",
    "MutationObserver",
    "location",
    "localStorage",
  ]) {
    Object.defineProperty(globalThis, key, {
      value: win[key],
      configurable: true,
      writable: true,
    });
  }
  globalThis.getComputedStyle = win.getComputedStyle.bind(win);
  globalThis.addEventListener = win.addEventListener.bind(win);
  globalThis.removeEventListener = win.removeEventListener.bind(win);
  globalThis.devicePixelRatio = 1;
  globalThis.Path2D = class {
    moveTo() {}
    lineTo() {}
    rect() {}
    arc() {}
    closePath() {}
    quadraticCurveTo() {}
    bezierCurveTo() {}
  } as unknown as typeof Path2D;
  globalThis.matchMedia = (() => ({
    matches: false,
    addListener() {},
    removeListener() {},
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof matchMedia;
  win.HTMLCanvasElement.prototype.getContext = (() => ({
    scale() {},
    translate() {},
    rotate() {},
    setTransform() {},
    save() {},
    restore() {},
    clearRect() {},
    fillRect() {},
    beginPath() {},
    moveTo() {},
    lineTo() {},
    stroke() {},
    fill() {},
    closePath() {},
    setLineDash() {},
    fillText(text: string) {
      drawnText.push(String(text));
    },
    clip() {},
    rect() {},
    arc() {},
    measureText: (t: string) => ({ width: String(t).length * 6 }),
    createLinearGradient: () => ({ addColorStop() {} }),
  })) as unknown as typeof win.HTMLCanvasElement.prototype.getContext;
  // Give the container a non-zero width for TimeChart.
  Object.defineProperty(win.HTMLElement.prototype, "clientWidth", {
    configurable: true,
    value: 700,
  });
  const reset = () => {
    drawnText.length = 0;
    const rootEl = win.document.getElementById("root")!;
    rootEl.innerHTML = "";
    return rootEl;
  };
  sharedEnv = { dom, drawnText, reset };
  return { dom, drawnText, container: reset() };
}

test("TimeChart updates x-axis IST date/time formatting when span grows across renders and respects daily", async () => {
  const { drawnText, container } = setupChartDom();
  const root = createRoot(container);
  try {
    // 1. Mount with a 1-hour intraday series (09:15 to 10:15 IST on 26 Nov 2026).
    const t0 = Date.parse("2026-11-26T09:15:00+05:30") / 1000;
    const intradayX = [t0, t0 + 1800, t0 + 3600];
    const panes = [
      {
        height: 220,
        series: [{ label: "Price", color: "--accent", data: [100, 101, 102] }],
      },
    ];
    root.render(
      <TimeChart x={intradayX} panes={panes} ariaLabel="test chart" />,
    );
    await until(() => drawnText.some((t) => /^\d{2}:\d{2}$/.test(t)));

    // 2. Switch to a 30-day series without changing pane structure.
    drawnText.length = 0;
    const d0 = Date.parse("2026-10-28T00:00:00+05:30") / 1000;
    const multiDayX = [d0, d0 + 15 * 86_400, d0 + 29 * 86_400];
    root.render(
      <TimeChart x={multiDayX} panes={panes} daily ariaLabel="test chart" />,
    );
    await until(() => drawnText.length > 0);
    assert.ok(
      drawnText.some((t) => /\b(Oct|Nov)\b/.test(t)),
      `expected date ticks on 30-day daily chart, got: ${drawnText.join(", ")}`,
    );
    assert.ok(
      !drawnText.some((t) => t === "00:00"),
      `did not expect 00:00 time ticks on daily chart, got: ${drawnText.join(", ")}`,
    );

    // 3. Two-day daily series (span === 86_400 s) also formats as dates, not 00:00.
    drawnText.length = 0;
    const twoDayX = [d0, d0 + 86_400];
    root.render(
      <TimeChart
        x={twoDayX}
        panes={[
          {
            height: 220,
            series: [{ label: "Price", color: "--accent", data: [100, 105] }],
          },
        ]}
        daily
        ariaLabel="test chart"
      />,
    );
    await until(() => drawnText.length > 0);
    assert.ok(
      drawnText.some((t) => /\b(Oct|Nov)\b/.test(t)),
      `expected date ticks on 2-day daily chart, got: ${drawnText.join(", ")}`,
    );
  } finally {
    root.unmount();
  }
});

test("Chart unfreezes and plots the new series when switching instruments after pausing", async () => {
  const { dom, drawnText, container } = setupChartDom();
  const root = createRoot(container);
  try {
    const t0 = Date.parse("2026-11-26T09:15:00+05:30");
    const pointsA = [
      { at: t0, value: 100 },
      { at: t0 + 60_000, value: 110 },
    ];
    const pointsB = [
      { at: t0, value: 5000 },
      { at: t0 + 60_000, value: 5100 },
    ];
    root.render(
      <Chart points={pointsA} label="RELIANCE" seriesKey="NSE_EQ|REL:today" />,
    );
    await until(() => drawnText.some((t) => /^10\d$/.test(t)));

    const pauseBtn = [...dom.window.document.querySelectorAll("button")].find(
      (b) => b.textContent?.includes("Pause"),
    )!;
    pauseBtn.click();
    await until(() => pauseBtn.textContent?.includes("Follow"));

    drawnText.length = 0;
    root.render(
      <Chart points={pointsB} label="TCS" seriesKey="NSE_EQ|TCS:today" />,
    );
    await until(() =>
      drawnText.some((t) => t.includes("5,100") || t.includes("5,000")),
    );
    assert.ok(
      drawnText.some((t) => t.includes("5,100") || t.includes("5,000")),
      `expected new instrument values plotted after switching, got: ${drawnText.join(", ")}`,
    );
  } finally {
    root.unmount();
  }
});

test("Lanes CostChart formats sub-cent USD axis ticks without rounding to $0.00 and skips lanes without costs", async () => {
  const { dom, drawnText, container } = setupChartDom();
  const root = createRoot(container);
  const t0 = Date.parse("2026-11-26T09:15:00+05:30");
  const app = createController(dom.window, {
    connect: false,
    fetch: async (url) => {
      const path = String(url).split("?")[0];
      if (path === "/api/v1/meta")
        return new Response(JSON.stringify({ mode: "paper" }));
      if (path === "/api/v1/summary")
        return new Response(
          JSON.stringify({
            mode: "paper",
            session: "2026-11-26",
            as_of_ms: t0 + 120_000,
            revisions: { agent: "a1", agent_turns: "t1" },
          }),
        );
      if (path === "/api/v1/agent")
        return new Response(
          JSON.stringify({
            turns: [
              {
                turn_id: "d1",
                lane: "decision",
                at_ms: t0 + 60_000,
                status: "ok",
                usage: { cost_usd: 0.002 },
              },
              {
                turn_id: "d2",
                lane: "decision",
                at_ms: t0 + 120_000,
                status: "ok",
                usage: { cost_usd: 0.003 },
              },
              {
                turn_id: "g1",
                lane: "gate",
                at_ms: t0 + 90_000,
                status: "ok",
                usage: null,
              },
            ],
            total: 3,
          }),
        );
      if (path === "/api/v1/agent/transcript")
        return new Response(
          JSON.stringify({ exchanges: [], sessions: [], total: 0 }),
        );
      return new Response(JSON.stringify({ items: [], total: 0 }));
    },
  });
  try {
    await app.authenticate("fixture-token");
    await until(() => app.getSnapshot().agentState === "ready");
    const { default: Lanes } = await import("./Lanes");
    root.render(
      <Context.Provider value={app}>
        <Lanes />
      </Context.Provider>,
    );
    await until(() =>
      dom.window.document.querySelector("details.session-charts"),
    );
    const details = dom.window.document.querySelector<HTMLDetailsElement>(
      "details.session-charts",
    )!;
    details.open = true;
    details.dispatchEvent(new dom.window.Event("toggle", { bubbles: true }));
    await until(() => drawnText.some((t) => t.startsWith("$")));
    assert.ok(
      drawnText.some((t) => /^\$0\.00[1-9]\d?$/.test(t)),
      `expected sub-cent precision on CostChart y-axis, got: ${drawnText.filter((t) => t.startsWith("$")).join(", ")}`,
    );
  } finally {
    app.stop();
    root.unmount();
  }
});

test("LivePriceChart shows 'No instrument selected' when watch is empty instead of hanging on Loading", async () => {
  const { dom, container } = setupChartDom();
  const root = createRoot(container);
  try {
    root.render(
      <LivePriceChart
        label="Selected instrument"
        state={{
          watch: "",
          points: [],
          series: { priceState: "idle" },
        }}
      />,
    );
    await until(() =>
      dom.window.document.body.textContent?.includes("No instrument selected"),
    );
    assert.ok(
      !dom.window.document.body.textContent?.includes("Loading…"),
      "empty watch should not show eternal Loading state",
    );
  } finally {
    root.unmount();
  }
});
