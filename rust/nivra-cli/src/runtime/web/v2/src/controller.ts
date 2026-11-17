import type {
  WorkspaceState,
  ChartSeries,
  ProjectionName,
  ProjectionData,
  SummaryResponse,
  PortfolioResponse,
  ScannerResponse,
  SeriesKind,
  SeriesResponse,
  StreamEvent,
  LoadState,
  Fetch,
} from "./types";
import { errorMessage } from "./types";
import type { BrowserWindow, EvidenceCache } from "./cache";
export interface ControllerOptions {
  fetch?: Fetch;
  cache?: EvidenceCache;
  connect?: boolean;
}
import { createApi } from "./api";
import { createEvidenceCache } from "./cache";
import { createEventStream } from "./events";
import {
  appendPoint,
  currentSession,
  finance,
  mergePriceBatch,
  numeric,
  observation,
  projectSeries,
  reconcileSeries,
  sessions,
  sessionKey,
} from "./data";

const emptySeries = (): ChartSeries => ({
  price: [],
  equity: [],
  priceState: "idle",
  equityState: "idle",
  priceCached: false,
  equityCached: false,
  priceCoverage: null,
  equityCoverage: null,
  priceSource: null,
  equitySource: null,
  priceNote: null,
  equityNote: null,
  priceWatermark: null,
  equityWatermark: null,
});
const projectionNames: ProjectionName[] = [
  "summary",
  "portfolio",
  "fills",
  "plans",
  "scanner",
  "sessions",
  "approvals",
];

export function createController(
  win: BrowserWindow,
  options: ControllerOptions = {},
) {
  let token = "";
  try {
    token = win.sessionStorage.getItem("paperToken") || "";
  } catch {
    /* memory only */
  }
  const initial = (): WorkspaceState => ({
    token: "",
    meta: null,
    summary: null,
    summaryState: "idle",
    portfolio: null,
    portfolioState: "idle",
    fills: null,
    fillsState: "idle",
    plans: null,
    plansState: "idle",
    scanner: null,
    scannerState: "idle",
    sessions: null,
    sessionsState: "idle",
    approvals: null,
    approvalsState: "idle",
    snapshotState: "idle",
    issues: [],
    received: null,
    connection: "locked",
    error: "",
    session: "",
    points: [],
    equityPoints: [],
    series: emptySeries(),
    priceBatchSeq: 0,
    watch: "",
    events: [],
    revision: 0,
  });
  let state: WorkspaceState = { ...initial(), token };
  let epoch = 0;
  let stream: ReturnType<typeof createEventStream> | null = null;
  let pending: Promise<ProjectionResults> | null = null;
  let dirty = false,
    stopped = false,
    gap = false,
    chartDeferred = false,
    lastConnection = "";
  const listeners = new Set<() => void>();
  const set = (patch: Partial<WorkspaceState>) => {
    state = { ...state, ...patch };
    listeners.forEach((f) => f());
  };
  const evidence = options.cache || createEvidenceCache(win);
  const summaryIdentity = (summary: SummaryResponse | null) =>
    summary
      ? [
          summary.broker?.data || "",
          summary.account || "",
          summary.session || "",
        ].join("|")
      : "";
  const seriesPatch = (
    kind: SeriesKind,
    body: SeriesResponse,
    cached: boolean,
  ): Partial<WorkspaceState> => {
    const parsed = projectSeries(body, kind);
    const prefix = kind === "account-equity" ? "equity" : "price";
    return {
      series: {
        ...state.series,
        [prefix]: reconcileSeries(
          state.series[prefix],
          parsed.points,
          parsed.watermark,
          parsed.coverage,
        ),
        [`${prefix}State`]: cached ? "cached" : "ready",
        [`${prefix}Cached`]: cached,
        [`${prefix}Coverage`]: parsed.coverage,
        [`${prefix}Source`]: parsed.source,
        [`${prefix}Note`]: parsed.note,
        [`${prefix}Watermark`]: parsed.watermark,
      },
    };
  };
  async function fetchPriceSeries(instrument = state.watch) {
    if (!state.token || !state.meta || !state.summary || !instrument || stopped)
      return;
    const generation = epoch,
      binding = summaryIdentity(state.summary),
      key = `${binding}|receipt-minute-bar|${instrument}|latest`;
    const cached = await evidence.get(key);
    if (generation !== epoch || stopped || instrument !== state.watch) return;
    if (cached?.body) set(seriesPatch("receipt-minute-bar", cached.body, true));
    try {
      const body = await api.series({
        instrument,
        kind: "receipt-minute-bar",
      });
      if (generation !== epoch || stopped || instrument !== state.watch) return;
      if (body?.unavailable) {
        if (!cached?.body)
          set({
            series: {
              ...state.series,
              price: [],
              priceState: "unavailable",
              priceCached: false,
              priceCoverage: null,
              priceSource: null,
              priceNote: body.reason || null,
              priceWatermark: null,
            },
          });
        return;
      }
      set(seriesPatch("receipt-minute-bar", body, false));
      void evidence.put(
        key,
        { body },
        {
          binding,
          kind: "receipt-minute-bar",
          instrument,
        },
      );
    } catch {
      if (generation !== epoch || instrument !== state.watch) return;
      if (!cached?.body)
        set({
          series: {
            ...state.series,
            priceState: "error",
            priceCached: false,
          },
        });
    }
  }
  async function fetchEquitySeries() {
    if (!state.token || !state.meta || !state.summary || stopped) return;
    const generation = epoch,
      binding = summaryIdentity(state.summary),
      key = `${binding}|account-equity||latest`;
    const cached = await evidence.get(key);
    if (generation !== epoch || stopped) return;
    if (cached?.body) set(seriesPatch("account-equity", cached.body, true));
    try {
      const body = await api.series({ kind: "account-equity" });
      if (generation !== epoch || stopped) return;
      if (body?.unavailable) {
        if (!cached?.body)
          set({
            series: {
              ...state.series,
              equity: [],
              equityState: "unavailable",
              equityCached: false,
              equityCoverage: null,
              equitySource: null,
              equityNote: body.reason || null,
              equityWatermark: null,
            },
          });
        return;
      }
      set(seriesPatch("account-equity", body, false));
      void evidence.put(key, { body }, { binding, kind: "account-equity" });
    } catch {
      if (generation !== epoch) return;
      if (!cached?.body)
        set({
          series: {
            ...state.series,
            equityState: "error",
            equityCached: false,
          },
        });
    }
  }
  const loadSeries = () => {
    void fetchPriceSeries();
    void fetchEquitySeries();
  };
  const lock = () => {
    epoch++;
    stream?.stop();
    stream = null;
    pending = null;
    dirty = false;
    gap = false;
    chartDeferred = false;
    lastConnection = "";
    api.clear();
    void evidence.clear?.();
    try {
      win.sessionStorage.removeItem("paperToken");
    } catch {
      /* memory only */
    }
    set(initial());
  };
  const api = createApi({
    fetchImpl: options.fetch || win.fetch.bind(win),
    getToken: () => state.token,
    onUnauthorized: lock,
  });
  function observe(
    summary: SummaryResponse | null,
    portfolio: PortfolioResponse | null,
    scanner: ScannerResponse | null,
  ) {
    const prospects = Array.isArray(scanner?.items) ? scanner.items : [];
    const row = prospects.find((r) => r.instrument_key === state.watch);
    const points = appendPoint(state.points, observation(row), gap);
    const record = currentSession(
      { summary, portfolio, scanner, sessions: state.sessions },
      state.meta,
    );
    const equity = finance(record).equity;
    const at = numeric(summary?.as_of_ms) ?? numeric(portfolio?.as_of_ms);
    const valid = at !== null && at > 0 && equity !== null;
    const equityPoints = valid
      ? appendPoint(state.equityPoints, { at, value: equity }, gap)
      : state.equityPoints;
    const equitySeries = valid
      ? appendPoint(
          state.series.equity,
          {
            at,
            value: equity,
            cash: numeric(record?.portfolio?.cash),
            source: "snapshot_checkpoint",
          },
          gap,
        )
      : state.series.equity;
    gap = false;
    return {
      points,
      equityPoints,
      series: { ...state.series, equity: equitySeries },
    };
  }
  function clearProjections(
    patch: Partial<WorkspaceState> = {},
  ): Partial<WorkspaceState> {
    return {
      summary: null,
      portfolio: null,
      fills: null,
      plans: null,
      scanner: null,
      sessions: null,
      approvals: null,
      summaryState: "error",
      portfolioState: "error",
      fillsState: "error",
      plansState: "error",
      scannerState: "error",
      sessionsState: "error",
      approvalsState: "error",
      points: [],
      equityPoints: [],
      series: emptySeries(),
      priceBatchSeq: 0,
      watch: "",
      session: "",
      ...patch,
    };
  }
  async function refresh() {
    if (!state.token || !state.meta || stopped) return;
    if (pending) {
      dirty = true;
      return pending;
    }
    const generation = epoch;
    set({ snapshotState: "loading", error: "" });
    const task = (async () => {
      const [summary, portfolio, fills, plans, scanner, sessions, approvals] =
        await Promise.all([
          settle(api.summary()),
          settle(api.portfolio()),
          settle(api.fills({ limit: 500 })),
          settle(api.plans()),
          settle(api.scanner({ segment: "prospects", limit: 500 })),
          settle(api.sessions({ limit: 200 })),
          settle(api.approvals()),
        ]);
      return { summary, portfolio, fills, plans, scanner, sessions, approvals };
    })();
    pending = task;
    try {
      const result = await task;
      if (generation !== epoch || stopped) return;
      const summaryResult = result.summary;
      if (
        !summaryResult?.ok ||
        !summaryResult.value ||
        summaryResult.value.unavailable === true
      ) {
        const reason =
          summaryResult?.value?.reason ||
          summaryResult?.error ||
          "summary_unavailable";
        set(
          clearProjections({
            summaryState:
              summaryResult?.value?.unavailable === true
                ? "unavailable"
                : "error",
            snapshotState:
              summaryResult?.value?.unavailable === true
                ? "unavailable"
                : "error",
            issues: [{ source: "summary", state: "error", reason }],
            error: "Session summary unavailable. Refresh to retry.",
          }),
        );
        return;
      }
      const summary = summaryResult.value;
      const identity = summaryIdentity(summary);
      if (state.summary && identity !== summaryIdentity(state.summary)) {
        set({
          points: [],
          equityPoints: [],
          series: emptySeries(),
          priceBatchSeq: 0,
          watch: "",
          session: "",
        });
        void evidence.clear();
      }
      const issues: WorkspaceState["issues"] = [];
      const patch: Partial<WorkspaceState> = {};
      for (const name of projectionNames) {
        const entry = result[name];
        const value = entry?.ok ? entry.value : null;
        assignProjection(patch, result, name);
        if (!entry?.ok) {
          patch[`${name}State`] = "error";
          issues.push({
            source: name,
            state: "error",
            reason: entry?.error || "request_failed",
          });
        } else if (value?.unavailable === true) {
          patch[`${name}State`] = "unavailable";
          issues.push({
            source: name,
            state: "unavailable",
            reason: value.reason || "unavailable",
          });
        } else {
          patch[`${name}State`] = "ready";
        }
      }
      const portfolio =
        patch.portfolioState === "ready" ? (patch.portfolio ?? null) : null;
      const scanner =
        patch.scannerState === "ready" ? (patch.scanner ?? null) : null;
      const observed = observe(summary, portfolio, scanner);
      const loaded: Partial<WorkspaceState> = {
        ...patch,
        summary,
        received: Date.now(),
        snapshotState: issues.length ? "unavailable" : "ready",
        issues,
        error: "",
        ...observed,
        // Each selector keeps only its own successful source. `observe` starts
        // from previous tab-local points, so clear explicitly when its source
        // projection failed instead of letting the old array survive.
        points: patch.scannerState === "ready" ? observed.points : [],
        equityPoints:
          patch.portfolioState === "ready" ? observed.equityPoints : [],
      };
      set(loaded);
      const first = Array.isArray(scanner?.items)
        ? scanner.items[0]?.instrument_key
        : null;
      if (!state.watch && first) {
        set({ watch: first });
        set(observe(summary, portfolio, scanner));
        void fetchPriceSeries(first);
      } else if (state.watch && state.series.priceState === "idle") {
        void fetchPriceSeries();
      }
      if (state.series.equityState === "idle") void fetchEquitySeries();
    } catch {
      if (generation === epoch && !stopped)
        set(
          clearProjections({
            snapshotState: "error",
            issues: [
              { source: "summary", state: "error", reason: "read_failed" },
            ],
            error: "Snapshot unavailable. Refresh to retry.",
          }),
        );
    } finally {
      if (pending === task) {
        pending = null;
        if (dirty) {
          dirty = false;
          void refresh();
        }
      }
    }
  }
  function onEvent(event: StreamEvent) {
    if (stopped || !state.token) return;
    if (event.kind === "price_batch") {
      if (win.document.hidden) {
        chartDeferred = true;
        return;
      }
      const session = state.summary?.session;
      if (event.session && session && event.session !== session) {
        chartDeferred = true;
        void refresh();
        return;
      }
      const merged = mergePriceBatch(
        state.series.price,
        event,
        state.watch,
        state.priceBatchSeq,
      );
      const tab = mergePriceBatch(
        state.points,
        event,
        state.watch,
        state.priceBatchSeq,
      );
      set({
        priceBatchSeq: merged.seq,
        points: tab.points,
        series: { ...state.series, price: merged.points },
      });
      if (merged.gap) void loadSeries();
      return;
    }
    if (event.kind === "gap") {
      gap = true;
      void loadSeries();
    }
    set({
      events: [...state.events, event].slice(-256),
      revision: state.revision + 1,
    });
    if (["summary_changed", "gap"].includes(event.kind) && !win.document.hidden)
      void refresh();
  }
  async function authenticate(value: unknown) {
    const generation = ++epoch;
    const candidate = String(value || "").trim();
    if (!candidate) throw Error("Enter a dashboard token.");
    set({ token: candidate, error: "" });
    try {
      const meta = await api.meta();
      if (generation !== epoch || stopped) return;
      set({ meta, connection: "stale" });
      try {
        win.sessionStorage.setItem("paperToken", candidate);
      } catch {
        /* memory only */
      }
      if (options.connect !== false) {
        stream?.stop();
        stream = createEventStream({
          fetchImpl: options.fetch || win.fetch.bind(win),
          getToken: () => state.token,
          onEvent,
          onState: (connection) => {
            const reconnected =
              connection === "live" &&
              !!lastConnection &&
              lastConnection !== "live";
            lastConnection = connection;
            set({ connection });
            if (reconnected) loadSeries();
          },
          onUnauthorized: lock,
        });
        stream.start();
      }
      void refresh();
    } catch (e) {
      if (generation === epoch) lock();
      throw e;
    }
  }
  const visible = () => {
    if (win.document.hidden || !state.meta) return;
    void refresh();
    if (chartDeferred) {
      chartDeferred = false;
      loadSeries();
    }
  };
  win.document.addEventListener("visibilitychange", visible);
  return {
    api,
    getSnapshot: () => state,
    subscribe: (fn: () => void) => {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    authenticate,
    refresh,
    lock,
    loadSeries,
    start: () => {
      if (token) void authenticate(token).catch(() => {});
    },
    watch: (watch: string) => {
      if (watch !== state.watch) {
        set({
          watch,
          points: [],
          priceBatchSeq: 0,
          series: {
            ...state.series,
            price: [],
            priceState: "idle",
            priceCached: false,
            priceCoverage: null,
            priceSource: null,
            priceNote: null,
            priceWatermark: null,
          },
        });
        set(observe(state.summary, state.portfolio, state.scanner));
        if (watch) void fetchPriceSeries(watch);
      }
    },
    selectSession: (session: string) => set({ session }),
    record: () => {
      const rows = sessions(state, state.meta);
      return (
        rows.find((r) => sessionKey(r) === state.session) ||
        currentSession(state, state.meta) ||
        null
      );
    },
    stop: () => {
      stopped = true;
      epoch++;
      stream?.stop();
      api.clear();
      evidence.close?.();
      win.document.removeEventListener("visibilitychange", visible);
      listeners.clear();
    },
  };
}

type ReadResult<T> = { ok: boolean; value?: T; error?: string };
type ProjectionResults = {
  [K in ProjectionName]: ReadResult<NonNullable<ProjectionData[K]>>;
};
async function settle<T>(read: Promise<T>): Promise<ReadResult<T>> {
  try {
    return { ok: true, value: await read };
  } catch (error) {
    return { ok: false, error: errorMessage(error) };
  }
}
function assignProjection<K extends ProjectionName>(
  patch: Partial<ProjectionData>,
  results: ProjectionResults,
  name: K,
) {
  patch[name] = (
    results[name].ok ? results[name].value : null
  ) as ProjectionData[K];
}
export type Controller = ReturnType<typeof createController>;
