import { test } from "node:test";
import assert from "node:assert/strict";
import {
  alertStoreLabel,
  calendarOutsideLabel,
  calendarSource,
  coverageLabel,
  daysRemaining,
  durationOrUnknown,
  exchangeDate,
  failureCategory,
  freshness,
  lastCompletedAt,
  livenessLabel,
  metric,
  operationalHealthState,
  secondsLabel,
  yesNo,
} from "./operational";

test("null operational counts stay unknown instead of becoming zero", () => {
  assert.equal(metric(null), "Not reported");
  assert.equal(metric(undefined), "Not reported");
  assert.equal(metric(0), "0");
  assert.equal(metric("7"), "7");
  assert.equal(yesNo(null), "Not reported");
  assert.equal(yesNo(false), "No");
  assert.equal(yesNo(true), "Yes");
  assert.equal(secondsLabel(3600), "3600 s");
  assert.equal(secondsLabel(null), "Not reported");
  assert.equal(durationOrUnknown(null), "Not reported");
  assert.equal(durationOrUnknown(1500), "1.5 s");
});

test("freshness uses the published capture and flags a future browser clock", () => {
  const recent = freshness(900_000, 960_000);
  assert.equal(recent.state, "recent");
  assert.equal(recent.age_ms, 60_000);
  assert.equal(recent.label, "Recent capture");
  const old = freshness(900_000, 2_000_000);
  assert.equal(old.state, "old");
  assert.equal(old.age_ms, 1_100_000);
  const future = freshness(900_000, 800_000);
  assert.equal(future.state, "future");
  assert.equal(future.label, "Clock anomaly");
  assert.equal(freshness(null, 1_000).state, "unknown");
  assert.equal(freshness(900_000, null).state, "unknown");
});

test("calendar labels name missing coverage and keep exchange dates explicit", () => {
  assert.equal(
    calendarOutsideLabel({ requested_session: "2027-01-04" }),
    "No 2027 data",
  );
  assert.equal(
    calendarOutsideLabel({ current_operational_date: "2027-01-01" }),
    "No 2027 data",
  );
  assert.equal(
    calendarOutsideLabel({
      covered: false,
      current_operational_date: "2027-01-01",
      requested_session: "2026-12-31",
    }),
    "No 2027 data",
  );
  assert.equal(
    calendarOutsideLabel({
      covered: true,
      requested_session_covered: false,
      requested_session: "2027-01-04",
      current_operational_date: "2026-12-31",
    }),
    "No 2027 data",
  );
  assert.equal(calendarOutsideLabel(null), "No calendar data");
  assert.equal(calendarSource("NSE/CMTR/71775"), "NSE/CMTR/71775");
  assert.equal(calendarSource("raw source with spaces"), "Not verified");
  assert.equal(calendarSource("NSE/EVIL/1"), "Not verified");
  assert.equal(coverageLabel(true), "Covered");
  assert.equal(coverageLabel(false), "Outside coverage");
  assert.equal(coverageLabel(null), "Not reported");
  assert.equal(daysRemaining(-3), "3 days past coverage");
  assert.equal(daysRemaining(0), "Expires today");
  assert.equal(daysRemaining(12), "12 days");
  assert.equal(daysRemaining(null), "Not reported");
  assert.equal(exchangeDate("2026-12-31"), "2026-12-31 IST");
  assert.equal(exchangeDate("not-a-date"), "Not reported");
});

test("alert and liveness labels come from trusted static enums only", () => {
  assert.equal(alertStoreLabel("available"), "Available");
  assert.equal(alertStoreLabel("provider raw text"), "Not reported");
  assert.equal(failureCategory("rate_limited"), "Rate limited");
  assert.equal(failureCategory("provider raw text"), "Unknown category");
  assert.equal(livenessLabel("future"), "Future clock");
  assert.equal(livenessLabel("provider raw text"), "Not reported");
});

test("operational health version and absence stay explicit", () => {
  assert.equal(operationalHealthState(null), "missing");
  assert.equal(operationalHealthState(undefined), "missing");
  assert.equal(operationalHealthState({ version: 2 }), "present");
  // version 1 is the retired shape; it is not interpreted
  assert.equal(operationalHealthState({ version: 1 }), "unsupported");
  assert.equal(operationalHealthState({}), "unsupported");
  assert.equal(
    operationalHealthState({ version: "2" } as unknown as Parameters<
      typeof operationalHealthState
    >[0]),
    "unsupported",
  );
  assert.equal(
    operationalHealthState(
      "raw" as unknown as Parameters<typeof operationalHealthState>[0],
    ),
    "unsupported",
  );
});

test("last completed timestamps require positive finite milliseconds", () => {
  assert.equal(lastCompletedAt({ last_completed_at_ms: 0 }, null), null);
  assert.equal(lastCompletedAt({ last_completed_at_ms: -1 }, null), null);
  assert.equal(
    lastCompletedAt({ last_completed_at_ms: "bad" as unknown as number }, null),
    null,
  );
  assert.equal(
    lastCompletedAt({ last_completed_at_ms: 900_000 }, null),
    900_000,
  );
  assert.equal(lastCompletedAt(null, { captured_at_ms: 900_000 }), 900_000);
  assert.equal(lastCompletedAt(null, { captured_at_ms: 0 }), null);
  assert.equal(freshness(0, 1_000).state, "unknown");
});
