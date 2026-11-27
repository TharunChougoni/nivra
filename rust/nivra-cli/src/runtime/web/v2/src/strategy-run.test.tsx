import { test } from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { JSDOM } from "jsdom";
import { StrategyRun } from "./StrategyRun";
import type { StrategyExecution } from "./types";

const text = (node: React.ReactElement) =>
  new JSDOM(renderToStaticMarkup(node)).window.document.body.textContent || "";

test("the strategy panel names the active strategy, its actions, refusals and fills in plain words", () => {
  const id = "ab12cd34".repeat(8);
  const run: StrategyExecution = {
    enabled: true,
    state: "ready",
    reason: null,
    strategy: {
      strategy_id: id,
      author: { lane: "research", model: "fixture-model" },
      summary: { trades: 12, net_pnl: "41.5" },
      tape_dates: ["2026-11-25"],
    },
    calls: 6,
    failures: 1,
    skipped: 2,
    last_failure: "strategy_wall_timeout",
    actions: [
      {
        at_ms: 1795578605000,
        instrument_key: "NSE_EQ|TESTA",
        outcome: "action_sell_unheld",
        units: 9,
        side: "sell",
      },
      {
        at_ms: 1795578600000,
        instrument_key: "NSE_EQ|TESTB",
        outcome: "price_above_max",
        units: 20,
        side: "buy",
      },
      {
        at_ms: 1795578600000,
        instrument_key: "NSE_EQ|TESTA",
        outcome: "stale_quote",
        units: 10,
        side: "buy",
      },
    ],
    refusals: [
      {
        at_ms: 1795578600000,
        reason: "price_above_max",
        action: { instrument_key: "NSE_EQ|TESTB" },
      },
    ],
    fills: [
      {
        at: 1795578600000,
        buy: true,
        units: 10,
        price: "100",
        instrument: { key: "NSE_EQ|TESTA", symbol: "TESTA" },
      },
    ],
  };
  const shown = text(<StrategyRun run={run} />);
  for (const phrase of [
    "Running on each scan",
    "ab12cd34",
    "Research · fixture-model",
    "12 trades",
    "6 · 1 failed · 2 skipped",
    "The strategy ran past its time limit",
    "Refused: nothing held to sell",
    "Refused: the price was above the strategy's limit",
    "Refused: the quote was too old",
    "TESTB",
    "Buy",
  ])
    assert.ok(shown.includes(phrase), phrase);
  // No code names or full hashes reach the page.
  for (const raw of [
    "action_sell_unheld",
    "price_above_max",
    "strategy_wall_timeout",
    id,
  ])
    assert.ok(!shown.includes(raw), raw);

  assert.ok(
    text(<StrategyRun run={{ enabled: false }} />).includes(
      "Strategy execution is off",
    ),
  );
  assert.ok(
    text(
      <StrategyRun
        run={{
          enabled: true,
          state: "blocked",
          reason: "strategy_engine_changed",
        }}
      />,
    ).includes("Accepted on a different engine build"),
  );
});

test("a fallback reads as a shipped strategy that has not passed acceptance, and says why it runs", () => {
  const fallback = text(
    <StrategyRun
      run={{
        enabled: true,
        state: "ready",
        reason: "no_active_strategy",
        fallback: ["vwap_trend", "opening_range_breakout"],
        strategy: {
          strategy_id: "fallback:vwap_trend",
          basis: "fallback",
          name: "vwap_trend",
        },
      }}
    />,
  );
  for (const phrase of [
    "VWAP trend",
    "Fallback: a shipped strategy that has not passed acceptance",
    "Why a fallback runs",
    "No accepted strategy yet",
    "VWAP trend, Opening range breakout",
  ])
    assert.ok(fallback.includes(phrase), phrase);
  for (const raw of ["fallback:", "vwap_trend", "no_active_strategy"])
    assert.ok(!fallback.includes(raw), raw);
  const none = text(
    <StrategyRun
      run={{
        enabled: true,
        state: "blocked",
        reason: "strategy_lane_stood_down",
      }}
    />,
  );
  for (const phrase of [
    "Why none is running",
    "The decision lane chose to run no strategy",
    "None configured",
  ])
    assert.ok(none.includes(phrase), phrase);
});
