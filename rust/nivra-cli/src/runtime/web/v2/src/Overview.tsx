import type { Fill, Position } from "./types";
import type { Column } from "./ui";
import React, { useEffect, useMemo, useState } from "react";
import { ArrowUpRight } from "lucide-react";
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
  numeric,
  observation,
  plansFor,
  sessionKey,
  usd,
  words,
} from "./data";
import {
  equityStats,
  instrumentIndex,
  instrumentLabel,
  journalCoverage,
  laneStats,
  modeOf,
  orderTracks,
  turnOutcome,
} from "./model";
import {
  Empty,
  InstrumentCell,
  KpiStrip,
  LaneSwatch,
  Panel,
  Segments,
  Status,
  Steps,
  Table,
  useWorkspace,
} from "./ui";
import type { KpiItem } from "./ui";
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
  ["all", "all"],
  ["cash", "cash"],
  ["future", "future"],
  ["option", "option"],
];
const kindGroup = (p: Position) => {
  const k = instrumentLabel(p.instrument).kind;
  return k === "call" || k === "put"
    ? "option"
    : k === "future"
      ? "future"
      : "cash";
};

export default function Overview() {
  const [state, app] = useWorkspace(),
    [view, setView] = useState("equity"),
    [kind, setKind] = useState("all"),
    [laneFilter, setLaneFilter] = useState("all");
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
  const lanes = useMemo(
    () => laneStats(turns, state.agent?.usage),
    [state.agent],
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
  const asOf = numeric(s?.as_of_ms);
  const kpis: KpiItem[] = [
    [
      "Equity",
      amount(f.equity),
      f.equity === null ? "No marked book recorded" : "Cash + recorded marks",
    ],
    [
      "Day P&L net",
      signed(stats.dayNet),
      stats.dayNet === null
        ? "Needs two recorded equity checkpoints"
        : `since ${clock(stats.first!.at)} checkpoint · after fees`,
      { tone: tone(stats.dayNet) },
    ],
    [
      "Realized",
      attribution?.realized_pnl == null
        ? "—"
        : signed(numeric(attribution.realized_pnl)),
      attribution?.realized_pnl == null
        ? "Health attribution not loaded"
        : "Activity attribution",
      { tone: tone(numeric(attribution?.realized_pnl)) },
    ],
    [
      "Unrealized",
      signed(f.unrealized),
      f.unrealized === null
        ? "A position lacks a recorded mark"
        : "Recorded marks − entry",
      { tone: tone(f.unrealized) },
    ],
    [
      "Fees",
      amount(f.fees),
      f.fees === null ? "Not recorded" : "Simulated, recorded",
    ],
    ["Cash", amount(f.cash), "Continuing balance"],
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
        ? "Needs two recorded equity checkpoints"
        : `${stats.count} checkpoints${stats.troughAt ? ` · trough ${clock(stats.troughAt)}` : ""}`,
      { tone: stats.maxDrawdownPct ? "neg" : undefined },
    ],
    [
      "Fills",
      fills ? count(fills.length) : "—",
      fills
        ? `${count(rejected)} rejected in current plan`
        : "Fill evidence unavailable",
    ],
    [
      "pi spend",
      usd(spend),
      spend === null
        ? "Usage not reported"
        : (coverage.label ?? `${count(state.agent?.total)} turns today`),
    ],
  ];
  const selected = prospects.find((v) => v.instrument_key === state.watch);
  const showEquity = view === "equity";
  const points = historical ? [] : showEquity ? equitySeries : priceSeries;
  const active = showEquity ? series.equity : series.price;
  const loadState = showEquity ? series.equityState : series.priceState;
  const cached =
    !!active?.length && (showEquity ? series.equityCached : series.priceCached);
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
    { key: "entry", label: "Avg", num: true, render: (p) => amount(p.entry) },
    { key: "mark", label: "Mark", num: true, render: (p) => amount(p.mark) },
    {
      key: "pnl",
      label: "Unreal.",
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
  const laneRows = laneStats(turns, state.agent?.usage);
  return (
    <>
      <BookContinuity summary={s} portfolio={state.portfolio} />
      <KpiStrip items={kpis} />
      <div className="grid-12">
        <Panel
          className="span-8"
          title={showEquity ? "Account equity" : "Instrument price"}
          note={
            historical
              ? "Selected archive · no live history inferred"
              : showEquity
                ? "recorded checkpoints · fills marked · drawdown below"
                : "receipt price · not executable depth"
          }
          actions={
            <>
              {!showEquity && (
                <select
                  aria-label="Chart instrument"
                  value={state.watch}
                  onChange={(e) => app.watch(e.target.value)}
                >
                  {!prospects.length && (
                    <option value="">No instruments supplied</option>
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
          <Chart
            points={points}
            coverage={showEquity ? series.equityCoverage : series.priceCoverage}
            label={
              showEquity
                ? "Account equity"
                : selected?.symbol || "Selected instrument"
            }
            baseline={showEquity ? (stats.first?.value ?? null) : null}
            drawdown={showEquity}
            markers={markers}
            source={
              historical
                ? "Archive has no chart history"
                : cached
                  ? "Cached series · stale · original receipt timestamps"
                  : active?.length
                    ? showEquity
                      ? series.equitySource
                        ? "Recorded portfolio checkpoints · exact timestamps"
                        : "This tab · original observation timestamps"
                      : series.priceSource
                        ? "Receipt-minute bars · display evidence, not certified exchange OHLCV"
                        : "Live receipt-price batches · this tab"
                    : "This tab · original observation timestamps"
            }
            height={showEquity ? 300 : 280}
            emptyTitle={
              showEquity
                ? "Account-equity series unavailable"
                : "Price series unavailable"
            }
            emptyDetail={
              historical
                ? "The selected session exposes a checkpoint only."
                : loadState === "loading" || loadState === "idle"
                  ? "Loading originally recorded observations. Daily allocations are never summed as wealth."
                  : showEquity
                    ? "No recorded account-equity checkpoints are available for this session. New checkpoints will appear with their original times."
                    : "No receipt-minute bars are available yet. Missing minutes remain missing; no synthetic candles are drawn."
            }
          />
        </Panel>
        <Panel
          className="span-4"
          title="pi lanes now"
          note="today"
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
                  <span>last</span>
                  <span title={l.last ? turnOutcome(l.last) : ""}>
                    {l.last ? turnOutcome(l.last) : "—"}
                  </span>
                </div>
                <div className="sub">
                  <span>p50 latency · fallbacks</span>
                  <span>
                    {duration(l.p50_ms)} · {count(l.fallbacks)}
                  </span>
                </div>
                <Sparkline
                  color={laneColor(l.lane)}
                  label={`${l.lane} activity`}
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
          <div className="panel-foot">
            Display evidence only · no order authority
          </div>
        </Panel>
        <Panel
          className="span-12"
          title="Lane timeline"
          note="each mark is one pi turn · height = latency · hollow = failed · ◆ gate escalated"
          actions={
            <Segments
              value={laneFilter}
              onChange={setLaneFilter}
              label="Lane filter"
              items={[["all", "all"], ...laneRows.map((l) => [l.lane, l.lane])]}
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
            <Empty title="No lane turns recorded">
              {state.agentState === "error"
                ? "The lane journal could not be read."
                : "Turns appear after an eligible pi lane runs."}
            </Empty>
          )}
        </Panel>
        <Panel
          className="span-7"
          title="Positions"
          note={greeks ? "cash + F&O · Greeks as recorded" : "cash + F&O"}
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
                    {t.outcome}
                  </span>
                ),
              },
            ]}
          />
          <div className="panel-foot">
            {mode.kind === "paper"
              ? "Paper: submit/ACK stages are simulated (dashed). An ACK is never shown as a fill."
              : "Submit/ACK come from recorded order events only. An ACK is never shown as a fill."}
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
            empty={
              s?.alerts ? "No derived alerts" : "Alert evidence not reported"
            }
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
                ? "Lane journal unavailable"
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
