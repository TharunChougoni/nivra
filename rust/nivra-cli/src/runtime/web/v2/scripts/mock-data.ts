// DEV-ONLY fixture generator for the dashboard mock API (scripts/mock-api.ts).
// Everything here is synthetic and seeded: one IST trading day of NSE_EQ minute
// bars, a paper book (cash, a future, a call and a put), plans, scanner rows,
// pi lane turns that all agree with each other.
// `createMockData(seed)` returns plain objects; `projectAt(data, now, wall)`
// derives the time-dependent wire projections. Nothing here is shipped: the
// production bundle never imports this file.
import type {
  LaneBudget,
  ActivityAttribution,
  AgentResponse,
  AgentTranscript,
  AgentTurn,
  TranscriptExchange,
  TranscriptItem,
  TranscriptSession,
  Alert,
  CatalogInstrument,
  ContractChainResponse,
  ContractStrike,
  Fill,
  FillsResponse,
  HealthResponse,
  Instrument,
  InstrumentCatalogResponse,
  MetaResponse,
  OperationalHealth,
  PlanAction,
  PlansResponse,
  PortfolioResponse,
  Position,
  ReplayExample,
  ReplayResponse,
  ResearchCandidate,
  ScannerResponse,
  ScannerRow,
  SeriesResponse,
  SessionRecord,
  SessionsResponse,
  SummaryResponse,
  ToolCallRecord,
  ToolsResponse,
} from "../src/types";

const IST = "+05:30";
const STEP_MS = 15_000;
const MINUTE_MS = 60_000;
export const MOCK_ACCOUNT = "paper-alpha";
export const MOCK_BROKER = "upstox";

// ---------------------------------------------------------------- randomness
export function rng(seed: number) {
  let a = seed >>> 0;
  const next = () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  const normal = () => {
    const u = Math.max(next(), 1e-12);
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * next());
  };
  const int = (lo: number, hi: number) =>
    lo + Math.floor(next() * (hi - lo + 1));
  const pick = <T>(items: readonly T[]) =>
    items[Math.floor(next() * items.length)];
  return { next, normal, int, pick };
}
type Rng = ReturnType<typeof rng>;
const hex = (r: Rng, length: number) =>
  Array.from({ length }, () => r.int(0, 15).toString(16)).join("");
const d2 = (n: number) => n.toFixed(2);
const roundTick = (n: number, tick = 0.05) =>
  Math.round(Math.round(n / tick) * tick * 100) / 100;

// ------------------------------------------------------------------ universe
interface EqDef {
  key: string;
  symbol: string;
  isin: string;
  px: number;
  vol: number; // bps standard deviation per 15 s step
  beta: number;
  lot: number; // F&O lot size of the symbol's derivatives
  liquid: boolean;
  tradedCash: boolean;
}
const EQUITIES: Omit<EqDef, "key">[] = [
  {
    symbol: "RELIANCE",
    isin: "INE002A01018",
    px: 1480,
    vol: 2.6,
    beta: 0.8,
    lot: 500,
    liquid: true,
    tradedCash: true,
  },
  {
    symbol: "TCS",
    isin: "INE467B01029",
    px: 3150,
    vol: 2.2,
    beta: 0.5,
    lot: 175,
    liquid: true,
    tradedCash: true,
  },
  {
    symbol: "HDFCBANK",
    isin: "INE040A01034",
    px: 990,
    vol: 2.4,
    beta: 0.9,
    lot: 550,
    liquid: true,
    tradedCash: true,
  },
  {
    symbol: "INFY",
    isin: "INE009A01021",
    px: 1560,
    vol: 2.5,
    beta: 0.6,
    lot: 400,
    liquid: true,
    tradedCash: false,
  },
  {
    symbol: "ICICIBANK",
    isin: "INE090A01021",
    px: 1380,
    vol: 2.6,
    beta: 0.9,
    lot: 700,
    liquid: true,
    tradedCash: true,
  },
  {
    symbol: "SBIN",
    isin: "INE062A01020",
    px: 960,
    vol: 3.1,
    beta: 1.0,
    lot: 750,
    liquid: true,
    tradedCash: false,
  },
  {
    symbol: "ITC",
    isin: "INE154A01025",
    px: 410,
    vol: 2.0,
    beta: 0.4,
    lot: 1600,
    liquid: true,
    tradedCash: true,
  },
  {
    symbol: "LT",
    isin: "INE018A01030",
    px: 3700,
    vol: 2.9,
    beta: 1.0,
    lot: 175,
    liquid: true,
    tradedCash: true,
  },
  {
    symbol: "BHARTIARTL",
    isin: "INE397D01024",
    px: 2050,
    vol: 2.7,
    beta: 0.7,
    lot: 475,
    liquid: true,
    tradedCash: true,
  },
  {
    symbol: "AXISBANK",
    isin: "INE238A01034",
    px: 1180,
    vol: 3.3,
    beta: 1.0,
    lot: 625,
    liquid: false,
    tradedCash: false,
  },
  {
    symbol: "TATASTEEL",
    isin: "INE081A01020",
    px: 172,
    vol: 3.8,
    beta: 1.2,
    lot: 5500,
    liquid: false,
    tradedCash: true,
  },
  {
    symbol: "MARUTI",
    isin: "INE585B01010",
    px: 15800,
    vol: 2.8,
    beta: 0.7,
    lot: 50,
    liquid: false,
    tradedCash: false,
  },
].map((e) => ({ ...e }));
// Catalog-only cash names: scanner "missing" rows and the instrument catalog.
const EXTRA_EQ = [
  ["WIPRO", "INE075A01022", 262],
  ["ONGC", "INE213A01029", 248],
  ["COALINDIA", "INE522F01014", 391],
  ["NTPC", "INE733E01010", 335],
  ["TITAN", "INE280A01028", 3480],
  ["SUNPHARMA", "INE044A01036", 1760],
  ["ADANIPORTS", "INE742F01042", 1405],
  ["HINDUNILVR", "INE030A01027", 2360],
] as const;

type FoKind = "future" | "call" | "put";
interface FoDef {
  key: string;
  symbol: string;
  kind: FoKind;
  underlying: string; // EQ key
  underlyingSymbol: string;
  expiry: string;
  expiryMs: number;
  strike: number | null;
  lot: number;
  premium0: number;
}
const expiryMs = (date: string) => Date.parse(`${date}T15:30:00${IST}`);
const EXPIRIES = ["2026-11-24", "2026-12-29", "2027-01-26"];
const monthLabel = (date: string) => {
  const [y, m, d] = date.split("-");
  const mon = [
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
  ][Number(m) - 1];
  return `${d} ${mon} ${y.slice(2)}`;
};

// ------------------------------------------------------------------ the day
export interface Bar {
  at: number;
  open: number;
  high: number;
  low: number;
  close: number;
  ticks: number;
}
export interface Gap {
  from_ms: number;
  to_ms: number;
  reason: "no_trade" | "unobserved";
  minutes: number;
}
export interface FillRec {
  decision: string;
  index: number;
  key: string;
  buy: boolean;
  units: number;
  expected: number;
  price: number;
  fee: number;
  at: number;
}
export interface DecisionRec {
  id: string;
  at: number;
  stance: string;
  trigger: { kind: string; reason: string };
  actions: {
    index: number;
    key: string | null;
    side: string;
    units: number;
    maxPrice: number | null;
    rationale: string;
    reason: string; // "accepted" or a rejection code
    verdict: string | null; // execution verdict for accepted actions
  }[];
}
export interface EquityPoint {
  at: number;
  equity: number;
  cash: number;
}
export interface MockData {
  seed: number;
  day: string;
  session: string;
  capital: number;
  openMs: number;
  closeMs: number;
  /** Where the virtual clock starts when the dev server boots. */
  startNowMs: number;
  equities: EqDef[];
  contracts: FoDef[];
  catalog: CatalogInstrument[];
  paths: Record<string, number[]>;
  bars: Record<string, Bar[]>;
  gaps: Record<string, Gap[]>;
  equity: EquityPoint[];
  fills: FillRec[];
  decisions: DecisionRec[];
  turns: AgentTurn[];
  triggerEvents: { at_ms: number; event: string; escalate?: boolean }[];
  toolCalls: ToolCallRecord[];
  archive: SessionRecord[];
  candidates: ResearchCandidate[];
  alerts: Alert[];
  strategies: {
    id: string;
    version: number;
    hash: string;
    scan: string;
    symbols: string[];
  }[];
  example: ReplayExample;
}

const wireInstrument = (def: EqDef | FoDef): Instrument => {
  if ("isin" in def)
    return {
      key: def.key,
      symbol: def.symbol,
      kind: "cash",
      lot_size: 1,
      segment: "NSE_EQ",
      ...({ tick_size: "0.05", version: "bod-1" } as object),
    };
  return {
    key: def.key,
    symbol: def.symbol,
    kind: def.kind,
    lot_size: def.lot,
    segment: "NSE_FO",
    underlying: def.underlying,
    expiry: def.expiry,
    ...(def.strike === null ? {} : { strike: d2(def.strike) }),
    ...({ tick_size: "0.05", version: "bod-1" } as object),
  };
};

const stepIndex = (data: Pick<MockData, "openMs">, at: number) =>
  Math.max(0, Math.min(1500, (at - data.openMs) / STEP_MS));

function interp(path: number[], index: number) {
  const lo = Math.floor(index);
  const hi = Math.min(path.length - 1, lo + 1);
  return path[lo] + (path[hi] - path[lo]) * (index - lo);
}

/** Receipt price of any instrument at `at`; F&O prices follow their underlying. */
export function quoteAt(data: MockData, key: string, at: number): number {
  const index = stepIndex(data, at);
  const path = data.paths[key];
  if (path) return roundTick(interp(path, index));
  const def = data.contracts.find((c) => c.key === key);
  if (!def) return 0;
  const s = interp(data.paths[def.underlying], index);
  const s0 = data.paths[def.underlying][0];
  if (def.kind === "future") return roundTick(s * 1.0062);
  if (def.kind === "call")
    return Math.max(0.05, roundTick(def.premium0 + 0.52 * (s - s0)));
  return Math.max(0.05, roundTick(def.premium0 - 0.46 * (s - s0)));
}

function feeFor(kind: string, buy: boolean, notional: number) {
  const brokerage = Math.min(20, 0.0003 * notional);
  const rate =
    kind === "cash"
      ? { txn: 0.0000297, stt: buy ? 0 : 0.00025, stamp: buy ? 0.00003 : 0 }
      : kind === "future"
        ? { txn: 0.0000173, stt: buy ? 0 : 0.0002, stamp: buy ? 0.00002 : 0 }
        : { txn: 0.0003503, stt: buy ? 0 : 0.001, stamp: buy ? 0.00003 : 0 };
  const txn = rate.txn * notional;
  const sebi = 0.000001 * notional;
  const gst = 0.18 * (brokerage + txn + sebi);
  return (
    Math.round(
      (brokerage +
        txn +
        sebi +
        gst +
        rate.stt * notional +
        rate.stamp * notional) *
        100,
    ) / 100
  );
}

// ----------------------------------------------------------------- generator
export function createMockData(
  seed = 20261123,
  options: { day?: string } = {},
): MockData {
  const day = options.day ?? "2026-11-23";
  const at = (hh: number, mm: number, ss = 0) =>
    Date.parse(
      `${day}T${String(hh).padStart(2, "0")}:${String(mm).padStart(2, "0")}:${String(ss).padStart(2, "0")}${IST}`,
    );
  const openMs = at(9, 15);
  const closeMs = at(15, 30);
  const r = rng(seed);

  const equities: EqDef[] = EQUITIES.map((e) => ({
    ...e,
    key: `NSE_EQ|${e.isin}`,
  }));
  const bySymbol = new Map(equities.map((e) => [e.symbol, e]));

  // 15 s price paths with a shared market factor, a morning dip, a 12:30 shock
  // and an afternoon recovery so the book shows a real drawdown.
  const steps = 1500;
  const market: number[] = [];
  for (let i = 0; i < steps; i++) {
    let drift = 0;
    if (i < 360) drift = -0.22;
    else if (i >= 700 && i < 1100) drift = 0.28;
    if (i >= 900 && i < 940) drift -= 1.5;
    market.push(drift + 2.1 * r.normal());
  }
  const paths: Record<string, number[]> = {};
  for (const e of equities) {
    const own = rng(
      seed ^ (e.isin.charCodeAt(5) * 977 + e.isin.charCodeAt(8) * 31),
    );
    const path = [e.px];
    for (let i = 0; i < steps; i++) {
      const idio = e.vol * own.normal() * 0.8;
      const z = e.beta * market[i] + idio;
      path.push(path[i] * Math.exp(z / 10_000));
    }
    paths[e.key] = path;
  }

  // Derivatives catalog: futures on five underlyings, option chains on three.
  const contracts: FoDef[] = [];
  let token = 41_000;
  const chainSymbols: Record<string, number> = {
    RELIANCE: 20,
    HDFCBANK: 10,
    SBIN: 10,
  };
  for (const symbol of ["RELIANCE", "HDFCBANK", "INFY", "SBIN", "TCS"]) {
    const under = bySymbol.get(symbol)!;
    for (const expiry of EXPIRIES) {
      contracts.push({
        key: `NSE_FO|${token++}`,
        symbol: `${symbol} FUT ${monthLabel(expiry)}`,
        kind: "future",
        underlying: under.key,
        underlyingSymbol: symbol,
        expiry,
        expiryMs: expiryMs(expiry),
        strike: null,
        lot: under.lot,
        premium0: 0,
      });
    }
    const spacing = chainSymbols[symbol];
    if (!spacing) continue;
    const atm = Math.round(under.px / spacing) * spacing;
    for (const expiry of EXPIRIES.slice(1)) {
      for (let k = -8; k <= 8; k++) {
        const strike = atm + k * spacing;
        const time = expiry === EXPIRIES[1] ? 1 : 1.5;
        const atmPremium = under.px * 0.028 * time;
        for (const kind of ["call", "put"] as const) {
          const intrinsic = Math.max(
            0,
            kind === "call" ? under.px - strike : strike - under.px,
          );
          const premium0 = Math.max(
            0.35,
            roundTick(
              Math.max(intrinsic, 0) +
                atmPremium * Math.exp(-Math.abs(k) * 0.28),
            ),
          );
          contracts.push({
            key: `NSE_FO|${token++}`,
            symbol: `${symbol} ${strike} ${kind === "call" ? "CE" : "PE"} ${monthLabel(expiry)}`,
            kind,
            underlying: under.key,
            underlyingSymbol: symbol,
            expiry,
            expiryMs: expiryMs(expiry),
            strike,
            lot: under.lot,
            premium0,
          });
        }
      }
    }
  }
  const findFo = (symbol: string) => {
    const def = contracts.find((c) => c.symbol === symbol);
    if (!def) throw new Error(`mock contract missing: ${symbol}`);
    return def;
  };
  const infyFut = findFo("INFY FUT 29 DEC 26");
  const hdfcCall = findFo("HDFCBANK 1000 CE 29 DEC 26");
  const sbinPut = findFo("SBIN 950 PE 29 DEC 26");
  const byKey = new Map<string, EqDef | FoDef>(
    [...equities, ...contracts].map((d) => [d.key, d]),
  );

  // Provisional data object so quoteAt works while the book is built.
  const data = {
    seed,
    day,
    session: day,
    capital: 2_500_000,
    openMs,
    closeMs,
    startNowMs: at(15, 12),
    equities,
    contracts,
    paths,
  } as MockData;

  // ---------------------------------------------------------- minute bars
  const bars: Record<string, Bar[]> = {};
  const gaps: Record<string, Gap[]> = {};
  const outageFrom = 147; // 11:42
  const outageTo = 152; // 11:47
  const illiquidSkip = new Map<string, number>([
    ["NSE_EQ|INE238A01034", 0.16],
    ["NSE_EQ|INE081A01020", 0.09],
    ["NSE_EQ|INE585B01010", 0.22],
  ]);
  for (const e of equities) {
    const own = rng(seed + e.isin.length * 7919 + e.isin.charCodeAt(9));
    const list: Bar[] = [];
    const missing: { minute: number; reason: Gap["reason"] }[] = [];
    for (let m = 0; m < 375; m++) {
      if (m >= outageFrom && m <= outageTo) {
        missing.push({ minute: m, reason: "unobserved" });
        continue;
      }
      const skip = illiquidSkip.get(e.key) ?? 0;
      if (skip && own.next() < skip) {
        missing.push({ minute: m, reason: "no_trade" });
        continue;
      }
      const slice = paths[e.key].slice(m * 4, m * 4 + 5);
      const wick = e.px * (0.4 + own.next()) * 0.00006;
      const open = roundTick(slice[0]);
      const close = roundTick(slice[4]);
      list.push({
        at: openMs + m * MINUTE_MS,
        open,
        close,
        high: roundTick(Math.max(...slice) + wick),
        low: roundTick(Math.min(...slice) - wick),
        ticks: e.liquid ? own.int(18, 140) : own.int(1, 12),
      });
    }
    bars[e.key] = list;
    const merged: Gap[] = [];
    for (const miss of missing) {
      const last = merged.at(-1);
      const from = openMs + miss.minute * MINUTE_MS;
      if (
        last &&
        last.reason === miss.reason &&
        last.to_ms + MINUTE_MS === from
      ) {
        last.to_ms = from;
        last.minutes++;
      } else
        merged.push({
          from_ms: from,
          to_ms: from,
          reason: miss.reason,
          minutes: 1,
        });
    }
    gaps[e.key] = merged;
  }
  // Derivatives get bars from the same model so their charts also load.
  for (const def of contracts.filter((c) => c.expiry === "2026-12-29")) {
    const list: Bar[] = [];
    const ug: Gap[] = gaps[def.underlying];
    const dead = (t: number) => ug.some((g) => t >= g.from_ms && t <= g.to_ms);
    for (let m = 0; m < 375; m++) {
      const t = openMs + m * MINUTE_MS;
      if (dead(t)) continue;
      const a = quoteAt(data, def.key, t);
      const b = quoteAt(data, def.key, t + MINUTE_MS - 1);
      list.push({
        at: t,
        open: a,
        close: b,
        high: roundTick(Math.max(a, b) + 0.05),
        low: Math.max(0.05, roundTick(Math.min(a, b) - 0.05)),
        ticks: 3 + ((m * 7) % 19),
      });
    }
    bars[def.key] = list;
    gaps[def.key] = ug.map((g) => ({ ...g }));
  }

  // ----------------------------------------------------- scripted decisions
  type Plan = [
    side: "buy" | "sell",
    name: string,
    units: number,
    outcome?: string,
  ];
  const script: {
    hms: [number, number, number];
    stance: string;
    trigger: [string, string];
    plan: Plan[];
    rationale: string[];
  }[] = [
    {
      hms: [9, 31, 20],
      stance: "build_long",
      trigger: ["entry_candidate", "opening range breakout"],
      plan: [
        ["buy", "RELIANCE", 300],
        ["buy", "HDFCBANK", 400],
      ],
      rationale: [
        "Opening range break on rising ticks with bid depth",
        "Bank basket leading, spread inside two ticks",
      ],
    },
    {
      hms: [9, 48, 5],
      stance: "build_long",
      trigger: ["entry_candidate", "relative strength vs index"],
      plan: [
        ["buy", "INFY FUT 29 DEC 26", 400],
        ["buy", "SBIN", 500, "price_limit_not_marketable"],
      ],
      rationale: [
        "Hold IT exposure through the future to keep cash for banks",
        "Limit set below the ask; chase refused",
      ],
    },
    {
      hms: [10, 12, 40],
      stance: "rotate",
      trigger: ["price_move", "move over 45 bps in 5m"],
      plan: [
        ["buy", "TCS", 100],
        ["buy", "ITC", 1500],
      ],
      rationale: [
        "Defensive rotation as the banks stall",
        "Low-beta carry while the index digests",
      ],
    },
    {
      hms: [10, 40, 15],
      stance: "take_profit",
      trigger: ["fill", "post-fill position review"],
      plan: [
        ["sell", "HDFCBANK", 400],
        ["buy", "ICICIBANK", 600],
      ],
      rationale: [
        "Trim the bank leg after a fast extension",
        "Swap into the cheaper relative mover",
      ],
    },
    {
      hms: [11, 5, 30],
      stance: "add_convexity",
      trigger: ["scheduled", "scheduled position review"],
      plan: [["buy", "HDFCBANK 1000 CE 29 DEC 26", 550]],
      rationale: ["Cheap call as a hedge for the long bank book"],
    },
    {
      hms: [11, 30, 10],
      stance: "reduce",
      trigger: ["price_move", "defensive name rolled over"],
      plan: [
        ["sell", "ITC", 1500],
        ["buy", "AXISBANK", 500, "instrument_not_eligible"],
      ],
      rationale: [
        "Exit the carry trade",
        "Instrument outside the verified allowlist",
      ],
    },
    {
      hms: [12, 20, 50],
      stance: "hedge",
      trigger: ["price_move", "index drawdown through 12:30 shock"],
      plan: [
        ["buy", "SBIN 950 PE 29 DEC 26", 750],
        ["sell", "TCS", 100],
      ],
      rationale: [
        "Put protects against a second leg lower",
        "Release capital from the weakest long",
      ],
    },
    {
      hms: [13, 5, 25],
      stance: "rebuild",
      trigger: ["entry_candidate", "reclaim of session VWAP"],
      plan: [
        ["buy", "LT", 100],
        ["buy", "TATASTEEL", 3000],
      ],
      rationale: [
        "Capex name reclaiming VWAP with volume",
        "High beta rebound candidate",
      ],
    },
    {
      hms: [13, 50, 10],
      stance: "cut_and_trim",
      trigger: ["price_move", "adverse move against TATASTEEL"],
      plan: [
        ["sell", "TATASTEEL", 3000],
        ["sell", "ICICIBANK", 300],
      ],
      rationale: [
        "Stop the failed rebound",
        "Bring the bank exposure back to target",
      ],
    },
    {
      hms: [14, 25, 35],
      stance: "add_leader",
      trigger: ["entry_candidate", "late session relative strength"],
      plan: [["buy", "BHARTIARTL", 300]],
      rationale: ["Telecom leads while the index drifts"],
    },
    {
      hms: [14, 48, 5],
      stance: "trim_into_close",
      trigger: ["scheduled", "pre-close review"],
      plan: [
        ["buy", "MARUTI", 20, "stale_quote"],
        ["sell", "LT", 50],
      ],
      rationale: [
        "Quote older than the freshness gate",
        "Reduce the capex name before the close",
      ],
    },
  ];

  const decisions: DecisionRec[] = [];
  const fills: FillRec[] = [];
  const slip = rng(seed + 31);
  script.forEach((s, di) => {
    const decisionAt = at(...s.hms);
    const id = `dec-${String(s.hms[0]).padStart(2, "0")}${String(s.hms[1]).padStart(2, "0")}-${hex(slip, 6)}`;
    const actions: DecisionRec["actions"] = [];
    s.plan.forEach(([side, name, units, outcome], index) => {
      const def = bySymbol.get(name) ?? findFo(name);
      const expected = quoteAt(data, def.key, decisionAt);
      const rejected = outcome && outcome !== "stale_quote";
      const maxPrice = roundTick(
        side === "buy" ? expected * 1.002 : expected * 0.998,
      );
      const entry = {
        index,
        key: rejected ? null : def.key,
        side,
        units: rejected ? 0 : units,
        maxPrice: rejected ? null : maxPrice,
        rationale: s.rationale[index] ?? "",
        reason: outcome && rejected ? outcome : "accepted",
        verdict: rejected
          ? null
          : outcome === "stale_quote"
            ? "not_filled_stale_quote"
            : "filled",
      };
      actions.push(entry);
      if (entry.verdict === "filled") {
        const bps = 2 + slip.next() * 4;
        const price = roundTick(
          expected * (1 + (side === "buy" ? bps : -bps) / 10_000),
        );
        const kind = "isin" in def ? "cash" : def.kind;
        fills.push({
          decision: id,
          index,
          key: def.key,
          buy: side === "buy",
          units,
          expected,
          price,
          fee: feeFor(kind, side === "buy", units * price),
          at: decisionAt + 1_200 + index * 650,
        });
      }
    });
    decisions.push({
      id,
      at: decisionAt,
      stance: s.stance,
      trigger: { kind: s.trigger[0], reason: s.trigger[1] },
      actions,
    });
    void di;
  });

  // ---------------------------------------------------------- book + equity
  const capital = data.capital;
  const book = (t: number) => {
    let cash = capital;
    const pos = new Map<
      string,
      { units: number; cost: number; decision: string; opened: number }
    >();
    let realized = 0;
    for (const f of fills) {
      if (f.at > t) break;
      const notional = f.units * f.price;
      cash += f.buy ? -notional - f.fee : notional - f.fee;
      const p = pos.get(f.key);
      if (f.buy) {
        if (p) {
          p.cost = (p.cost * p.units + notional) / (p.units + f.units);
          p.units += f.units;
        } else
          pos.set(f.key, {
            units: f.units,
            cost: f.price,
            decision: f.decision,
            opened: f.at,
          });
      } else if (p) {
        realized += (f.price - p.cost) * f.units;
        p.units -= f.units;
        if (p.units <= 0) pos.delete(f.key);
      }
    }
    return { cash, pos, realized };
  };
  const equity: EquityPoint[] = [];
  for (let t = openMs; t <= closeMs; t += STEP_MS) {
    const b = book(t);
    let value = b.cash;
    for (const [key, p] of b.pos) value += p.units * quoteAt(data, key, t);
    equity.push({
      at: t,
      equity: Math.round(value * 100) / 100,
      cash: Math.round(b.cash * 100) / 100,
    });
  }
  Object.assign(data, { fills, equity });

  // ------------------------------------------------------------ catalog
  const cashCatalog = (def: EqDef, index: number): CatalogInstrument => ({
    instrument_key: def.key,
    symbol: def.symbol,
    segment: "NSE_EQ",
    series: "EQ",
    exchange: "NSE",
    kind: "cash",
    instrument_type: "EQ",
    lot_size: 1,
    tick: "0.05",
    tick_unit: "INR",
    cash_execution_verified: true,
    display_only: true,
    margin: null,
    margin_reason: index < 0 ? null : "not_captured",
  });
  const extraEq: EqDef[] = EXTRA_EQ.map(([symbol, isin, px]) => ({
    key: `NSE_EQ|${isin}`,
    symbol,
    isin,
    px,
    vol: 2.5,
    beta: 0.8,
    lot: 1,
    liquid: true,
    tradedCash: false,
  }));
  const catalog: CatalogInstrument[] = [
    ...[...equities, ...extraEq].map(cashCatalog),
    ...contracts.map<CatalogInstrument>((c) => ({
      instrument_key: c.key,
      symbol: c.symbol,
      segment: "NSE_FO",
      exchange: "NSE",
      kind: c.kind,
      instrument_type:
        c.kind === "future" ? "FUT" : c.kind === "call" ? "CE" : "PE",
      underlying_key: c.underlying,
      underlying_symbol: c.underlyingSymbol,
      expiry_ms: c.expiryMs,
      ...(c.strike === null ? {} : { strike: d2(c.strike) }),
      lot_size: c.lot,
      provider_tick: "5",
      cash_execution_verified: false,
      display_only: true,
      margin: null,
      margin_reason: "not_captured",
    })),
  ];

  // ----------------------------------------------------------- agent turns
  const turns: AgentTurn[] = [];
  const triggerEvents: MockData["triggerEvents"] = [];
  const tr = rng(seed + 4242);
  const MODEL = {
    decision: {
      provider: "openai-codex",
      id: "gpt-6.1-sol",
      fallback: "gpt-6.1-terra",
    },
    gate: {
      provider: "google",
      id: "gemini-3-flash",
      fallback: "gemini-3-flash-lite",
    },
    research: {
      provider: "anthropic",
      id: "claude-sonnet-5",
      fallback: "claude-sonnet-4-6",
    },
    review: {
      provider: "anthropic",
      id: "claude-opus-5",
      fallback: "claude-opus-4-8",
    },
  } as const;
  const price = {
    decision: [1.25e-6, 7e-6],
    gate: [0.1e-6, 0.4e-6],
    research: [3e-6, 15e-6],
    review: [5e-6, 25e-6],
  } as const;
  let seq = 0;
  const usage = (
    lane: keyof typeof MODEL,
    input: number,
    output: number,
    cache: number,
    calls: number,
  ) => {
    const [pi, po] = price[lane];
    return {
      input,
      output,
      cache_read: cache,
      calls,
      cost_usd:
        Math.round(
          ((input - cache) * pi + cache * pi * 0.1 + output * po) * 1e4,
        ) / 1e4,
    };
  };
  const clock = (t: number) =>
    new Date(t + 19_800_000).toISOString().slice(11, 19).replaceAll(":", "");
  const mkTurn = (
    lane: keyof typeof MODEL,
    t: number,
    trigger: AgentTurn["trigger"],
    rest: Partial<AgentTurn> = {},
  ): AgentTurn => {
    const m = MODEL[lane];
    const range = {
      decision: [8_000, 40_000],
      gate: [1_300, 5_200],
      research: [20_000, 90_000],
      review: [9_000, 30_000],
    }[lane];
    const input = {
      decision: tr.int(18_000, 45_000),
      gate: tr.int(3_000, 6_500),
      research: tr.int(30_000, 80_000),
      review: tr.int(12_000, 25_000),
    }[lane];
    const output = {
      decision: tr.int(1_000, 3_500),
      gate: tr.int(120, 320),
      research: tr.int(1_500, 5_000),
      review: tr.int(600, 2_000),
    }[lane];
    const turn: AgentTurn = {
      turn_id: `${lane}-${clock(t)}-${String(++seq).padStart(3, "0")}`,
      lane,
      at_ms: t,
      status: "ok",
      error: null,
      provider: m.provider,
      model: m.id,
      elapsed_ms: tr.int(range[0], range[1]),
      usage: usage(
        lane,
        input,
        output,
        Math.floor(input * (0.4 + tr.next() * 0.4)),
        tr.int(1, lane === "gate" ? 2 : 4),
      ),
      fallback: null,
      trigger,
      tool_calls: [],
      plan: null,
      verdict: null,
      ...rest,
    };
    return turn;
  };
  const instKeys = (names: string[]) =>
    names.map((n) => (bySymbol.get(n) ?? findFo(n)).key);
  const reasonsDeclined = [
    "one tick, no depth",
    "spread wider than two ticks",
    "already positioned in this name",
    "move reversed inside the gate window",
    "no volume confirmation",
    "stale receipt on the second leg",
    "inside the opening noise band",
    "correlated with an open position",
  ];
  const reasonsEscalated = [
    "breakout with bid depth confirmed",
    "relative strength persists across three samples",
    "volume and spread both improved",
    "protective review is due on an open position",
  ];
  const toolsFor = (
    names: string[],
    failing?: { name: string; error: string },
  ) =>
    names.map((name) =>
      failing && failing.name === name
        ? { name, ok: false, error: failing.error }
        : { name, ok: true, error: null },
    );

  // Escalated gate -> decision -> review chain for each scripted decision.
  decisions.forEach((d, i) => {
    const names = script[i].plan.map((p) => p[1]);
    const trig = {
      kind: d.trigger.kind,
      reason: d.trigger.reason,
      instruments: instKeys(names),
    };
    const validated = d.actions.filter((a) => a.reason === "accepted").length;
    if (d.trigger.kind !== "scheduled") {
      const t = d.at - 42_000;
      turns.push(
        mkTurn("gate", t, trig, {
          tool_calls: toolsFor(["snapshot", "verdict"]),
          verdict: { escalate: true, reason: tr.pick(reasonsEscalated) },
        }),
      );
      triggerEvents.push({ at_ms: t, event: "gated", escalate: true });
    }
    turns.push(
      mkTurn("decision", d.at - 18_000, trig, {
        tool_calls: toolsFor(
          ["snapshot", "quotes", "bars", "positions", "propose_plan"],
          i === 6
            ? { name: "bars", error: "instrument_not_in_universe" }
            : undefined,
        ),
        plan: {
          status: validated === d.actions.length ? "validated" : "partial",
          stance: d.stance,
          actions: d.actions.length,
          error: null,
        },
      }),
    );
    turns.push(
      mkTurn(
        "review",
        d.at + 180_000,
        {
          kind: "post_fill",
          reason: "review fills against the plan",
          instruments: trig.instruments,
        },
        {
          tool_calls: toolsFor(["fills", "positions"]),
          plan: {
            status: "validated",
            stance: validated ? "hold" : "no_change",
            actions: 0,
            error: null,
          },
        },
      ),
    );
  });
  // Scheduled position reviews and noise: declined gates, research turns.
  const scheduled = 14,
    research = 14;
  const target = 120;
  const declined = target - turns.length - scheduled - research;
  const spread = (n: number, from: number, to: number) =>
    Array.from({ length: n }, () =>
      Math.round(from + tr.next() * (to - from)),
    ).sort((a, b) => a - b);
  for (const t of spread(scheduled, at(9, 40), at(15, 8))) {
    const held = book(t).pos.size;
    turns.push(
      mkTurn(
        "decision",
        t,
        {
          kind: "scheduled",
          reason: "scheduled position review",
          instruments: [],
        },
        {
          tool_calls: toolsFor(["snapshot", "positions", "propose_plan"]),
          plan: {
            status: "validated",
            stance: held ? "hold" : "flat",
            actions: 0,
            error: null,
          },
        },
      ),
    );
  }
  for (const t of spread(declined, at(9, 16), at(15, 26))) {
    const names = [tr.pick(equities).symbol];
    turns.push(
      mkTurn(
        "gate",
        t,
        {
          kind: tr.pick(["entry_candidate", "price_move", "entry_candidate"]),
          reason: tr.pick([
            "momentum screen",
            "move over 30 bps in 5m",
            "volume spike",
            "tick imbalance",
          ]),
          instruments: instKeys(names),
        },
        {
          tool_calls: toolsFor(["snapshot", "verdict"]),
          verdict: { escalate: false, reason: tr.pick(reasonsDeclined) },
        },
      ),
    );
    triggerEvents.push({ at_ms: t, event: "gated", escalate: false });
  }
  const researchTopics = [
    "bank earnings preview",
    "crude oil move",
    "FII flow data",
    "RBI commentary",
    "auto sales numbers",
    "IT deal pipeline",
    "metal prices",
    "market breadth",
  ];
  spread(research, at(9, 20), at(15, 0)).forEach((t, i) => {
    turns.push(
      mkTurn(
        "research",
        t,
        {
          kind: "research_due",
          reason: "research cadence",
          instruments: [],
        },
        {
          tool_calls: toolsFor(
            ["web_search", "news_lookup", "web_fetch", "read_evidence"],
            i === 4
              ? { name: "web_fetch", error: "http_503" }
              : i === 9
                ? { name: "web_search", error: "timeout" }
                : undefined,
          ),
        },
      ),
    );
  });
  turns.sort((a, b) => (a.at_ms ?? 0) - (b.at_ms ?? 0));
  turns.forEach((t, i) => {
    t.turn_id = t.turn_id.replace(
      /-\d{3}$/,
      `-${String(i + 1).padStart(3, "0")}`,
    );
  });
  // A few fallbacks and errors for the badge and failure rendering.
  const mark = (lane: string, nth: number, patch: (t: AgentTurn) => void) => {
    const t = turns.filter((x) => x.lane === lane)[nth];
    if (t) patch(t);
  };
  mark("decision", 3, (t) => {
    t.fallback = {
      to: { id: MODEL.decision.fallback },
      reason: "provider_overloaded",
    };
    t.model = MODEL.decision.fallback;
  });
  mark("gate", 9, (t) => {
    t.fallback = { to: { id: MODEL.gate.fallback }, reason: "rate_limited" };
    t.model = MODEL.gate.fallback;
  });
  mark("decision", 8, (t) => {
    t.status = "error";
    t.error = "turn_timeout";
    t.plan = null;
    t.elapsed_ms = 60_000;
  });
  mark("research", 6, (t) => {
    t.status = "error";
    t.error = "provider_overloaded";
  });
  const triggerFired = turns.map((t) => ({
    at_ms: t.at_ms ?? 0,
    event: "fired",
  }));
  triggerEvents.push(...triggerFired);
  triggerEvents.sort((a, b) => a.at_ms - b.at_ms);

  // Recorded web tool calls come from the research turns' tool calls.
  const toolCalls: ToolCallRecord[] = [];
  const queries = [
    "nifty bank outlook",
    "RBI policy minutes",
    "crude oil brent price",
    "FII DII net flows today",
    "reliance retail results",
    "auto sales november",
    "it services deal wins",
    "india vix level",
  ];
  turns
    .filter((t) => t.lane === "research")
    .forEach((t, ti) => {
      (t.tool_calls ?? [])
        .filter((c) => c.name !== "read_evidence")
        .forEach((c, ci) => {
          const web = c.name === "web_fetch";
          toolCalls.push({
            v: 1,
            at_ms: (t.at_ms ?? 0) + 4_000 + ci * 7_000,
            session: day,
            run: `harness-decision-${ti + 1}`,
            turn: 0,
            call: `call_${ti}_${ci}`,
            tool: c.name,
            provider: web ? "direct" : "searxng",
            query: web
              ? null
              : queries[(ti + ci) % queries.length] +
                (c.name === "news_lookup" ? " news" : ""),
            url: web
              ? `https://example.com/markets/${researchTopics[ti % researchTopics.length].replaceAll(" ", "-")}`
              : null,
            status: c.ok ? "ok" : "error",
            reason: c.ok ? null : (c.error ?? "failed"),
            latency_ms: c.ok ? tr.int(180, 1_900) : 8_000,
            bytes: c.ok ? tr.int(2_000, 60_000) : 0,
            results: c.ok ? tr.int(2, 9) : 0,
            execution_authorized: false,
          });
        });
    });
  toolCalls.sort((a, b) => (b.at_ms ?? 0) - (a.at_ms ?? 0));

  // ------------------------------------------------- archive + notes + misc
  const archive: SessionRecord[] = [];
  const ar = rng(seed + 555);
  let date = new Date(`${day}T00:00:00Z`);
  for (let n = 0; n < 4; n++) {
    do date = new Date(date.getTime() - 86_400_000);
    while ([0, 6].includes(date.getUTCDay()));
    const stamp = date.toISOString().slice(0, 10);
    const afills: Fill[] = [];
    let cash = capital;
    for (let k = 0; k < 2; k++) {
      const e = equities[ar.int(0, 8)];
      const units = ar.int(1, 4) * 100;
      const buy = roundTick(e.px * (0.99 + ar.next() * 0.02));
      const sell = roundTick(buy * (1 + (ar.next() - 0.42) * 0.012));
      const t = Date.parse(`${stamp}T10:${10 + k * 20}:00${IST}`);
      const f1 = feeFor("cash", true, units * buy),
        f2 = feeFor("cash", false, units * sell);
      cash += -units * buy - f1 + units * sell - f2;
      afills.push(
        {
          instrument: wireInstrument(e),
          buy: true,
          units: String(units),
          price: d2(buy),
          fee: d2(f1),
          at: t,
          decision_id: `dec-${stamp}-${k}a`,
          expected_price: d2(buy),
        },
        {
          instrument: wireInstrument(e),
          buy: false,
          units: String(units),
          price: d2(sell),
          fee: d2(f2),
          at: t + 2_400_000,
          decision_id: `dec-${stamp}-${k}b`,
          expected_price: d2(sell),
        },
      );
    }
    archive.push({
      id: { broker: MOCK_BROKER, account: MOCK_ACCOUNT, date: stamp },
      mode: "paper",
      paper_only: true,
      live_enabled: false,
      status: "archived_snapshot",
      capital: d2(capital),
      stale: true,
      provenance: "declared_report_hash_checked",
      positions: 0,
      fills: afills.length,
      portfolio: {
        cash: d2(cash),
        equity: d2(cash),
        positions: {},
        fills: afills,
        stopped: false,
        halted: false,
        closed: true,
        initial_allocation: d2(capital),
      },
    });
  }
  const sha = (n: number) => hex(rng(seed + n), 64);
  const candidates: ResearchCandidate[] = [
    {
      candidate_id: "cand-orb-001",
      session: day,
      state: "observation_recorded",
      hypothesis:
        "Opening-range breakouts with bid depth continue for 20 minutes",
      source_sha256: sha(1),
      result_sha256: sha(2),
      scientific_state: "insufficient_evidence",
      origin_session: day,
      continued: false,
    },
    {
      candidate_id: "cand-vwap-002",
      session: day,
      state: "analysis_complete",
      hypothesis:
        "VWAP reclaim on rising volume mean-reverts within 15 minutes",
      source_sha256: sha(3),
      result_sha256: sha(4),
      scientific_state: "eligible",
      origin_session: archive[0].id?.date ?? day,
      continued: true,
    },
    {
      candidate_id: "cand-gap-003",
      session: day,
      state: "analysis_cancelled",
      hypothesis: "Overnight gap fades in the first 30 minutes",
      source_sha256: sha(5),
      result_sha256: null,
      scientific_state: "rejected",
      origin_session: day,
      continued: false,
    },
    {
      candidate_id: "cand-bank-004",
      session: archive[1].id?.date ?? day,
      state: "retired",
      hypothesis: "Bank basket leads index into 14:00",
      source_sha256: sha(6),
      result_sha256: sha(7),
      scientific_state: "retired",
      origin_session: archive[1].id?.date ?? day,
      continued: false,
    },
  ];
  const alerts: Alert[] = [
    {
      id: "read_failure",
      severity: "error",
      message: "The last market read failed; a valid receipt is required.",
      source: "market_data",
      observed_at_ms: at(11, 42, 20),
      delivery: "workspace_only",
      execution_authority: false,
    },
    {
      id: "feed_stale",
      severity: "warning",
      message: "The market feed is not currently fresh.",
      source: "market_feed",
      observed_at_ms: at(11, 42, 30),
      delivery: "workspace_only",
      execution_authority: false,
    },
    {
      id: "history_partial",
      severity: "info",
      message: "History preparation is incomplete; gaps stay explicit.",
      source: "history_preparation",
      observed_at_ms: at(9, 14, 40),
      delivery: "workspace_only",
      execution_authority: false,
    },
  ];
  const strategies = [
    {
      id: "opening-range-breakout",
      version: 3,
      hash: sha(11),
      scan: "scan-0931",
      symbols: ["RELIANCE", "HDFCBANK", "ICICIBANK", "SBIN", "LT"],
    },
    {
      id: "vwap-reclaim",
      version: 1,
      hash: sha(12),
      scan: "scan-1305",
      symbols: ["TCS", "BHARTIARTL", "TATASTEEL"],
    },
  ];
  const example: ReplayExample = {
    label: "Synthetic example · two instruments, six quotes",
    manifest: {
      universe: [
        { key: "SYNTHETIC|A", symbol: "SYNA", kind: "cash", lot_size: 1 },
        { key: "SYNTHETIC|B", symbol: "SYNB", kind: "cash", lot_size: 1 },
      ],
    },
    events: Array.from({ length: 6 }, (_, i) =>
      JSON.stringify({
        key: i % 2 ? "SYNTHETIC|B" : "SYNTHETIC|A",
        receipt_ms: openMs + i * 60_000,
        bid: d2(100 + i),
        ask: d2(100.1 + i),
      }),
    ).join("\n"),
  };

  Object.assign(data, {
    bars,
    gaps,
    catalog,
    decisions,
    turns,
    triggerEvents,
    toolCalls,
    archive,
    candidates,
    alerts,
    strategies,
    example,
  });
  void byKey;
  return data;
}

// ---------------------------------------------------------------- projection
const envelopeFor = (data: MockData, now: number) => ({
  v: 1,
  mode: "paper",
  paper_only: true,
  live_enabled: false,
  account: MOCK_ACCOUNT,
  broker: { data: MOCK_BROKER, orders: "paper" },
  session: data.session,
  binding: `${MOCK_BROKER}|${MOCK_ACCOUNT}|${data.session}`,
  as_of_ms: now,
  execution_authority: false as const,
});
const instrumentOf = (data: MockData, key: string): Instrument => {
  const eq = data.equities.find((e) => e.key === key);
  if (eq) return wireInstrument(eq);
  return wireInstrument(data.contracts.find((c) => c.key === key)!);
};

export function bookAt(data: MockData, now: number) {
  let cash = data.capital;
  let realized = 0;
  let fees = 0;
  let slippage = 0;
  let notional = 0;
  const pos = new Map<
    string,
    { units: number; cost: number; decision: string }
  >();
  const seen: FillRec[] = [];
  for (const f of data.fills) {
    if (f.at > now) break;
    seen.push(f);
    const n = f.units * f.price;
    cash += f.buy ? -n - f.fee : n - f.fee;
    fees += f.fee;
    notional += n;
    slippage += Math.abs(f.price - f.expected) * f.units;
    const p = pos.get(f.key);
    if (f.buy) {
      if (p) {
        p.cost = (p.cost * p.units + n) / (p.units + f.units);
        p.units += f.units;
      } else
        pos.set(f.key, { units: f.units, cost: f.price, decision: f.decision });
    } else if (p) {
      realized += (f.price - p.cost) * f.units;
      p.units -= f.units;
      if (p.units <= 0) pos.delete(f.key);
    }
  }
  let marked = 0;
  let exposure = 0;
  for (const [key, p] of pos) {
    const m = quoteAt(data, key, now);
    exposure += p.units * m;
    marked += (m - p.cost) * p.units;
  }
  return {
    cash,
    realized,
    fees,
    slippage,
    notional,
    pos,
    seen,
    marked,
    exposure,
    equity: cash + exposure,
  };
}

const equityUpTo = (data: MockData, now: number) =>
  data.equity.filter((p) => p.at <= now);

function drawdownOf(points: EquityPoint[]) {
  let high = 0,
    max = 0,
    maxPct = 0,
    since = 0,
    longest = 0;
  for (const p of points) {
    if (p.equity >= high) {
      high = p.equity;
      since = p.at;
    }
    const dd = high - p.equity;
    if (dd > max) {
      max = dd;
      maxPct = (dd / high) * 100;
    }
    longest = Math.max(longest, p.at - since);
  }
  return { high, max, maxPct, longest };
}

export function metaFor(data: MockData): MetaResponse {
  return {
    v: 1,
    version: "0.0.0-mock",
    mode: "paper",
    paper_only: true,
    live_enabled: false,
    broker: { data: MOCK_BROKER, orders: "paper" },
    account: MOCK_ACCOUNT,
    capabilities: [
      "summary",
      "portfolio",
      "positions",
      "orders",
      "fills",
      "plans",
      "scanner",
      "instruments",
      "sessions",
      "health",
      "alerts",
      "series",
      "agent",
      "agent_transcript",
      "events",
      "tools",
      "backtest",
    ],
    limits: {
      sse_max_streams: 3,
      sse_heartbeat_ms: 15_000,
      longpoll_hold_ms: 25_000,
      series_max_points: 2_000,
      series_max_range_ms: 86_400_000,
      archive_minute_max_range_ms: 14 * 86_400_000,
      archive_daily_max_range_ms: 366 * 86_400_000,
      equity_max_points: 4_096,
      equity_sample_interval_ms: 15_000,
      price_batch_instruments: 24,
      price_batch_interval_ms: 2_000,
    },
  };
  void data;
}

function positionsAt(data: MockData, now: number): Position[] {
  const b = bookAt(data, now);
  return [...b.pos].map(([key, p]) => {
    const instrument = instrumentOf(data, key);
    const mark = quoteAt(data, key, now);
    const fut = data.contracts.find(
      (c) => c.key === key && c.kind === "future",
    );
    return {
      instrument,
      units: String(p.units),
      entry: d2(p.cost),
      mark: d2(mark),
      marked_at: now,
      decision_id: p.decision,
      exit_intent: { target: d2(p.cost * 1.012), stop: d2(p.cost * 0.992) },
      margin: fut
        ? {
            instrument_key: key,
            span_margin: d2(p.units * p.cost * 0.1),
            exposure_margin: d2(p.units * p.cost * 0.02),
            total_margin: d2(p.units * p.cost * 0.12),
            display_only: true as const,
          }
        : null,
    };
  });
}

function fillRows(data: MockData, now: number): Fill[] {
  return data.fills
    .filter((f) => f.at <= now)
    .map((f) => ({
      instrument: instrumentOf(data, f.key),
      buy: f.buy,
      units: String(f.units),
      price: d2(f.price),
      fee: d2(f.fee),
      at: f.at,
      decision_id: f.decision,
      expected_price: d2(f.expected),
      slippage_bps: d2(
        ((f.price - f.expected) / f.expected) * 10_000 * (f.buy ? 1 : -1),
      ),
    }))
    .reverse();
}

function planAt(data: MockData, now: number): PlansResponse {
  const d = [...data.decisions].reverse().find((x) => x.at <= now);
  const env = envelopeFor(data, now);
  if (!d)
    return {
      ...env,
      planner: {
        status: "idle",
        decision_id: null,
        pending_events: 0,
      },
      actions: [],
      execution: [],
      note: "Raw actions and host validation are distinct; execution verdicts are not fills.",
    };
  const actions: PlanAction[] = d.actions.map((a) => ({
    index: a.index,
    units: String(a.units),
    reason: a.reason,
    action: a.key
      ? {
          instrument_key: a.key,
          side: a.side,
          units: String(a.units),
          max_price: d2(a.maxPrice ?? 0),
          rationale: a.rationale,
        }
      : undefined,
  }));
  const execution = d.actions
    .filter((a) => a.verdict)
    .map((a) => ({ decision_id: d.id, index: a.index, verdict: a.verdict! }));
  const used = data.decisions.filter((x) => x.at <= now).length;
  return {
    ...env,
    planner: {
      status: "idle",
      decision_id: d.id,
      pending_attempt: null,
      pending_events: 0,
      budget_used: { decisions: used },
      budgets: { decisions_per_day: 60 },
    },
    actions,
    execution,
    note: "Raw actions and host validation are distinct; execution verdicts are not fills.",
  };
}

function scannerAt(
  data: MockData,
  now: number,
  segment: string,
): ScannerResponse {
  const env = envelopeFor(data, now);
  const lookback = now - 15 * MINUTE_MS;
  const strategyFor = (symbol: string) =>
    data.strategies.find((s) => s.symbols.includes(symbol));
  const prospects: ScannerRow[] = data.equities.map((e, i) => {
    const price = quoteAt(data, e.key, now);
    const prev = quoteAt(data, e.key, Math.max(data.openMs, lookback));
    const s = strategyFor(e.symbol);
    return {
      instrument_key: e.key,
      symbol: e.symbol,
      instrument: wireInstrument(e),
      latest_screening_price: d2(price),
      sampled_price_ms: now - 400 - i * 37,
      source_ms: now - 900 - i * 37,
      observed_ms: now - 350 - i * 37,
      price_kind: "receipt_price",
      sample_count: 40 + ((i * 13) % 31),
      sample_change_bps: d2(((price - prev) / prev) * 10_000),
      screening_reason: i < 9 ? "fresh" : "fresh_low_liquidity",
      ...(s
        ? {
            strategy_spec_id: s.id,
            strategy_spec_version: s.version,
            strategy_spec_hash: s.hash,
            strategy_scan_id: s.scan,
            strategy_score: d2(0.55 + ((i * 7) % 40) / 100),
          }
        : {}),
    };
  });
  const extra: ScannerRow[] = [
    ["WIPRO", "stale", "last receipt older than the scan window"],
    ["ONGC", "stale", "last receipt older than the scan window"],
    ["COALINDIA", "unseen", "no valid receipt this session"],
    ["NTPC", "unseen", "no valid receipt this session"],
    ["TITAN", "unseen", "subscription not yet confirmed"],
    ["SUNPHARMA", "stale", "spread above the screening bound"],
    ["ADANIPORTS", "unseen", "no valid receipt this session"],
    ["HINDUNILVR", "unseen", "no valid receipt this session"],
  ].map(([symbol, reason, diagnostic], i) => {
    const row = data.catalog.find((c) => c.symbol === symbol)!;
    return {
      instrument_key: row.instrument_key,
      symbol,
      reason,
      diagnostic,
      last_observed_ms:
        reason === "stale" ? now - (20 + i * 3) * MINUTE_MS : undefined,
    };
  });
  const reasons = {
    fresh: 9,
    fresh_low_liquidity: 3,
    stale: 3,
    unseen: 5,
    spread_above_bound: 2,
    no_valid_receipt: 5,
  };
  const coverage = {
    market_segment: "NSE_EQ",
    instrument_kind: "cash",
    display_only: true,
    fresh: 12,
    stale: 3,
    unseen: 5,
    eligible: 12,
    presented: 12,
    universe: 20,
    oldest_fresh_age_ms: 2_100,
    max_scan_age_ms: 60_000,
    unavailable_count: extra.length,
    unavailable_truncated: false,
  };
  const base = {
    ...env,
    segment,
    coverage,
    screen_reasons: reasons,
    settings: {
      series_max_points: 2000,
      price_batch_instruments: 24,
      price_batch_interval_ms: 2000,
    },
    next: null,
  };
  if (segment === "missing")
    return { ...base, total: extra.length, items: extra };
  if (segment === "screens")
    return {
      ...base,
      total: Object.keys(reasons).length,
      items: Object.entries(reasons).map(([reason, total]) => ({
        reason,
        total,
      })),
    };
  return { ...base, total: prospects.length, items: prospects };
}

export function seriesAt(
  data: MockData,
  now: number,
  query: {
    kind?: string;
    instrument?: string;
    from?: number;
    to?: number;
    limit?: number;
  },
): SeriesResponse {
  const env = envelopeFor(data, now);
  const limit = Math.max(1, Math.min(query.limit ?? 2000, 10_000));
  const from = query.from ?? data.openMs;
  const to = Math.min(query.to ?? now, now);
  if (query.kind === "account-equity") {
    const points = equityUpTo(data, to)
      .filter((p) => p.at >= from)
      .slice(-limit)
      .map((p) => ({
        at: p.at,
        equity: d2(p.equity),
        cash: d2(p.cash),
        mark_basis: "recorded_position_marks",
      }));
    return {
      ...env,
      kind: "account-equity",
      limit,
      range: { from: query.from ?? null, to: query.to ?? null },
      points,
      watermark_ms: points.at(-1)?.at ?? null,
      unavailable: false,
      source: "recorded_portfolio_checkpoints",
      mark_basis: "recorded_position_marks",
      note: "Recorded portfolio checkpoints with their original times.",
    };
  }
  const key = query.instrument ?? "";
  if (query.kind === "archive-daily-bar" || query.kind === "archive-minute-bar") {
    const known = data.bars[key];
    if (!known)
      return {
        ...env,
        kind: query.kind,
        instrument_key: key,
        unavailable: true,
        reason: "archive_not_configured",
        points: [],
      };
    const isDaily = query.kind === "archive-daily-bar";
    const end = query.to ?? now;
    const start = query.from ?? end - (isDaily ? 30 : 5) * 86_400_000;
    const baseClose = Number(known.at(-1)?.close ?? 100);
    const stepMs = isDaily ? 86_400_000 : MINUTE_MS;
    const points = [];
    for (
      let at = Math.max(start, end - (isDaily ? 30 : 5) * 86_400_000), i = 0;
      at <= end && points.length < limit;
      at += stepMs, i++
    ) {
      if (!isDaily) {
        const istMin = Math.floor(((at + 19_800_000) % 86_400_000) / MINUTE_MS);
        if (istMin < 555 || istMin >= 930) continue;
      }
      const close = baseClose * (1 + Math.sin(i / 25) * 0.012);
      points.push({
        at,
        open: d2(close * 0.999),
        high: d2(close * 1.002),
        low: d2(close * 0.998),
        close: d2(close),
        tick_count: isDaily ? null : 4,
        price_kind: isDaily ? "archive_candle" : "receipt_price",
        bar_kind: isDaily ? "archive_daily" : "archive_quote_minute",
        certified_ohlcv: false,
        volume: isDaily ? 50_000 : null,
      });
    }
    const sliced = points.slice(-limit);
    return {
      ...env,
      kind: query.kind,
      limit,
      range: { from: start, to: end },
      instrument_key: key,
      points: sliced,
      watermark_ms: sliced.at(-1)?.at ?? null,
      coverage: {
        from_ms: sliced[0]?.at ?? null,
        to_ms: sliced.at(-1)?.at ?? null,
        interval_ms: stepMs,
        observed: sliced.length,
        expected: sliced.length,
        gap_count: 0,
        gaps: [],
      },
      next: null,
      truncated: false,
      unavailable: false,
      source: "market_archive",
    };
  }
  const known = data.bars[key];
  if (!known)
    return {
      ...env,
      kind: "receipt-minute-bar",
      instrument_key: key,
      unavailable: true,
      reason: "instrument_not_in_feature_store",
      points: [],
    };
  const complete = known.filter(
    (b) => b.at >= from && b.at <= to && b.at + MINUTE_MS <= now,
  );
  const points = complete.slice(-limit).map((b) => ({
    at: b.at,
    open: d2(b.open),
    high: d2(b.high),
    low: d2(b.low),
    close: d2(b.close),
    tick_count: b.ticks,
    price_kind: "receipt_price",
    bar_kind: "receipt_minute",
    certified_ohlcv: false,
    volume: null,
  }));
  const firstAt = points[0]?.at ?? from;
  const lastAt = points.at(-1)?.at ?? from;
  const gaps = (data.gaps[key] ?? []).filter(
    (g) => g.from_ms >= firstAt && g.to_ms <= lastAt,
  );
  const expected = Math.floor((lastAt - firstAt) / MINUTE_MS) + 1;
  return {
    ...env,
    kind: "receipt-minute-bar",
    limit,
    range: { from: query.from ?? null, to: query.to ?? null },
    instrument_key: key,
    points,
    watermark_ms: lastAt,
    coverage: {
      from_ms: firstAt,
      to_ms: lastAt,
      interval_ms: MINUTE_MS,
      observed: points.length,
      expected,
      gap_count: Math.max(0, expected - points.length),
      gaps,
      store_start_ms: data.openMs,
      session_open_ms: data.openMs,
      store_started_after_open: false,
    },
    next: null,
    truncated: false,
    unavailable: false,
    source: "receipt_derived_feature_store",
    note: "Receipt-derived minute bars are display evidence, not certified exchange OHLCV.",
  };
}

export function agentAt(
  data: MockData,
  now: number,
  limit = 50,
  cursor = 0,
): AgentResponse {
  const visible = data.turns.filter((t) => (t.at_ms ?? 0) <= now);
  const page = [...visible].reverse().slice(cursor, cursor + limit);
  const lanes: Record<
    string,
    {
      input: number;
      output: number;
      cache_read: number;
      cost_usd: number;
      calls: number;
    }
  > = {};
  for (const t of visible) {
    const u = (lanes[t.lane] ??= {
      input: 0,
      output: 0,
      cache_read: 0,
      cost_usd: 0,
      calls: 0,
    });
    u.input += t.usage?.input ?? 0;
    u.output += t.usage?.output ?? 0;
    u.cache_read += t.usage?.cache_read ?? 0;
    u.cost_usd =
      Math.round((u.cost_usd + (t.usage?.cost_usd ?? 0)) * 1e4) / 1e4;
    u.calls += t.usage?.calls ?? 0;
  }
  const triggers: Record<string, number> = {};
  for (const e of data.triggerEvents) {
    if (e.at_ms > now) break;
    const k =
      e.event === "gated"
        ? e.escalate
          ? "gate_escalated"
          : "gate_declined"
        : e.event;
    triggers[k] = (triggers[k] ?? 0) + 1;
  }
  return {
    v: 1,
    turns: page,
    total: visible.length,
    next:
      cursor + page.length < visible.length
        ? String(cursor + page.length)
        : null,
    triggers,
    usage: { date: data.day, lanes },
    execution_authorized: false,
  };
}

/** pi session ids per lane: the first covers the morning, a restart starts the second. */
const sessionIds = (lane: string) =>
  [1, 2].map(
    (n) =>
      `0192a3b${n}-${lane.slice(0, 4).padEnd(4, "0")}-7079-a3a5-5c0de0000000`,
  );
const TOOL_ARGS: Record<string, (instruments: string[]) => unknown> = {
  snapshot: (i) => ({ instruments: i.slice(0, 3) }),
  quotes: (i) => ({ instruments: i }),
  intraday: (i) => ({ instrument: i[0], bars: 30 }),
  bars: (i) => ({ instrument: i[0], interval: "1m", limit: 60 }),
  positions: () => ({}),
  fills: () => ({ limit: 20 }),
  propose_plan: () => ({ stance: "see answer", actions: "see plan" }),
  verdict: () => ({ escalate: "see answer" }),
  web_search: () => ({ query: "market breadth NSE today" }),
  web_fetch: () => ({ url: "https://example.com/markets/breadth" }),
  news_lookup: () => ({ topic: "sector news", limit: 5 }),
  read_evidence: () => ({ evidence_id: "bundle" }),
};
/** Synthetic pi transcript for one lane, derived from the mock turns so both agree. */
export function transcriptAt(
  data: MockData,
  now: number,
  {
    lane,
    cursor = 0,
    limit = 20,
  }: { lane: string; cursor?: number; limit?: number },
): AgentTranscript {
  const rows = data.turns.filter(
    (t) => t.lane === lane && (t.at_ms ?? 0) <= now,
  );
  const ids = sessionIds(lane);
  const split = Math.floor(rows.length / 2);
  const sessions: TranscriptSession[] = ids.map((id, n) => ({
    id,
    file: `fixture-${n + 1}.jsonl`,
    started_ms: rows[n ? split : 0]?.at_ms ?? data.openMs,
    provider: rows[0]?.provider ?? null,
    model: rows[0]?.model ?? null,
    thinking: lane === "gate" ? "low" : "medium",
  }));
  const exchanges = rows.map((turn, i): TranscriptExchange => {
    const keys = turn.trigger?.instruments ?? [];
    const at_ms = (turn.at_ms ?? 0) + 20;
    const failed = turn.status === "error";
    const calls = [...(turn.tool_calls ?? [])];
    // Multi-name decisions look at each name, which makes a long tool run.
    if (lane === "decision" && keys.length > 2)
      calls.splice(
        2,
        0,
        ...keys.map(() => ({ name: "intraday", ok: true, error: null })),
      );
    const items: TranscriptItem[] = [
      {
        kind: "thinking",
        text: `${turn.trigger?.reason ?? "Scheduled turn"}.\nCheck the book first, then look at fresh evidence before answering.`,
        truncated: false,
      },
    ];
    calls.forEach((c, n) => {
      const args = JSON.stringify(TOOL_ARGS[c.name]?.(keys) ?? {});
      const last = failed && n === calls.length - 1;
      items.push({
        kind: "tool",
        id: `call_${i}_${n}`,
        name: c.name,
        args: { text: args, truncated: false },
        result: last
          ? null
          : {
              text: c.ok
                ? JSON.stringify({
                    status: "available",
                    execution_authorized: false,
                    rows: n + 2,
                  })
                : JSON.stringify({ error: c.error }),
              truncated: n === 1 && i % 5 === 0,
            },
        is_error: last ? null : !c.ok,
        duration_ms: last ? null : 6 + ((i * 7 + n * 13) % 80),
      });
    });
    if (failed) {
      if (turn.error === "provider_overloaded")
        items.push({
          kind: "error",
          text: "529 provider overloaded; retries exhausted",
        });
    } else
      items.push({
        kind: "text",
        text: turn.verdict
          ? `${turn.verdict.escalate ? "Escalating" : "Declining"}: ${turn.verdict.reason}.`
          : turn.plan
            ? `Stance ${turn.plan.stance ?? "hold"}; ${turn.plan.actions ?? 0} action(s) proposed for validation.`
            : "Findings recorded for the evidence library.",
        truncated: false,
      });
    const body = `Brief:\n${JSON.stringify({ session: data.day, lane, trigger: turn.trigger, book: { cash: "5000", positions: 0 } }, null, 2)}`;
    const long = i % 7 === 3;
    return {
      session: ids[i < split ? 0 : 1],
      at_ms,
      ended_ms: failed ? null : at_ms + (turn.elapsed_ms ?? 0),
      complete: !failed,
      brief: {
        text: long ? body.slice(0, Math.floor(body.length * 0.6)) : body,
        bytes: long ? body.length * 3 : body.length,
        truncated: long,
      },
      items,
      turn,
    };
  });
  const page = exchanges.reverse().slice(cursor, cursor + limit);
  return {
    v: 1,
    lane,
    sessions,
    exchanges: page,
    total: rows.length,
    next:
      cursor + page.length < rows.length ? String(cursor + page.length) : null,
    truncated: false,
    execution_authorized: false,
  };
}

function attributionAt(data: MockData, now: number): ActivityAttribution {
  const b = bookAt(data, now);
  const visible = data.turns.filter((t) => (t.at_ms ?? 0) <= now);
  const gates = visible.filter((t) => t.lane === "gate").length;
  const decided = data.decisions.filter((d) => d.at <= now);
  const proposed = decided.reduce((s, d) => s + d.actions.length, 0);
  const validated = decided.reduce(
    (s, d) => s + d.actions.filter((a) => a.reason === "accepted").length,
    0,
  );
  const declined = visible.filter(
    (t) => t.verdict && !t.verdict.escalate,
  ).length;
  const dd = drawdownOf(equityUpTo(data, now));
  const current = b.equity;
  const net = b.realized + b.marked - b.fees;
  const elapsed = visible
    .filter((t) => t.lane === "decision" && t.status === "ok")
    .map((t) => t.elapsed_ms ?? 0)
    .sort((x, y) => x - y);
  const q = (p: number) =>
    elapsed[Math.min(elapsed.length - 1, Math.floor(p * elapsed.length))] ?? 0;
  const ratio = (a: number, c: number) =>
    c ? Math.round((a / c) * 1e4) / 1e4 : 0;
  const rejected = proposed - validated;
  const abst = {
    model_abstained: 2,
    risk_preflight_blocked: 1,
    freshness_expired: 1,
    spread_cost_excessive: Math.floor(declined / 4),
    market_closed_or_halted: 0,
    universe_filter_excluded: 1,
    candidate_confidence_low: Math.floor(declined / 3),
    duplicate_or_in_flight: 1,
    other: Math.max(0, rejected - 3),
  };
  return {
    session: data.session,
    conversion: {
      candidates_evaluated: gates,
      decisions_considered: decided.length,
      actions_proposed: proposed,
      actions_validated: validated,
      fills_executed: b.seen.length,
      candidate_to_decision_rate: ratio(decided.length, gates),
      decision_to_action_rate: ratio(validated, decided.length * 2),
      action_to_fill_rate: ratio(b.seen.length, validated),
      overall_conversion_rate: ratio(b.seen.length, gates),
    },
    abstentions: {
      total_abstentions: Object.values(abst).reduce((s, v) => s + v, 0),
      ...abst,
    },
    financials: {
      realized_pnl: d2(b.realized),
      marked_pnl: d2(b.marked),
      gross_profit: d2(b.realized + b.marked),
      total_fees: d2(b.fees),
      total_slippage: d2(b.slippage),
      total_cost_drag: d2(b.fees + b.slippage),
      net_after_cost_pnl: d2(net),
    },
    turnover: {
      notional_traded: d2(b.notional),
      capital: d2(data.capital),
      turnover_ratio: ratio(b.notional, data.capital),
      fee_drag_bps: Math.round((b.fees / b.notional) * 1e5) / 10,
      slippage_drag_bps: Math.round((b.slippage / b.notional) * 1e5) / 10,
      total_cost_drag_bps:
        Math.round(((b.fees + b.slippage) / b.notional) * 1e5) / 10,
    },
    drawdown: {
      high_water_mark: d2(dd.high),
      current_equity: d2(current),
      current_drawdown: d2(Math.max(0, dd.high - current)),
      max_drawdown: d2(dd.max),
      max_drawdown_pct: Math.round(dd.maxPct * 100) / 100,
      max_drawdown_duration_ms: dd.longest,
    },
    drawdown_coverage: {
      basis: "recorded_checkpoints",
      complete: false,
      reason: "historical_position_marks_not_retained",
    },
    latency: {
      count: elapsed.length,
      min_ms: elapsed[0] ?? 0,
      max_ms: elapsed.at(-1) ?? 0,
      mean_ms: Math.round(
        elapsed.reduce((s, v) => s + v, 0) / Math.max(1, elapsed.length),
      ),
      p50_ms: q(0.5),
      p90_ms: q(0.9),
      p95_ms: q(0.95),
      p99_ms: q(0.99),
    },
    experiment_attributions: data.strategies.map((s, i) => ({
      source_hash: s.hash,
      spec_id: s.id,
      selection_reason: i ? "challenger_trial" : "champion",
      decisions_count: i ? 3 : 8,
      fills_count: i ? 4 : 11,
      notional_volume: d2(b.notional * (i ? 0.31 : 0.69)),
      realized_pnl: d2(b.realized * (i ? 0.2 : 0.8)),
      marked_pnl: d2(b.marked * (i ? 0.35 : 0.65)),
      fees_incurred: d2(b.fees * (i ? 0.3 : 0.7)),
      net_after_cost_pnl: d2(net * (i ? 0.25 : 0.75)),
      trial_status: i ? "in_trial" : "reviewed",
    })),
  };
}

function operationalAt(data: MockData, wall: number): OperationalHealth {
  return {
    version: 2,
    captured_at_ms: wall - 4_000,
    calendar: {
      source: "NSE/CMTR/71775",
      covered_from: "2026-01-01",
      covered_to: "2026-12-31",
      current_operational_date: data.day,
      covered: true,
      requested_session: data.day,
      requested_session_covered: true,
      historical_session: data.archive[0]?.id?.date ?? null,
      historical_session_covered: true,
      days_remaining: 38,
      expiring: false,
      expired: false,
      expiry_warning_days: 30,
      expiry_warning_basis: "operator_chosen_default_not_exchange_guarantee",
      update_prerequisite: "verified_exchange_calendar_update_required",
    },
    alert_transport: {
      status: "available",
      store: "alert_outbox",
      counts_scope: "host_alert_outbox_not_account_filtered",
      scope: {
        account: MOCK_ACCOUNT,
        session: data.day,
        store_scope: "host_alert_outbox",
      },
      counts: {
        queued: 0,
        retrying: 0,
        transport_accepted: 3,
        transport_accepted_scope: "retained_window_only_not_lifetime",
        dead: 0,
        dead_definition: "quarantined_outbox_rows",
        dropped: null,
        in_flight_or_transport_error: 0,
        in_flight_or_transport_error_definition:
          "status_zero_or_null_in_flight_or_transport_error_not_confirmed_failure",
        outbox_scope: "current_host_outbox_not_delivery_window",
        outbox_max_rows: 64,
        queued_scope: "current_outbox_rows_not_retained_delivery_window",
        retrying_scope: "current_outbox_rows_not_retained_delivery_window",
        dead_scope: "current_outbox_quarantined_rows_not_delivery_window",
        in_flight_or_transport_error_scope: "retained_window_only_not_lifetime",
      },
      retained_window: {
        start_ms: wall - 3_600_000,
        end_ms: wall,
        scope: "retained_rows_within_window_lower_bound_not_lifetime",
        max_retained_rows: 20,
        retained_delivery_rows: 3,
        rows_in_window: 3,
        truncation: "unknown_cap_or_lazy_pruning",
        clock_anomaly: false,
      },
      last_accepted_at_ms: wall - 1_800_000,
      last_accepted_scope: "retained_window_only_not_lifetime",
      last_failure_at_ms: null,
      last_failure_category: null,
      last_failure_scope: null,
      failure_marker_present: false,
      failure_marker_definition: "marker_presence_not_attempt_outcome",
      retention_window_seconds: 3600,
      timestamp_normalization:
        "delivery_at_unix_seconds_checked_mul_1000_unknown_on_overflow",
      dropped_source: "not_recorded",
      ack_meaning: "sender_http_2xx_receipt_not_human_read",
      delivery_guarantee: false,
    },
  };
}

function summaryAt(data: MockData, now: number, wall: number): SummaryResponse {
  const b = bookAt(data, now);
  const env = envelopeFor(data, now);
  const hist = {
    status: "PARTIAL",
    ready_instruments: 19,
    instruments: 20,
    target_session: data.day,
    failure: null,
  };
  const sha = hex(rng(data.seed + 77), 64);
  return {
    ...env,
    status: "open",
    model_status: "ready",
    entry_status: "entries_open",
    model_led: true,
    capital: d2(data.capital),
    cash: d2(b.cash),
    equity: d2(b.equity),
    counts: {
      positions: b.pos.size,
      fills: b.seen.length,
      universe: 20,
      scanned: 20,
      pending: 0,
    },
    state: {
      stopped: false,
      halted: false,
      closed: false,
      health: "ok",
      phase: "open",
    },
    market_feed: {
      state: "live",
      mode: "websocket",
      last_tick_age_ms: 380,
      cached_keys: 20,
      reconnects: 1,
      fallback: false,
      reason: null,
    },
    instrument_display: {
      status: "ready",
      source_sha256: sha,
      received_at_ms: data.openMs - 3_600_000,
    },
    readiness: {
      status: "ready",
      reason: "controller_ready",
      history: hist,
      lifecycle: { status: "running", reason: "market_open" },
      checked_at_ms: now - 2_000,
    },
    planner: {
      status: "idle",
      decision_id: planAt(data, now).planner?.decision_id ?? null,
      pending_attempt: null,
      pending_events: 0,
    },
    oldest_scan_age_ms: 2_100,
    history_preparation: hist,
    research_library: {
      status: "ready",
      items: data.candidates,
      truncated: false,
      activation_authorized: false,
    },
    research_tasks: {
      status: "ready",
      observation: {
        version: 1,
        status: "available",
        source: "original_queue_and_lifecycle_read_only",
        measurement: {
          evaluation_scope: "independent_episodes",
          strategy_portfolio_claimed: false,
          portfolio_return: null,
          aggregate_net_pnl: null,
        },
        as_of_ms: wall - 4_000,
        queue: {
          pending: 1,
          running: 1,
          unknown: 0,
          cancel_requested: 0,
          succeeded: 6,
          total: 8,
          truncated: false,
          oldest_created_ms: wall - 5_400_000,
          oldest_age_ms: 5_400_000,
          next_deadline_ms: wall + 900_000,
          deadline_remaining_ms: 900_000,
        },
        lifecycle: {
          status: "ready",
          recorded: 6,
          matured: 3,
          truncated: false,
          milestones: data.candidates.slice(0, 3).map((c) => ({
            candidate_id: c.candidate_id,
            state: c.state,
            original_recorded_state: c.state,
            effective_state: c.state,
            effective_scientific_state: c.scientific_state,
            origin_session: c.origin_session,
            continued: c.continued,
            observation_scope: "independent_virtual_episode",
            status: "recorded",
            measurement_status:
              c.scientific_state === "eligible" ? "verified" : "not_matured",
            exit_status: "pending",
            processed_at_ms: wall - 1_000_000,
            exit_at_ms: wall + 600_000,
          })),
        },
        trials: {
          status: "partial",
          reason: "partial_read",
          counts_lower_bound: true,
          scope: "current_ledger",
          gaps: ["archived_sessions_not_scanned"],
          known_lower_bound_counts: { admitted: 9, executed: 6, terminal: 5 },
          global_tested_trial_count: 9,
          admitted_trial_count: 9,
          executed_trial_count: 6,
          terminal_trial_count: 5,
          unknown_exposure_count: 0,
          distinct_terminal_tasks: 5,
          distinct_lifecycle_candidates: 4,
          distinct_sessions: 3,
          distinct_sources: 2,
          sessions: [data.day],
          family_key_is_label_only: true,
          family_count_scope: "label_only",
          multiple_testing: "not_corrected",
          reset: false,
        },
        capture: {
          enabled: true,
          status: "idle",
          reason: null,
          coverage: "journal_readability_only",
          coverage_label: "journal_readability_only",
          current_eligibility: "not_evaluated",
          eligible_for_attempt: false,
          used: 2,
          remaining: 18,
          max_calls_per_session: 20,
          max_keys: 24,
          max_rows: 500,
          min_interval_ms: 60_000,
          last_capture_ms: wall - 900_000,
          last_capture_age_ms: 900_000,
          provider_called: false,
          state_written: false,
          as_of_ms: wall - 4_000,
          timeout_ms: 5_000,
        },
        observation_refs: [],
        observation_refs_status: "complete",
        observation_refs_truncated: false,
        observation_refs_partial: false,
        observation_refs_scanned: 6,
        observation_refs_limit: 50,
        activation_authorized: false,
      },
      observation_capture: undefined,
    },
    operational_health: operationalAt(data, wall),
    alert_count: data.alerts.length,
    alerts: data.alerts,
  };
}

function portfolioAt(data: MockData, now: number): PortfolioResponse {
  const b = bookAt(data, now);
  const dd = drawdownOf(equityUpTo(data, now));
  return {
    ...envelopeFor(data, now),
    capital: d2(data.capital),
    cash: d2(b.cash),
    equity: d2(b.equity),
    initial_allocation: d2(data.capital),
    high_water: d2(dd.high),
    stopped: false,
    halted: false,
    closed: false,
    health: "ok",
    positions: positionsAt(data, now),
    fills_count: b.seen.length,
    recorded_fees: d2(b.fees),
    model_led: true,
  };
}

export function sessionsAt(data: MockData, now: number): SessionsResponse {
  const b = bookAt(data, now);
  const current: SessionRecord = {
    id: { broker: MOCK_BROKER, account: MOCK_ACCOUNT, date: data.session },
    mode: "paper",
    paper_only: true,
    live_enabled: false,
    status: "open_snapshot",
    phase: "open",
    current: true,
    as_of_ms: now,
    capital: d2(data.capital),
    cash: d2(b.cash),
    positions: b.pos.size,
    fills: b.seen.length,
  };
  const items = [current, ...data.archive];
  return { ...envelopeFor(data, now), total: items.length, items, next: null };
}

const LANE_BUDGETS: Record<string, [string, number, number]> = {
  decision: ["gpt-6.1-sol", 60, 4],
  gate: ["gemini-3-flash", 200, 1],
  research: ["claude-sonnet-5", 20, 3],
  review: ["claude-opus-5", 16, 2],
};
function laneBudgetsAt(data: MockData, now: number): LaneBudget[] {
  const usage = agentAt(data, now).usage?.lanes ?? {};
  return Object.entries(LANE_BUDGETS).map(([role, [model, calls, usd]]) => {
    const spent = usage[role];
    return {
      role,
      enabled: true,
      model,
      transport: "pi",
      provider: "openrouter",
      status: "unknown",
      reason: "agent_provider_not_checked",
      daily_call_budget: calls,
      budget_unit: "request",
      daily_cost_usd: usd,
      current_usage: spent?.calls ?? null,
      current_cost_usd: spent?.cost_usd ?? null,
      remaining_current: spent ? Math.max(0, calls - (spent.calls ?? 0)) : null,
      remaining_cost_usd: spent
        ? Math.max(0, usd - (spent.cost_usd ?? 0))
        : null,
    };
  });
}

export function healthAt(
  data: MockData,
  now: number,
  wall: number,
): HealthResponse {
  const s = summaryAt(data, now, wall);
  return {
    ...s,
    // Health carries the full readiness, including one budget row per lane.
    readiness: { ...s.readiness, models: { roles: laneBudgetsAt(data, now) } },
    version: "0.0.0-mock",
    activity_attribution: attributionAt(data, now),
    controller_liveness: {
      status: "healthy",
      last_completed_at_ms: wall - 3_000,
      age_ms: 3_000,
      max_age_ms: 30_000,
      expected_deadline_ms: wall + 27_000,
      loop_generation: 4812,
    },
    book: { stopped: false, halted: false, closed: false, health: "ok" },
    model_initialization: { status: "ready" },
    retention: { status: "disabled", reason: "retention_not_configured" },
    independent_telemetry: {
      status: "unavailable",
      reason: "independent_telemetry_not_connected",
    },
  };
}

export function toolsAt(
  data: MockData,
  now: number,
  limit = 50,
): ToolsResponse {
  const calls = data.toolCalls.filter((c) => (c.at_ms ?? 0) <= now);
  return {
    v: 1,
    session: data.session,
    calls: calls.slice(0, limit),
    total: calls.length,
    next: null,
    execution_authorized: false,
  };
}

export function instrumentsAt(
  data: MockData,
  now: number,
  q: {
    scope?: string;
    market_segment?: string;
    kind?: string;
    q?: string;
    cursor?: number;
    limit?: number;
  },
): InstrumentCatalogResponse {
  const all = q.scope === "all";
  let rows = data.catalog.filter((c) => (all ? true : c.segment === "NSE_EQ"));
  if (q.market_segment)
    rows = rows.filter((c) => c.segment === q.market_segment);
  if (q.kind) rows = rows.filter((c) => c.kind === q.kind);
  if (q.q) {
    const needle = q.q.toLowerCase();
    rows = rows.filter((c) =>
      [c.symbol, c.instrument_key, c.underlying_symbol]
        .filter(Boolean)
        .join(" ")
        .toLowerCase()
        .includes(needle),
    );
  }
  const cursor = q.cursor ?? 0,
    limit = Math.min(q.limit ?? 50, 500);
  const items = rows.slice(cursor, cursor + limit);
  const segments: Record<string, number> = {},
    kinds: Record<string, number> = {};
  for (const c of data.catalog) {
    segments[c.segment] = (segments[c.segment] ?? 0) + 1;
    kinds[c.kind ?? "unknown"] = (kinds[c.kind ?? "unknown"] ?? 0) + 1;
  }
  const h = (n: number) => hex(rng(data.seed + n), 64);
  return {
    ...envelopeFor(data, now),
    source: "session_nse_bod",
    scope: all ? "all" : "verified_cash",
    coverage_kind: "contract_metadata_only",
    segment: "NSE_EQ",
    series: "EQ",
    file_date: data.day,
    received_at_ms: data.openMs - 3_600_000,
    source_hashes: {
      nse_cm_security_sha256: h(21),
      upstox_nse_bod_sha256: h(22),
      upstox_suspended_bod_sha256: h(23),
    },
    cash_source_hashes: { nse_cm_security_sha256: h(21) },
    segments,
    kinds,
    rows_seen: data.catalog.length,
    rejected_rows: 0,
    display_only: true,
    total: rows.length,
    items,
    next: cursor + limit < rows.length ? cursor + limit : null,
  };
}

export function chainAt(
  data: MockData,
  now: number,
  q: {
    underlying_key?: string;
    expiry_ms?: number;
    market_segment?: string;
    cursor?: number;
    limit?: number;
  },
): ContractChainResponse {
  const legs = data.catalog.filter(
    (c) => c.underlying_key === q.underlying_key && c.expiry_ms === q.expiry_ms,
  );
  const strikes = new Map<string, ContractStrike>();
  for (const c of legs.filter((x) => x.kind === "call" || x.kind === "put")) {
    const k = String(c.strike);
    const row = strikes.get(k) ?? { strike: c.strike ?? "0" };
    if (c.kind === "call") row.call = c;
    else row.put = c;
    strikes.set(k, row);
  }
  const sorted = [...strikes.values()].sort(
    (a, b) => Number(a.strike) - Number(b.strike),
  );
  const cursor = q.cursor ?? 0,
    limit = Math.min(q.limit ?? 50, 500);
  const futures = legs.filter((x) => x.kind === "future");
  return {
    ...envelopeFor(data, now),
    source: "session_nse_bod",
    received_at_ms: data.openMs - 3_600_000,
    source_hashes: { upstox_nse_bod_sha256: hex(rng(data.seed + 22), 64) },
    underlying_key: q.underlying_key,
    expiry_ms: q.expiry_ms,
    total: sorted.length,
    items: sorted.slice(cursor, cursor + limit),
    futures,
    futures_total: futures.length,
    futures_truncated: false,
    unavailable_strikes: 0,
    next: cursor + limit < sorted.length ? cursor + limit : null,
    display_only: true,
  };
}

export function alertsAt(
  data: MockData,
  now: number,
): { v: 1 } & SessionsResponse {
  return {
    ...envelopeFor(data, now),
    total: data.alerts.length,
    items: data.alerts as never,
    next: null,
  } as never;
}

export function replayExampleResult(data: MockData): ReplayResponse {
  const curve = Array.from({ length: 7 }, (_, i) => ({
    at: data.openMs + i * 60_000,
    equity: d2(1_000_000 + [0, 120, 80, 310, 260, 540, 480][i]),
  }));
  return {
    report: {
      realized_pnl_if_flat: "480.00",
      maximum_drawdown: "50.00",
      fees: "36.40",
      events: 6,
      session: data.day,
      receipt_clock: "recorded_receipt_ms",
      limitations: ["Synthetic quotes", "No candle-to-quote conversion"],
      portfolio: {
        cash: "1000480.00",
        positions: {},
        fills: [],
        closed: false,
      },
    },
    equity_curve: curve,
    engine_hash: hex(rng(5), 64),
    manifest_hash: hex(rng(6), 64),
    data_hash: hex(rng(7), 64),
  };
}

/** Everything the dev server answers that depends on the clock, in one place. */
export const projections = {
  summary: summaryAt,
  portfolio: portfolioAt,
  positions: (d: MockData, n: number) => positionsAt(d, n),
  fills: (d: MockData, n: number, cursor = 0, limit = 500): FillsResponse => {
    const rows = fillRows(d, n);
    return {
      ...envelopeFor(d, n),
      total: rows.length,
      items: rows.slice(cursor, cursor + limit),
      next: cursor + limit < rows.length ? cursor + limit : null,
    };
  },
  plans: planAt,
  scanner: scannerAt,
};
