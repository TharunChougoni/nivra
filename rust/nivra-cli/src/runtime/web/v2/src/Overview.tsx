import React, { useEffect, useMemo, useState } from "react";
import { ArrowUpRight } from "lucide-react";
import { BookContinuity } from "./Operational";
import { OverviewChartsPanel } from "./OverviewCharts";
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
  numeric,
  plansFor,
  sessionKey,
  signed,
  tone,
  usd,
} from "./data";
import { sentenceCase, turnOutcome } from "./labels";
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
import { useRead, useRevision } from "./reads";
import type { Position } from "./types";
import {
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
import type { Column, KpiItem } from "./ui";

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

function BookOverview() {
  const [state, app] = useWorkspace(),
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
  const asOf = numeric(s?.as_of_ms);
  const todayIst =
    s?.session || current?.id?.date || istDateString(asOf ?? Date.now());

  const series = state.series || {};
  const equitySeries = series.equity?.length
    ? series.equity
    : state.equityPoints;
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
      historical || attribution?.realized_pnl == null
        ? "—"
        : signed(numeric(attribution.realized_pnl)),
      historical
        ? "Archived session"
        : attribution?.realized_pnl == null
          ? "Not loaded"
          : "recorded",
      {
        tone: historical ? undefined : tone(numeric(attribution?.realized_pnl)),
      },
    ],
    [
      "Unrealized",
      signed(f.unrealized),
      f.unrealized === null ? "A position has no mark" : "vs entry",
      { tone: tone(f.unrealized) },
    ],
    ["Fees", amount(f.fees), f.fees === null ? "Not recorded" : "recorded"],
    ["Cash", amount(f.cash), f.cash === null ? "Not reported" : "recorded"],
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
        <OverviewChartsPanel
          historical={historical}
          todaySession={todayIst}
          prospects={prospects}
          index={index}
          baseline={stats.first?.value ?? null}
          fills={fills ?? []}
        />
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
