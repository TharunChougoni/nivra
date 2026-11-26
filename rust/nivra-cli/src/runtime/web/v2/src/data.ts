import type {
  SummaryResponse,
  PortfolioResponse,
  MetaResponse,
  ProjectionSource,
  SessionRecord,
  Fill,
  PlansResponse,
  ScannerRow,
  Point,
  SeriesResponse,
  SeriesKind,
  SeriesCoverage,
  StreamEvent,
  StrategyReference,
  Instrument,
} from "./types";
// Display projections only. Original identity, quote clocks and unknowns survive.
export const numeric = (v: unknown) =>
  (typeof v === "number" || typeof v === "string") &&
  v !== "" &&
  Number.isFinite(Number(v))
    ? Number(v)
    : null;
export const amount = (v: unknown) =>
  numeric(v) === null
    ? "—"
    : new Intl.NumberFormat("en-IN", {
        style: "currency",
        currency: "INR",
        maximumFractionDigits: 2,
      }).format(Number(v));
export const usd = (v: unknown) =>
  numeric(v) === null
    ? "—"
    : "$" +
      Number(v).toLocaleString("en-US", {
        minimumFractionDigits: 2,
        maximumFractionDigits: Math.abs(Number(v)) < 1 ? 4 : 2,
      });
export const signed = (v: number | null) =>
  v === null ? "—" : (v > 0 ? "+" : "") + amount(v);
export const tone = (v: number | null) =>
  v === null || v === 0
    ? undefined
    : v > 0
      ? ("pos" as const)
      : ("neg" as const);
export const percent = (value?: number | null) =>
  typeof value === "number" && Number.isFinite(value)
    ? `${(value * 100).toFixed(2)}%`
    : "Not reported";
export const percentPoints = (value?: number | null) =>
  typeof value === "number" && Number.isFinite(value)
    ? `${value.toFixed(2)}%`
    : "Not reported";
export const count = (v: unknown) =>
  numeric(v) === null
    ? "—"
    : new Intl.NumberFormat("en-IN", { maximumFractionDigits: 2 }).format(
        Number(v),
      );
export const clock = (v: unknown) =>
  numeric(v) === null
    ? "—"
    : new Intl.DateTimeFormat("en-IN", {
        timeZone: "Asia/Kolkata",
        hour: "2-digit",
        minute: "2-digit",
        hour12: false,
      }).format(Number(v));
export const datetime = (v: unknown) => {
  if (v === null || v === undefined || v === "") return "Not reported";
  const at = numeric(v) ?? Date.parse(String(v));
  return Number.isFinite(at)
    ? new Intl.DateTimeFormat("en-IN", {
        timeZone: "Asia/Kolkata",
        day: "2-digit",
        month: "short",
        hour: "2-digit",
        minute: "2-digit",
        hour12: false,
      }).format(at) + " IST"
    : "Not reported";
};
export const shortDate = (v: unknown) => {
  if (v === null || v === undefined || v === "") return "Not reported";
  const at = numeric(v) ?? Date.parse(String(v));
  return Number.isFinite(at)
    ? new Intl.DateTimeFormat("en-IN", {
        timeZone: "Asia/Kolkata",
        day: "2-digit",
        month: "short",
        year: "numeric",
      }).format(at)
    : "Not reported";
};
const IST_OFFSET_MS = 19_800_000;
const DAY_MS = 86_400_000;
export function istDateString(ms: number): string {
  return new Date(ms + IST_OFFSET_MS).toISOString().slice(0, 10);
}
export function shiftIstDate(date: string, days: number): string {
  const start = Date.parse(`${date}T00:00:00Z`);
  if (!Number.isFinite(start)) return date;
  return new Date(start + days * DAY_MS).toISOString().slice(0, 10);
}
export function istDayStartMs(date: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) return null;
  const ms = Date.parse(`${date}T00:00:00+05:30`);
  return Number.isFinite(ms) ? ms : null;
}
export function istDayEndMs(date: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) return null;
  const ms = Date.parse(`${date}T23:59:59.999+05:30`);
  return Number.isFinite(ms) ? ms : null;
}
export const duration = (v: unknown) =>
  numeric(v) === null
    ? "—"
    : Number(v) < 1000
      ? `${v} ms`
      : `${(Number(v) / 1000).toFixed(1)} s`;
export const sessionKey = (r?: { id?: SessionRecord["id"] } | null) =>
  [r?.id?.broker, r?.id?.account, r?.id?.date].join("|");
export function projectedStatus(
  portfolio: PortfolioResponse | null | undefined,
  summary: SummaryResponse | null | undefined,
) {
  if (portfolio?.stopped === true) return "stopped";
  if (portfolio?.halted === true) return "halted";
  if (portfolio?.closed === true) return "closed";
  if (
    portfolio &&
    [portfolio.stopped, portfolio.halted, portfolio.closed].every(
      (v) => v === false,
    )
  )
    return "open_snapshot";
  if (summary?.state?.stopped === true) return "stopped";
  if (summary?.state?.halted === true) return "halted";
  if (summary?.state?.closed === true) return "closed";
  return undefined;
}
// v1 sessions carries the current checkpoint and the archive; the account
// projection carries the exact current book. Missing projections never become
// an invented flat or archived record.
export function sessions(
  state?: ProjectionSource | null,
  meta?: MetaResponse | null,
): SessionRecord[] {
  const page = state?.sessions;
  const summary = state?.summary;
  const portfolio =
    state?.portfolio && state.portfolio.unavailable !== true
      ? state.portfolio
      : null;
  const result = new Map<string, SessionRecord>();
  // Accept both the v1 sessions projection and the former aggregate snapshot
  // shape so archived session tests and callers cannot confuse the two.
  for (const row of Array.isArray(page)
    ? page
    : Array.isArray(page?.items)
      ? page.items
      : [])
    result.set(sessionKey(row), { ...row });
  if (summary) {
    const id = {
      broker:
        summary.broker?.data ||
        portfolio?.broker?.data ||
        meta?.broker?.data ||
        null,
      account: summary.account || portfolio?.account || null,
      date: summary.session || portfolio?.session || null,
    };
    const key = sessionKey({ id });
    const existing = result.get(key) || null;
    const row = {
      ...(existing || {}),
      id: { ...id, ...(existing?.id || {}) },
      current: true,
      mode: summary.mode ?? existing?.mode ?? null,
      paper_only: summary.paper_only ?? existing?.paper_only ?? null,
      live_enabled: summary.live_enabled ?? existing?.live_enabled ?? null,
      phase: existing?.phase ?? summary.status ?? null,
      status: existing?.status || projectedStatus(portfolio, summary) || null,
      as_of_ms: portfolio?.as_of_ms ?? summary.as_of_ms ?? null,
      capital:
        portfolio?.capital ?? summary.capital ?? existing?.capital ?? null,
      cash: portfolio?.cash ?? summary.cash ?? existing?.cash ?? null,
      ...(portfolio ? { portfolio } : {}),
    };
    result.set(key, row);
  }
  return [...result.values()].sort((a, b) =>
    String(b.id?.date).localeCompare(String(a.id?.date)),
  );
}
export function currentSession(
  state?: ProjectionSource | null,
  meta?: MetaResponse | null,
) {
  const rows = sessions(state, meta);
  return (
    rows.find((r) => r.current === true) ||
    rows.find(
      (r) =>
        state?.summary &&
        sessionKey(r) ===
          sessionKey({
            id: {
              broker: state.summary.broker?.data,
              account: state.summary.account,
              date: state.summary.session,
            },
          }),
    ) ||
    rows[0] ||
    null
  );
}
// Current fills are newest-first in /api/v1/fills; archived report fills are
// oldest-first. Normalise display order only, never invent missing rows.
export function fillsFor(
  state: ProjectionSource | null | undefined,
  record: SessionRecord | null | undefined,
) {
  if (record?.current === true) {
    if (
      !state?.fills ||
      state.fillsState === "error" ||
      state.fills.unavailable === true
    )
      return null;
    return Array.isArray(state.fills.items) ? state.fills.items : null;
  }
  return Array.isArray(record?.portfolio?.fills)
    ? [...record.portfolio.fills].reverse()
    : null;
}
export function plansFor(
  state: ProjectionSource | null | undefined,
  record: SessionRecord | null | undefined,
) {
  if (record?.current !== true || state?.plansState === "error") return null;
  return state?.plans || null;
}
export function finance(r?: SessionRecord | null) {
  const p = r?.portfolio;
  const positionsKnown =
    Array.isArray(p?.positions) ||
    (!!p?.positions && typeof p.positions === "object");
  const positions = Array.isArray(p?.positions)
    ? p.positions
    : positionsKnown
      ? Object.values(p?.positions || {})
      : [];
  const fills = Array.isArray(p?.fills) ? p.fills : null;
  const cash = numeric(p?.cash) ?? numeric(r?.cash);
  const capital = numeric(r?.capital) ?? numeric(p?.initial_allocation);
  const declaredEquity = numeric(p?.equity);
  const marked =
    positionsKnown &&
    cash !== null &&
    positions.every(
      (v) => numeric(v.units) !== null && numeric(v.mark) !== null,
    );
  const exposure = marked
    ? positions.reduce((sum, v) => sum + Number(v.units) * Number(v.mark), 0)
    : null;
  const recordedFees = numeric(p?.recorded_fees);
  return {
    cash,
    positions,
    positionsKnown,
    fills,
    exposure,
    // Long and short legs both consume exposure; unknown marks stay unknown.
    gross: marked
      ? positions.reduce(
          (sum, v) => sum + Math.abs(Number(v.units) * Number(v.mark)),
          0,
        )
      : null,
    unrealized:
      positionsKnown &&
      positions.every((v) =>
        [v.mark, v.entry, v.units].every((x) => numeric(x) !== null),
      )
        ? positions.reduce(
            (sum, v) =>
              sum + (Number(v.mark) - Number(v.entry)) * Number(v.units),
            0,
          )
        : null,
    equity:
      declaredEquity !== null
        ? declaredEquity
        : marked
          ? cash! + exposure!
          : null,
    net:
      positionsKnown &&
      positions.length === 0 &&
      cash !== null &&
      capital !== null
        ? cash - capital
        : null,
    fees:
      recordedFees !== null
        ? recordedFees
        : fills && fills.every((v) => numeric(v.fee) !== null)
          ? fills.reduce((sum, v) => sum + Number(v.fee), 0)
          : null,
  };
}
export const instrumentClassLabels: Record<string, string> = {
  cash: "Cash",
  index: "Index",
  etf: "ETF",
  future: "Future",
  call: "Call",
  put: "Put",
};
const classOrder = ["cash", "index", "etf", "future", "call", "put"];
export function instrumentClass(
  row?: { instrument?: Instrument; kind?: string | null } | null,
) {
  const kind = row?.instrument?.kind ?? row?.kind ?? null;
  return typeof kind === "string" && kind.trim()
    ? kind.trim().toLowerCase()
    : null;
}
// Only recorded lot metadata supports a whole-lot display. Unknown, invalid
// and non-divisible quantities stay unknown instead of assuming a lot of one.
export function lotQuantity(
  row?: { instrument?: Instrument; units?: string | number } | null,
) {
  const units = numeric(row?.units),
    lot = numeric(row?.instrument?.lot_size ?? row?.instrument?.lot);
  if (
    units === null ||
    lot === null ||
    !Number.isSafeInteger(units) ||
    !Number.isSafeInteger(lot) ||
    units < 0 ||
    lot <= 0 ||
    units % lot !== 0
  )
    return null;
  return { lots: units / lot, lot_size: lot, units };
}
export function instrumentClasses(
  rows?: { instrument?: Instrument; kind?: string | null }[] | null,
) {
  const found = new Set(
    (Array.isArray(rows) ? rows : [])
      .map(instrumentClass)
      .filter((kind) => kind !== null),
  );
  return [...found].sort((a, b) => {
    const left = classOrder.indexOf(a),
      right = classOrder.indexOf(b);
    return (
      (left < 0 ? classOrder.length : left) -
        (right < 0 ? classOrder.length : right) || a.localeCompare(b)
    );
  });
}
// Links only what the projections actually record. Null means the source
// projection is unavailable; an empty list means no matching record.
export function linkedPlan(
  plans: PlansResponse | null | undefined,
  decisionId?: string | null,
  instrumentKey?: string | null,
) {
  if (!plans || plans.unavailable === true) return null;
  const decision = plans.planner?.decision_id || null;
  // A row without both recorded identities is not linked by instrument alone.
  if (!decisionId || !instrumentKey)
    return { decision, actions: [], execution: [] };
  if (!decision || String(decisionId) !== String(decision))
    return { decision, actions: [], execution: [] };
  const execution = Array.isArray(plans.execution) ? plans.execution : [];
  const actions = (Array.isArray(plans.actions) ? plans.actions : [])
    .filter((row) => row.action?.instrument_key === instrumentKey)
    .map((row) => ({
      ...row,
      execution:
        execution.find(
          (entry) =>
            entry.decision_id === decision && entry.index === row.index,
        ) || null,
    }));
  return { decision, actions, execution };
}
export function linkedFills(
  fills: Fill[] | null | undefined,
  decisionId?: string | null,
  instrumentKey?: string | null,
) {
  if (fills == null) return null;
  if (!decisionId || !instrumentKey) return [];
  return fills.filter(
    (row) =>
      row.instrument?.key === instrumentKey &&
      String(row.decision_id) === String(decisionId),
  );
}
const strategyFields: (keyof ScannerRow)[] = [
  "strategy_spec_id",
  "strategy_spec_version",
  "strategy_spec_hash",
  "strategy_scan_id",
  "strategy_score",
];
export function strategyLinks(rows?: ScannerRow[] | null) {
  return (Array.isArray(rows) ? rows : []).filter((row) =>
    strategyFields.some(
      (field) => row?.[field] !== null && row?.[field] !== undefined,
    ),
  );
}
// One row per recorded strategy identity. Prospects are the only v1 projection
// that carries these fields; library snapshots and switch history do not exist
// in this projection and are never reconstructed from these references.
export function strategyReferences(rows?: ScannerRow[] | null) {
  const references = new Map<string, StrategyReference>();
  for (const row of strategyLinks(rows)) {
    const key = [
      row.strategy_spec_id,
      row.strategy_spec_version,
      row.strategy_spec_hash,
      row.strategy_scan_id,
    ].join("|");
    const existing = references.get(key);
    if (existing) {
      existing.instruments.push(row.instrument_key);
      if (row.strategy_score !== null && row.strategy_score !== undefined)
        existing.scores.push(row.strategy_score);
      continue;
    }
    references.set(key, {
      key,
      strategy_spec_id: row.strategy_spec_id ?? null,
      strategy_spec_version: row.strategy_spec_version ?? null,
      strategy_spec_hash: row.strategy_spec_hash ?? null,
      strategy_scan_id: row.strategy_scan_id ?? null,
      strategy_score: row.strategy_score ?? null,
      scores:
        row.strategy_score === null || row.strategy_score === undefined
          ? []
          : [row.strategy_score],
      instruments: [row.instrument_key],
    });
  }
  return [...references.values()].map((reference) => {
    const scores = [
      ...new Map(reference.scores.map((v) => [String(v), v])).values(),
    ];
    return {
      ...reference,
      // A grouped scan can carry a different recorded score per instrument.
      // Expose one value only when every row agrees; otherwise keep the list.
      strategy_score: scores.length === 1 ? scores[0] : null,
      scores,
    };
  });
}
export function strategyLinkFor(
  rows?: ScannerRow[] | null,
  instrumentKey?: string | null,
) {
  if (!instrumentKey) return null;
  return (
    (Array.isArray(rows) ? rows : []).find(
      (row) =>
        row.instrument_key === instrumentKey && strategyLinks([row]).length,
    ) || null
  );
}
export function observation(row?: ScannerRow | null) {
  const value = numeric(row?.latest_screening_price ?? row?.latest_sampled_bid);
  const at = numeric(row?.sampled_price_ms ?? row?.sampled_bid_ms);
  return value !== null && value > 0 && at !== null && at > 0
    ? {
        at,
        value,
        source: numeric(row?.source_ms),
        kind: row?.price_kind || "sampled bid",
      }
    : null;
}
export function appendPoint(
  points: Point[],
  point: Point | null,
  gap = false,
): Point[] {
  if (!point || !Number.isFinite(point.at) || !Number.isFinite(point.value))
    return points;
  const last = points.at(-1);
  if (last && point.at <= last.at) return points;
  const next = [...points];
  if (gap && last)
    next.push({
      at: last.at + (point.at - last.at) / 2,
      value: null,
      gap: true,
      streamGap: true,
    });
  next.push(point);
  return next.slice(-2048); // Browser display memory only; not market retention.
}
function carriedPoint(at: number, value: number | null): Point {
  return {
    at,
    value,
    open: value,
    high: value,
    low: value,
    close: value,
    tickCount: 0,
    priceKind: "receipt_price",
    barKind: "carry_forward",
    carried: true,
    certified: false,
    volume: null,
    source: "carry_forward",
  };
}
export function projectSeries(
  body: SeriesResponse | null | undefined,
  kind: SeriesKind,
) {
  const points: Point[] = [];
  const bars: Point[] = [];
  for (const row of Array.isArray(body?.points) ? body.points : []) {
    const at = numeric(row?.at);
    if (at === null || at <= 0) continue;
    if (kind === "account-equity") {
      const value = numeric(row?.equity);
      if (value === null) continue;
      points.push({
        at,
        value,
        cash: numeric(row?.cash),
        markBasis: row?.mark_basis || null,
        source: "portfolio_checkpoint",
      });
    } else {
      const value = numeric(row?.close);
      if (value === null || value <= 0) continue;
      const point = {
        at,
        value,
        open: numeric(row?.open),
        high: numeric(row?.high),
        low: numeric(row?.low),
        close: numeric(row?.close),
        tickCount: numeric(row?.tick_count),
        priceKind: row?.price_kind || null,
        barKind: row?.bar_kind || null,
        certified: row?.certified_ohlcv === true,
        volume: numeric(row?.volume),
        source: body?.source || "receipt_minute_bar",
      };
      bars.push(point);
      points.push(point);
    }
  }
  const barAt = new Map(bars.map((point) => [point.at, point]));
  const gapRows =
    kind === "archive-daily-bar" ? [] : body?.coverage?.gaps || [];
  for (const gap of gapRows) {
    const from = numeric(gap?.from_ms);
    const to = numeric(gap?.to_ms) ?? from;
    if (from === null || from <= 0 || to === null || to < from) continue;
    const reason = String(gap?.reason || "gap");
    const previous = bars.filter((point) => point.at <= from).at(-1) || null;
    // A no-trade minute was observed but had no tick: carry the last receipt
    // close forward as a display-only step. Unobserved spans stay null.
    if (reason === "no_trade" && previous) {
      for (
        let at = from, guard = 0;
        at <= to && guard < 10_000;
        guard++, at += 60_000
      )
        if (!barAt.has(at)) points.push(carriedPoint(at, previous.value));
    } else {
      points.push({
        at: from,
        value: null,
        gap: true,
        gapTo: to,
        reason,
      });
    }
  }
  points.sort((a, b) => a.at - b.at);
  const unique: Point[] = [];
  for (const point of points) {
    const last = unique.at(-1);
    if (last && last.at === point.at) unique[unique.length - 1] = point;
    else unique.push(point);
  }
  const annotated = unique.map((point, index) => {
    if (point.value === null) return point;
    const previous = unique[index - 1]?.value ?? null;
    const next = unique[index + 1]?.value ?? null;
    return { ...point, isolated: previous === null && next === null };
  });
  return {
    points: annotated,
    coverage: body?.coverage || null,
    watermark: numeric(body?.watermark_ms),
    source: body?.source || null,
    note: body?.note || null,
  };
}
export function reconcileSeries(
  previous: Point[],
  points: Point[],
  watermark: number | null,
  coverage: SeriesCoverage | null = null,
) {
  const floor = numeric(watermark);
  const coveredFrom = numeric(coverage?.from_ms);
  const coveredTo = numeric(coverage?.to_ms);
  const merged = new Map<number, Point>();
  for (const point of Array.isArray(points) ? points : [])
    merged.set(point.at, point);
  for (const point of Array.isArray(previous) ? previous : []) {
    if (numeric(point?.at) === null) continue;
    // An authoritative refetch that spans a local SSE sequence marker proves
    // the break was transport-local, so the stale marker is discarded.
    if (
      point.streamGap &&
      coveredFrom !== null &&
      coveredTo !== null &&
      point.at >= coveredFrom &&
      point.at <= coveredTo
    )
      continue;
    if ((floor === null || point.at > floor) && !merged.has(point.at))
      merged.set(point.at, point);
  }
  return [...merged.values()].sort((a, b) => a.at - b.at).slice(-2048);
}
export function mergePriceBatch(
  points: Point[],
  event: StreamEvent,
  watch: string,
  lastSeq = 0,
) {
  const seq = numeric(event?.batch_seq);
  const reset = seq !== null && lastSeq > 0 && seq <= lastSeq;
  const gap = seq !== null && lastSeq > 0 && !reset && seq > lastSeq + 1;
  const row = (event?.items || []).find(
    (item) => item?.instrument_key === watch,
  );
  const at = numeric(row?.receipt_ms);
  const value = numeric(row?.last_price);
  const point =
    at !== null && at > 0 && value !== null && value > 0
      ? {
          at,
          value,
          source: "price_batch",
          kind: event?.price_kind || row?.price_kind || "receipt_price",
        }
      : null;
  return {
    points: appendPoint(points, point, gap),
    seq: seq ?? lastSeq,
    gap,
    matched: point !== null,
  };
}
export function downsamplePoints(points: Point[], maxPoints = 1200): Point[] {
  if (!Array.isArray(points) || points.length <= maxPoints) return points;
  const start = points[0].at,
    end = points.at(-1)!.at,
    span = Math.max(1, end - start),
    buckets = Math.max(2, Math.floor(maxPoints / 4)),
    keep = new Set([0, points.length - 1]),
    extrema = new Map<number, { min: number; max: number }>();
  points.forEach((point, index) => {
    if (point.value === null || !Number.isFinite(point.at)) {
      keep.add(index);
      return;
    }
    const bucket = Math.min(
      buckets - 1,
      Math.max(0, Math.floor(((point.at - start) / span) * buckets)),
    );
    const state = extrema.get(bucket);
    if (!state) extrema.set(bucket, { min: index, max: index });
    else {
      if (point.value < points[state.min].value!) state.min = index;
      if (point.value > points[state.max].value!) state.max = index;
    }
  });
  for (const { min, max } of extrema.values()) {
    keep.add(min);
    keep.add(max);
  }
  return [...keep].sort((a, b) => a - b).map((index) => points[index]);
}
export const ROUTES = [
  "overview",
  "portfolio",
  "instruments",
  "lanes",
  "scanner",
  "strategies",
  "backtest",
  "sessions",
  "health",
] as const;
export type RouteName = (typeof ROUTES)[number];
/** `#/models` and `#/runs/...` are retired pages; they land on the pi lanes. */
export function route(hash: string): { name: RouteName | "missing" } {
  const raw = String(hash || "").replace(/^#\/?/, "");
  if (!raw) return { name: "overview" };
  if (raw === "models" || raw === "runs" || raw.startsWith("runs/"))
    return { name: "lanes" };
  const name = ROUTES.find((v) => v === raw);
  return { name: name || "missing" };
}

/** Why the paper desk declined an order the model proposed. */
export const refusalLabels: Record<string, string> = {
  book_stopped_or_closed: "the book is stopped or closed",
  halted: "the book is halted",
  exit_time_passed: "the exit time had already passed",
  zero_units: "the size came to zero units",
  stale_quote: "the quote was too old",
  crossed_quote: "the bid was above the ask",
  outside_session: "the market session was closed",
  option_contract_invalid: "the option contract or lot size did not check out",
  price_above_max: "the price was above the model's limit",
  instrument_mismatch: "the held instrument did not match",
  insufficient_depth: "the market did not have enough size",
  insufficient_cash: "there was not enough cash",
  stale_position_mark: "a held position had an old price",
  no_held_units: "there were not enough held units to sell",
  sell_cash_negative: "the sale would leave cash below zero",
};

/** An execution verdict with its refusal reason, in plain words. */
export function executionText(
  e?: null | {
    verdict?: string;
    reason?: string | null;
  },
): string | null {
  if (!e?.verdict) return null;
  const word = e.verdict.replaceAll("_", " ");
  if (e.verdict !== "execution_gate_rejected")
    return word.charAt(0).toUpperCase() + word.slice(1);
  const why = e.reason ? refusalLabels[e.reason] : undefined;
  return why ? `Refused: ${why}` : "Refused";
}
