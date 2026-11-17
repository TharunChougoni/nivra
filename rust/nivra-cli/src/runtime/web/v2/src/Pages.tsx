import type {
  Fill,
  Position,
  SessionRecord,
  PlansResponse,
  RunSummary,
  Availability,
  LoadState,
  ScannerRow,
  CatalogInstrument,
  ContractStrike,
  StrategyReference,
  ReplayPayload,
  ReplayResult,
  Instrument as InstrumentRecord,
} from "./types";
import { errorMessage } from "./types";
import type { Api } from "./api";
import type { Column } from "./ui";
import type { LucideIcon } from "lucide-react";
import React, { useEffect, useRef, useState } from "react";
import {
  ArrowRight,
  ArrowUpRight,
  FileUp,
  Download,
  Check,
  ChevronRight,
  Radio,
  TrendingUp,
  Layers,
  ShieldCheck,
  GitBranch,
  ScanLine,
  Database,
  Bot,
  Clock,
  Search,
  CircleAlert,
} from "lucide-react";
import Chart from "./Chart";
import Models, { ModelRoster } from "./Models";
import { BookContinuity, OperationalPanel } from "./Operational";
import {
  amount,
  classLabel,
  clock,
  count,
  currentSession,
  datetime,
  duration,
  finance,
  fillsFor,
  identityLabels,
  identities,
  instrumentClass,
  instrumentClasses,
  linkedFills,
  linkedPlan,
  lotQuantity,
  numeric,
  observation,
  plansFor,
  runHref,
  sessionKey,
  sessions,
  strategyLinkFor,
  strategyLinks,
  strategyReferences,
  words,
} from "./data";
import {
  Button,
  Drawer,
  Empty,
  Facts,
  Instrument,
  MetricStrip,
  Notice,
  Panel,
  Segments,
  Status,
  Table,
  useWorkspace,
} from "./ui";
const fillColumns: Column<Fill>[] = [
  {
    key: "instrument",
    label: "Instrument",
    render: (v) => (
      <Instrument
        symbol={v.instrument?.symbol}
        instrumentKey={v.instrument?.key}
      />
    ),
  },
  {
    key: "buy",
    label: "Side",
    render: (v) => (
      <span className={v.buy ? "buy-text" : "sell-text"}>
        {v.buy ? "BUY" : "SELL"}
      </span>
    ),
  },
  { key: "units", label: "Units", render: (v) => count(v.units) },
  { key: "price", label: "Fill price", render: (v) => amount(v.price) },
  { key: "fee", label: "Fees", render: (v) => amount(v.fee) },
  {
    key: "decision_id",
    label: "Decision",
    render: (v) =>
      v.decision_id ? <code>{v.decision_id}</code> : "Not recorded",
  },
  { key: "at", label: "Recorded · IST", render: (v) => datetime(v.at) },
];
function Ledger({
  fills,
  empty,
  onSelect,
  selected,
}: {
  fills?: Fill[] | null;
  empty?: string;
  onSelect?: (fill: Fill) => void;
  selected?: Fill | null;
}) {
  return (
    <Table
      columns={fillColumns}
      rows={fills || []}
      selected={selected}
      onSelect={onSelect}
      empty={
        empty ||
        (fills === null ? "Fill evidence unavailable" : "No recorded fills")
      }
    />
  );
}
function ReadinessItem({
  icon: Icon,
  label,
  value,
  description,
}: {
  icon: LucideIcon;
  label: string;
  value?: string | null;
  description: string;
}) {
  return (
    <div className="readiness-item">
      <Icon size={15} />
      <div>
        <strong>{label}</strong>
        <p>{description}</p>
      </div>
      <Status value={value} />
    </div>
  );
}
function percent(value?: number | null) {
  return typeof value === "number" && Number.isFinite(value)
    ? `${(value * 100).toFixed(2)}%`
    : "Not reported";
}
function percentPoints(value?: number | null) {
  return typeof value === "number" && Number.isFinite(value)
    ? `${value.toFixed(2)}%`
    : "Not reported";
}
function useRecentRuns() {
  const [state, app] = useWorkspace(),
    [runs, setRuns] = useState<RunSummary[]>([]);
  const terminal = state.events
    .filter((e) => ["run_started", "run_ended"].includes(e.kind))
    .at(-1)?.seq;
  useEffect(() => {
    let active = true;
    const token = state.token;
    app.api
      .runs({ limit: 6 })
      .then((v) => {
        if (active && token === app.getSnapshot().token) setRuns(v.runs || []);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [terminal]);
  return runs;
}
function useRead<T extends Availability>(
  load: (api: Api) => Promise<T>,
  key = "",
) {
  const [state, app] = useWorkspace(),
    [result, setResult] = useState<{ value: T | null; state: LoadState }>({
      value: null,
      state: "idle",
    }),
    token = state.token;
  useEffect(() => {
    let active = true;
    if (!token) {
      setResult({ value: null, state: "idle" });
      return;
    }
    setResult({ value: null, state: "loading" });
    Promise.resolve(load(app.api))
      .then((value) => {
        if (!active) return;
        setResult({
          value,
          state: value?.unavailable ? "unavailable" : "ready",
        });
      })
      .catch(() => {
        if (active) setResult({ value: null, state: "error" });
      });
    return () => {
      active = false;
    };
  }, [token, key]);
  return result;
}
export function Overview() {
  const [state, app] = useWorkspace(),
    [view, setView] = useState("price");
  const r = app.record(),
    f = finance(r),
    s = state.summary;
  const prospects = Array.isArray(state.scanner?.items)
      ? state.scanner.items
      : [],
    recent = useRecentRuns();
  useEffect(() => {
    if (!state.watch && prospects[0]?.instrument_key)
      app.watch(prospects[0].instrument_key);
  }, [prospects, state.watch]);
  const current = currentSession(state, state.meta),
    historical = sessionKey(r) !== sessionKey(current);
  const series = state.series || {};
  const priceSeries = series.price?.length ? series.price : state.points;
  const equitySeries = series.equity?.length
    ? series.equity
    : state.equityPoints;
  const points = historical
    ? []
    : view === "equity"
      ? equitySeries
      : priceSeries;
  const activeSeries = view === "equity" ? series.equity : series.price;
  const seriesState =
    view === "equity" ? series.equityState : series.priceState;
  const cached =
    !!activeSeries?.length &&
    (view === "equity" ? series.equityCached : series.priceCached);
  const selected = prospects.find((v) => v.instrument_key === state.watch);
  const fills = Array.isArray(state.fills?.items) ? state.fills.items : null;
  return (
    <>
      <BookContinuity summary={s} portfolio={state.portfolio} />
      <MetricStrip
        items={[
          ["Account equity", amount(f.equity), "Cash + recorded marks"],
          ["Available cash", amount(f.cash), "Continuing paper account"],
          [
            "Net P&L · flat book",
            amount(f.net),
            f.positions.length
              ? "Withheld while holdings remain"
              : "After recorded simulated fees",
          ],
          [
            "Positions / fills",
            `${f.positionsKnown ? f.positions.length : "—"} / ${fills ? fills.length : "—"}`,
            "Recorded paper execution",
          ],
        ]}
      />
      <div className="overview-layout">
        <div className="overview-primary">
          <Panel
            title="Market & account"
            note={
              historical
                ? "Selected archive · no live history inferred"
                : "Explore original observations, not a reconstructed market day."
            }
            actions={
              <Segments
                value={view}
                onChange={setView}
                items={[
                  ["price", "Market price"],
                  ["equity", "Account equity"],
                ]}
                label="Chart series"
              />
            }
          >
            <div className="instrument-toolbar">
              {view === "price" ? (
                <>
                  <label className="instrument-picker">
                    <ScanLine size={14} />
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
                          {p.symbol || p.instrument_key}
                        </option>
                      ))}
                    </select>
                  </label>
                  <span className="caption">
                    {selected?.price_kind
                      ? words(selected.price_kind)
                      : "Screening observations"}{" "}
                    · not executable depth
                  </span>
                </>
              ) : (
                <span className="caption">
                  Account mark observations · no daily allocations summed as
                  wealth
                </span>
              )}
            </div>
            <Chart
              points={points}
              coverage={
                view === "equity" ? series.equityCoverage : series.priceCoverage
              }
              label={
                view === "price"
                  ? selected?.symbol || "Selected instrument"
                  : "Account equity"
              }
              source={
                historical
                  ? "Archive has no chart history"
                  : cached
                    ? "Cached series · stale · original receipt timestamps"
                    : activeSeries?.length
                      ? view === "equity"
                        ? series.equitySource
                          ? "Recorded portfolio checkpoints · exact timestamps"
                          : "This tab · original observation timestamps"
                        : series.priceSource
                          ? "Receipt-minute bars · display evidence, not certified exchange OHLCV"
                          : "Live receipt-price batches · this tab"
                      : "This tab · original observation timestamps"
              }
              height={300}
              emptyTitle={
                view === "equity"
                  ? "Account-equity series unavailable"
                  : "Price series unavailable"
              }
              emptyDetail={
                historical
                  ? "The selected session exposes a checkpoint only."
                  : view === "equity"
                    ? seriesState === "loading" || seriesState === "idle"
                      ? "Loading originally recorded portfolio checkpoints. Daily allocations are never summed as wealth."
                      : "No recorded account-equity checkpoints are available for this session. New checkpoints will appear with their original times."
                    : seriesState === "loading" || seriesState === "idle"
                      ? "Loading receipt-minute observations from the authenticated series API."
                      : "No receipt-minute bars are available yet. Missing minutes remain missing; no synthetic candles are drawn."
              }
            />
          </Panel>
          <Panel
            title="Decision activity"
            note="The latest recorded model runs. Select one to follow the evidence."
            actions={
              <a href="#/models" className="text-link">
                Open Models <ArrowUpRight size={13} />
              </a>
            }
          >
            {recent.length ? (
              <div className="activity-list">
                {recent.map((run) => (
                  <a key={run.run} href={runHref(run.run)}>
                    <span className={"activity-node " + (run.role || "")}>
                      <Bot size={14} />
                    </span>
                    <div>
                      <strong>
                        {words(run.role)}{" "}
                        <span>{run.model || "Model unknown"}</span>
                      </strong>
                      <p>
                        {typeof run.trigger === "string"
                          ? run.trigger
                          : run.trigger?.kind ||
                            run.trigger_kind ||
                            "Trigger not reported"}{" "}
                        · {duration(run.duration_ms)} · {count(run.tool_calls)}{" "}
                        tools
                      </p>
                    </div>
                    <div className="activity-result">
                      <Status value={run.running ? "running" : run.outcome} />
                      <time>{datetime(run.started_ms)}</time>
                    </div>
                    <ChevronRight size={14} />
                  </a>
                ))}
              </div>
            ) : (
              <Empty title="No recent run evidence">
                Model activity appears after an eligible run is archived.
              </Empty>
            )}
          </Panel>
        </div>
        <aside className="overview-rail">
          <Panel
            title="Model loop"
            actions={
              <a href="#/models" aria-label="Inspect models">
                <ArrowUpRight size={15} />
              </a>
            }
          >
            <ModelRoster compact />
            <div className="loop-caption">
              <span>Evidence</span>
              <ArrowRight size={12} />
              <span>Handoff</span>
              <ArrowRight size={12} />
              <span>Decision</span>
            </div>
          </Panel>
          <Panel title="Session readiness">
            <ReadinessItem
              icon={Radio}
              label="Controller"
              value={s?.readiness?.status}
              description={words(s?.readiness?.reason)}
            />
            <ReadinessItem
              icon={Database}
              label="History preparation"
              value={(s?.readiness?.history || s?.history_preparation)?.status}
              description="Provider-empty history stays partial"
            />
            <ReadinessItem
              icon={ScanLine}
              label="Scanner"
              value={
                state.scanner
                  ? state.scanner.unavailable
                    ? "unavailable"
                    : "observed"
                  : "unknown"
              }
              description={
                state.scanner?.coverage
                  ? `${count(state.scanner.coverage.fresh)} fresh / ${count(state.scanner.coverage.universe)} supplied`
                  : "Coverage not reported"
              }
            />
            <a className="rail-link" href="#/health">
              Inspect system health <ArrowRight size={13} />
            </a>
          </Panel>
          <Panel title="Paper book">
            <Facts
              items={[
                ["Session", r?.id?.date],
                ["State", words(r?.status || r?.phase)],
                ["Recorded fees", amount(f.fees)],
                ["Planner", words(state.plans?.planner?.status)],
              ]}
            />
            <div className="rail-foot">
              <ShieldCheck size={13} /> Original two-second financial gate
            </div>
          </Panel>
        </aside>
      </div>
    </>
  );
}
export function Portfolio() {
  const [state, app] = useWorkspace(),
    r = app.record(),
    f = finance(r);
  const [tab, setTab] = useState("positions"),
    [side, setSide] = useState("all"),
    [kind, setKind] = useState("all"),
    [selected, setSelected] = useState<Position | null>(null),
    [selectedFill, setSelectedFill] = useState<Fill | null>(null);
  const fills = fillsFor(state, r),
    plans = plansFor(state, r),
    current = currentSession(state, state.meta),
    historical = sessionKey(r) !== sessionKey(current);
  const classes = instrumentClasses(
    tab === "fills" ? fills || [] : tab === "positions" ? f.positions : [],
  );
  const activeKind = classes.includes(kind) ? kind : "all";
  const positions = f.positions.filter(
    (p) => activeKind === "all" || instrumentClass(p) === activeKind,
  );
  const fillRows =
    fills &&
    fills.filter(
      (v) =>
        (side === "all" || (v.buy ? "buy" : "sell") === side) &&
        (activeKind === "all" || instrumentClass(v) === activeKind),
    );
  const columns: Column<Position>[] = [
    {
      key: "instrument",
      label: "Instrument",
      render: (p) => (
        <Instrument
          symbol={p.instrument?.symbol}
          sub={[p.instrument?.key, classLabel(instrumentClass(p))]
            .filter(Boolean)
            .join(" · ")}
        />
      ),
    },
    { key: "units", label: "Units", render: (p) => count(p.units) },
    {
      key: "lots",
      label: "Lots",
      render: (p) => {
        const quantity = lotQuantity(p);
        return quantity
          ? `${count(quantity.lots)} × ${count(quantity.lot_size)}`
          : "Not reported";
      },
    },
    { key: "entry", label: "Entry", render: (p) => amount(p.entry) },
    { key: "mark", label: "Mark", render: (p) => amount(p.mark) },
    {
      key: "value",
      label: "Marked value",
      render: (p) =>
        numeric(p.mark) !== null && numeric(p.units) !== null
          ? amount(Number(p.mark) * Number(p.units))
          : "—",
    },
    {
      key: "pnl",
      label: "Unrealized P&L",
      render: (p) =>
        [p.mark, p.entry, p.units].every((v) => numeric(v) !== null)
          ? amount((Number(p.mark) - Number(p.entry)) * Number(p.units))
          : "—",
    },
    {
      key: "decision",
      label: "Decision",
      render: (p) =>
        p.decision_id ? <code>{p.decision_id}</code> : "Not recorded",
    },
    {
      key: "marked_at",
      label: "Mark time",
      render: (p) => datetime(p.marked_at),
    },
  ];
  const linked = linkedPlan(
      plans,
      selected?.decision_id,
      selected?.instrument?.key,
    ),
    linkedFillRows = linkedFills(
      fills,
      selected?.decision_id,
      selected?.instrument?.key,
    ),
    strategy =
      !historical && selected
        ? strategyLinkFor(state.scanner?.items, selected?.instrument?.key)
        : null,
    fillLinked = linkedPlan(
      plans,
      selectedFill?.decision_id,
      selectedFill?.instrument?.key,
    ),
    fillStrategy =
      !historical && selectedFill
        ? strategyLinkFor(state.scanner?.items, selectedFill?.instrument?.key)
        : null;
  return (
    <>
      <MetricStrip
        items={[
          ["Account equity", amount(f.equity), "Recorded checkpoint"],
          ["Cash", amount(f.cash), "Continuing cash balance"],
          ["Marked holdings", amount(f.exposure), "Open paper exposure"],
          ["Recorded fees", amount(f.fees), "Simulation assumptions"],
        ]}
      />
      <Panel
        title="Portfolio ledger"
        note="Marks, fills and validation are distinct records."
        actions={
          <Segments
            value={tab}
            onChange={setTab}
            items={[
              ["positions", "Positions"],
              ["fills", "Fills"],
              ["plans", "Plans & validation"],
            ]}
          />
        }
      >
        {(state.portfolioState === "error" ||
          state.portfolioState === "unavailable") && (
          <Notice>
            Portfolio projection{" "}
            {state.portfolioState === "unavailable" ? "unavailable" : "failed"}.
            Positions, marks, equity and fees are not inferred from another
            source; only explicitly supplied session fields remain visible.
          </Notice>
        )}
        {(state.fillsState === "error" ||
          state.fillsState === "unavailable") && (
          <Notice>
            Fill projection{" "}
            {state.fillsState === "unavailable" ? "unavailable" : "failed"}. No
            fill rows or fee totals are reconstructed.
          </Notice>
        )}
        {tab === "plans" ? (
          <Plans plans={plans} fills={fills} />
        ) : (
          <>
            <div className="filter-bar">
              <div className="table-controls">
                <span>
                  {tab === "fills"
                    ? `${count(fills?.length)} recorded fills`
                    : `${count(f.positionsKnown ? f.positions.length : null)} recorded positions`}
                </span>
                {tab === "fills" && (
                  <Segments
                    value={side}
                    onChange={setSide}
                    items={[
                      ["all", "All sides"],
                      ["buy", "Buy"],
                      ["sell", "Sell"],
                    ]}
                  />
                )}
              </div>
              {classes.length ? (
                <Segments
                  value={activeKind}
                  onChange={setKind}
                  items={[
                    ["all", "All classes"],
                    ...classes.map((value) => [value, classLabel(value)]),
                  ]}
                  label="Instrument class"
                />
              ) : (
                <span className="caption">
                  No instrument-class rows are recorded for this projection.
                </span>
              )}
            </div>
            {tab === "positions" ? (
              <Table
                getKey={(p) => p.instrument?.key}
                rows={positions}
                columns={columns}
                empty={
                  f.positionsKnown
                    ? activeKind === "all"
                      ? "The recorded book is flat"
                      : "No recorded position in this instrument class"
                    : "Position ledger unavailable"
                }
                onSelect={(p) => {
                  setSelected(p);
                  if (!historical && p.instrument?.key)
                    app.watch(p.instrument.key);
                }}
              />
            ) : (
              <Ledger
                fills={fillRows}
                onSelect={setSelectedFill}
                selected={selectedFill}
                empty={
                  fills === null
                    ? "Fill evidence unavailable"
                    : "No matching recorded fills"
                }
              />
            )}
          </>
        )}
      </Panel>
      <p className="footnote">
        Paper ledger · source marks are not executable prices · an
        acknowledgement is not a fill. Instrument classes come from recorded
        instrument.kind; ETF and derivative coverage follows the D5 contract.
      </p>
      <Drawer
        open={!!selected}
        onClose={() => setSelected(null)}
        title={selected?.instrument?.symbol || "Position evidence"}
        description={
          selected?.instrument?.key || "Original instrument identity"
        }
      >
        <Facts
          items={[
            ["Session", r?.id?.date],
            [
              "Broker / account",
              [r?.id?.broker, r?.id?.account].filter(Boolean).join(" / "),
            ],
            ["Instrument class", classLabel(instrumentClass(selected))],
            ["Segment", selected?.instrument?.segment],
            ["Underlying", selected?.instrument?.underlying],
            ["Expiry", selected?.instrument?.expiry],
            ["Strike", amount(selected?.instrument?.strike)],
            ["Lots", count(lotQuantity(selected)?.lots)],
            ["Lot size", count(lotQuantity(selected)?.lot_size)],
            [
              "Margin · display only",
              selected?.margin?.display_only === true &&
              selected.margin.instrument_key === selected.instrument?.key
                ? amount(selected.margin.total_margin)
                : "Not reported",
            ],
            ["Units", count(selected?.units)],
            ["Entry", amount(selected?.entry)],
            ["Recorded mark", amount(selected?.mark)],
            ["Mark time", datetime(selected?.marked_at)],
            ["Decision", selected?.decision_id || "Not recorded"],
            ["Exit target", amount(selected?.exit_intent?.target)],
            ["Exit stop", amount(selected?.exit_intent?.stop)],
          ]}
        />
        <Chart
          label={selected?.instrument?.symbol || "Position price"}
          points={
            historical
              ? []
              : state.series?.price?.length
                ? state.series.price
                : state.points
          }
          height={220}
          source={
            state.series?.priceCached && state.series?.price?.length
              ? "Cached receipt-minute bars · stale"
              : state.series?.price?.length
                ? "Receipt-minute bars · original receipt timestamps"
                : "Tab-local screening observations"
          }
        />
        <Panel title="Linked plan & validation">
          {linked === null ? (
            <p className="panel-note">
              Plans projection unavailable for this selection. No order or
              strategy state is inferred.
            </p>
          ) : linked.actions.length ? (
            <Table
              rows={linked.actions}
              columns={[
                { key: "index", label: "Action" },
                {
                  key: "key",
                  label: "Instrument",
                  render: (v) => v.action?.instrument_key || "Not reported",
                },
                {
                  key: "side",
                  label: "Side",
                  render: (v) => words(v.action?.side),
                },
                { key: "units", label: "Units", render: (v) => count(v.units) },
                {
                  key: "reason",
                  label: "Validation",
                  render: (v) => <Status value={v.reason} />,
                },
                {
                  key: "execution",
                  label: "Execution",
                  render: (v) => words(v.execution?.verdict),
                },
              ]}
            />
          ) : (
            <p className="panel-note">
              {selected?.decision_id && selected?.instrument?.key
                ? "No loaded plan action matched this decision and instrument."
                : "No decision ID and instrument identity were recorded for this position."}
            </p>
          )}
        </Panel>
        <Panel title="Strategy link">
          {strategy ? (
            <Facts
              items={[
                ["Spec ID", strategy.strategy_spec_id],
                ["Spec version", strategy.strategy_spec_version],
                ["Spec hash", strategy.strategy_spec_hash],
                ["Scan ID", strategy.strategy_scan_id],
                ["Strategy score", count(strategy.strategy_score)],
              ]}
            />
          ) : (
            <p className="panel-note">
              {historical
                ? "The selected archive has no scanner projection; no strategy reference is inferred."
                : state.scanner?.unavailable || state.scannerState === "error"
                  ? "Scanner projection unavailable; no strategy reference is inferred."
                  : "No strategy reference was recorded for this instrument."}
            </p>
          )}
        </Panel>
        <Panel title="Related fills">
          <Ledger
            fills={linkedFillRows}
            empty={
              linkedFillRows === null
                ? "Fill evidence unavailable"
                : selected?.decision_id && selected?.instrument?.key
                  ? "No recorded fills match this decision and instrument"
                  : "No decision ID and instrument identity were recorded for this position"
            }
          />
        </Panel>
      </Drawer>
      <Drawer
        open={!!selectedFill}
        onClose={() => setSelectedFill(null)}
        title={selectedFill?.instrument?.symbol || "Fill evidence"}
        description={
          selectedFill?.instrument?.key || "Original instrument identity"
        }
      >
        <Facts
          items={[
            ["Session", r?.id?.date],
            [
              "Broker / account",
              [r?.id?.broker, r?.id?.account].filter(Boolean).join(" / "),
            ],
            ["Instrument class", classLabel(instrumentClass(selectedFill))],
            ["Segment", selectedFill?.instrument?.segment],
            ["Lots", count(lotQuantity(selectedFill)?.lots)],
            ["Lot size", count(lotQuantity(selectedFill)?.lot_size)],
            ["Side", selectedFill ? (selectedFill.buy ? "BUY" : "SELL") : null],
            ["Units", count(selectedFill?.units)],
            ["Fill price", amount(selectedFill?.price)],
            ["Expected price", amount(selectedFill?.expected_price)],
            ["Slippage · bps", count(selectedFill?.slippage_bps)],
            ["Fee", amount(selectedFill?.fee)],
            ["Decision", selectedFill?.decision_id || "Not recorded"],
            ["Recorded", datetime(selectedFill?.at)],
          ]}
        />
        <Panel title="Linked plan & validation">
          {fillLinked === null ? (
            <p className="panel-note">
              Plans projection unavailable for this selection. No order or
              strategy state is inferred.
            </p>
          ) : fillLinked.actions.length ? (
            <Table
              rows={fillLinked.actions}
              columns={[
                { key: "index", label: "Action" },
                {
                  key: "key",
                  label: "Instrument",
                  render: (v) => v.action?.instrument_key || "Not reported",
                },
                {
                  key: "side",
                  label: "Side",
                  render: (v) => words(v.action?.side),
                },
                { key: "units", label: "Units", render: (v) => count(v.units) },
                {
                  key: "reason",
                  label: "Validation",
                  render: (v) => <Status value={v.reason} />,
                },
                {
                  key: "execution",
                  label: "Execution",
                  render: (v) => words(v.execution?.verdict),
                },
              ]}
            />
          ) : (
            <p className="panel-note">
              {selectedFill?.decision_id && selectedFill?.instrument?.key
                ? "No loaded plan action matched this decision and instrument."
                : "No decision ID and instrument identity were recorded for this fill."}
            </p>
          )}
        </Panel>
        <Panel title="Strategy link">
          {fillStrategy ? (
            <Facts
              items={[
                ["Spec ID", fillStrategy.strategy_spec_id],
                ["Spec version", fillStrategy.strategy_spec_version],
                ["Spec hash", fillStrategy.strategy_spec_hash],
                ["Scan ID", fillStrategy.strategy_scan_id],
                ["Strategy score", count(fillStrategy.strategy_score)],
              ]}
            />
          ) : (
            <p className="panel-note">
              {historical
                ? "The selected archive has no scanner projection; no strategy reference is inferred."
                : state.scanner?.unavailable || state.scannerState === "error"
                  ? "Scanner projection unavailable; no strategy reference is inferred."
                  : "No strategy reference was recorded for this instrument."}
            </p>
          )}
        </Panel>
      </Drawer>
    </>
  );
}
function Plans({
  plans,
  fills,
}: {
  plans: PlansResponse | null;
  fills: Fill[] | null;
}) {
  if (!plans)
    return (
      <Empty title="Plan projection unavailable">
        No recorded planner actions or execution verdicts are loaded for this
        session. Nothing is reconstructed from model prose or fills.
      </Empty>
    );
  if (plans.unavailable)
    return (
      <Empty title="Plan projection unavailable">
        {words(plans.reason)}. Raw model output does not establish an accepted
        plan or fill.
      </Empty>
    );
  const p = plans.planner;
  return (
    <>
      <div className="validation-summary">
        <Status value={p?.status} />
        <span>
          Decision <code>{p?.decision_id || "not reported"}</code>
        </span>
        <span>{count(p?.pending_events)} pending events</span>
        <a href="#/models" className="text-link">
          Inspect model output <ArrowUpRight size={12} />
        </a>
      </div>
      <Table
        rows={plans.actions || []}
        columns={[
          { key: "index", label: "Action" },
          {
            key: "key",
            label: "Instrument",
            render: (v) => v.action?.instrument_key || "Not reported",
          },
          { key: "side", label: "Side", render: (v) => words(v.action?.side) },
          { key: "units", label: "Units", render: (v) => count(v.units) },
          {
            key: "reason",
            label: "Validation",
            render: (v) => <Status value={v.reason} />,
          },
          {
            key: "execution",
            label: "Execution",
            render: (v) =>
              words(
                plans.execution?.find(
                  (e) =>
                    e.index === v.index && e.decision_id === p?.decision_id,
                )?.verdict,
              ),
          },
          {
            key: "fills",
            label: "Linked fills",
            render: (v) => {
              const rows = linkedFills(
                fills,
                p?.decision_id,
                v.action?.instrument_key,
              );
              return rows === null ? "—" : count(rows.length);
            },
          },
        ]}
        empty="No recorded proposed actions"
      />
      <div className="panel-note">
        An abstention is a valid outcome. Raw model output does not prove host
        acceptance or execution; execution verdicts are not fills.
      </div>
    </>
  );
}
export function Scanner() {
  const [state, app] = useWorkspace(),
    missing = useRead(
      (api) => api.scanner({ segment: "missing", limit: 500 }),
      "missing",
    ),
    screens = useRead(
      (api) => api.scanner({ segment: "screens", limit: 500 }),
      "screens",
    );
  const [tab, setTab] = useState("prospects"),
    [query, setQuery] = useState(""),
    [sort, setSort] = useState("priority"),
    [selection, setSelected] = useState<ScannerRow | null>(null);
  const prospects =
      state.scannerState === "ready" && Array.isArray(state.scanner?.items)
        ? state.scanner.items
        : [],
    missingRows = Array.isArray(missing.value?.items)
      ? missing.value.items
      : [],
    screenRows = Array.isArray(screens.value?.items) ? screens.value.items : [],
    coverage =
      state.scanner?.coverage ||
      missing.value?.coverage ||
      screens.value?.coverage ||
      null;
  const sourceState =
      tab === "prospects"
        ? state.scannerState
        : tab === "missing"
          ? missing.state
          : screens.state,
    sourceUnavailable =
      sourceState === "unavailable" ||
      (tab === "prospects" && state.scanner?.unavailable === true);
  const selected = selection
    ? [...prospects, ...missingRows].find(
        (v) => v.instrument_key === selection.instrument_key,
      ) || selection
    : null;
  const match = (r: ScannerRow) =>
    [r.symbol, r.instrument_key, r.reason, r.strategy_spec_id]
      .filter(Boolean)
      .join(" ")
      .toLowerCase()
      .includes(query.toLowerCase());
  const rows = (
    tab === "prospects" ? prospects : tab === "missing" ? missingRows : []
  )
    .filter(match)
    .slice();
  if (sort === "change")
    rows.sort(
      (a, b) =>
        Math.abs(Number(b.sample_change_bps) || 0) -
        Math.abs(Number(a.sample_change_bps) || 0),
    );
  else if (sort === "symbol")
    rows.sort((a, b) => String(a.symbol).localeCompare(String(b.symbol)));
  const columns: Column<ScannerRow>[] =
    tab === "prospects"
      ? [
          {
            key: "symbol",
            label: "Instrument",
            render: (r) => (
              <Instrument
                symbol={r.symbol}
                instrumentKey={r.instrument_key}
                sub={classLabel(instrumentClass(r))}
              />
            ),
          },
          {
            key: "price",
            label: "Screening price",
            render: (r) =>
              amount(r.latest_screening_price ?? r.latest_sampled_bid),
          },
          {
            key: "sample_change_bps",
            label: "Sampled Δ · bps",
            render: (r) => (
              <span
                className={
                  Number(r.sample_change_bps) < 0 ? "sell-text" : "buy-text"
                }
              >
                {count(r.sample_change_bps)}
              </span>
            ),
          },
          {
            key: "sample_count",
            label: "Samples",
            render: (r) => count(r.sample_count),
          },
          {
            key: "observed_ms",
            label: "Original receipt",
            render: (r) => datetime(r.observed_ms),
          },
          {
            key: "strategy_spec_id",
            label: "Strategy reference",
            render: (r) =>
              strategyLinks([r]).length ? (
                <code>{r.strategy_spec_id || "Recorded reference"}</code>
              ) : (
                "Not recorded"
              ),
          },
          {
            key: "screening_reason",
            label: "Screen",
            render: (r) => (
              <span className="caption">{words(r.screening_reason)}</span>
            ),
          },
        ]
      : [
          columnsInstrument(),
          {
            key: "reason",
            label: "Reason",
            render: (r) => <Status value={r.reason} />,
          },
          {
            key: "diagnostic",
            label: "Diagnostic",
            render: (r) => words(r.diagnostic),
          },
          {
            key: "last_observed_ms",
            label: "Last valid receipt",
            render: (r) => datetime(r.last_observed_ms),
          },
        ];
  function inspect(row: ScannerRow) {
    setSelected(row);
    if (row.instrument_key) app.watch(row.instrument_key);
  }
  const strategy =
    selected && strategyLinks([selected]).length ? selected : null;
  const tableEmpty =
    tab === "screens"
      ? screens.state === "error"
        ? "Screen outcomes unavailable"
        : "No screen outcomes supplied"
      : sourceUnavailable
        ? tab === "missing"
          ? "Missing observations unavailable"
          : "Scanner projection unavailable"
        : sourceState === "error"
          ? tab === "missing"
            ? "Missing observations unavailable"
            : "Scanner projection unavailable"
          : "No matching observations";
  return (
    <>
      <MetricStrip
        items={[
          [
            "Coverage scope",
            [coverage?.market_segment, classLabel(coverage?.instrument_kind)]
              .filter(Boolean)
              .join(" · "),
            "Only the reported segment",
          ],
          ["Universe", count(coverage?.universe), "Supplied instruments"],
          ["Fresh", count(coverage?.fresh), "Screening observations"],
          ["Stale", count(coverage?.stale), "Retained with original age"],
          ["Unseen", count(coverage?.unseen), "No valid observation"],
        ]}
      />
      <Panel
        title="Discovery workspace"
        note="Compare evidence, then select an instrument to inspect its original observations."
        actions={<Status value={state.summary?.market_feed?.state} />}
      >
        <div className="filter-bar">
          <Segments
            value={tab}
            onChange={setTab}
            items={[
              ["prospects", "Prospects"],
              ["missing", "Stale & missing"],
              ["screens", "Screen outcomes"],
            ]}
          />
          {tab !== "screens" && (
            <div className="flex items-center gap-2">
              <label className="search-field">
                <Search size={13} />
                <input
                  type="search"
                  placeholder="Find an instrument…"
                  aria-label="Search scanner"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                />
              </label>
              <select
                aria-label="Scanner sort"
                value={sort}
                onChange={(e) => setSort(e.target.value)}
              >
                <option value="priority">Original priority</option>
                <option value="change">Absolute sampled change</option>
                <option value="symbol">Symbol A–Z</option>
              </select>
            </div>
          )}
        </div>
        {tab === "screens" ? (
          <Table
            rows={screenRows}
            columns={[
              {
                key: "reason",
                label: "Screen outcome",
                render: (r) => words(r.reason),
              },
              {
                key: "total",
                label: "Instruments",
                render: (r) => count(r.total),
              },
            ]}
            empty={tableEmpty}
          />
        ) : (
          <Table
            rows={rows}
            columns={columns}
            getKey={(r) => r.instrument_key}
            selected={selected?.instrument_key}
            onSelect={inspect}
            empty={tableEmpty}
          />
        )}
        <div className="table-footer">
          <span>
            {tab === "screens"
              ? `${count(screenRows.length)} screen outcomes · /api/v1/scanner?segment=screens`
              : `${count(coverage?.presented ?? rows.length)} presented · ${count(coverage?.eligible)} eligible · ${
                  tab === "prospects"
                    ? coverage?.unavailable_truncated
                      ? "missing rows truncated by source"
                      : "missing rows as supplied"
                    : "missing rows loaded on this page"
                }`}
          </span>
          <span>Priority is not expected profit</span>
        </div>
      </Panel>
      <Drawer
        open={!!selected}
        onClose={() => setSelected(null)}
        title={selected?.symbol || "Instrument evidence"}
        description={selected?.instrument_key || "Instrument identity"}
      >
        <div className="instrument-summary">
          <strong>
            {amount(
              selected?.latest_screening_price ?? selected?.latest_sampled_bid,
            )}
          </strong>
          <span>{count(selected?.sample_change_bps)} bps sampled change</span>
          <Status value={selected?.screening_reason || selected?.reason} />
        </div>
        <Chart
          label={selected?.symbol || "Price"}
          points={
            state.series?.price?.length ? state.series.price : state.points
          }
          source={
            state.series?.priceCached && state.series?.price?.length
              ? "Cached receipt-minute bars · stale"
              : state.series?.price?.length
                ? "Receipt-minute bars · original receipt timestamps"
                : "Original sampled-price timestamps · this tab only"
          }
          height={250}
        />
        <Facts
          items={[
            ["Original receipt", datetime(selected?.observed_ms)],
            [
              "Sampled price time",
              datetime(selected?.sampled_price_ms ?? selected?.sampled_bid_ms),
            ],
            ["Provider source time", datetime(selected?.source_ms)],
            ["Price kind", words(selected?.price_kind)],
            ["Retained samples at source", count(selected?.sample_count)],
            ["Quote status", "Screening evidence · not executable depth"],
            ["Diagnostic", words(selected?.diagnostic)],
          ]}
        />
        <Panel title="Recorded strategy reference">
          {strategy ? (
            <Facts
              items={[
                ["Spec ID", strategy.strategy_spec_id],
                ["Spec version", strategy.strategy_spec_version],
                ["Spec hash", strategy.strategy_spec_hash],
                ["Scan ID", strategy.strategy_scan_id],
                ["Strategy score", count(strategy.strategy_score)],
                [
                  "Scope",
                  "Scanner prospect fields only; library and switch history are not exposed.",
                ],
              ]}
            />
          ) : (
            <p className="panel-note">
              {sourceUnavailable
                ? "Scanner projection unavailable; no strategy reference is inferred."
                : "No strategy reference was recorded for this instrument."}
            </p>
          )}
        </Panel>
        <details className="raw-evidence">
          <summary>Inspect raw scanner evidence</summary>
          <pre>{JSON.stringify(selected, null, 2)}</pre>
        </details>
      </Drawer>
      <VerifiedCatalog />
    </>
  );
}
function catalogTick(row: CatalogInstrument) {
  return row.tick != null && row.tick_unit
    ? `${row.tick} ${row.tick_unit}`
    : row.provider_tick != null
      ? `${row.provider_tick} provider units`
      : "Not reported";
}
const catalogColumns: Column<CatalogInstrument>[] = [
  {
    key: "symbol",
    label: "Symbol",
    render: (row) => (
      <Instrument
        symbol={row.symbol ?? undefined}
        instrumentKey={row.instrument_key}
      />
    ),
  },
  { key: "segment", label: "Segment" },
  { key: "kind", label: "Class", render: (row) => classLabel(row.kind) },
  {
    key: "lot_size",
    label: "Units / lot",
    render: (row) =>
      row.lot_size == null ? "Not reported" : String(row.lot_size),
  },
  {
    key: "expiry_ms",
    label: "Expiry",
    render: (row) => datetime(row.expiry_ms),
  },
  { key: "tick", label: "Tick", render: catalogTick },
];
function VerifiedCatalog() {
  const [state] = useWorkspace(),
    [cursor, setCursor] = useState(0),
    [segment, setSegment] = useState(""),
    [kind, setKind] = useState(""),
    [search, setSearch] = useState(""),
    [selected, setSelected] = useState<CatalogInstrument | null>(null),
    catalog = useRead(
      (api) =>
        api.instruments({
          scope: "all",
          cursor,
          limit: 50,
          ...(segment ? { market_segment: segment } : {}),
          ...(kind ? { kind } : {}),
          ...(search ? { q: search } : {}),
        }),
      `${cursor}:${segment}:${kind}:${search}:${state.summary?.binding ?? state.summary?.session ?? ""}:${state.summary?.instrument_display?.status ?? ""}:${state.summary?.instrument_display?.source_sha256 ?? ""}:${state.scanner?.coverage?.universe ?? ""}`,
    ),
    response = catalog.value,
    rows = response?.items || [];
  useEffect(() => {
    setSelected(null);
    setCursor(0);
  }, [state.summary?.binding, state.summary?.session]);
  return (
    <>
      <Panel
        title="Instrument catalog"
        note="Captured BOD metadata · cash execution allowlist is separate · quotes and margins are not inferred"
        actions={
          <div className="panel-actions">
            <Button
              disabled={cursor === 0}
              onClick={() => setCursor(Math.max(0, cursor - 50))}
            >
              Previous
            </Button>
            <Button
              disabled={response?.next == null}
              onClick={() => setCursor(response?.next ?? cursor)}
            >
              Next
            </Button>
          </div>
        }
      >
        <div className="filter-bar">
          <label>
            Market segment{" "}
            <select
              aria-label="Catalog segment"
              value={segment}
              onChange={(e) => {
                setSegment(e.target.value);
                setCursor(0);
              }}
            >
              <option value="">All captured segments</option>
              {Object.entries(response?.segments || {}).map(([name, n]) => (
                <option key={name} value={name}>
                  {name} ({count(n)})
                </option>
              ))}
            </select>
          </label>
          <label>
            Instrument class{" "}
            <select
              aria-label="Catalog class"
              value={kind}
              onChange={(e) => {
                setKind(e.target.value);
                setCursor(0);
              }}
            >
              <option value="">All recorded classes</option>
              {Object.entries(response?.kinds || {}).map(([name, n]) => (
                <option key={name} value={name}>
                  {classLabel(name)} ({count(n)})
                </option>
              ))}
            </select>
          </label>
          <input
            aria-label="Search catalog"
            placeholder="Symbol, key or underlying…"
            value={search}
            onChange={(e) => {
              setSearch(e.target.value);
              setCursor(0);
            }}
          />
        </div>
        {catalog.state === "idle" || catalog.state === "loading" ? (
          <p className="panel-note">Loading instrument metadata…</p>
        ) : catalog.state === "unavailable" ? (
          <p className="panel-note">
            Display catalog unavailable · {words(response?.reason)}
          </p>
        ) : catalog.state === "error" ? (
          <p className="panel-note">Instrument catalog request failed.</p>
        ) : (
          <>
            <Facts
              items={[
                ["Source", response?.source],
                ["Coverage", words(response?.coverage_kind)],
                ["Source received", datetime(response?.received_at_ms)],
                ["Matching rows", count(response?.total)],
                ["Rows read", count(response?.rows_seen)],
                ["Rejected / ambiguous rows", count(response?.rejected_rows)],
              ]}
            />
            <Table
              columns={catalogColumns}
              rows={rows}
              getKey={(row) => row.instrument_key}
              onSelect={setSelected}
              empty="No captured instruments match these filters"
            />
            {response?.source_hashes && (
              <details className="raw-evidence">
                <summary>Inspect catalog source hashes</summary>
                <Facts
                  items={Object.entries(response.source_hashes).map(
                    ([name, hash]) => [name, hash],
                  )}
                />
              </details>
            )}
          </>
        )}
      </Panel>
      <Drawer
        open={!!selected}
        onClose={() => setSelected(null)}
        title={selected?.symbol || "Instrument metadata"}
        description={selected?.instrument_key}
      >
        <Facts
          items={[
            ["Class", classLabel(selected?.kind)],
            ["Segment", selected?.segment],
            ["Exchange", selected?.exchange],
            ["Provider type", selected?.instrument_type],
            ["Underlying", selected?.underlying_key],
            ["Expiry", datetime(selected?.expiry_ms)],
            [
              "Strike · provider value",
              selected?.strike == null
                ? "Not reported"
                : String(selected.strike),
            ],
            [
              "Units per lot",
              selected?.lot_size == null
                ? "Not reported"
                : String(selected.lot_size),
            ],
            ["Tick", selected ? catalogTick(selected) : null],
            [
              "Cash allowlist",
              selected?.cash_execution_verified
                ? "Verified cash identity; fresh execution checks still required"
                : "Display only",
            ],
            [
              "Margin",
              selected?.margin?.display_only &&
              selected.margin.instrument_key === selected.instrument_key
                ? amount(selected.margin.total_margin)
                : "Not captured",
            ],
            ["Quote", "Not captured by BOD catalog"],
          ]}
        />
        {selected?.underlying_key && selected.expiry_ms && (
          <ContractChain
            key={`${selected.segment}:${selected.underlying_key}:${selected.expiry_ms}`}
            instrument={selected}
          />
        )}
      </Drawer>
    </>
  );
}
function ContractChain({ instrument }: { instrument: CatalogInstrument }) {
  const [cursor, setCursor] = useState(0),
    chain = useRead(
      (api) =>
        api.instrumentChain({
          underlying_key: instrument.underlying_key!,
          expiry_ms: instrument.expiry_ms!,
          market_segment: instrument.segment,
          cursor,
          limit: 50,
        }),
      `${instrument.instrument_key}:${cursor}`,
    ),
    response = chain.value;
  const leg = (
    row: CatalogInstrument | null | undefined,
    ambiguous?: boolean,
  ) =>
    ambiguous ? (
      "Ambiguous contracts"
    ) : row ? (
      <>
        <Instrument
          symbol={row.symbol ?? undefined}
          instrumentKey={row.instrument_key}
        />
        <small>
          {row.lot_size == null ? "Not reported" : String(row.lot_size)} units /
          lot
        </small>
      </>
    ) : (
      "Not captured"
    );
  const columns: Column<ContractStrike>[] = [
    {
      key: "call",
      label: "Call",
      render: (row) => leg(row.call, row.ambiguous_call),
    },
    {
      key: "strike",
      label: "Strike · provider value",
      render: (row) => String(row.strike),
    },
    {
      key: "put",
      label: "Put",
      render: (row) => leg(row.put, row.ambiguous_put),
    },
  ];
  return (
    <Panel
      title="F&O contract chain"
      note="Captured contract definitions only · missing legs stay unknown · no quote, margin or execution authority"
      actions={
        <div className="panel-actions">
          <Button
            disabled={cursor === 0}
            onClick={() => setCursor(Math.max(0, cursor - 50))}
          >
            Previous strikes
          </Button>
          <Button
            disabled={response?.next == null}
            onClick={() => setCursor(response?.next ?? cursor)}
          >
            Next strikes
          </Button>
        </div>
      }
    >
      {chain.state === "loading" || chain.state === "idle" ? (
        <p className="panel-note">Loading contract chain…</p>
      ) : chain.state === "unavailable" ? (
        <p className="panel-note">
          Contract chain unavailable · {words(response?.reason)}
        </p>
      ) : chain.state === "error" ? (
        <p className="panel-note">Contract chain request failed.</p>
      ) : (
        <>
          <Facts
            items={[
              ["Source", response?.source],
              ["Received", datetime(response?.received_at_ms)],
              ["Strikes", count(response?.total)],
              [
                "Contracts without usable strikes",
                count(response?.unavailable_strikes),
              ],
            ]}
          />
          <Table
            columns={columns}
            rows={response?.items || []}
            getKey={(row) => String(row.strike)}
            empty="No matching strikes were captured"
          />
          <p className="panel-note">
            Futures: {count(response?.futures_total)} recorded
            {response?.futures_truncated ? " · list truncated" : ""}
          </p>
          <Table
            columns={catalogColumns}
            rows={response?.futures || []}
            getKey={(row) => row.instrument_key}
            empty="No matching futures were captured"
          />
        </>
      )}
    </Panel>
  );
}
function columnsInstrument(): Column<ScannerRow> {
  return {
    key: "symbol",
    label: "Instrument",
    render: (r) => (
      <Instrument symbol={r.symbol} instrumentKey={r.instrument_key} />
    ),
  };
}
function SessionNotes({ date }: { date: string }) {
  const notes = useRead((api) => api.session(date), date),
    items = notes.value?.notes?.notes || [];
  return (
    <Panel
      title="Recorded operator notes"
      note="Read-only evidence recorded with this session. Notes are never written from this workspace."
    >
      {(notes.state === "idle" || notes.state === "loading") && (
        <p className="panel-note">Loading recorded notes…</p>
      )}
      {(notes.state === "error" || notes.state === "unavailable") && (
        <Notice error>Recorded notes are unavailable for this session.</Notice>
      )}
      {notes.state === "ready" && items.length === 0 && (
        <Empty>No operator notes were recorded for this session.</Empty>
      )}
      {items.length > 0 && (
        <div className="session-notes">
          {items.map((note) => (
            <article className="session-note" key={note.id}>
              <Status value={note.state}>{note.state}</Status>
              <p>{note.text}</p>
              <small>
                revision {note.revision} · by {note.updated_by || "host"}
                {note.keep ? " · keep" : ""}
              </small>
            </article>
          ))}
        </div>
      )}
    </Panel>
  );
}
function sessionMode(row: SessionRecord) {
  if (row.mode === "paper" || row.mode === "live")
    return row.mode.toUpperCase();
  if (row.paper_only === true && row.live_enabled === false) return "PAPER";
  if (row.paper_only === false && row.live_enabled === true) return "LIVE";
  return "Not reported";
}
export function Sessions() {
  const [state, app] = useWorkspace(),
    [query, setQuery] = useState(""),
    [selected, setSelected] = useState<SessionRecord | null>(null);
  const records = sessions(state, state.meta),
    rows = records.filter((r) =>
      [r.id?.date, r.id?.broker, r.id?.account, r.status, r.phase]
        .join(" ")
        .toLowerCase()
        .includes(query.toLowerCase()),
    );
  const f = finance(selected);
  const archiveUnavailable =
    state.sessionsState === "error" || state.sessionsState === "unavailable";
  return (
    <>
      <Panel
        title="Session archive"
        note="Dated books retain broker, account, disposition and original evidence."
        actions={
          <label className="search-field">
            <Search size={13} />
            <input
              type="search"
              placeholder="Find session or account…"
              aria-label="Search sessions"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
        }
      >
        {archiveUnavailable && (
          <Notice>
            Session archive projection{" "}
            {state.sessionsState === "unavailable" ? "unavailable" : "failed"}.
            {state.summary
              ? " The current checkpoint is still shown from the summary projection."
              : " No session rows are inferred."}
          </Notice>
        )}
        <Table
          getKey={sessionKey}
          rows={rows}
          onSelect={setSelected}
          columns={[
            {
              key: "id",
              label: "Session / account",
              render: (r) => (
                <Instrument symbol={r.id?.date} sub={r.id?.account} />
              ),
            },
            {
              key: "broker",
              label: "Broker",
              render: (r) => r.id?.broker || "Not reported",
            },
            {
              key: "mode",
              label: "Mode",
              render: sessionMode,
            },
            {
              key: "state",
              label: "Disposition",
              render: (r) => <Status value={r.status || r.phase} />,
            },
            {
              key: "evidence",
              label: "Evidence",
              render: (r) =>
                r.unavailable
                  ? "Unavailable"
                  : r.stale
                    ? "Archived checkpoint"
                    : r.portfolio
                      ? "Ledger snapshot"
                      : "Registry only",
            },
            {
              key: "net",
              label: "Net P&L · flat book",
              render: (r) => amount(finance(r).net),
            },
            {
              key: "open",
              label: "Positions",
              render: (r) =>
                finance(r).positionsKnown ? finance(r).positions.length : "—",
            },
          ]}
        />
        <div className="table-footer">
          <span>
            {count(state.sessions?.total ?? records.length)} supplied sessions
            {state.sessions?.next != null
              ? " · more archive pages available"
              : ""}
          </span>
          <span>Daily allocations are never summed as wealth.</span>
        </div>
      </Panel>
      <Drawer
        open={!!selected}
        onClose={() => setSelected(null)}
        title={"Session " + (selected?.id?.date || "")}
        description={[selected?.id?.broker, selected?.id?.account]
          .filter(Boolean)
          .join(" / ")}
      >
        <MetricStrip
          items={[
            ["Cash", amount(f.cash)],
            ["Net P&L · flat", amount(f.net)],
          ]}
        />
        <Facts
          items={[
            ["Disposition", words(selected?.status || selected?.phase)],
            [
              "Checkpoint at",
              datetime(selected?.as_of_ms || selected?.observed_at_ms),
            ],
            [
              "Stopped",
              selected?.portfolio
                ? words(selected.portfolio.stopped)
                : "Not reported",
            ],
            [
              "Halted",
              selected?.portfolio
                ? words(selected.portfolio.halted)
                : "Not reported",
            ],
            [
              "Closed",
              selected?.portfolio
                ? words(selected.portfolio.closed)
                : "Not reported",
            ],
            [
              "Evidence",
              selected?.portfolio
                ? "Ledger snapshot"
                : "Registry lifecycle only",
            ],
          ]}
        />
        {selected?.id?.date && <SessionNotes date={selected.id.date} />}
        <Button
          className="button-primary w-full mt-5"
          icon={ArrowUpRight}
          onClick={() => {
            app.selectSession(sessionKey(selected));
            location.hash = "#/portfolio";
          }}
        >
          Inspect this portfolio
        </Button>
      </Drawer>
    </>
  );
}
export function Strategies() {
  const [state] = useWorkspace(),
    [tab, setTab] = useState("references"),
    [selected, setSelected] = useState<StrategyReference | null>(null);
  const references = strategyReferences(state.scanner?.items);
  const library = state.summary?.research_library;
  return (
    <>
      <div className="strategy-header">
        <div>
          <span className="eyebrow">PROJECTED STRATEGY EVIDENCE</span>
          <h2>
            {references.length
              ? `${count(references.length)} recorded strategy reference${references.length === 1 ? "" : "s"}`
              : "No strategy reference supplied"}
          </h2>
          <p>
            The v1 scanner exposes prospect strategy IDs, versions, hashes, scan
            IDs and scores. The Library tab shows original registered-research
            versions and terminal outcomes for the current controller session.
            Strategy qualification and switch history remain separate.
          </p>
        </div>
        <div>
          <Status value={library?.status || "not_reported"}>
            Research library: {words(library?.status || "not_reported")}
          </Status>
          <a href="#/backtest" className="text-link">
            Open backtest lab <ArrowUpRight size={13} />
          </a>
        </div>
      </div>
      <Panel
        title="Research workspace"
        note="Registered analysis outcomes are research evidence. They do not establish executable strategies, accepted edges or activation authority."
        actions={
          <Segments
            value={tab}
            onChange={setTab}
            items={[
              ["references", "Strategy references"],
              ["library", "Library & switches"],
            ]}
          />
        }
      >
        {tab === "references" ? (
          <>
            {state.scannerState === "error" && (
              <Notice>
                Scanner projection failed; no strategy reference is inferred.
              </Notice>
            )}
            <Table
              rows={references}
              onSelect={(row) => setSelected(row)}
              columns={[
                {
                  key: "id",
                  label: "Strategy reference",
                  render: (r) => (
                    <Instrument
                      symbol={r.strategy_spec_id || "Spec ID not recorded"}
                      sub={
                        r.strategy_spec_version
                          ? "version " + r.strategy_spec_version
                          : "Version not recorded"
                      }
                    />
                  ),
                },
                {
                  key: "hash",
                  label: "Spec hash",
                  render: (r) => (
                    <code>{r.strategy_spec_hash || "Not recorded"}</code>
                  ),
                },
                {
                  key: "scan",
                  label: "Scan ID",
                  render: (r) => (
                    <code>{r.strategy_scan_id || "Not recorded"}</code>
                  ),
                },
                {
                  key: "score",
                  label: "Recorded score",
                  render: (r) =>
                    r.scores.length
                      ? r.scores.map((v) => count(v)).join(" / ")
                      : "Not recorded",
                },
                {
                  key: "instruments",
                  label: "Instruments",
                  render: (r) =>
                    r.instruments.length
                      ? `${count(r.instruments.length)} · ${r.instruments.slice(0, 2).join(", ")}${r.instruments.length > 2 ? "…" : ""}`
                      : "—",
                },
              ]}
              empty={
                state.scannerState === "unavailable"
                  ? "Scanner projection unavailable; no strategy references are inferred"
                  : "No strategy references recorded in the scanner projection"
              }
            />
          </>
        ) : (
          <>
            <p className="panel-note">
              Original registered-research ledger · current controller session.
              No automatic promotion. Strategy ABI, holding horizon and regime
              remain unknown unless separately qualified.
              {library?.truncated
                ? " The configured page/byte limit was reached."
                : ""}
            </p>
            <Table
              rows={library?.items || []}
              columns={[
                {
                  key: "candidate_id",
                  label: "Candidate",
                  render: (r) => <code>{r.candidate_id}</code>,
                },
                { key: "session", label: "Session" },
                {
                  key: "state",
                  label: "Outcome",
                  render: (r) => <Status value={r.state} />,
                },
                { key: "hypothesis", label: "Hypothesis" },
                {
                  key: "source_sha256",
                  label: "Source hash",
                  render: (r) => <code>{r.source_sha256 || "Unknown"}</code>,
                },
                {
                  key: "result_sha256",
                  label: "Result hash",
                  render: (r) => (
                    <code>{r.result_sha256 || "Not recorded"}</code>
                  ),
                },
                {
                  key: "scientific_state",
                  label: "Qualification",
                  render: (r) => <Status value={r.scientific_state} />,
                },
              ]}
              empty={
                library?.status === "unavailable"
                  ? "Original research library unavailable; no records reconstructed"
                  : "No registered-research candidates recorded"
              }
            />
          </>
        )}
      </Panel>
      <div className="panel-note">
        <GitBranch size={14} /> The {identityLabels.orchestrator} model handles
        strategy and reviews. Research evidence does not authorize automatic
        promotion.
      </div>
      <Drawer
        open={!!selected}
        onClose={() => setSelected(null)}
        title="Research artifact"
        description="Original supplied record · advisory evidence"
      >
        <pre>{JSON.stringify(selected, null, 2)}</pre>
      </Drawer>
    </>
  );
}
export function Backtest() {
  const [state, app] = useWorkspace(),
    [files, setFiles] = useState<Record<string, File | undefined>>({}),
    [input, setInput] = useState<ReplayPayload | null>(null),
    [result, setResult] = useState<ReplayResult | null>(null),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const alive = useRef(true);
  useEffect(
    () => () => {
      alive.current = false;
    },
    [],
  );
  async function run(example = false) {
    if (busy) return;
    setBusy(true);
    setError("");
    const token = state.token;
    try {
      let payload = input;
      if (example) {
        const v = await app.api.example();
        payload = {
          manifest: JSON.stringify(v.manifest),
          events: v.events,
          synthetic: true,
          label: v.label,
        };
        if (alive.current) setInput(payload);
      }
      if (!payload) {
        if (!files.manifest || !files.events)
          throw Error("Select both the manifest and original event file.");
        if (files.manifest.size > 524288 || files.events.size > 4194304)
          throw Error(
            "Existing preview limits: 512 KiB manifest and 4 MiB events.",
          );
        payload = {
          manifest: await files.manifest.text(),
          events: await files.events.text(),
          label: files.manifest.name,
        };
        payload.synthetic = (
          (JSON.parse(payload.manifest) as { universe?: InstrumentRecord[] })
            .universe || []
        ).some((v) => /^(SYNTHETIC|TEST)\|/.test(v.key || ""));
      }
      const v = await app.api.replay({
        manifest: payload.manifest,
        events: payload.events,
      });
      if (alive.current && token === app.getSnapshot().token)
        setResult({ ...v, synthetic: payload.synthetic, label: payload.label });
    } catch (e) {
      if (alive.current && token === app.getSnapshot().token)
        setError(words(errorMessage(e)) + " · No new result published.");
    } finally {
      if (alive.current) setBusy(false);
    }
  }
  function download() {
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(result, null, 2)], { type: "application/json" }),
    );
    const a = document.createElement("a");
    a.href = url;
    a.download = "nivra-replay-report.json";
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 0);
  }
  const points = (result?.equity_curve || [])
    .filter((p) => numeric(p.at) !== null && numeric(p.equity) !== null)
    .map((p) => ({ at: Number(p.at), value: Number(p.equity) }));
  return (
    <>
      <div className="lab-layout">
        <Panel
          title="Dataset"
          note="Reproduce original quote evidence with the existing deterministic replay engine."
          className="lab-input"
        >
          <div className="file-drop">
            <FileUp size={22} />
            <strong>Original manifest & event bytes</strong>
            <p>
              Files are read locally, then sent only to this dashboard's bounded
              replay endpoint.
            </p>
            {[
              ["manifest", "Manifest · JSON", ".json"],
              ["events", "Quote events · NDJSON", ".ndjson,.jsonl,.json"],
            ].map(([id, label, accept]) => (
              <label key={id} className="file-choice">
                <span>{label}</span>
                <input
                  type="file"
                  accept={accept}
                  aria-label={label}
                  onChange={(e) => {
                    setInput(null);
                    setFiles({ ...files, [id]: e.target.files?.[0] });
                  }}
                />
                <small>{files[id]?.name || "No file selected"}</small>
              </label>
            ))}
          </div>
          <div className="lab-run">
            <Button
              className="button-primary"
              onClick={() => run()}
              disabled={busy}
            >
              {busy ? "Validating & replaying…" : "Run offline replay"}
            </Button>
            <Button onClick={() => run(true)} disabled={busy}>
              Run synthetic example
            </Button>
          </div>
          <Notice error>{error}</Notice>
          <div className="panel-note">
            Local research only. Active paper workers and remote origins may
            refuse this endpoint. The v1 projections do not expose a separate
            replay-availability or fee-policy flag; the endpoint response stays
            authoritative.
          </div>
        </Panel>
        <div className="lab-results">
          <Panel
            title={
              result
                ? result.synthetic
                  ? "Synthetic demonstration"
                  : "Replay result · provenance unverified"
                : "Replay output"
            }
            actions={
              result && (
                <Button icon={Download} onClick={download}>
                  Export JSON
                </Button>
              )
            }
          >
            {result ? (
              <>
                <MetricStrip
                  items={[
                    [
                      "Net P&L · flat",
                      amount(result.report?.realized_pnl_if_flat),
                    ],
                    ["Drawdown", amount(result.report?.maximum_drawdown)],
                    ["Fees", amount(result.report?.fees)],
                    ["Events", count(result.report?.events)],
                  ]}
                />
                <Chart
                  points={points}
                  label="Replay equity"
                  source="Exact replay report timestamps"
                  emptyTitle="No equity observations in this report"
                  emptyDetail="The replay result did not supply enough time-aligned equity points."
                />
                <Ledger fills={result.report?.portfolio?.fills} />
              </>
            ) : (
              <Empty
                icon={Layers}
                title="Inspect the outcome, not just the return"
              >
                A replay will show its equity path, fills, fees and exact input
                fingerprints here.
              </Empty>
            )}
          </Panel>
          {result && (
            <Panel title="Reproducibility">
              <Facts
                items={[
                  ["Session", result.report?.session],
                  ["Dataset", result.label],
                  ["Engine hash", result.engine_hash],
                  ["Manifest hash", result.manifest_hash],
                  ["Event bytes hash", result.data_hash],
                  ["Receipt clock", result.report?.receipt_clock],
                  ["Limitations", result.report?.limitations?.join(" · ")],
                  ["Acceptance", "Research only · no strategy promotion"],
                ]}
              />
            </Panel>
          )}
        </div>
      </div>
    </>
  );
}
export function Health() {
  const [state] = useWorkspace(),
    health = useRead((api) => api.health(), "health"),
    alerts = useRead((api) => api.alerts({ limit: 100 }), "alerts"),
    s = state.summary,
    h =
      health.value?.history_preparation ||
      s?.readiness?.history ||
      s?.history_preparation,
    feed = health.value?.market_feed || s?.market_feed,
    readiness = health.value?.readiness || s?.readiness,
    roles = Array.isArray(state.approvals?.models?.roles)
      ? state.approvals.models.roles
      : [],
    attribution = health.value?.activity_attribution,
    approvalUnavailable =
      state.approvalsState === "error" ||
      state.approvalsState === "unavailable";
  return (
    <>
      <div className="health-summary">
        <div className="health-symbol">
          <ActivityIcon />
        </div>
        <div>
          <h2>Operating evidence</h2>
          <p>
            Transport, model approvals, feed quality and financial freshness are
            separate observations.
          </p>
        </div>
        <Status value={readiness?.status} />
      </div>
      <ModelRoster />
      <OperationalPanel
        summary={s}
        portfolio={state.portfolio}
        health={health.value}
      />
      {(health.state === "error" || health.state === "unavailable") && (
        <Notice>
          Health projection{" "}
          {health.state === "unavailable" ? "unavailable" : "failed"}. Only
          summary readiness and feed fields with explicit values are shown.
        </Notice>
      )}
      <div className="grid gap-4 lg:grid-cols-2">
        <Panel title="Market data">
          <Facts
            items={[
              ["Feed mode", words(feed?.mode)],
              ["Connection state", words(feed?.state)],
              ["Last tick age", duration(feed?.last_tick_age_ms)],
              ["Cached keys", count(feed?.cached_keys)],
              ["Reconnects", count(feed?.reconnects)],
              ["Fallback", feed ? words(feed.fallback) : "Not reported"],
              ["Reason", words(feed?.reason)],
            ]}
          />
        </Panel>
        <Panel title="History & readiness">
          <Facts
            items={[
              ["History status", words(h?.status)],
              [
                "Ready / universe",
                count(h?.ready_instruments) + " / " + count(h?.instruments),
              ],
              ["Target session", h?.target_session],
              ["Lifecycle", words(readiness?.lifecycle?.status)],
              ["Checked", datetime(readiness?.checked_at_ms)],
              ["Failure", words(h?.failure)],
              ["Provider-empty dates", "Remain partial; not counted as ready"],
            ]}
          />
        </Panel>
      </div>
      <Panel
        title="Activity attribution"
        note={
          attribution?.session
            ? `Controller report for ${attribution.session}; read-only paper activity and financial attribution. Net after-cost P&L subtracts fees; slippage is diagnostic because it is already reflected in fill prices. Drawdown coverage: ${words(attribution.drawdown_coverage?.reason)}`
            : "Read-only activity and financial attribution from the controller snapshot. Net after-cost P&L subtracts fees; slippage is diagnostic because it is already reflected in fill prices. Drawdown coverage is incomplete."
        }
      >
        {attribution ? (
          <>
            <div className="grid gap-4 lg:grid-cols-2">
              <Facts
                items={[
                  [
                    "Candidates evaluated",
                    count(attribution.conversion?.candidates_evaluated),
                  ],
                  [
                    "Decisions considered",
                    count(attribution.conversion?.decisions_considered),
                  ],
                  [
                    "Actions proposed / validated",
                    `${count(attribution.conversion?.actions_proposed)} / ${count(attribution.conversion?.actions_validated)}`,
                  ],
                  [
                    "Simulated fills",
                    count(attribution.conversion?.fills_executed),
                  ],
                  [
                    "Candidate → decision",
                    percent(attribution.conversion?.candidate_to_decision_rate),
                  ],
                  [
                    "Decision → action",
                    percent(attribution.conversion?.decision_to_action_rate),
                  ],
                  [
                    "Action → fill",
                    percent(attribution.conversion?.action_to_fill_rate),
                  ],
                  [
                    "Overall conversion",
                    percent(attribution.conversion?.overall_conversion_rate),
                  ],
                  [
                    "Abstentions",
                    count(attribution.abstentions?.total_abstentions),
                  ],
                  [
                    "Decision latency · p50 / p95",
                    `${count(attribution.latency?.p50_ms)} / ${count(attribution.latency?.p95_ms)} ms`,
                  ],
                ]}
              />
              <Facts
                items={[
                  [
                    "Realized P&L",
                    amount(attribution.financials?.realized_pnl),
                  ],
                  ["Marked P&L", amount(attribution.financials?.marked_pnl)],
                  ["Fees deducted", amount(attribution.financials?.total_fees)],
                  [
                    "Slippage · diagnostic",
                    amount(attribution.financials?.total_slippage),
                  ],
                  [
                    "Total cost drag · diagnostic",
                    amount(attribution.financials?.total_cost_drag),
                  ],
                  [
                    "Net after cost",
                    amount(attribution.financials?.net_after_cost_pnl),
                  ],
                  [
                    "Traded notional",
                    amount(attribution.turnover?.notional_traded),
                  ],
                  [
                    "Cost drag · bps · diagnostic",
                    count(attribution.turnover?.total_cost_drag_bps),
                  ],
                  [
                    "Current / max drawdown",
                    `${amount(attribution.drawdown?.current_drawdown)} / ${amount(attribution.drawdown?.max_drawdown)}`,
                  ],
                  [
                    "Maximum drawdown · %",
                    percentPoints(attribution.drawdown?.max_drawdown_pct),
                  ],
                ]}
              />
            </div>
            <Panel
              title="Experiment attribution"
              note="Recorded selection provenance and associated paper fills; empty rows mean no eligible source attribution was recorded."
              className="mt-4"
            >
              <Table
                rows={attribution.experiment_attributions || []}
                columns={[
                  {
                    key: "spec",
                    label: "Spec / source",
                    render: (row) => (
                      <Instrument
                        symbol={row.spec_id || "Spec not recorded"}
                        sub={row.source_hash || "Source hash not recorded"}
                      />
                    ),
                  },
                  {
                    key: "decisions",
                    label: "Decisions",
                    render: (row) => count(row.decisions_count),
                  },
                  {
                    key: "fills",
                    label: "Fills",
                    render: (row) => count(row.fills_count),
                  },
                  {
                    key: "net",
                    label: "Net after cost",
                    render: (row) => amount(row.net_after_cost_pnl),
                  },
                  {
                    key: "trial",
                    label: "Trial",
                    render: (row) => words(row.trial_status),
                  },
                ]}
                empty="No source-level experiment attribution recorded"
              />
            </Panel>
          </>
        ) : (
          <Empty title="Activity attribution unavailable">
            The loaded controller snapshot did not include an activity
            attribution report.
          </Empty>
        )}
      </Panel>
      <Panel
        title="Approval evidence"
        note="Two identities; digest and orchestrator usage remain separate."
      >
        {approvalUnavailable && (
          <Notice>
            Approvals projection{" "}
            {state.approvalsState === "unavailable" ? "unavailable" : "failed"}.
            Model identities remain visible without inferred allowances.
          </Notice>
        )}
        <Table
          rows={roles}
          columns={[
            {
              key: "role",
              label: "Identity / task",
              render: (v) => (
                <Instrument symbol={words(v.role)} sub={v.model} />
              ),
            },
            {
              key: "status",
              label: "Readiness",
              render: (v) => <Status value={v.status} />,
            },
            {
              key: "expiry",
              label: "Approval expiry",
              render: (v) => datetime(v.credential_check?.approval_expires_at),
            },
            {
              key: "remaining_current",
              label: "Requests remaining",
              render: (v) => count(v.remaining_current),
            },
            { key: "reason", label: "Reason", render: (v) => words(v.reason) },
          ]}
          empty={
            approvalUnavailable
              ? "Approval evidence unavailable"
              : "No role approvals supplied"
          }
        />
      </Panel>
      <Panel
        title="Derived alerts"
        note="Alerts are derived from the same recorded status view; they are not worker telemetry."
      >
        {alerts.state === "error" || alerts.state === "unavailable" ? (
          <Empty title="Alert projection unavailable">
            {alerts.state === "unavailable"
              ? words(alerts.value?.reason)
              : "The authenticated alerts read failed."}{" "}
            No alert count is inferred.
          </Empty>
        ) : (
          <Table
            rows={Array.isArray(alerts.value?.items) ? alerts.value.items : []}
            columns={[
              {
                key: "severity",
                label: "Severity",
                render: (v) => <Status value={v.severity} />,
              },
              { key: "message", label: "Alert" },
              {
                key: "source",
                label: "Source",
                render: (v) => words(v.source),
              },
              {
                key: "observed_at_ms",
                label: "Observed",
                render: (v) => datetime(v.observed_at_ms),
              },
            ]}
            empty="No derived alerts supplied"
          />
        )}
      </Panel>
      <div className="grid gap-4 lg:grid-cols-2">
        <Panel title="Financial protection">
          <Facts
            items={[
              ["Mode", state.meta?.mode || "Unknown"],
              [
                "Live activation",
                health.value?.live_enabled === true
                  ? "Enabled"
                  : health.value?.live_enabled === false
                    ? "Disabled"
                    : "Not reported",
              ],
              [
                "Market data broker",
                words(health.value?.broker?.data ?? state.meta?.broker?.data),
              ],
              [
                "Order broker",
                words(
                  health.value?.broker?.orders ?? state.meta?.broker?.orders,
                ),
              ],
              [
                "Dashboard authority",
                health.value?.execution_authority === false
                  ? "Display only"
                  : "Not reported",
              ],
              ["Execution gate", "Original source & receipt · 2 seconds"],
              ["Stopped / halted books", "Preserved; no dashboard controls"],
              ["Independent protection", "Observation not supplied"],
            ]}
          />
        </Panel>
        <Panel title="Unconnected telemetry">
          <Facts
            items={[
              ["Worker heartbeat", "A snapshot does not establish liveness"],
              ["Updater / tunnel", "Not supplied"],
              [
                "Disk / retention / request metrics",
                "No independent projection supplied",
              ],
              [
                "Health projection",
                health.state === "ready"
                  ? `${count(health.value?.alert_count)} alerts · /api/v1/health`
                  : health.state === "unavailable"
                    ? `Unavailable · ${words(health.value?.reason)}`
                    : "Failed or not loaded",
              ],
              [
                "Alert projection",
                alerts.state === "ready"
                  ? `${count(alerts.value?.total ?? alerts.value?.items?.length)} alerts · /api/v1/alerts`
                  : alerts.state === "unavailable"
                    ? `Unavailable · ${words(alerts.value?.reason)}`
                    : "Failed or not loaded",
              ],
              [
                "Historical price stream",
                health.state === "ready"
                  ? "Receipt-minute display series; not certified exchange OHLCV"
                  : "Series evidence loads from the authenticated API",
              ],
            ]}
          />
        </Panel>
      </div>
    </>
  );
}
function ActivityIcon() {
  return <Radio size={22} />;
}
export const PAGES: Record<
  string,
  React.ComponentType<{ selectedRun?: string }>
> = {
  overview: Overview,
  portfolio: Portfolio,
  scanner: Scanner,
  sessions: Sessions,
  strategies: Strategies,
  backtest: Backtest,
  health: Health,
  models: Models,
};
