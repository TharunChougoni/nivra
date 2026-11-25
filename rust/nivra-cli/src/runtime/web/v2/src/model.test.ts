import { test } from "node:test";
import assert from "node:assert/strict";
import type {
  AgentTurn,
  Fill,
  MetaResponse,
  PlansResponse,
  SummaryResponse,
} from "./types";
import {
  conditionOf,
  equityStats,
  instrumentLabel,
  journalCoverage,
  laneStats,
  modeOf,
  orderTracks,
} from "./model";
import type { Mode } from "./model";

const paper: Mode = { kind: "paper" };
test("modeOf is paper only when every source says paper, live only when every source says live", () => {
  const paperMeta: MetaResponse = { mode: "paper", paper_only: true };
  assert.deepEqual(modeOf(paperMeta, null), { kind: "paper" });
  assert.deepEqual(modeOf(paperMeta, { mode: "paper" }), { kind: "paper" });
  assert.deepEqual(
    modeOf(
      {
        mode: "live",
        live_enabled: true,
        broker: { orders: "kotak" },
        account: "acct-1",
      },
      { mode: "live", live_enabled: true },
    ),
    { kind: "live", orderBroker: "kotak", account: "acct-1" },
  );
});
test("modeOf is unknown when evidence is missing or contradictory, never paper by default", () => {
  assert.deepEqual(modeOf(null, null), { kind: "unknown" });
  assert.deepEqual(modeOf({}, {}), { kind: "unknown" });
  // meta and summary disagree
  const live: SummaryResponse = { mode: "live", live_enabled: true };
  assert.deepEqual(modeOf({ mode: "paper", paper_only: true }, live), {
    kind: "unknown",
  });
  // one source contradicts itself
  assert.deepEqual(modeOf({ mode: "paper", live_enabled: true }, null), {
    kind: "unknown",
  });
  assert.deepEqual(modeOf({ mode: "live", paper_only: true }, null), {
    kind: "unknown",
  });
});

test("instrumentLabel builds derivative labels only from recorded parts", () => {
  assert.deepEqual(
    instrumentLabel({
      symbol: "NIFTY 24500 CE NOV",
      kind: "call",
      underlying_symbol: "NIFTY",
      expiry: "2026-11-25",
      strike: "24500.00",
    }),
    {
      primary: "NIFTY 25NOV 24500 CE",
      kind: "call",
      expiry: "25NOV",
      strike: "24500",
    },
  );
  assert.equal(
    instrumentLabel({
      kind: "future",
      underlying_symbol: "NIFTY",
      expiry_ms: Date.parse("2026-11-25T15:30:00+05:30"),
    }).primary,
    "NIFTY 25NOV FUT",
  );
  // a missing strike stays out; nothing is guessed
  assert.equal(
    instrumentLabel({
      kind: "put",
      underlying_symbol: "BANKNIFTY",
      expiry: "2026-12-30",
    }).primary,
    "BANKNIFTY 30DEC PE",
  );
  // no recorded underlying: the recorded symbol is the label
  assert.deepEqual(
    instrumentLabel({ symbol: "RELIANCE", key: "NSE_EQ|X", kind: "cash" }),
    { primary: "RELIANCE", kind: "cash" },
  );
  assert.equal(
    instrumentLabel({
      kind: "future",
      symbol: "NIFTY FUT",
      expiry: "2026-11-25",
    }).primary,
    "NIFTY FUT",
  );
  assert.equal(
    instrumentLabel({ key: "K|1", kind: "mystery" }).kind,
    "unknown",
  );
  assert.equal(instrumentLabel(null).primary, "Unknown instrument");
});

const plans = (
  actions: PlansResponse["actions"],
  execution: PlansResponse["execution"] = [],
): PlansResponse => ({
  planner: { decision_id: "d1" },
  actions,
  execution,
});
const fill = (over: Partial<Fill> = {}): Fill => ({
  instrument: { key: "A|1" },
  buy: true,
  at: 5000,
  decision_id: "d1",
  ...over,
});
test("orderTracks marks filled only from a fill record and simulates submit/ack in paper", () => {
  const tracks = orderTracks(
    plans(
      [
        {
          index: 0,
          reason: "accepted",
          action: { instrument_key: "A|1", side: "buy" },
        },
        {
          index: 1,
          reason: "accepted",
          action: { instrument_key: "B|2", side: "sell" },
        },
        {
          index: 2,
          reason: "tick_invalid",
          action: { instrument_key: "C|3", side: "buy" },
        },
      ],
      [
        {
          decision_id: "d1",
          index: 1,
          verdict: "execution_gate_rejected",
          reason: "stale_quote",
        },
      ],
    ),
    [fill()],
    paper,
  );
  const by = (key: string) => tracks.find((t) => t.instrument === key)!;
  assert.deepEqual(by("A|1").stages, {
    intent: "done",
    validated: "done",
    submitted: "simulated",
    acknowledged: "simulated",
    filled: "done",
  });
  // accepted and executed without a fill: never filled, never submitted
  assert.deepEqual(by("B|2").stages, {
    intent: "done",
    validated: "done",
    submitted: "absent",
    acknowledged: "absent",
    filled: "pending",
  });
  assert.equal(by("B|2").outcome, "refused: the quote was too old");
  // a rejected validation ends the track
  assert.deepEqual(by("C|3").stages, {
    intent: "done",
    validated: "failed",
    submitted: "absent",
    acknowledged: "absent",
    filled: "absent",
  });
  assert.match(by("C|3").outcome, /rejected · tick invalid/);
});
test("orderTracks keeps fills no plan explains, newest first, and never invents a time", () => {
  const tracks = orderTracks(
    plans([
      {
        index: 0,
        reason: "accepted",
        action: { instrument_key: "A|1", side: "buy" },
      },
    ]),
    [
      fill({ instrument: { key: "Z|9" }, decision_id: "old", at: 1000 }),
      fill({
        instrument: { key: "Y|8" },
        decision_id: "old2",
        at: 3000,
        buy: false,
      }),
    ],
    paper,
  );
  // the unfilled current action has no time and leads
  assert.deepEqual(
    tracks.map((t) => [t.instrument, t.at_ms]),
    [
      ["A|1", null],
      ["Y|8", 3000],
      ["Z|9", 1000],
    ],
  );
  assert.equal(tracks[1].side, "sell");
  assert.equal(tracks[1].stages.filled, "done");
  assert.equal(orderTracks(null, null, paper).length, 0);
});
test("in live mode submit/ack are never simulated and wait for real records", () => {
  const live: Mode = { kind: "live", orderBroker: "b", account: "a" };
  const [open] = orderTracks(
    plans([
      {
        index: 0,
        reason: "accepted",
        action: { instrument_key: "A|1", side: "buy" },
      },
    ]),
    [],
    live,
  );
  assert.equal(open.stages.submitted, "pending");
  assert.equal(open.stages.acknowledged, "pending");
  assert.equal(open.stages.filled, "pending");
  const [filled] = orderTracks(
    plans([
      {
        index: 0,
        reason: "accepted",
        action: { instrument_key: "A|1", side: "buy" },
      },
    ]),
    [fill()],
    live,
  );
  assert.equal(filled.stages.submitted, "done");
  assert.equal(filled.stages.filled, "done");
});

const turn = (lane: string, over: Partial<AgentTurn> = {}): AgentTurn => ({
  turn_id: `${lane}-${over.at_ms ?? 0}`,
  lane,
  at_ms: 1,
  status: "ok",
  elapsed_ms: 1000,
  usage: { input: 10, output: 5, cache_read: 2, cost_usd: 0.5, calls: 1 },
  ...over,
});
test("laneStats orders lanes, counts failures and fallbacks, and takes totals from the API", () => {
  const stats = laneStats(
    [
      turn("review", { at_ms: 9 }),
      turn("decision", { at_ms: 1, elapsed_ms: 100 }),
      turn("decision", { at_ms: 2, elapsed_ms: 300, status: "failed" }),
      turn("decision", {
        at_ms: 3,
        elapsed_ms: 200,
        fallback: { reason: "timeout" },
      }),
      turn("decision", { at_ms: 4, elapsed_ms: 900, error: "boom" }),
      turn("custom", { at_ms: 5 }),
    ],
    {
      lanes: {
        decision: {
          input: 1000,
          output: 500,
          cache_read: 7,
          cost_usd: 2.25,
          calls: 8,
        },
      },
    },
  );
  assert.deepEqual(
    stats.map((s) => s.lane),
    ["decision", "gate", "research", "review", "custom"],
  );
  const decision = stats[0];
  assert.equal(decision.turns, 4);
  assert.equal(decision.failed, 2);
  assert.equal(decision.fallbacks, 1);
  assert.equal(decision.p50_ms, 200);
  assert.equal(decision.p95_ms, 900);
  // API totals win over the loaded page
  assert.equal(decision.cost_usd, 2.25);
  assert.equal(decision.input, 1000);
  assert.equal(decision.usage_basis, "api");
  assert.equal(decision.last?.at_ms, 4);
  // no API row: sum what was loaded and say so
  assert.equal(stats[3].cost_usd, 0.5);
  assert.equal(stats[3].usage_basis, "loaded");
});
test("laneStats keeps unknown usage unknown instead of zero", () => {
  const [decision, gate] = laneStats(
    [turn("decision", { usage: null, elapsed_ms: undefined })],
    null,
  );
  assert.equal(decision.cost_usd, null);
  assert.equal(decision.p50_ms, null);
  assert.equal(decision.usage_basis, null);
  assert.equal(gate.turns, 0);
  assert.equal(gate.last, undefined);
});
test("equityStats reads drawdown and day net only from recorded checkpoints", () => {
  const pts = [100, 110, 99, 105].map((value, i) => ({ at: i * 1000, value }));
  const stats = equityStats(pts);
  assert.equal(stats.dayNet, 5);
  assert.equal(stats.maxDrawdownPct, -10);
  assert.equal(stats.troughAt, 2000);
  // one checkpoint is not a day
  assert.equal(equityStats([{ at: 1, value: 5 }]).dayNet, null);
  assert.equal(equityStats([{ at: 1, value: 5 }]).maxDrawdownPct, null);
  // carried and gap points are not observations
  assert.equal(
    equityStats([
      { at: 1, value: 100 },
      { at: 2, value: 50, carried: true },
      { at: 3, value: null, gap: true },
      { at: 4, value: 102 },
    ]).maxDrawdownPct,
    0,
  );
});
test("a truncated journal is reported as newest N with the rest not loaded, never as a total", () => {
  const rows = [turn("gate"), turn("gate")];
  assert.equal(journalCoverage({ turns: rows, total: 2 }).label, null);
  const cut = journalCoverage({
    turns: rows,
    total: 500,
    truncated: true,
    rows_not_loaded: { turns: 498, triggers: 0 },
  });
  assert.equal(cut.label, "showing newest 2; 498 older not loaded");
  assert.equal(
    journalCoverage({ turns: rows, truncated: true }).label,
    "showing newest 2; older turns not loaded",
  );
});

test("workspace condition: a halted book outranks research mode, which outranks missing sources", () => {
  const missing = [
    {
      source: "portfolio",
      state: "unavailable" as const,
      reason: "portfolio_not_reported",
    },
  ];
  const at = (summary: SummaryResponse | null) =>
    conditionOf({ summary, issues: missing, snapshotState: "unavailable" })
      .kind;
  assert.equal(at({ status: "starting" }), "starting");
  assert.equal(
    at({ status: "research_only", state: { halted: true } }),
    "halted",
  );
  assert.equal(at({ status: "research_only" }), "research");
  assert.equal(at({ state: { closed: true } }), "closed");
  assert.equal(at({ status: "running" }), "degraded");
  assert.equal(
    conditionOf({
      summary: { status: "running" },
      issues: [],
      snapshotState: "ready",
    }).kind,
    "nominal",
  );
});
