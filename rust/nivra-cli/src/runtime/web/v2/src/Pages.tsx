import type {
  Fill,
  Position,
  SessionRecord,
  PlansResponse,
  ScannerRow,
  StrategyReference,
  ReplayPayload,
  ReplayResult,
  HealthResponse,
  Instrument as UniverseInstrument,
} from "./types";
import { errorMessage } from "./types";
import type { Column } from "./ui";
import React, { useEffect, useRef, useState } from "react";
import {
  ArrowUpRight,
  FileUp,
  Download,
  Radio,
  Layers,
  Search,
} from "lucide-react";
import Chart from "./Chart";
import Lanes from "./Lanes";
import Instruments from "./Instruments";
import Overview from "./Overview";
import { useRead, useRevision } from "./reads";
import { instrumentIndex, modeOf, orderTracks } from "./model";
import type { InstrumentRecord, OrderTrack } from "./model";
import { OperationalPanel } from "./Operational";
import {
  amount,
  count,
  currentSession,
  datetime,
  duration,
  executionText,
  finance,
  fillsFor,
  instrumentClass,
  instrumentClasses,
  linkedFills,
  linkedPlan,
  lotQuantity,
  numeric,
  plansFor,
  sessionKey,
  sessions,
  strategyLinkFor,
  strategyLinks,
  strategyReferences,
} from "./data";
import { classLabel, instrumentName, plain, sentenceCase } from "./labels";
import {
  Button,
  Drawer,
  Empty,
  Facts,
  Instrument,
  InstrumentCell,
  KpiStrip,
  ShortId,
  Steps,
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
      <InstrumentCell record={v.instrument} fallback={v.instrument?.key} />
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
  { key: "units", label: "Units", num: true, render: (v) => count(v.units) },
  {
    key: "price",
    label: "Fill price",
    num: true,
    render: (v) => amount(v.price),
  },
  { key: "fee", label: "Fees", num: true, render: (v) => amount(v.fee) },
  {
    key: "decision_id",
    label: "Decision",
    render: (v) =>
      v.decision_id ? <code>{v.decision_id}</code> : "Not recorded",
  },
  {
    key: "at",
    label: "Time (IST)",
    num: true,
    render: (v) => datetime(v.at),
  },
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
      empty={empty || (fills === null ? "Fills unavailable" : "No fills")}
    />
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
/** Plan actions that match one position or fill. */
function PlanActions({
  linked,
  missing,
  index,
}: {
  linked: ReturnType<typeof linkedPlan>;
  missing: string;
  index: Map<string, InstrumentRecord>;
}) {
  if (linked === null)
    return <p className="panel-note">Plan data unavailable.</p>;
  if (!linked.actions.length) return <p className="panel-note">{missing}</p>;
  return (
    <Table
      rows={linked.actions}
      columns={[
        { key: "index", label: "Action" },
        {
          key: "key",
          label: "Instrument",
          render: (v) => instrumentName(v.action?.instrument_key, index),
        },
        { key: "side", label: "Side", render: (v) => plain(v.action?.side) },
        { key: "units", label: "Units", render: (v) => count(v.units) },
        {
          key: "reason",
          label: "Validation",
          render: (v) => <Status value={v.reason} />,
        },
        {
          key: "execution",
          label: "Execution",
          render: (v) => executionText(v.execution) ?? plain(null),
        },
      ]}
    />
  );
}
/** The strategy a scanner row came from, or why there is none. */
function StrategyLink({
  link,
  none,
}: {
  link: StrategyReference | ScannerRow | null;
  none: string;
}) {
  if (!link) return <p className="panel-note">{none}</p>;
  return (
    <Facts
      items={[
        ["Strategy", plain(link.strategy_spec_id)],
        ["Version", link.strategy_spec_version],
        ["Spec hash", <ShortId key="h" value={link.strategy_spec_hash} />],
        ["Scan", link.strategy_scan_id],
        ["Score", count(link.strategy_score)],
      ]}
    />
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
    historical = sessionKey(r) !== sessionKey(current),
    mode = modeOf(state.meta, state.summary);
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
        <InstrumentCell record={p.instrument} fallback={p.instrument?.key} />
      ),
    },
    { key: "units", label: "Units", num: true, render: (p) => count(p.units) },
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
    { key: "entry", label: "Entry", num: true, render: (p) => amount(p.entry) },
    { key: "mark", label: "Mark", num: true, render: (p) => amount(p.mark) },
    {
      key: "value",
      label: "Value",
      num: true,
      render: (p) =>
        numeric(p.mark) !== null && numeric(p.units) !== null
          ? amount(Number(p.mark) * Number(p.units))
          : "—",
    },
    {
      key: "pnl",
      label: "P&L",
      num: true,
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
      label: "Marked at",
      render: (p) => datetime(p.marked_at),
    },
  ];
  const index = instrumentIndex({
      positions: f.positions,
      fills,
      scanner: state.scanner?.items,
    }),
    linked = linkedPlan(
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
      <KpiStrip
        items={[
          ["Account equity", amount(f.equity), "Latest checkpoint"],
          ["Cash", amount(f.cash), "Recorded balance"],
          ["Holdings", amount(f.exposure), "At marked prices"],
          ["Fees", amount(f.fees), "Paid so far"],
        ]}
      />
      <Panel
        title="Portfolio"
        actions={
          <Segments
            value={tab}
            onChange={setTab}
            items={[
              ["positions", "Positions"],
              ["orders", "Orders"],
              ["fills", "Fills"],
              ["plans", "Plans"],
            ]}
          />
        }
      >
        {(state.portfolioState === "error" ||
          state.portfolioState === "unavailable") && (
          <Notice>
            Portfolio data{" "}
            {state.portfolioState === "unavailable"
              ? "unavailable"
              : "could not be read"}
            . Positions and fees are unknown.
          </Notice>
        )}
        {(state.fillsState === "error" ||
          state.fillsState === "unavailable") && (
          <Notice>
            Fill data{" "}
            {state.fillsState === "unavailable"
              ? "unavailable"
              : "could not be read"}
            . Fills and fee totals are unknown.
          </Notice>
        )}
        {tab === "plans" ? (
          <Plans plans={plans} fills={fills} />
        ) : tab === "orders" ? (
          <OrderLifecycle
            tracks={orderTracks(plans, fills, mode)}
            index={index}
            paper={mode.kind === "paper"}
            historical={historical}
          />
        ) : (
          <>
            <div className="filter-bar">
              <div className="table-controls">
                <span>
                  {tab === "fills"
                    ? `${count(fills?.length)} fills`
                    : `${count(f.positionsKnown ? f.positions.length : null)} positions`}
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
              ) : null}
            </div>
            {tab === "positions" ? (
              <Table
                getKey={(p) => p.instrument?.key}
                rows={positions}
                columns={columns}
                empty={
                  f.positionsKnown
                    ? activeKind === "all"
                      ? "No open positions"
                      : "No positions in this class"
                    : "Positions unavailable"
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
                  fills === null ? "Fills unavailable" : "No matching fills"
                }
              />
            )}
          </>
        )}
      </Panel>
      <p className="footnote">
        Marks are not fill prices, and an acknowledgement is not a fill.
      </p>
      <Drawer
        open={!!selected}
        onClose={() => setSelected(null)}
        title={selected?.instrument?.symbol || "Position"}
        description={instrumentName(selected?.instrument?.key, index)}
      >
        <Facts
          items={[
            ["Session", r?.id?.date],
            [
              "Broker / account",
              [r?.id?.broker, r?.id?.account].filter(Boolean).join(" / "),
            ],
            ["Class", classLabel(instrumentClass(selected))],
            ["Segment", plain(selected?.instrument?.segment)],
            ["Underlying", selected?.instrument?.underlying],
            ["Expiry", selected?.instrument?.expiry],
            ["Strike", amount(selected?.instrument?.strike)],
            ["Lots", count(lotQuantity(selected)?.lots)],
            ["Lot size", count(lotQuantity(selected)?.lot_size)],
            [
              "Margin",
              selected?.margin?.display_only === true &&
              selected.margin.instrument_key === selected.instrument?.key
                ? amount(selected.margin.total_margin)
                : "Not reported",
            ],
            ["Units", count(selected?.units)],
            ["Entry", amount(selected?.entry)],
            ["Mark", amount(selected?.mark)],
            ["Marked at", datetime(selected?.marked_at)],
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
              ? "Cached minute bars, may be stale"
              : state.series?.price?.length
                ? "Minute bars from received quotes"
                : "Seen in this tab"
          }
        />
        <Panel title="Plan">
          <PlanActions
            linked={linked}
            index={index}
            missing={
              selected?.decision_id && selected?.instrument?.key
                ? "No matching plan action."
                : "No decision recorded for this position."
            }
          />
        </Panel>
        <Panel title="Strategy">
          <StrategyLink
            link={strategy}
            none={
              historical
                ? "Archives keep no strategy reference."
                : state.scanner?.unavailable || state.scannerState === "error"
                  ? "Scanner data unavailable."
                  : "No strategy recorded for this instrument."
            }
          />
        </Panel>
        <Panel title="Related fills">
          <Ledger
            fills={linkedFillRows}
            empty={
              linkedFillRows === null
                ? "Fills unavailable"
                : selected?.decision_id && selected?.instrument?.key
                  ? "No fills match this decision"
                  : "No decision recorded for this position"
            }
          />
        </Panel>
      </Drawer>
      <Drawer
        open={!!selectedFill}
        onClose={() => setSelectedFill(null)}
        title={selectedFill?.instrument?.symbol || "Fill"}
        description={instrumentName(selectedFill?.instrument?.key, index)}
      >
        <Facts
          items={[
            ["Session", r?.id?.date],
            [
              "Broker / account",
              [r?.id?.broker, r?.id?.account].filter(Boolean).join(" / "),
            ],
            ["Class", classLabel(instrumentClass(selectedFill))],
            ["Segment", plain(selectedFill?.instrument?.segment)],
            ["Lots", count(lotQuantity(selectedFill)?.lots)],
            ["Lot size", count(lotQuantity(selectedFill)?.lot_size)],
            ["Side", selectedFill ? (selectedFill.buy ? "Buy" : "Sell") : null],
            ["Units", count(selectedFill?.units)],
            ["Fill price", amount(selectedFill?.price)],
            ["Expected price", amount(selectedFill?.expected_price)],
            ["Slippage (bps)", count(selectedFill?.slippage_bps)],
            ["Fee", amount(selectedFill?.fee)],
            ["Decision", selectedFill?.decision_id || "Not recorded"],
            ["Time", datetime(selectedFill?.at)],
          ]}
        />
        <Panel title="Plan">
          <PlanActions
            linked={fillLinked}
            index={index}
            missing={
              selectedFill?.decision_id && selectedFill?.instrument?.key
                ? "No matching plan action."
                : "No decision recorded for this fill."
            }
          />
        </Panel>
        <Panel title="Strategy">
          <StrategyLink
            link={fillStrategy}
            none={
              historical
                ? "Archives keep no strategy reference."
                : state.scanner?.unavailable || state.scannerState === "error"
                  ? "Scanner data unavailable."
                  : "No strategy recorded for this instrument."
            }
          />
        </Panel>
      </Drawer>
    </>
  );
}
function OrderLifecycle({
  tracks,
  index,
  paper,
  historical,
}: {
  tracks: OrderTrack[];
  index: Map<string, InstrumentRecord>;
  paper: boolean;
  historical: boolean;
}) {
  return (
    <>
      <Table
        rows={tracks}
        getKey={(t, i) => `${t.decisionId}:${t.instrument}:${t.at_ms}:${i}`}
        empty={
          historical ? "Archives keep fills only" : "No plan actions or fills"
        }
        columns={[
          {
            key: "at",
            label: "Time (IST)",
            num: true,
            render: (t) => (t.at_ms ? datetime(t.at_ms) : "—"),
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
              <span className={t.side === "buy" ? "buy-text" : "sell-text"}>
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
            render: (t) => sentenceCase(t.outcome),
          },
          {
            key: "decision",
            label: "Decision",
            render: (t) =>
              t.decisionId ? <code>{t.decisionId}</code> : "Not recorded",
          },
        ]}
      />
      <div className="panel-foot">
        {paper ? "Dashed stages are simulated. " : ""}An acknowledgement is not
        a fill.
      </div>
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
  if (!plans) return <Empty title="Plans unavailable" />;
  if (plans.unavailable)
    return <Empty title="Plans unavailable">{plain(plans.reason)}</Empty>;
  const p = plans.planner;
  return (
    <>
      <div className="validation-summary">
        <Status value={p?.status} />
        <span>
          Decision <code>{p?.decision_id || "not reported"}</code>
        </span>
        <span>{count(p?.pending_events)} pending</span>
        <a href="#/lanes" className="text-link">
          See lane turns <ArrowUpRight size={12} />
        </a>
      </div>
      <Table
        rows={plans.actions || []}
        columns={[
          { key: "index", label: "Action" },
          {
            key: "key",
            label: "Instrument",
            render: (v) => instrumentName(v.action?.instrument_key),
          },
          { key: "side", label: "Side", render: (v) => plain(v.action?.side) },
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
              executionText(
                plans.execution?.find(
                  (e) =>
                    e.index === v.index && e.decision_id === p?.decision_id,
                ),
              ) ?? plain(null),
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
        empty="No proposed actions"
      />
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
            label: "Price",
            num: true,
            render: (r) =>
              amount(r.latest_screening_price ?? r.latest_sampled_bid),
          },
          {
            key: "sample_change_bps",
            label: "Change (bps)",
            num: true,
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
            num: true,
            render: (r) => count(r.sample_count),
          },
          {
            key: "observed_ms",
            label: "Received",
            num: true,
            render: (r) => datetime(r.observed_ms),
          },
          {
            key: "strategy_spec_id",
            label: "Strategy",
            render: (r) =>
              strategyLinks([r]).length
                ? plain(r.strategy_spec_id || "recorded")
                : "Not recorded",
          },
          {
            key: "screening_reason",
            label: "Status",
            render: (r) => (
              <span className="caption">{plain(r.screening_reason)}</span>
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
            render: (r) => plain(r.diagnostic),
          },
          {
            key: "last_observed_ms",
            label: "Last received",
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
        ? "Screen results unavailable"
        : "No screen results"
      : sourceUnavailable || sourceState === "error"
        ? tab === "missing"
          ? "Stale and missing data unavailable"
          : "Scanner unavailable"
        : "No matches";
  return (
    <>
      <KpiStrip
        items={[
          [
            "Scope",
            [
              coverage?.market_segment && plain(coverage.market_segment),
              classLabel(coverage?.instrument_kind),
            ]
              .filter(Boolean)
              .join(" · "),
          ],
          ["Universe", count(coverage?.universe), "Instruments tracked"],
          ["Fresh", count(coverage?.fresh), "Recent quote"],
          ["Stale", count(coverage?.stale), "Old quote kept"],
          ["Unseen", count(coverage?.unseen), "No quote yet"],
        ]}
      />
      <Panel
        title="Scanner"
        actions={<Status value={state.summary?.market_feed?.state} />}
      >
        <div className="filter-bar">
          <Segments
            value={tab}
            onChange={setTab}
            items={[
              ["prospects", "Picks"],
              ["missing", "Stale or missing"],
              ["screens", "Screen results"],
            ]}
          />
          {tab !== "screens" && (
            <div className="row">
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
                <option value="priority">Scanner order</option>
                <option value="change">Biggest move</option>
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
                label: "Result",
                render: (r) => plain(r.reason),
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
              ? `${count(screenRows.length)} results`
              : `${count(coverage?.presented ?? rows.length)} shown · ${count(coverage?.eligible)} eligible${
                  tab === "prospects" && coverage?.unavailable_truncated
                    ? " · missing rows cut off"
                    : ""
                }`}
          </span>
        </div>
      </Panel>
      <Drawer
        open={!!selected}
        onClose={() => setSelected(null)}
        title={selected?.symbol || "Instrument"}
        description={instrumentName(selected?.instrument_key)}
      >
        <div className="instrument-summary">
          <strong>
            {amount(
              selected?.latest_screening_price ?? selected?.latest_sampled_bid,
            )}
          </strong>
          <span>{count(selected?.sample_change_bps)} bps change</span>
          <Status value={selected?.screening_reason || selected?.reason} />
        </div>
        <Chart
          label={selected?.symbol || "Price"}
          points={
            state.series?.price?.length ? state.series.price : state.points
          }
          source={
            state.series?.priceCached && state.series?.price?.length
              ? "Cached minute bars, may be stale"
              : state.series?.price?.length
                ? "Minute bars from received quotes"
                : "Seen in this tab"
          }
          height={250}
        />
        <Facts
          items={[
            ["Received", datetime(selected?.observed_ms)],
            [
              "Price time",
              datetime(selected?.sampled_price_ms ?? selected?.sampled_bid_ms),
            ],
            ["Source time", datetime(selected?.source_ms)],
            ["Price type", plain(selected?.price_kind)],
            ["Samples", count(selected?.sample_count)],
            ["Diagnostic", plain(selected?.diagnostic)],
          ]}
        />
        <Panel title="Strategy">
          <StrategyLink
            link={strategy}
            none={
              sourceUnavailable
                ? "Scanner data unavailable."
                : "No strategy recorded for this instrument."
            }
          />
        </Panel>
        <details className="raw-evidence">
          <summary>Raw data</summary>
          <pre>{JSON.stringify(selected, null, 2)}</pre>
        </details>
      </Drawer>
    </>
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
const yesNo = (v: unknown) => (v == null ? "Not reported" : v ? "Yes" : "No");
function sessionMode(row: SessionRecord) {
  if (row.mode === "paper" || row.mode === "live") return plain(row.mode);
  if (row.paper_only === true && row.live_enabled === false) return "Paper";
  if (row.paper_only === false && row.live_enabled === true) return "Live";
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
        title="Sessions"
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
            Session archive{" "}
            {state.sessionsState === "unavailable"
              ? "unavailable"
              : "could not be read"}
            .{state.summary ? " Showing the current session only." : ""}
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
              label: "Status",
              render: (r) => <Status value={r.status || r.phase} />,
            },
            {
              key: "evidence",
              label: "Evidence",
              render: (r) =>
                r.unavailable
                  ? "Unavailable"
                  : r.stale
                    ? "Archived"
                    : r.portfolio
                      ? "Full ledger"
                      : "Listed only",
            },
            {
              key: "net",
              label: "Net P&L",
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
            {count(state.sessions?.total ?? records.length)} sessions
            {state.sessions?.next != null ? " · more available" : ""}
          </span>
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
        <KpiStrip
          items={[
            ["Cash", amount(f.cash)],
            ["Net P&L", amount(f.net)],
          ]}
        />
        <Facts
          items={[
            ["Status", plain(selected?.status || selected?.phase)],
            [
              "Checkpoint",
              datetime(selected?.as_of_ms || selected?.observed_at_ms),
            ],
            [
              "Stopped",
              selected?.portfolio
                ? yesNo(selected.portfolio.stopped)
                : "Not reported",
            ],
            [
              "Halted",
              selected?.portfolio
                ? yesNo(selected.portfolio.halted)
                : "Not reported",
            ],
            [
              "Closed",
              selected?.portfolio
                ? yesNo(selected.portfolio.closed)
                : "Not reported",
            ],
            ["Evidence", selected?.portfolio ? "Full ledger" : "Listed only"],
          ]}
        />
        <Button
          className="button-primary button-block"
          icon={ArrowUpRight}
          onClick={() => {
            app.selectSession(sessionKey(selected));
            location.hash = "#/portfolio";
          }}
        >
          Open portfolio
        </Button>
      </Drawer>
    </>
  );
}
export function Strategies() {
  const [state] = useWorkspace(),
    [tab, setTab] = useState("references"),
    [selected, setSelected] = useState<StrategyReference | null>(null);
  const references = strategyReferences(state.scanner?.items),
    index = instrumentIndex({ scanner: state.scanner?.items });
  const library = state.summary?.research_library;
  return (
    <>
      <div className="strategy-header">
        <div>
          <h2>
            {references.length
              ? `${count(references.length)} ${references.length === 1 ? "strategy" : "strategies"} behind today's picks`
              : "No strategies recorded"}
          </h2>
        </div>
        <div>
          <Status value={library?.status || "not_reported"}>
            Research library: {plain(library?.status || "not_reported")}
          </Status>
          <a href="#/backtest" className="text-link">
            Open backtest lab <ArrowUpRight size={13} />
          </a>
        </div>
      </div>
      <Panel
        title="Research"
        actions={
          <Segments
            value={tab}
            onChange={setTab}
            items={[
              ["references", "In use"],
              ["library", "Library"],
            ]}
          />
        }
      >
        {tab === "references" ? (
          <>
            {state.scannerState === "error" && (
              <Notice>Scanner data could not be read.</Notice>
            )}
            <Table
              rows={references}
              onSelect={(row) => setSelected(row)}
              columns={[
                {
                  key: "id",
                  label: "Strategy",
                  render: (r) => (
                    <Instrument
                      symbol={
                        r.strategy_spec_id
                          ? plain(r.strategy_spec_id)
                          : "Not recorded"
                      }
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
                  render: (r) => <ShortId value={r.strategy_spec_hash} />,
                },
                {
                  key: "scan",
                  label: "Scan",
                  render: (r) => (
                    <code>{r.strategy_scan_id || "Not recorded"}</code>
                  ),
                },
                {
                  key: "score",
                  label: "Score",
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
                      ? `${count(r.instruments.length)} · ${r.instruments
                          .slice(0, 2)
                          .map((k) => instrumentName(k, index))
                          .join(", ")}${r.instruments.length > 2 ? "…" : ""}`
                      : "—",
                },
              ]}
              empty={
                state.scannerState === "unavailable"
                  ? "Scanner unavailable"
                  : "No strategies recorded"
              }
            />
          </>
        ) : (
          <>
            {library?.truncated && (
              <p className="panel-note">
                Limit reached. Some rows are missing.
              </p>
            )}
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
                  label: "Source",
                  render: (r) => <ShortId value={r.source_sha256} />,
                },
                {
                  key: "result_sha256",
                  label: "Result",
                  render: (r) => <ShortId value={r.result_sha256} />,
                },
                {
                  key: "scientific_state",
                  label: "Qualification",
                  render: (r) => <Status value={r.scientific_state} />,
                },
              ]}
              empty={
                library?.status === "unavailable"
                  ? "Research library unavailable"
                  : "No research candidates yet"
              }
            />
          </>
        )}
      </Panel>
      <Drawer
        open={!!selected}
        onClose={() => setSelected(null)}
        title="Strategy details"
      >
        {selected && (
          <Facts
            items={[
              ["Strategy", plain(selected.strategy_spec_id)],
              ["Version", selected.strategy_spec_version],
              [
                "Spec hash",
                <ShortId key="h" value={selected.strategy_spec_hash} />,
              ],
              ["Scan", selected.strategy_scan_id],
              ["Score", selected.scores.map((v) => count(v)).join(" / ")],
              [
                "Instruments",
                selected.instruments
                  .map((k) => instrumentName(k, index))
                  .join(", "),
              ],
            ]}
          />
        )}
        <details className="raw-evidence">
          <summary>Raw data</summary>
          <pre>{JSON.stringify(selected, null, 2)}</pre>
        </details>
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
        if (!files.manifest || !files.events) throw Error("Choose both files.");
        if (files.manifest.size > 524288 || files.events.size > 4194304)
          throw Error(
            "File too large. Limits: 512 KiB manifest, 4 MiB events.",
          );
        payload = {
          manifest: await files.manifest.text(),
          events: await files.events.text(),
          label: files.manifest.name,
        };
        payload.synthetic = (
          (JSON.parse(payload.manifest) as { universe?: UniverseInstrument[] })
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
        setError(plain(errorMessage(e)) + " · Result not updated.");
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
        <Panel title="Replay files" className="lab-input">
          <div className="file-drop">
            <FileUp size={22} />
            <strong>Manifest and quote events</strong>
            <p>Read in your browser, sent to this dashboard only.</p>
            {[
              ["manifest", "Manifest (JSON)", ".json"],
              ["events", "Quote events (NDJSON)", ".ndjson,.jsonl,.json"],
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
              {busy ? "Replaying…" : "Run replay"}
            </Button>
            <Button onClick={() => run(true)} disabled={busy}>
              Run example
            </Button>
          </div>
          <Notice error>{error}</Notice>
          <div className="panel-note">
            Local use only. A running paper session may refuse a replay.
          </div>
        </Panel>
        <div className="lab-results">
          <Panel
            title={
              result
                ? result.synthetic
                  ? "Example result"
                  : "Replay result"
                : "Result"
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
                <KpiStrip
                  items={[
                    ["Net P&L", amount(result.report?.realized_pnl_if_flat)],
                    ["Drawdown", amount(result.report?.maximum_drawdown)],
                    ["Fees", amount(result.report?.fees)],
                    ["Events", count(result.report?.events)],
                  ]}
                />
                <Chart
                  points={points}
                  label="Replay equity"
                  source="Replay timestamps"
                  emptyTitle="No equity data"
                  emptyDetail="The replay returned too few equity points."
                />
                <Ledger fills={result.report?.portfolio?.fills} />
              </>
            ) : (
              <Empty icon={Layers} title="No replay yet">
                Run a replay to see equity, fills and fees.
              </Empty>
            )}
          </Panel>
          {result && (
            <Panel title="Reproducibility">
              <Facts
                items={[
                  ["Session", result.report?.session],
                  ["Dataset", result.label],
                  ["Engine", <ShortId key="e" value={result.engine_hash} />],
                  [
                    "Manifest",
                    <ShortId key="m" value={result.manifest_hash} />,
                  ],
                  ["Events", <ShortId key="d" value={result.data_hash} />],
                  [
                    "Clock",
                    result.report?.receipt_clock
                      ? plain(result.report.receipt_clock)
                      : null,
                  ],
                  [
                    "Limitations",
                    result.report?.limitations
                      ?.map((l) => plain(l))
                      .join(" · "),
                  ],
                ]}
              />
            </Panel>
          )}
        </div>
      </div>
    </>
  );
}
function LiveReadiness({ health }: { health: HealthResponse | null }) {
  const [state] = useWorkspace();
  const mode = modeOf(state.meta, state.summary);
  const liveEnabled =
    health?.live_enabled ??
    state.summary?.live_enabled ??
    state.meta?.live_enabled;
  return (
    <Panel title="Live readiness">
      <Facts
        items={[
          [
            "Mode",
            mode.kind === "paper"
              ? "Paper"
              : mode.kind === "live"
                ? "Live"
                : "Unknown",
          ],
          [
            "Order broker",
            mode.kind === "live"
              ? mode.orderBroker
              : state.meta?.broker?.orders || "Not reported",
          ],
          ["Data broker", state.meta?.broker?.data || "Not reported"],
          [
            "Account",
            (mode.kind === "live" ? mode.account : state.meta?.account) ||
              "Not reported",
          ],
          [
            "Live trading",
            liveEnabled === undefined || liveEnabled === null
              ? "Not reported"
              : liveEnabled
                ? "On"
                : "Off",
          ],
          ["Paper only", yesNo(state.meta?.paper_only)],
          ["This dashboard", "Read only"],
        ]}
      />
    </Panel>
  );
}
export function Health() {
  const [state] = useWorkspace(),
    // A stalled controller publishes no new revision; reread its liveness.
    health = useRead(
      (api) => api.health(),
      useRevision("health") + (state.stalled ? "|stalled" : ""),
    ),
    alerts = useRead(
      (api) => api.alerts({ limit: 100 }),
      useRevision("alerts"),
    ),
    s = state.summary,
    h =
      health.value?.history_preparation ||
      s?.readiness?.history ||
      s?.history_preparation,
    // The summary's feed block refreshes at least every 5 s; health's only
    // when its content changes, so its ages would hold still.
    feed = s?.market_feed || health.value?.market_feed,
    readiness = health.value?.readiness || s?.readiness,
    attribution = health.value?.activity_attribution;
  return (
    <>
      <div className="health-summary">
        <div className="health-symbol">
          <ActivityIcon />
        </div>
        <div>
          <h2>System health</h2>
        </div>
        <Status value={readiness?.status} />
      </div>
      <OperationalPanel
        summary={s}
        portfolio={state.portfolio}
        health={health.value}
      />
      {(health.state === "error" || health.state === "unavailable") && (
        <Notice>
          Health data{" "}
          {health.state === "unavailable" ? "unavailable" : "could not be read"}
          .
        </Notice>
      )}
      <div className="cols-2">
        <Panel title="Market data">
          <Facts
            items={[
              ["Mode", plain(feed?.mode)],
              ["Connection", plain(feed?.state)],
              ["Last quote age", duration(feed?.last_tick_age_ms)],
              ["Cached instruments", count(feed?.cached_keys)],
              ["Reconnects", count(feed?.reconnects)],
              ["Fallback", feed ? yesNo(feed.fallback) : "Not reported"],
              ["Reason", plain(feed?.reason)],
            ]}
          />
        </Panel>
        <Panel title="History">
          <Facts
            items={[
              ["Status", plain(h?.status)],
              [
                "Ready / universe",
                count(h?.ready_instruments) + " / " + count(h?.instruments),
              ],
              ["Target session", h?.target_session],
              ["Lifecycle", plain(readiness?.lifecycle?.status)],
              ["Checked", datetime(readiness?.checked_at_ms)],
              ["Failure", plain(h?.failure)],
            ]}
          />
        </Panel>
      </div>
      <Panel
        title="Activity"
        note={
          attribution?.session ? `Session ${attribution.session}` : undefined
        }
      >
        {attribution ? (
          <>
            <div className="cols-2">
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
                    "Decision time (typical / slow)",
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
                  ["Slippage", amount(attribution.financials?.total_slippage)],
                  [
                    "Cost drag",
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
                    "Cost drag (bps)",
                    count(attribution.turnover?.total_cost_drag_bps),
                  ],
                  [
                    "Current / max drawdown",
                    `${amount(attribution.drawdown?.current_drawdown)} / ${amount(attribution.drawdown?.max_drawdown)}`,
                  ],
                  [
                    "Max drawdown (%)",
                    percentPoints(attribution.drawdown?.max_drawdown_pct),
                  ],
                  ...(attribution.drawdown_coverage?.reason
                    ? [
                        [
                          "Drawdown coverage",
                          plain(attribution.drawdown_coverage.reason),
                        ] as [string, string],
                      ]
                    : []),
                ]}
              />
            </div>
            <Panel title="By strategy" className="stack-top">
              <Table
                rows={attribution.experiment_attributions || []}
                columns={[
                  {
                    key: "spec",
                    label: "Strategy",
                    render: (row) => (
                      <Instrument
                        symbol={
                          row.spec_id ? plain(row.spec_id) : "Not recorded"
                        }
                        sub={<ShortId value={row.source_hash} />}
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
                    label: "Net P&L",
                    render: (row) => amount(row.net_after_cost_pnl),
                  },
                  {
                    key: "trial",
                    label: "Trial",
                    render: (row) => plain(row.trial_status),
                  },
                ]}
                empty="No strategy attribution recorded"
              />
            </Panel>
          </>
        ) : (
          <Empty title="Activity unavailable" />
        )}
      </Panel>
      <Panel title="Alerts">
        {alerts.state === "error" || alerts.state === "unavailable" ? (
          <Empty title="Alerts unavailable">
            {alerts.state === "unavailable"
              ? plain(alerts.value?.reason)
              : "Could not read alerts."}
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
                render: (v) => plain(v.source),
              },
              {
                key: "observed_at_ms",
                label: "Observed",
                render: (v) => datetime(v.observed_at_ms),
              },
            ]}
            empty="No alerts"
          />
        )}
      </Panel>
      <LiveReadiness health={health.value} />
      <div className="cols-2">
        <Panel title="Other checks">
          <Facts
            items={[
              ["Quote freshness limit", "2 seconds from receipt"],
              ["Protection check", "Not reported"],
              ["Worker heartbeat", "Not reported"],
              ["Updater / tunnel", "Not reported"],
              ["Disk and request metrics", "Not reported"],
              [
                "Health data",
                health.state === "ready"
                  ? `${count(health.value?.alert_count)} alerts`
                  : health.state === "unavailable"
                    ? `Unavailable · ${plain(health.value?.reason)}`
                    : "Not loaded",
              ],
              [
                "Alert data",
                alerts.state === "ready"
                  ? `${count(alerts.value?.total ?? alerts.value?.items?.length)} alerts`
                  : alerts.state === "unavailable"
                    ? `Unavailable · ${plain(alerts.value?.reason)}`
                    : "Not loaded",
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
export const PAGES: Record<string, React.ComponentType> = {
  overview: Overview,
  instruments: Instruments,
  portfolio: Portfolio,
  scanner: Scanner,
  sessions: Sessions,
  strategies: Strategies,
  backtest: Backtest,
  health: Health,
  lanes: Lanes,
};
