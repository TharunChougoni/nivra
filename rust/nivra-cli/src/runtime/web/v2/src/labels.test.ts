import assert from "node:assert/strict";
import test from "node:test";
import {
  TOOL_LABELS,
  TURN_ERROR_LABELS,
  instrumentName,
  plain,
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
