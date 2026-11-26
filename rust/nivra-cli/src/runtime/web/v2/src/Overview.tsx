import type { Fill, Point, Position, SeriesCoverage } from "./types";
import type { Column } from "./ui";
import React, { useEffect, useMemo, useState } from "react";
import { ArrowUpRight } from "lucide-react";
import { ApiError } from "./api";
import Chart from "./Chart";
import { BookContinuity } from "./Operational";
import LaneTimeline, { laneColor, sessionRange } from "./charts/LaneTimeline";
import Sparkline, { bucketize } from "./charts/Sparkline";
import {
  amount,
  clock,
  count,
  currentSession,
  datetime,
  duration,
  finance,
  fillsFor,
  istDateString,
  istDayEndMs,
  istDayStartMs,
  numeric,
  plansFor,
  projectSeries,
  sessionKey,
  shiftIstDate,
  usd,
} from "./data";
import {
  conditionOf,
  equityStats,
  instrumentIndex,
  instrumentLabel,
  journalCoverage,
  laneStats,
  modeOf,
  orderTracks,
} from "./model";
import {
  Button,
  Empty,
  InstrumentCell,
  KpiStrip,
  LaneSwatch,
  NAV,
  Panel,
  Segments,
  Status,
  Steps,
  Table,
  useWorkspace,
} from "./ui";
import type { KpiItem } from "./ui";
import {
  ARCHIVE_LABELS,
  ARCHIVE_RANGE_LABELS,
  plain,
  sentenceCase,
  turnOutcome,
} from "./labels";
import { useRead, useRevision } from "./reads";

const signed = (v: number | null) =>
  v === null ? "—" : (v > 0 ? "+" : "") + amount(v);
const tone = (v: number | null) =>
  v === null || v === 0
    ? undefined
    : v > 0
      ? ("pos" as const)
      : ("neg" as const);
const KINDS: [string, string][] = [
  ["all", "All"],
  ["cash", "Cash"],
  ["future", "Future"],
  ["option", "Option"],
];
const kindGroup = (p: Position) => {
  const k = instrumentLabel(p.instrument).kind;
  return k === "call" || k === "put"
    ? "option"
    : k === "future"
      ? "future"
      : "cash";
};

const RESEARCH_DESTINATIONS = [
  "backtest",
  "strategies",
  "sessions",
  "instruments",
  "health",
];
/** Research-only dashboards have no book: show what can be inspected instead. */
function ResearchOverview() {
  return (
    <Panel title="Available in research mode" note="Works without a paper book">
      <div className="destination-grid">
        {RESEARCH_DESTINATIONS.flatMap((id) => {
          const item = NAV.find((n) => n.id === id);
          if (!item) return [];
          const Icon = item.icon;
          return [
            <a key={id} href={"#/" + id} className="destination">
              <span className="destination-icon" aria-hidden="true">
                <Icon size={16} />
              </span>
              <strong>
                {item.label} <ArrowUpRight size={12} />
              </strong>
              <small>{item.blurb}</small>
            </a>,
          ];
        })}
      </div>
    </Panel>
  );
}

export default function Overview() {
  const [state] = useWorkspace();
  return conditionOf(state).kind === "research" ? (
    <ResearchOverview />
  ) : (
    <BookOverview />
  );
}

type HistoryPreset = "today" | "5d" | "7d" | "30d" | "90d" | "custom";
type ArchiveKind = "archive-minute-bar" | "archive-daily-bar";
interface ArchiveSeriesState {
  status: "idle" | "loading" | "ready" | "unavailable" | "error";
  points: Point[];
  coverage: SeriesCoverage | null;
  reason: string | null;
  kind: ArchiveKind;
}
const emptyArchive = (
  kind: ArchiveKind = "archive-minute-bar",
): ArchiveSeriesState => ({
  status: "idle",
  points: [],
  coverage: null,
  reason: null,
  kind,
});

function BookOverview() {
  const [state, app] = useWorkspace(),
    [view, setView] = useState("equity"),
    [kind, setKind] = useState("all"),
    [laneFilter, setLaneFilter] = useState("all"),
    [historyPreset, setHistoryPreset] = useState<HistoryPreset>("today"),
    [draftFrom, setDraftFrom] = useState(""),
    [draftTo, setDraftTo] = useState(""),
    [draftKind, setDraftKind] = useState<ArchiveKind>("archive-minute-bar"),
    [customRequest, setCustomRequest] = useState<{
      fromDate: string;
      toDate: string;
      kind: ArchiveKind;
      seq: number;
    } | null>(null),
    [archive, setArchive] = useState<ArchiveSeriesState>(() => emptyArchive());
  const r = app.record(),
    f = finance(r),
    s = state.summary,
    mode = modeOf(state.meta, s);
  const health = useRead((api) => api.health(), useRevision("health"));
  const prospects = Array.isArray(state.scanner?.items)
    ? state.scanner.items
    : [];
  useEffect(() => {
    if (!state.watch && prospects[0]?.instrument_key)
      app.watch(prospects[0].instrument_key);
  }, [prospects, state.watch]);
  const current = currentSession(state, state.meta),
    historical = sessionKey(r) !== sessionKey(current);
  const asOf = numeric(s?.as_of_ms);
  const todayIst =
    s?.session || current?.id?.date || istDateString(asOf ?? Date.now());
  const archiveRequest = useMemo(() => {
    if (view === "equity" || historyPreset === "today" || !state.watch)
      return null;
    if (historyPreset === "custom") {
      if (!customRequest) return null;
      return {
        instrument: state.watch,
        kind: customRequest.kind,
        fromDate: customRequest.fromDate,
        toDate: customRequest.toDate,
        seq: customRequest.seq,
      };
    }
    const lookbackDays =
      historyPreset === "5d"
        ? 4
        : historyPreset === "7d"
          ? 6
          : historyPreset === "30d"
            ? 29
            : 89;
    const reqKind: ArchiveKind =
      historyPreset === "5d" || historyPreset === "7d"
        ? "archive-minute-bar"
        : "archive-daily-bar";
    return {
      instrument: state.watch,
      kind: reqKind,
      fromDate: shiftIstDate(todayIst, -lookbackDays),
      toDate: todayIst,
      seq: 0,
    };
  }, [view, historyPreset, state.watch, todayIst, customRequest]);
  useEffect(() => {
    if (!archiveRequest) {
      setArchive(emptyArchive());
      return;
    }
    const from = istDayStartMs(archiveRequest.fromDate);
    const to = istDayEndMs(archiveRequest.toDate);
    if (from === null || to === null || to < from) {
      setArchive({
        status: "error",
        points: [],
        coverage: null,
        reason: "series_range_invalid",
        kind: archiveRequest.kind,
      });
      return;
    }
    let cancelled = false;
    setArchive({
      status: "loading",
      points: [],
      coverage: null,
      reason: null,
      kind: archiveRequest.kind,
    });
    (async () => {
      try {
        const collected: Point[] = [];
        let cursorTo = to;
        let lastCoverage: SeriesCoverage | null = null;
        for (let page = 0; page < 5; page++) {
          const body = await app.api.series({
            kind: archiveRequest.kind,
            instrument: archiveRequest.instrument,
            from,
            to: cursorTo,
          });
          if (cancelled) return;
          if (body?.unavailable) {
            setArchive({
              status: "unavailable",
              points: [],
              coverage: null,
              reason: body.reason || "archive_unavailable",
              kind: archiveRequest.kind,
            });
            return;
          }
          const projected = projectSeries(body, archiveRequest.kind);
          if (page === 0) lastCoverage = projected.coverage;
          collected.push(...projected.points);
          if (!body?.truncated || body.next == null || body.next < from) break;
          cursorTo = body.next;
        }
        collected.sort((a, b) => a.at - b.at);
        const deduped: Point[] = [];
        for (const p of collected) {
          const prev = deduped.at(-1);
          if (prev && prev.at === p.at) deduped[deduped.length - 1] = p;
          else deduped.push(p);
        }
        if (!cancelled)
          setArchive({
            status: "ready",
            points: deduped,
            coverage: lastCoverage,
            reason: null,
            kind: archiveRequest.kind,
          });
      } catch (e) {
        if (!cancelled)
          setArchive({
            status: "error",
            points: [],
            coverage: null,
            reason: e instanceof ApiError ? e.code : "archive_unavailable",
            kind: archiveRequest.kind,
          });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [archiveRequest, app]);
  const selectHistoryPreset = (next: HistoryPreset) => {
    setHistoryPreset(next);
    if (next === "custom") {
      const fromDate = draftFrom || shiftIstDate(todayIst, -4);
      const toDate = draftTo || todayIst;
      setDraftFrom(fromDate);
      setDraftTo(toDate);
      setCustomRequest((prev) => ({
        fromDate,
        toDate,
        kind: draftKind,
        seq: (prev?.seq ?? 0) + 1,
      }));
    }
  };
  const applyCustomRange = () => {
    const fromDate = draftFrom || shiftIstDate(todayIst, -4);
    const toDate = draftTo || todayIst;
    setCustomRequest((prev) => ({
      fromDate,
      toDate,
      kind: draftKind,
      seq: (prev?.seq ?? 0) + 1,
    }));
  };
  const series = state.series || {};
  const equitySeries = series.equity?.length
    ? series.equity
    : state.equityPoints;
  const priceSeries = series.price?.length ? series.price : state.points;
  const fills = fillsFor(state, r),
    plans = plansFor(state, r);
  const stats = useMemo(
    () => equityStats(historical ? [] : equitySeries),
    [equitySeries, historical],
  );
  const turns = state.agent?.turns ?? [],
    coverage = journalCoverage(state.agent);
  const laneRows = useMemo(
    () => laneStats(turns, state.agent?.usage),
    [turns, state.agent],
  );
  const spend = Object.values(state.agent?.usage?.lanes ?? {}).reduce<
    number | null
  >(
    (sum, u) =>
      numeric(u.cost_usd) === null ? sum : (sum ?? 0) + Number(u.cost_usd),
    null,
  );
  const range = useMemo(
    () => sessionRange(s?.session, turns),
    [s?.session, turns],
  );
  const index = useMemo(
    () =>
      instrumentIndex({ positions: f.positions, fills, scanner: prospects }),
    [f.positions, fills, prospects],
  );
  const tracks = useMemo(
    () => orderTracks(plans, fills, mode),
    [plans, fills, mode],
  );
  const rejected = (Array.isArray(plans?.actions) ? plans.actions : []).filter(
    (a) => a.reason && !/^accepted$/i.test(a.reason),
  ).length;
  const attribution = health.value?.activity_attribution?.financials;
  const kpis: KpiItem[] = [
    [
      "Equity",
      amount(f.equity),
      f.equity === null ? "No marked book recorded" : "cash + marks",
    ],
    [
      "Day P&L net",
      signed(stats.dayNet),
      stats.dayNet === null
        ? "Needs two checkpoints"
        : `since ${clock(stats.first!.at)}, after fees`,
      { tone: tone(stats.dayNet) },
    ],
    [
      "Realized",
      attribution?.realized_pnl == null
        ? "—"
        : signed(numeric(attribution.realized_pnl)),
      attribution?.realized_pnl == null ? "Not loaded" : "recorded",
      { tone: tone(numeric(attribution?.realized_pnl)) },
    ],
    [
      "Unrealized",
      signed(f.unrealized),
      f.unrealized === null ? "A position has no mark" : "vs entry",
      { tone: tone(f.unrealized) },
    ],
    ["Fees", amount(f.fees), f.fees === null ? "Not recorded" : "recorded"],
    ["Cash", amount(f.cash), "recorded"],
    [
      "Gross exposure",
      amount(f.gross),
      f.gross !== null && f.equity
        ? `${(f.gross / f.equity).toFixed(2)}× equity`
        : "Needs every mark",
    ],
    [
      "Max drawdown",
      stats.maxDrawdownPct === null
        ? "—"
        : `${stats.maxDrawdownPct.toFixed(2)}%`,
      stats.maxDrawdownPct === null
        ? "Needs two checkpoints"
        : `${stats.count} pts${stats.troughAt ? ` · ${clock(stats.troughAt)}` : ""}`,
      { tone: stats.maxDrawdownPct ? "neg" : undefined },
    ],
    [
      "Fills",
      fills ? count(fills.length) : "—",
      fills ? `${count(rejected)} rejected` : "Fill evidence unavailable",
    ],
    [
      "Model spend",
      usd(spend),
      spend === null
        ? "Usage not reported"
        : (coverage.label ?? `${count(turns.length)} turns`),
    ],
  ];
  const selected = prospects.find((v) => v.instrument_key === state.watch);
  const showEquity = view === "equity";
  const showArchive = !showEquity && historyPreset !== "today";
  const points = showArchive
    ? archive.points
    : historical
      ? []
      : showEquity
        ? equitySeries
        : priceSeries;
  const active = showEquity ? series.equity : series.price;
  const loadState = showArchive
    ? archive.status
    : showEquity
      ? series.equityState
      : series.priceState;
  const cached =
    !showArchive &&
    !!active?.length &&
    (showEquity ? series.equityCached : series.priceCached);
  const markers = (fills ?? []).flatMap((fill: Fill) => {
    const at = numeric(fill.at);
    return at === null || (!showEquity && fill.instrument?.key !== state.watch)
      ? []
      : [{ at, side: fill.buy ? ("buy" as const) : ("sell" as const) }];
  });
  const positions = f.positions.filter(
    (p) => kind === "all" || kindGroup(p) === kind,
  );
  const greeks = f.positions.some((p) => numeric(p.greeks?.delta) !== null);
  const columns: Column<Position>[] = [
    {
      key: "instrument",
      label: "Instrument",
      render: (p) => <InstrumentCell record={p.instrument} />,
    },
    { key: "units", label: "Qty", num: true, render: (p) => count(p.units) },
    {
      key: "entry",
      label: "Avg price",
      num: true,
      render: (p) => amount(p.entry),
    },
    { key: "mark", label: "Mark", num: true, render: (p) => amount(p.mark) },
    {
      key: "pnl",
      label: "P&L",
      num: true,
      render: (p) => {
        const v = [p.mark, p.entry, p.units].every((x) => numeric(x) !== null)
          ? (Number(p.mark) - Number(p.entry)) * Number(p.units)
          : null;
        return <span className={tone(v) ?? ""}>{signed(v)}</span>;
      },
    },
    ...(greeks
      ? [
          {
            key: "delta",
            label: "Δ",
            num: true,
            render: (p: Position) => count(p.greeks?.delta),
          },
        ]
      : []),
    {
      key: "age",
      label: "Mark age",
      num: true,
      render: (p) => {
        const at =
          numeric(p.marked_at) ?? (Date.parse(String(p.marked_at)) || null);
        return at !== null && asOf !== null && asOf >= at
          ? duration(asOf - at)
          : "—";
      },
    },
    {
      key: "expiry",
      label: "Expiry",
      render: (p) => instrumentLabel(p.instrument).expiry ?? "—",
    },
    {
      key: "margin",
      label: "Margin",
      num: true,
      render: (p) =>
        p.margin?.display_only === true &&
        p.margin.instrument_key === p.instrument?.key
          ? amount(p.margin.total_margin)
          : "—",
    },
  ];
  const shown = tracks.slice(0, 8);
  return (
    <>
      <BookContinuity summary={s} portfolio={state.portfolio} />
      <KpiStrip items={kpis} />
      <div className="grid-12">
        <Panel
          className="span-8"
          title={showEquity ? "Account equity" : "Instrument price"}
          note={
            showArchive
              ? ARCHIVE_RANGE_LABELS[historyPreset]
              : historical
                ? "Archived session"
                : showEquity
                  ? "Fills marked, drawdown below"
                  : "Last received price"
          }
          actions={
            <>
              {!showEquity && (
                <>
                  <select
                    aria-label="Chart instrument"
                    value={state.watch}
                    onChange={(e) => app.watch(e.target.value)}
                  >
                    {!prospects.length && (
                      <option value="">No instruments</option>
                    )}
                    {prospects.map((p) => (
                      <option key={p.instrument_key} value={p.instrument_key}>
                        {
                          instrumentLabel({ ...p.instrument, symbol: p.symbol })
                            .primary
                        }
                      </option>
                    ))}
                  </select>
                  <select
                    aria-label="History range"
                    value={historyPreset}
                    onChange={(e) =>
                      selectHistoryPreset(e.target.value as HistoryPreset)
                    }
                  >
                    {Object.entries(ARCHIVE_RANGE_LABELS).map(([id, text]) => (
                      <option key={id} value={id}>
                        {text}
                      </option>
                    ))}
                  </select>
                </>
              )}
              <Segments
                value={view}
                onChange={setView}
                items={[
                  ["equity", "Equity"],
                  ["price", "Instrument price"],
                ]}
                label="Chart series"
              />
            </>
          }
        >
          {!showEquity && historyPreset === "custom" && (
            <div className="filter-bar">
              <div className="filter-fields">
                <label>
                  From{" "}
                  <input
                    type="date"
                    aria-label="History start date"
                    value={draftFrom}
                    onChange={(e) => setDraftFrom(e.target.value)}
                  />
                </label>
                <label>
                  To{" "}
                  <input
                    type="date"
                    aria-label="History end date"
                    value={draftTo}
                    onChange={(e) => setDraftTo(e.target.value)}
                  />
                </label>
                <label>
                  Interval{" "}
                  <select
                    aria-label="History interval"
                    value={draftKind}
                    onChange={(e) =>
                      setDraftKind(e.target.value as ArchiveKind)
                    }
                  >
                    <option value="archive-minute-bar">1 min</option>
                    <option value="archive-daily-bar">Daily</option>
                  </select>
                </label>
                <Button type="button" onClick={applyCustomRange}>
                  Load
                </Button>
              </div>
            </div>
          )}
          <Chart
            points={points}
            coverage={
              showArchive
                ? archive.coverage
                : showEquity
                  ? series.equityCoverage
                  : series.priceCoverage
            }
            label={
              showEquity
                ? "Account equity"
                : selected?.symbol || "Selected instrument"
            }
            baseline={showEquity ? (stats.first?.value ?? null) : null}
            drawdown={showEquity}
            markers={showArchive ? [] : markers}
            multiDay={showArchive}
            daily={showArchive && archive.kind === "archive-daily-bar"}
            source={
              showArchive
                ? plain("market_archive", ARCHIVE_LABELS)
                : historical
                  ? "Archive has no chart history"
                  : cached
                    ? "Cached, may be stale"
                    : active?.length
                      ? showEquity
                        ? series.equitySource
                          ? "Recorded checkpoints"
                          : "Seen in this tab"
                        : series.priceSource
                          ? "Minute bars from received quotes"
                          : "Live quotes, this tab"
                      : "Seen in this tab"
            }
            height={showEquity ? 300 : 280}
            emptyTone={
              showArchive
                ? archive.status === "unavailable"
                  ? "unavailable"
                  : archive.status === "error"
                    ? "failed"
                    : "empty"
                : undefined
            }
            emptyTitle={
              showArchive
                ? archive.status === "unavailable"
                  ? "Archive unavailable"
                  : archive.status === "error"
                    ? "Could not load archive history"
                    : "No archive history"
                : showEquity
                  ? "No equity history"
                  : "No price history"
            }
            emptyDetail={
              showArchive
                ? loadState === "loading" || loadState === "idle"
                  ? "Loading…"
                  : archive.status === "unavailable" || archive.status === "error"
                    ? plain(archive.reason, ARCHIVE_LABELS)
                    : "No archived bars for this range."
                : historical
                  ? "This archive has one checkpoint only."
                  : loadState === "loading" || loadState === "idle"
                    ? "Loading…"
                    : showEquity
                      ? "No equity checkpoints recorded yet."
                      : "No minute bars yet. Missing minutes stay blank."
            }
          />
        </Panel>
        <Panel
          className="span-4"
          title="pi lanes"
          note="Today"
          actions={
            <a href="#/lanes" className="text-link">
              Lanes <ArrowUpRight size={12} />
            </a>
          }
        >
          <div className="lane-rows">
            {laneRows.map((l) => (
              <div className="lane-row" key={l.lane}>
                <div className="top">
                  <LaneSwatch lane={l.lane} />
                  <span className="num muted">
                    {count(l.turns)} turns · {usd(l.cost_usd)}
                  </span>
                </div>
                <div className="sub">
                  <span>Last</span>
                  <span title={l.last ? turnOutcome(l.last) : ""}>
                    {l.last ? turnOutcome(l.last) : "—"}
                  </span>
                </div>
                <div className="sub">
                  <span>Reply time · fallbacks</span>
                  <span>
                    {duration(l.p50_ms)} · {count(l.fallbacks)}
                  </span>
                </div>
                <Sparkline
                  color={laneColor(l.lane)}
                  label={`${sentenceCase(l.lane)} activity`}
                  values={bucketize(
                    turns
                      .filter((t) => t.lane === l.lane)
                      .map((t) => t.at_ms ?? NaN),
                    range[0],
                    range[1],
                  )}
                />
              </div>
            ))}
          </div>
        </Panel>
        <Panel
          className="span-12"
          title="Lane timeline"
          note="One mark per turn · taller = slower · hollow = failed · ◆ gate escalated"
          actions={
            <Segments
              value={laneFilter}
              onChange={setLaneFilter}
              label="Lane filter"
              items={[
                ["all", "All"],
                ...laneRows.map((l) => [l.lane, sentenceCase(l.lane)]),
              ]}
            />
          }
        >
          {turns.length ? (
            <div className="panel-body">
              <LaneTimeline
                turns={turns}
                lanes={laneRows
                  .map((l) => l.lane)
                  .filter((l) => laneFilter === "all" || l === laneFilter)}
                range={range}
                now={asOf}
                rowHeight={laneFilter === "all" ? 30 : 44}
              />
            </div>
          ) : (
            <Empty title="No turns recorded">
              {state.agentState === "error"
                ? "Lane history could not be read."
                : undefined}
            </Empty>
          )}
        </Panel>
        <Panel
          className="span-7"
          title="Positions"
          note="Cash and F&O"
          actions={
            <Segments
              value={kind}
              onChange={setKind}
              items={KINDS}
              label="Position kind"
            />
          }
        >
          <Table
            rows={positions}
            columns={columns}
            getKey={(p) => p.instrument?.key}
            empty={
              f.positionsKnown
                ? "The recorded book is flat"
                : "Position ledger unavailable"
            }
          />
        </Panel>
        <Panel
          className="span-5"
          title="Order lifecycle"
          note="intent → validated → submitted → ACK → fill"
        >
          <Table
            rows={shown}
            getKey={(t) =>
              `${t.decisionId}:${t.instrument}:${t.at_ms}:${t.side}`
            }
            empty="No plan actions or fills recorded"
            columns={[
              {
                key: "at",
                label: "Time",
                num: true,
                render: (t) => (t.at_ms ? clock(t.at_ms) : "—"),
              },
              {
                key: "instrument",
                label: "Instrument",
                render: (t) => (
                  <InstrumentCell
                    record={t.instrument ? index.get(t.instrument) : undefined}
                    fallback={t.instrument}
                  />
                ),
              },
              {
                key: "side",
                label: "Side",
                render: (t) => (
                  <span
                    className={
                      t.side === "buy" ? "pos" : t.side === "sell" ? "neg" : ""
                    }
                  >
                    {t.side ? t.side.toUpperCase() : "—"}
                  </span>
                ),
              },
              {
                key: "stages",
                label: "Stages",
                render: (t) => <Steps track={t} />,
              },
              {
                key: "outcome",
                label: "State",
                render: (t) => (
                  <span
                    className={
                      t.stages.validated === "failed" ? "neg" : "muted"
                    }
                  >
                    {sentenceCase(t.outcome)}
                  </span>
                ),
              },
            ]}
          />
          <div className="panel-foot">
            {mode.kind === "paper" ? "Dashed stages are simulated. " : ""}An
            acknowledgement is not a fill.
          </div>
        </Panel>
        <Panel
          className="span-6"
          title="Recent alerts"
          actions={
            <a href="#/health" className="text-link">
              Health <ArrowUpRight size={12} />
            </a>
          }
        >
          <Table
            rows={(s?.alerts ?? []).slice(0, 6)}
            getKey={(a) =>
              a.id ?? `${a.source}:${a.observed_at_ms}:${a.message}`
            }
            empty={s?.alerts ? "No alerts" : "Alerts not reported"}
            columns={[
              {
                key: "severity",
                label: "Severity",
                render: (a) => <Status value={a.severity} />,
              },
              { key: "message", label: "Alert" },
              {
                key: "observed_at_ms",
                label: "Observed",
                num: true,
                render: (a) => datetime(a.observed_at_ms),
              },
            ]}
          />
        </Panel>
        <Panel
          className="span-6"
          title="Latest lane turns"
          actions={
            <a href="#/lanes" className="text-link">
              Lanes <ArrowUpRight size={12} />
            </a>
          }
        >
          <Table
            rows={turns.slice(0, 6)}
            getKey={(t) => t.turn_id}
            empty={
              state.agentState === "error"
                ? "Lane history unavailable"
                : "No lane turns yet"
            }
            columns={[
              {
                key: "at_ms",
                label: "Time",
                num: true,
                render: (t) => clock(t.at_ms),
              },
              {
                key: "lane",
                label: "Lane",
                render: (t) => <LaneSwatch lane={t.lane} />,
              },
              {
                key: "outcome",
                label: "Outcome",
                render: (t) => turnOutcome(t),
              },
              {
                key: "elapsed_ms",
                label: "Took",
                num: true,
                render: (t) => duration(t.elapsed_ms),
              },
            ]}
          />
        </Panel>
      </div>
    </>
  );
}
