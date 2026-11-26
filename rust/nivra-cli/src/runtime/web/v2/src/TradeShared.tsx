import React from "react";
import { amount, count, datetime, executionText, linkedPlan } from "./data";
import { instrumentName, plain } from "./labels";
import type { InstrumentRecord } from "./model";
import type { Fill, ScannerRow, StrategyReference } from "./types";
import type { Column } from "./ui";
import { Facts, InstrumentCell, ShortId, Status, Table } from "./ui";

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

export function Ledger({
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

/** Plan actions that match one position or fill. */
export function PlanActions({
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
export function StrategyLink({
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
