import assert from "node:assert/strict";
import test from "node:test";
import {
  TOOL_LABELS,
  TRIGGER_LABELS,
  TURN_ERROR_LABELS,
  instrumentName,
  plain,
  prospectTitle,
  shortId,
  spoken,
  turnOutcome,
} from "./labels";

test("labels humanize codes, shorten ids and drop instrument segment prefixes", () => {
  assert.equal(plain("paper_only"), "Paper only");
  assert.equal(plain("turn_timeout", TURN_ERROR_LABELS), "Timed out");
  assert.equal(plain("web_fetch", TOOL_LABELS), "Read page");
  assert.equal(plain("turn_timeout"), "Turn timeout");
  assert.equal(plain("PARTIAL"), "Partial");
  assert.equal(plain("opening-range-breakout"), "Opening range breakout");
  assert.equal(spoken("Stance take_profit; hold"), "Stance take profit; hold");
  assert.equal(plain(null), "Not reported");
  assert.equal(plain(""), "Not reported");
  assert.equal(
    turnOutcome({
      turn_id: "t",
      lane: "decision",
      plan: { status: "validated", actions: 0 },
    }),
    "No trade",
  );

  const prospect = {
    rank: 1,
    instrument_key: "NSE_FO|58743",
    idea: "buy_call",
    strike: "26000",
    expiry: "2026-10-20",
    lot_size: 65,
    evidence: "premium +20%",
    why: "momentum",
  };
  assert.equal(
    turnOutcome({
      turn_id: "g",
      lane: "gate",
      verdict: { escalate: true, reason: "ATM call jumped", prospects: [prospect] },
    }),
    "Escalated · 1 prospect · ATM call jumped",
  );
  assert.equal(
    prospectTitle(
      prospect,
      new Map([["NSE_FO|58743", { key: "NSE_FO|58743", symbol: "NIFTY 26000 CE" }]]),
    ),
    "NIFTY 26000 CE · Buy call · strike 26000 · expiry 2026-10-20 · lot 65",
  );
  assert.equal(plain("premium_move", TRIGGER_LABELS), "option premium moved");

  const hash = "a".repeat(64);
  assert.deepEqual(shortId(hash), { text: "aaaaaaaa", title: hash });
  assert.deepEqual(shortId("scan-1"), { text: "scan-1" });
  assert.deepEqual(shortId(null), { text: "Not reported" });
  assert.equal(instrumentName("NSE_EQ|INE397D01024"), "INE397D01024");
  assert.equal(instrumentName("RELIANCE"), "RELIANCE");
  const index = new Map([
    ["NSE_EQ|INE002A01018", { key: "NSE_EQ|INE002A01018", symbol: "RELIANCE" }],
  ]);
  assert.equal(instrumentName("NSE_EQ|INE002A01018", index), "RELIANCE");
  assert.equal(instrumentName(null), "Unknown instrument");
});
