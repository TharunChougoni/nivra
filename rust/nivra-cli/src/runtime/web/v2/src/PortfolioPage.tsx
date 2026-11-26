import React, { useState } from "react";
import { ArrowUpRight } from "lucide-react";
import {
  amount,
  count,
  currentSession,
  datetime,
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
  strategyLinkFor,
} from "./data";
import { classLabel, instrumentName, plain, sentenceCase } from "./labels";
import { LivePriceChart } from "./LivePriceChart";
import { instrumentIndex, modeOf, orderTracks } from "./model";
import type { InstrumentRecord, OrderTrack } from "./model";
import { Ledger, PlanActions, StrategyLink } from "./TradeShared";
import type { Fill, PlansResponse, Position } from "./types";
import type { Column } from "./ui";
import {
  Drawer,
  Empty,
  Facts,
  InstrumentCell,
  KpiStrip,
  Notice,
  Panel,
  Segments,
  Status,
  Steps,
  Table,
  useWorkspace,
} from "./ui";

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
          <Plans plans={plans} fills={fills} index={index} />
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
        <LivePriceChart
          state={state}
          label={selected?.instrument?.symbol || "Position price"}
          seriesKey={selected?.instrument?.key || undefined}
          height={220}
          historical={historical}
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
  index,
}: {
  plans: PlansResponse | null;
  fills: Fill[] | null;
  index: Map<string, InstrumentRecord>;
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
