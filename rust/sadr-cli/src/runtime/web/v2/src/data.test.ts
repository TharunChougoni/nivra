import type { Point } from "./types";
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  appendPoint,
  downsamplePoints,
  finance,
  fillsFor,
  instrumentClass,
  lotQuantity,
  instrumentClasses,
  linkedFills,
  linkedPlan,
  mergePriceBatch,
  observation,
  plansFor,
  projectSeries,
  reconcileSeries,
  route,
  identities,
  groupRole,
  sessions,
  strategyLinkFor,
  strategyReferences,
} from "./data";
test("financial unknowns and holdings cannot manufacture zero or flat-book returns", () => {
  assert.equal(finance({}).equity, null);
  assert.equal(finance({ portfolio: { cash: "5000" } }).equity, null);
  assert.equal(
    finance({ portfolio: { cash: "5000", positions: {} } }).net,
    null,
  );
  assert.equal(
    finance({
      capital: "5000",
      portfolio: { cash: "5000", positions: {}, fills: [] },
    }).net,
    0,
  );
  assert.equal(
    finance({
      portfolio: { cash: "5000", positions: { a: { mark: null, units: "1" } } },
    }).equity,
    null,
  );
});
test("charts retain original sampled clocks, ignore duplicate/out-of-order observations, and break gaps", () => {
  const point = observation({
    latest_screening_price: "123.45",
    sampled_price_ms: 1000,
    observed_ms: 2000,
    source_ms: 990,
  });
  assert.deepEqual(point, {
    at: 1000,
    value: 123.45,
    source: 990,
    kind: "sampled bid",
  });
  assert.equal(
    observation({ latest_screening_price: "123.45", observed_ms: 2000 }),
    null,
  );
  assert.equal(
    observation({ latest_screening_price: "-1", sampled_price_ms: 2000 }),
    null,
  );
  const points = appendPoint([], point);
  assert.equal(appendPoint(points, { at: 999, value: 10 }), points);
  assert.equal(appendPoint(points, { at: 1000, value: 10 }), points);
  const gapped = appendPoint(points, { at: 3000, value: 124 }, true);
  assert.equal(gapped[1].value, null);
  assert.equal(gapped[2].at, 3000);
});
test("receipt series carry no-trade minutes, keep unobserved breaks and mark isolated points", () => {
  const body = {
    points: [
      {
        at: 1000,
        open: "100",
        high: "102",
        low: "99",
        close: "101",
        tick_count: 3,
        price_kind: "receipt_price",
        bar_kind: "receipt_minute",
        certified_ohlcv: false,
        volume: null,
      },
      { at: 3000, close: "103", tick_count: 1 },
      { at: 6000, close: "104", tick_count: 2 },
      { at: 9000, close: "105", tick_count: 1 },
    ],
    coverage: {
      gaps: [
        { from_ms: 2000, to_ms: 2000, reason: "no_trade" },
        { from_ms: 4000, to_ms: 5000, reason: "unobserved" },
        { from_ms: 7000, to_ms: 8000, reason: "unobserved" },
      ],
    },
    watermark_ms: 9000,
    source: "receipt_derived_feature_store",
  };
  const projected = projectSeries(body, "receipt-minute-bar");
  assert.deepEqual(
    projected.points.map((v) => v.at),
    [1000, 2000, 3000, 4000, 6000, 7000, 9000],
  );
  const carry = projected.points[1];
  assert.equal(carry.value, 101);
  assert.equal(carry.carried, true);
  assert.equal(carry.barKind, "carry_forward");
  assert.equal(carry.tickCount, 0);
  assert.equal(carry.certified, false);
  const unobserved = projected.points[3];
  assert.equal(unobserved.value, null);
  assert.equal(unobserved.gap, true);
  assert.equal(unobserved.reason, "unobserved");
  assert.equal(unobserved.gapTo, 5000);
  assert.equal(projected.points[4].isolated, true);
  assert.equal(projected.points[6].isolated, true);
  assert.equal(projected.points[0].isolated, false);
  assert.equal(projected.points[2].isolated, false);
  assert.equal(projected.points[0].volume, null);
  assert.equal(projected.points[0].certified, false);
  assert.equal(projected.points[0].tickCount, 3);
  assert.equal(projected.watermark, 9000);
  const equity = projectSeries(
    {
      points: [
        {
          at: 1000,
          equity: "5000",
          cash: "5000",
          mark_basis: "recorded_position_marks",
        },
        { at: 2000, equity: null },
      ],
    },
    "account-equity",
  );
  assert.equal(equity.points.length, 1);
  assert.equal(equity.points[0].value, 5000);
  assert.equal(equity.points[0].markBasis, "recorded_position_marks");
});
test("price batches keep a local sequence, flag gaps and reset without a false gap", () => {
  const batch = (seq: number, at: number, price: string) => ({
    kind: "price_batch",
    batch_seq: seq,
    items: [{ instrument_key: "A", receipt_ms: at, last_price: price }],
  });
  const first = mergePriceBatch([], batch(3, 1000, "10"), "A", 0);
  assert.equal(first.matched, true);
  assert.equal(first.gap, false);
  const gap = mergePriceBatch(first.points, batch(5, 3000, "11"), "A", 3);
  assert.equal(gap.gap, true);
  assert.deepEqual(
    gap.points.map((v) => v.at),
    [1000, 2000, 3000],
  );
  assert.equal(gap.points[1].value, null);
  const reset = mergePriceBatch(gap.points, batch(1, 4000, "12"), "A", 5);
  assert.equal(reset.gap, false);
  assert.equal(reset.seq, 1);
  assert.equal(reset.points.at(-1)!.value, 12);
  assert.equal(
    mergePriceBatch([], batch(2, 1000, "10"), "OTHER", 0).matched,
    false,
  );
});
test("authoritative series reads preserve newer stream evidence without duplicating timestamps", () => {
  const body = projectSeries(
    {
      points: [
        { at: 1000, close: "10" },
        { at: 2000, close: "11" },
      ],
      watermark_ms: 2000,
    },
    "receipt-minute-bar",
  );
  const merged = reconcileSeries(
    [
      { at: 3000, value: 30, source: "price_batch" },
      { at: 2000, value: 99, source: "price_batch" },
    ],
    body.points,
    2000,
  );
  assert.deepEqual(
    merged.map((v) => v.at),
    [1000, 2000, 3000],
  );
  assert.equal(merged[1].value, 11);
  assert.equal(merged[2].value, 30);
  assert.deepEqual(
    reconcileSeries([{ at: 900, value: 9 }], body.points, 2000),
    body.points,
  );
  const refetched = reconcileSeries(
    [
      { at: 1500, value: null, gap: true, streamGap: true },
      { at: 3000, value: 30, source: "price_batch" },
    ],
    body.points,
    1000,
    { from_ms: 1000, to_ms: 2000 },
  );
  assert.deepEqual(
    refetched.map((v) => v.at),
    [1000, 2000, 3000],
  );
  assert.equal(
    refetched.some((v) => v.streamGap),
    false,
  );
  const outside = reconcileSeries(
    [{ at: 5000, value: null, gap: true, streamGap: true }],
    body.points,
    1000,
    { from_ms: 1000, to_ms: 2000 },
  );
  assert.equal(outside.at(-1)!.streamGap, true);
});
test("display downsampling preserves extrema, endpoints and explicit gaps", () => {
  const points: Point[] = Array.from({ length: 1000 }, (_, index) => ({
    at: index * 1000,
    value: index === 500 ? -999 : index === 700 ? 999 : Math.sin(index),
  }));
  points.splice(600, 0, { at: 600_500, value: null, gap: true });
  const sampled = downsamplePoints(points, 100);
  assert.ok(sampled.length <= 120, `${sampled.length} points`);
  assert.equal(sampled[0].at, points[0].at);
  assert.equal(sampled.at(-1)!.at, points.at(-1)!.at);
  assert.ok(sampled.some((v) => v.value === -999));
  assert.ok(sampled.some((v) => v.value === 999));
  assert.ok(sampled.some((v) => v.value === null && v.gap));
  assert.deepEqual(
    downsamplePoints(points.slice(0, 4), 100),
    points.slice(0, 4),
  );
});
test("two identities retain separate legacy review allowances and bounded archive routes", () => {
  const ids = identities({
    readiness: {
      models: {
        roles: [
          {
            role: "orchestrator",
            model: "one-access-identity",
            remaining_current: 10,
          },
          { role: "operations", remaining_current: 3 },
          { role: "strategy", remaining_current: 8 },
        ],
      },
    },
  });
  assert.equal(ids.length, 2);
  assert.equal(ids[1].label, "Orchestrator");
  assert.equal(ids[1].role!.model, "one-access-identity");
  assert.deepEqual(
    ids[1].legacy.map((v) => v.remaining_current),
    [3, 8],
  );
  assert.equal(groupRole("supervisor"), "orchestrator");
  assert.equal(groupRole("scorer"), "orchestrator");
  assert.equal(groupRole("decide"), "decide");
  assert.deepEqual(route("#/runs/harness-digest-1"), {
    name: "models",
    run: "harness-digest-1",
  });
  assert.deepEqual(route("#/runs/../secret"), { name: "missing" });
  assert.deepEqual(route("#/now"), { name: "overview" });
  assert.deepEqual(route("#/chat"), { name: "missing" });
});
test("approval projection identities stay separate from legacy task grants", () => {
  const ids = identities({
    approvals: {
      identities: [
        { identity: "digest", role: { role: "digest", model: "luna" } },
        { identity: "decide", role: { role: "decide", model: "grok" } },
        {
          identity: "orchestrator",
          role: { role: "operations", model: "shared" },
          tasks: [
            { role: "operations", remaining_current: 4 },
            { role: "strategy", remaining_current: 9 },
            { role: "scorer", remaining_current: 2 },
          ],
        },
      ],
    },
  });
  assert.deepEqual(
    ids.map((v) => v.label),
    ["Digest", "Orchestrator"],
  );
  assert.equal(ids[1].role!.model, "shared");
  assert.deepEqual(
    ids[1].legacy.map((v) => v.role),
    ["operations", "strategy", "scorer"],
  );
});
test("v1 sessions merge the current projection and preserve archived fill order", () => {
  const state = {
    summary: {
      broker: { data: "broker" },
      account: "account",
      session: "2026-10-29",
      capital: "5000",
      cash: "4900",
      as_of_ms: 2000,
      status: "open",
    },
    portfolio: {
      cash: "4900",
      equity: "4990",
      positions: [{ instrument: { kind: "index" }, units: "1", mark: "90" }],
      recorded_fees: "10",
    },
    sessions: {
      items: [
        {
          id: { broker: "broker", account: "account", date: "2026-10-28" },
          portfolio: {
            capital: "5000",
            cash: "5000",
            positions: {},
            fills: [
              { fee: "1", at: 10 },
              { fee: "2", at: 20 },
            ],
          },
        },
      ],
    },
  };
  const rows = sessions(state, { broker: { data: "broker" } });
  assert.equal(rows.length, 2);
  assert.equal(rows[0].current, true);
  assert.equal(finance(rows[0]).equity, 4990);
  assert.equal(finance(rows[0]).fees, 10);
  assert.deepEqual(fillsFor(state, rows[0]), null);
  assert.deepEqual(
    fillsFor(state, rows[1])!.map((v) => v.at),
    [20, 10],
  );
  assert.equal(plansFor(state, rows[1]), null);
});
test("instrument classes and links use only fields recorded by projections", () => {
  const positions = [
    { instrument: { kind: "index" }, units: "2", mark: "10" },
    { instrument: { kind: "call" }, units: "1", mark: "5" },
    { instrument: { kind: "call" }, units: "3", mark: "2" },
  ];
  assert.deepEqual(instrumentClasses(positions), ["index", "call"]);
  assert.equal(instrumentClass({ instrument: { kind: "PUT" } }), "put");
  assert.equal(instrumentClass({}), null);
  assert.deepEqual(
    instrumentClasses([
      { instrument: { kind: "future" } },
      { instrument: { kind: "etf" } },
    ]),
    ["etf", "future"],
  );
  assert.deepEqual(lotQuantity({ instrument: { lot_size: 25 }, units: "50" }), {
    lots: 2,
    lot_size: 25,
    units: 50,
  });
  assert.equal(lotQuantity({ instrument: { lot_size: 25 }, units: 26 }), null);
  assert.equal(lotQuantity({ units: 25 }), null);
  assert.equal(lotQuantity({ instrument: { lot_size: 0 }, units: 0 }), null);
  assert.equal(
    lotQuantity({ instrument: { lot_size: 1 }, units: "9007199254740993" }),
    null,
  );

  const plans = {
    planner: { decision_id: "d1" },
    actions: [
      { index: 0, action: { instrument_key: "A", side: "buy" } },
      { index: 1, action: { instrument_key: "B", side: "sell" } },
    ],
    execution: [{ decision_id: "d1", index: 0, verdict: "filled" }],
  };
  const linked = linkedPlan(plans, "d1", "A");
  assert.ok(linked);
  assert.ok(linked.actions[0].execution);
  assert.equal(linked.actions.length, 1);
  assert.equal(linked.actions[0].execution.verdict, "filled");
  assert.deepEqual(linkedPlan(plans, "d2", "A")!.actions, []);
  assert.deepEqual(linkedPlan(plans, undefined, "A")!.actions, []);
  assert.deepEqual(linkedPlan(plans, "d1", undefined)!.actions, []);
  assert.equal(linkedPlan({ unavailable: true }, "d1", "A"), null);
  const fills = [
    { instrument: { key: "A" }, decision_id: "d1", fee: "1" },
    { instrument: { key: "A" }, decision_id: "d2", fee: "2" },
  ];
  assert.deepEqual(
    linkedFills(fills, "d1", "A")!.map((v) => v.decision_id),
    ["d1"],
  );
  assert.deepEqual(linkedFills(fills, undefined, "A"), []);
  assert.deepEqual(linkedFills(fills, "d1", undefined), []);
  assert.equal(linkedFills(null, "d1", "A"), null);
  const scanner = [
    {
      instrument_key: "A",
      strategy_spec_id: "champion",
      strategy_spec_version: 2,
      strategy_spec_hash: "hash",
      strategy_scan_id: "scan",
      strategy_score: 0.5,
    },
    {
      instrument_key: "B",
      strategy_spec_id: "champion",
      strategy_spec_version: 2,
      strategy_spec_hash: "hash",
      strategy_scan_id: "scan",
      strategy_score: 0.7,
    },
    { instrument_key: "C" },
  ];
  const references = strategyReferences(scanner);
  assert.equal(references.length, 1);
  assert.deepEqual(references[0].instruments, ["A", "B"]);
  assert.deepEqual(references[0].scores, [0.5, 0.7]);
  assert.equal(references[0].strategy_score, null);
  assert.equal(strategyLinkFor(scanner, "A")!.strategy_scan_id, "scan");
  assert.equal(strategyLinkFor(scanner, "C"), null);
});
