// Domain models for the redesigned workspace. Pure functions over recorded
// projections: nothing here fetches, guesses or fills in a missing value.
import type {
  AgentResponse,
  AgentTurn,
  Fill,
  Instrument,
  MetaResponse,
  PlansResponse,
  SummaryResponse,
} from "./types";
import { numeric } from "./data";

// ---------------------------------------------------------------- mode
export type Mode =
  | { kind: "paper" }
  | { kind: "live"; orderBroker: string; account: string }
  | { kind: "unknown" };

type ModeClaim = "paper" | "live" | "none" | "conflict";
function claimOf(
  source?: Pick<MetaResponse, "mode" | "paper_only" | "live_enabled"> | null,
): ModeClaim {
  if (!source) return "none";
  const live = source.mode === "live" || source.live_enabled === true;
  const paper = source.mode === "paper" || source.paper_only === true;
  if (live && paper) return "conflict";
  if (live) return "live";
  if (paper) return "paper";
  return "none";
}
/** The one source for mode chrome. Missing or contradictory evidence is `unknown`. */
export function modeOf(
  meta: MetaResponse | null | undefined,
  summary?: SummaryResponse | null,
): Mode {
  const claims = [claimOf(meta), claimOf(summary)].filter((v) => v !== "none");
  if (!claims.length || claims.includes("conflict")) return { kind: "unknown" };
  if (claims.some((v) => v !== claims[0])) return { kind: "unknown" };
  if (claims[0] === "paper") return { kind: "paper" };
  return {
    kind: "live",
    orderBroker:
      meta?.broker?.orders || summary?.broker?.orders || "Not reported",
    account: meta?.account || summary?.account || "Not reported",
  };
}

// ------------------------------------------------------- instrument label
export type InstrumentKind =
  "index" | "cash" | "etf" | "future" | "call" | "put" | "unknown";
const KIND_ALIASES: Record<string, InstrumentKind> = {
  index: "index",
  cash: "cash",
  etf: "etf",
  future: "future",
  fut: "future",
  call: "call",
  ce: "call",
  put: "put",
  pe: "put",
};
const MONTHS = [
  "JAN",
  "FEB",
  "MAR",
  "APR",
  "MAY",
  "JUN",
  "JUL",
  "AUG",
  "SEP",
  "OCT",
  "NOV",
  "DEC",
];
export interface InstrumentRecord extends Pick<
  Instrument,
  "key" | "symbol" | "kind" | "underlying" | "expiry" | "strike"
> {
  // Catalog rows record these under other names.
  instrument_key?: string | null;
  underlying_symbol?: string | null;
  expiry_ms?: number | null;
  instrument_type?: string | null;
}
function expiryLabel(rec: InstrumentRecord): string | undefined {
  const text = typeof rec.expiry === "string" ? rec.expiry.trim() : "";
  const iso = /^(\d{4})-(\d{2})-(\d{2})/.exec(text);
  if (iso) return iso[3] + MONTHS[Number(iso[2]) - 1];
  const ms = numeric(rec.expiry_ms);
  if (ms !== null && ms > 0) {
    // Exchange dates are IST calendar dates.
    const d = new Date(ms + 5.5 * 3_600_000);
    return String(d.getUTCDate()).padStart(2, "0") + MONTHS[d.getUTCMonth()];
  }
  return text || undefined;
}
function strikeLabel(rec: InstrumentRecord): string | undefined {
  const value = numeric(rec.strike);
  if (value === null) return undefined;
  return Number.isInteger(value) ? String(value) : String(rec.strike);
}
export function instrumentLabel(rec?: InstrumentRecord | null): {
  primary: string;
  kind: InstrumentKind;
  expiry?: string;
  strike?: string;
} {
  const rawKind = String(rec?.kind ?? rec?.instrument_type ?? "")
    .trim()
    .toLowerCase();
  const kind = KIND_ALIASES[rawKind] ?? "unknown";
  const fallback =
    rec?.symbol || rec?.key || rec?.instrument_key || "Unknown instrument";
  const expiry = expiryLabel(rec ?? {});
  const strike = strikeLabel(rec ?? {});
  const derivative = kind === "future" || kind === "call" || kind === "put";
  const underlying = rec?.underlying_symbol?.trim();
  if (!derivative || !underlying)
    return {
      primary: fallback,
      kind,
      ...(derivative && expiry ? { expiry } : {}),
      ...(derivative && strike ? { strike } : {}),
    };
  const suffix = kind === "future" ? "FUT" : kind === "call" ? "CE" : "PE";
  return {
    primary: [
      underlying,
      expiry,
      kind === "future" ? undefined : strike,
      suffix,
    ]
      .filter(Boolean)
      .join(" "),
    kind,
    ...(expiry ? { expiry } : {}),
    ...(strike ? { strike } : {}),
  };
}

// -------------------------------------------------------- order tracks
export type Stage =
  "intent" | "validated" | "submitted" | "acknowledged" | "filled";
export type StageState = "done" | "failed" | "simulated" | "pending" | "absent";
export const STAGES: Stage[] = [
  "intent",
  "validated",
  "submitted",
  "acknowledged",
  "filled",
];
export interface OrderTrack {
  decisionId: string | null;
  instrument: string | null;
  side: "buy" | "sell" | null;
  at_ms: number | null;
  stages: Record<Stage, StageState>;
  outcome: string;
}
const sideOf = (v?: string | null) => {
  const s = String(v ?? "").toLowerCase();
  return s === "buy" || s === "sell" ? s : null;
};
const fillAt = (f: Fill) => numeric(f.at) ?? (Date.parse(String(f.at)) || null);
function track(
  base: Pick<OrderTrack, "decisionId" | "instrument" | "side" | "at_ms">,
  validated: StageState,
  filled: boolean,
  mode: Mode,
  outcome: string,
): OrderTrack {
  const refused = validated === "failed";
  // Submission and ACK are never inferred from the book alone. In paper the
  // deterministic ledger simulates them; live will read real order records.
  const orderStage: StageState = refused
    ? "absent"
    : mode.kind === "paper"
      ? filled
        ? "simulated"
        : "absent"
      : filled
        ? "done"
        : validated === "done"
          ? "pending"
          : "absent";
  return {
    ...base,
    stages: {
      intent: "done",
      validated,
      submitted: orderStage,
      acknowledged: orderStage,
      filled: filled ? "done" : refused ? "absent" : "pending",
    },
    outcome,
  };
}
const ACCEPTED = /^accepted$/i;
/**
 * One track per recorded plan action of the current decision, plus one per
 * fill that no plan action explains. `filled` comes from a fill record only.
 * Newest first; actions without a time lead.
 */
export function orderTracks(
  plans: PlansResponse | null | undefined,
  fills: Fill[] | null | undefined,
  mode: Mode,
): OrderTrack[] {
  const decision = plans?.planner?.decision_id ?? null;
  const execution = Array.isArray(plans?.execution) ? plans.execution : [];
  const pool = [...(fills ?? [])];
  const out: OrderTrack[] = [];
  for (const row of Array.isArray(plans?.actions) ? plans.actions : []) {
    const key = row.action?.instrument_key ?? null;
    const side = sideOf(row.action?.side);
    const reason = row.reason ?? null;
    const validated: StageState = !reason
      ? "pending"
      : ACCEPTED.test(reason)
        ? "done"
        : "failed";
    const at = pool.findIndex(
      (f) =>
        decision !== null &&
        String(f.decision_id) === String(decision) &&
        f.instrument?.key === key &&
        (side === null || f.buy === (side === "buy")),
    );
    const fill = at >= 0 ? pool.splice(at, 1)[0] : null;
    const verdict = execution.find(
      (e) => e.decision_id === decision && e.index === row.index,
    )?.verdict;
    out.push(
      track(
        {
          decisionId: decision,
          instrument: key,
          side,
          at_ms: fill ? fillAt(fill) : null,
        },
        validated,
        !!fill,
        mode,
        fill
          ? "filled"
          : validated === "failed"
            ? `rejected · ${reason!.replaceAll("_", " ")}`
            : verdict
              ? verdict.replaceAll("_", " ")
              : validated === "done"
                ? "awaiting execution record"
                : "validation not recorded",
      ),
    );
  }
  for (const f of pool)
    out.push(
      track(
        {
          decisionId: f.decision_id ?? null,
          instrument: f.instrument?.key ?? null,
          side: f.buy === undefined ? null : f.buy ? "buy" : "sell",
          at_ms: fillAt(f),
        },
        // A fill only exists for an action the deterministic validator passed.
        "done",
        true,
        mode,
        "filled",
      ),
    );
  return out.sort((a, b) => (b.at_ms ?? Infinity) - (a.at_ms ?? Infinity));
}

// ---------------------------------------------------------- lane stats
export const LANE_ORDER = ["decision", "gate", "research", "review"];
export interface LaneStats {
  lane: string;
  /** Turns in the loaded page. Failed, fallbacks and latency come from these. */
  turns: number;
  /** Provider calls from the API usage totals, when reported. */
  calls: number | null;
  failed: number;
  fallbacks: number;
  p50_ms: number | null;
  p95_ms: number | null;
  input: number | null;
  output: number | null;
  cache_read: number | null;
  cost_usd: number | null;
  usage_basis: "api" | "loaded" | null;
  last?: AgentTurn;
}
const FAILED = /fail|error|abort|timeout/i;
export const turnFailed = (t: AgentTurn) =>
  !!t.error || (!!t.status && FAILED.test(t.status));
function percentile(sorted: number[], p: number): number | null {
  if (!sorted.length) return null;
  return sorted[Math.min(sorted.length - 1, Math.ceil(p * sorted.length) - 1)];
}
export function laneStats(
  turns: AgentTurn[] | null | undefined,
  usage?: AgentResponse["usage"],
): LaneStats[] {
  const byLane = new Map<string, AgentTurn[]>(LANE_ORDER.map((l) => [l, []]));
  for (const t of turns ?? []) {
    const list = byLane.get(t.lane) ?? [];
    list.push(t);
    byLane.set(t.lane, list);
  }
  for (const lane of Object.keys(usage?.lanes ?? {}))
    if (!byLane.has(lane)) byLane.set(lane, []);
  const lanes = [...byLane.keys()].sort((a, b) => {
    const left = LANE_ORDER.indexOf(a),
      right = LANE_ORDER.indexOf(b);
    return (
      (left < 0 ? LANE_ORDER.length : left) -
        (right < 0 ? LANE_ORDER.length : right) || a.localeCompare(b)
    );
  });
  return lanes.map((lane) => {
    const rows = byLane.get(lane)!;
    const latencies = rows
      .map((t) => numeric(t.elapsed_ms))
      .filter((v): v is number => v !== null)
      .sort((a, b) => a - b);
    const api = usage?.lanes?.[lane];
    const loadedUsage = rows.filter((t) => t.usage);
    const sum = (pick: (u: NonNullable<AgentTurn["usage"]>) => unknown) => {
      const values = loadedUsage
        .map((t) => numeric(pick(t.usage!)))
        .filter((v): v is number => v !== null);
      return values.length ? values.reduce((a, b) => a + b, 0) : null;
    };
    const total = (
      pick: (u: NonNullable<AgentTurn["usage"]>) => unknown,
    ): number | null => (api ? numeric(pick(api)) : sum(pick));
    return {
      lane,
      turns: rows.length,
      calls: numeric(api?.calls),
      failed: rows.filter(turnFailed).length,
      fallbacks: rows.filter((t) => !!t.fallback).length,
      p50_ms: percentile(latencies, 0.5),
      p95_ms: percentile(latencies, 0.95),
      input: total((u) => u.input),
      output: total((u) => u.output),
      cache_read: total((u) => u.cache_read),
      cost_usd: total((u) => u.cost_usd),
      usage_basis: api ? "api" : loadedUsage.length ? "loaded" : null,
      last: rows.reduce<AgentTurn | undefined>(
        (best, t) => (!best || (t.at_ms ?? 0) >= (best.at_ms ?? 0) ? t : best),
        undefined,
      ),
    };
  });
}
