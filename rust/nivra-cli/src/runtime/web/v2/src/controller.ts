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
  Fetch,
  Revisions,
  RevisionName,
} from "./types";
import { errorMessage, PROJECTION_NAMES } from "./types";
import type { BrowserWindow, EvidenceCache } from "./cache";
export interface ControllerOptions {
  fetch?: Fetch;
  cache?: EvidenceCache;
  connect?: boolean;
}
import { createApi } from "./api";
import { createEvidenceCache } from "./cache";
import { createEventStream } from "./events";
import { stalledController } from "./model";
import { controllerLivenessFrom } from "./operational";
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

/**
 * With no `summary_changed` for three of the server's 5 s summary refreshes
 * (`REFRESH_MS` in web/v1/summary.rs), reread summary and health directly.
 */
export const WATCHDOG_MS = 15_000;

type SeriesRead =
  | { kind: "receipt-minute-bar"; instrument: string }
  | { kind: "account-equity" };

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
    agent: null,
    agentState: "idle",
    quotes: {},
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
    stalled: null,
  });
  let state: WorkspaceState = { ...initial(), token };
  let epoch = 0;
  let stream: ReturnType<typeof createEventStream> | null = null;
  let pending: Promise<ProjectionResults> | null = null;
  let agentPending: Promise<void> | null = null;
  const seriesPending = {
    price: null as string | null,
    equity: null as string | null,
  };
  // Revision of each projection as last fetched, per the summary's `revisions`.
  let fetched: Revisions = {};
  let dirty: Pace | null = null,
    agentDirty = false,
    stopped = false,
    gap = false,
    chartDeferred = false,
    lastConnection = "";
  // When the last `summary_changed` (or watchdog check) happened, client clock.
  let announcedAt = 0;
  let watchdog: ReturnType<typeof setInterval> | null = null;
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
  const clearSeries = (
    prefix: "price" | "equity",
    loadState: "idle" | "unavailable",
    reason: string | null = null,
  ): ChartSeries => ({
    ...state.series,
    [prefix]: [],
    [`${prefix}State`]: loadState,
    [`${prefix}Cached`]: false,
    [`${prefix}Coverage`]: null,
    [`${prefix}Source`]: null,
    [`${prefix}Note`]: reason,
    [`${prefix}Watermark`]: null,
  });
  async function fetchSeries(query: SeriesRead) {
    if (!state.token || !state.meta || !state.summary || stopped) return;
    const { kind } = query;
    const instrument = kind === "receipt-minute-bar" ? query.instrument : null;
    if (instrument === "") return;
    const prefix = instrument === null ? "equity" : "price";
    const generation = epoch;
    const binding = summaryIdentity(state.summary);
    const pendingKey = `${generation}:${binding}:${instrument ?? ""}`;
    if (seriesPending[prefix] === pendingKey) return;
    seriesPending[prefix] = pendingKey;
    const current = () =>
      generation === epoch &&
      !stopped &&
      binding === summaryIdentity(state.summary) &&
      (instrument === null || instrument === state.watch);
    const key = `${binding}|${kind}|${instrument ?? ""}|latest`;
    try {
      const cached = await evidence.get(key);
      if (!current()) return;
      if (cached?.body) set(seriesPatch(kind, cached.body, true));
      try {
        const body = await api.series(query);
        if (!current()) return;
        if (body?.unavailable) {
          if (!cached?.body)
            set({
              series: clearSeries(prefix, "unavailable", body.reason || null),
            });
          return;
        }
        set(seriesPatch(kind, body, false));
        void evidence.put(key, { body }, { binding, ...query });
      } catch {
        if (current() && !cached?.body)
          set({
            series: {
              ...state.series,
              [`${prefix}State`]: "error",
              [`${prefix}Cached`]: false,
            },
          });
      }
    } finally {
      if (seriesPending[prefix] === pendingKey) seriesPending[prefix] = null;
    }
  }
  const loadSeries = () => {
    void fetchSeries({ kind: "receipt-minute-bar", instrument: state.watch });
    void fetchSeries({ kind: "account-equity" });
  };
  const lock = () => {
    epoch++;
    stopWatchdog();
    stream?.stop();
    stream = null;
    pending = null;
    agentPending = null;
    fetched = {};
    dirty = null;
    agentDirty = false;
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
      summaryState: "error",
      portfolioState: "error",
      fillsState: "error",
      plansState: "error",
      scannerState: "error",
      sessionsState: "error",
      agent: null,
      agentState: "idle",
      quotes: {},
      points: [],
      equityPoints: [],
      series: emptySeries(),
      priceBatchSeq: 0,
      watch: "",
      session: "",
      ...patch,
    };
  }
  const PARALLEL = [
    "portfolio",
    "fills",
    "plans",
    "scanner",
    "sessions",
  ] as const;
  type Read = (typeof PARALLEL)[number];
  const reads: { [K in Read]: () => Promise<ProjectionData[K]> } = {
    portfolio: () => api.portfolio(),
    fills: () => api.fills({ limit: 500 }),
    plans: () => api.plans(),
    scanner: () => api.scanner({ segment: "prospects", limit: 500 }),
    sessions: () => api.sessions({ limit: 200 }),
  };
  /**
   * "all": every projection (Refresh, gap, visibility, older backends).
   * "changed": only projections whose summary revision differs from the one
   * last fetched; every projection when the summary carries no revisions.
   */
  async function reload(pace: Pace) {
    if (!state.token || !state.meta || stopped) return;
    if (pending) {
      dirty = dirty === "all" || pace === "all" ? "all" : "changed";
      return pending;
    }
    const generation = epoch;
    set({ snapshotState: "loading", error: "" });
    const task = (async () => {
      const summary = settle(api.summary());
      let wanted: readonly Read[] = PARALLEL;
      const started = {} as {
        [K in Read]?: Promise<ReadResult<ProjectionData[K]>>;
      };
      const start = <K extends Read>(name: K) => {
        started[name] = settle(reads[name]()) as never;
      };
      if (pace === "all") PARALLEL.forEach(start);
      else {
        const first = await summary;
        const revisions = first.ok ? first.value.revisions : undefined;
        wanted = PARALLEL.filter(
          (name) =>
            !revisions ||
            revisions[name] === undefined ||
            revisions[name] !== fetched[name] ||
            state[`${name}State`] !== "ready",
        );
        wanted.forEach(start);
      }
      const done = await Promise.all(wanted.map((name) => started[name]));
      const result: ProjectionResults = { summary: await summary };
      wanted.forEach((name, i) => Object.assign(result, { [name]: done[i] }));
      return result;
    })();
    pending = task;
    try {
      const result = await task;
      if (generation !== epoch || stopped) return;
      const summaryResult = result.summary;
      const summary = summaryResult.ok ? summaryResult.value : null;
      if (!summary || summary.unavailable === true) {
        const reason =
          summary?.reason ||
          (!summaryResult.ok && summaryResult.error) ||
          "summary_unavailable";
        const availability =
          summary?.unavailable === true ? "unavailable" : "error";
        set(
          clearProjections({
            summaryState: availability,
            snapshotState: availability,
            issues: [{ source: "summary", state: "error", reason }],
            error: "Session summary unavailable. Refresh to retry.",
          }),
        );
        return;
      }
      const identity = summaryIdentity(summary);
      if (state.summary && identity !== summaryIdentity(state.summary)) {
        fetched = {};
        set({
          points: [],
          equityPoints: [],
          series: emptySeries(),
          priceBatchSeq: 0,
          watch: "",
          session: "",
          quotes: {},
        });
        void evidence.clear();
      }
      const attempted = new Set<string>(Object.keys(result));
      const issues: WorkspaceState["issues"] = state.issues.filter(
        (issue) => !attempted.has(issue.source),
      );
      const patch: Partial<WorkspaceState> = {};
      for (const name of PROJECTION_NAMES) {
        const entry = result[name];
        if (!entry) continue;
        const value = entry.ok ? entry.value : null;
        Object.assign(patch, { [name]: value });
        if (!entry.ok) {
          patch[`${name}State`] = "error";
          issues.push({
            source: name,
            state: "error",
            reason: entry.error || "request_failed",
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
          // Only a successful read advances the fetched revision, so a failed
          // projection is retried on the next change.
          const revision = summary.revisions?.[name as RevisionName];
          if (name !== "summary" && revision !== undefined)
            fetched = { ...fetched, [name]: revision };
        }
      }
      const current = { ...state, ...patch };
      const portfolio =
        current.portfolioState === "ready" ? current.portfolio : null;
      const scanner = current.scannerState === "ready" ? current.scanner : null;
      const observed = observe(summary, portfolio, scanner);
      // A controller in its startup phase has not published the v1 view yet;
      // that is a loading state, not unavailable evidence (#20).
      const starting =
        summary.status === "starting" || summary.state?.phase === "starting";
      const loaded: Partial<WorkspaceState> = {
        ...patch,
        summary,
        received: Date.now(),
        snapshotState: starting
          ? "loading"
          : issues.length
            ? "unavailable"
            : "ready",
        issues,
        error: "",
        ...observed,
        // Each selector keeps only its own successful source. `observe` starts
        // from previous tab-local points, so clear explicitly when its source
        // projection failed instead of letting the old array survive.
        points: current.scannerState === "ready" ? observed.points : [],
        equityPoints:
          current.portfolioState === "ready" ? observed.equityPoints : [],
      };
      set(loaded);
      if (
        pace === "all" ||
        !summary.revisions ||
        summary.revisions.agent === undefined ||
        summary.revisions.agent !== fetched.agent
      )
        void loadAgent(summary.revisions?.agent);
      const first = Array.isArray(scanner?.items)
        ? scanner.items[0]?.instrument_key
        : null;
      if (!state.watch && first) {
        set({ watch: first });
        set(observe(summary, portfolio, scanner));
        void fetchSeries({ kind: "receipt-minute-bar", instrument: first });
      } else if (state.watch && state.series.priceState !== "ready") {
        void fetchSeries({
          kind: "receipt-minute-bar",
          instrument: state.watch,
        });
      }
      if (state.series.equityState !== "ready")
        void fetchSeries({ kind: "account-equity" });
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
          const next = dirty;
          dirty = null;
          void reload(next);
        }
      }
    }
  }
  const refresh = () => reload("all");
  /** The pi lane journal. Display evidence; reloaded on `revisions.agent`, `run_ended` and visibility. */
  async function loadAgent(revision?: string) {
    if (!state.token || !state.meta || stopped) return;
    if (agentPending) {
      agentDirty = true;
      return agentPending;
    }
    const generation = epoch;
    if (!state.agent) set({ agentState: "loading" });
    const task: Promise<void> = (async () => {
      try {
        const body = await api.agent({ limit: 200 });
        if (generation !== epoch || stopped) return;
        set({
          agent: body?.unavailable ? null : body,
          agentState: body?.unavailable ? "unavailable" : "ready",
        });
        if (revision !== undefined && !body?.unavailable)
          fetched = { ...fetched, agent: revision };
      } catch {
        if (generation === epoch && !stopped) set({ agentState: "error" });
      }
    })().finally(() => {
      if (agentPending !== task) return;
      agentPending = null;
      if (agentDirty) {
        agentDirty = false;
        void loadAgent(revision);
      }
    });
    agentPending = task;
    return task;
  }
  /**
   * The summary announcements come from the controller loop; SSE heartbeats
   * and price batches do not. When announcements go quiet, read the summary
   * and the server's liveness verdict directly instead of trusting the last one.
   */
  async function checkLiveness() {
    const generation = epoch,
      since = announcedAt;
    const [health] = await Promise.all([
      settle(api.health()),
      reload("changed"),
    ]);
    if (generation !== epoch || stopped || announcedAt !== since) return;
    const liveness = health.ok ? controllerLivenessFrom(health.value) : null;
    set({ stalled: stalledController(state.summary, liveness) });
  }
  function startWatchdog() {
    stopWatchdog();
    announcedAt = Date.now();
    watchdog = setInterval(() => {
      if (stopped || !state.meta || win.document.hidden) return;
      if (Date.now() - announcedAt < WATCHDOG_MS) return;
      announcedAt = Date.now();
      void checkLiveness();
    }, WATCHDOG_MS / 3);
  }
  function stopWatchdog() {
    if (watchdog !== null) clearInterval(watchdog);
    watchdog = null;
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
        void reload("all");
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
        quotes: mergeQuotes(state.quotes, event),
      });
      if (merged.gap) void loadSeries();
      return;
    }
    if (event.kind === "gap") {
      gap = true;
      void loadSeries();
    }
    if (event.kind === "summary_changed") {
      announcedAt = Date.now();
      if (state.stalled) set({ stalled: null });
    }
    set({
      events: [...state.events, event].slice(-256),
      revision: state.revision + 1,
    });
    if (win.document.hidden) return;
    if (event.kind === "gap") void reload("all");
    else if (event.kind === "summary_changed") {
      // A payload that names the revisions we already hold needs no reads.
      if (event.revisions && sameRevisions(event.revisions, fetched)) return;
      void reload("changed");
    } else if (event.kind === "run_ended") void loadAgent();
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
        startWatchdog();
      }
      void refresh();
    } catch (e) {
      if (generation === epoch) lock();
      throw e;
    }
  }
  const visible = () => {
    if (win.document.hidden || !state.meta) return;
    void reload("all");
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
    loadAgent: () => loadAgent(),
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
          series: clearSeries("price", "idle"),
        });
        set(observe(state.summary, state.portfolio, state.scanner));
        if (watch)
          void fetchSeries({ kind: "receipt-minute-bar", instrument: watch });
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
      stopWatchdog();
      stream?.stop();
      api.clear();
      evidence.close?.();
      win.document.removeEventListener("visibilitychange", visible);
      listeners.clear();
    },
  };
}

type Pace = "all" | "changed";
type ReadResult<T> = { ok: true; value: T } | { ok: false; error: string };
type ProjectionResults = {
  summary: ReadResult<NonNullable<ProjectionData["summary"]>>;
} & {
  [K in Exclude<ProjectionName, "summary">]?: ReadResult<
    NonNullable<ProjectionData[K]>
  >;
};
function sameRevisions(event: Revisions, held: Revisions) {
  const names = Object.keys(event) as RevisionName[];
  return names.length > 0 && names.every((name) => event[name] === held[name]);
}
/** Latest receipt price per instrument from a (possibly partial) price batch. */
function mergeQuotes(quotes: WorkspaceState["quotes"], event: StreamEvent) {
  // A full frame is the whole snapshot; a partial one carries only moved rows.
  const next = event.full === true ? {} : { ...quotes };
  for (const item of event.items || []) {
    const key = item?.instrument_key,
      at = numeric(item?.receipt_ms),
      price = numeric(item?.last_price);
    if (!key || at === null || price === null || price <= 0) continue;
    if ((next[key]?.at ?? 0) <= at) next[key] = { at, price };
  }
  return next;
}
async function settle<T>(read: Promise<T>): Promise<ReadResult<T>> {
  try {
    return { ok: true, value: await read };
  } catch (error) {
    return { ok: false, error: errorMessage(error) };
  }
}
export type Controller = ReturnType<typeof createController>;
