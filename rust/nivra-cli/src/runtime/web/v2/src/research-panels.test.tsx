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

const now = Date.now();
const envelope = {
  v: 1,
  mode: "paper",
  paper_only: true,
  broker: { data: "fixture-broker" },
  account: "private-fixture",
  session: "2026-10-29",
  as_of_ms: now - 5_000,
};

function summary(extra: Record<string, unknown> = {}): SummaryResponse {
  return {
    ...envelope,
    capital: "5000",
    cash: "5000",
    equity: "5000",
    status: "open",
    state: { stopped: false, halted: false, closed: false, health: "ok" },
    research_library: {
      status: "ready",
      items: [],
      truncated: false,
      activation_authorized: false,
    },
    ...extra,
  } as unknown as SummaryResponse;
}

function portfolio(
  overrides: Partial<PortfolioResponse> = {},
): PortfolioResponse {
  return {
    ...envelope,
    capital: "5000",
    cash: "5000",
    equity: "5000",
    positions: [],
    fills: [],
    stopped: false,
    halted: false,
    closed: false,
    ...overrides,
  };
}

function text(node: React.ReactElement) {
  const dom = new JSDOM(renderToStaticMarkup(node));
  return dom.window.document.body.textContent || "";
}

test("book continuity keeps stopped and halted books explicit without controls", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        state: {
          stopped: true,
          halted: false,
          closed: false,
          health: "stopped",
        },
      })}
      portfolio={portfolio({ stopped: true })}
    />,
  );
  assert.match(textValue, /Paper finances/);
  assert.match(textValue, /Stopped/);
  assert.match(textValue, /₹5,000\.00/);
});

test("halted book stays explicit", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        state: {
          stopped: false,
          halted: true,
          closed: false,
          health: "halted",
        },
      })}
      portfolio={portfolio({ halted: true })}
    />,
  );
  assert.match(textValue, /Halted/);
});

test("the research loop says it waits for tape and shows today's allowance and its latest verdict", () => {
  const loop = (extra: Record<string, unknown>) =>
    text(
      <OperationalPanel
        summary={summary({
          research_loop: {
            enabled: true,
            state: "waiting",
            reason: "waiting_for_tape",
            next_step_ms: now + 300_000,
            budget: { variants_left: 29, turns_left: 58, calls_left: 190, usd_left: 7.85 },
            campaign: { name: "bars-v1", trials_used: 1, trial_cap: 60 },
            latest: {
              ordinal: 1,
              status: "scored",
              verdict: "keep",
              net_pnl: "120",
              idea: { family: "breakout" },
            },
            incumbents: 1,
            ...extra,
          },
        })}
        portfolio={portfolio()}
      />,
    );
  const waiting = loop({});
  assert.match(waiting, /Research loop/);
  assert.match(waiting, /Waiting for tape/);
  assert.match(waiting, /29 ideas · 58 turns · \$7\.85/);
  assert.match(waiting, /1 of 60/);
  assert.match(waiting, /Idea 1 · Breakout · Kept · ₹120\.00 after costs/);
  assert.doesNotMatch(waiting, /waiting_for_tape|day_usd/);
  assert.match(loop({ reason: "day_usd_exhausted" }), /Today's research spend is used up/);
  assert.match(loop({ enabled: false }), /The research loop is off/);
});
