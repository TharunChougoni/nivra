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
