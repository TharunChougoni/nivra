import { test } from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { JSDOM } from "jsdom";
import { BookContinuity } from "./Operational";
import type { SummaryResponse } from "./types";

test("the paper account shows each sleeve's money in plain words", () => {
  const summary = {
    v: 1,
    mode: "paper",
    paper_only: true,
    session: "2026-11-30",
    as_of_ms: 1_700_000_000_000,
    capital: "10000",
    state: { stopped: false, halted: false, closed: false, health: "ok" },
    sleeves: [
      {
        sleeve: "cash",
        capital: "5000",
        cash: "3000",
        equity: "5100",
        pnl: "100",
        open_positions: 2,
      },
      {
        sleeve: "options",
        capital: "5000",
        cash: "5000",
        equity: "4950",
        pnl: "-50",
        open_positions: 0,
      },
    ],
  } as SummaryResponse;
  const html = renderToStaticMarkup(<BookContinuity summary={summary} />);
  const text = new JSDOM(html).window.document.body.textContent ?? "";
  for (const shown of [
    "Stock strategies",
    "Nifty options",
    "₹5,100",
    "+₹100",
    "-₹50",
  ]) {
    assert.ok(text.includes(shown), `${shown} missing from: ${text}`);
  }
  assert.ok(!text.includes("options_sleeve") && !text.includes("cash_sleeve"));
});
