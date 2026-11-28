// The paper controller's strategy executor: which strategy runs on each scan,
// whether research accepted it or it is a shipped fallback, what it asked
// for, what filled and what the gate refused.
import type React from "react";
import { amount, count, datetime } from "./data";
import {
  REFERENCE_STRATEGY_LABELS,
  STRATEGY_LABELS,
  SHADOW_LABELS,
  instrumentName,
  plain,
  sentenceCase,
  strategyOutcome,
} from "./labels";
import type { ForwardShadow, ShadowOutcome, StrategyExecution } from "./types";
import { Empty, Facts, Panel, ShortId, Status, Table } from "./ui";

type Fact = [string, React.ReactNode];

function ForwardTest({ report }: { report: ForwardShadow }) {
  const labels = SHADOW_LABELS;
  const summary = report.summary;
  const rows = [
    { label: labels.paper, id: report.comparator_id, outcome: summary?.live },
    { label: labels.shadow, id: report.strategy_id, outcome: summary?.shadow },
  ];
  const refusals = (outcome?: ShadowOutcome | null) =>
    outcome
      ? count(Object.entries(outcome.outcomes)
          .filter(([code]) => code !== "filled" && code !== "exit_revised")
          .reduce((total, [, value]) => total + value, 0))
      : "—";
  return (
    <section aria-label={labels.title}>
      <h3>{labels.title}</h3>
      <Status value={report.state}>{labels[report.state]}</Status>
      {report.reason && <p>{plain(report.reason, labels)}</p>}
      <Facts
        items={[
          [labels.coverage, summary?.eligible_scans === undefined
            ? "—"
            : `${count(summary.evaluated_scans)} of ${count(summary.eligible_scans)}`],
          [labels.scheduledEnd, report.scheduled_end_ms ? datetime(report.scheduled_end_ms) : "—"],
          [labels.workerFailures, count(Object.values(summary?.failures ?? {}).reduce((a, b) => a + b, 0))],
        ]}
      />
      {report.strategy_id && (
        <Table
          compact
          rows={rows}
          columns={[
            { key: "label", label: "Strategy", render: (row) => <>{row.label} <ShortId value={row.id ?? undefined} /></> },
            { key: "pnl", label: labels.markedPnl, num: true, render: (row) => amount(row.outcome?.marked_pnl) },
            { key: "fees", label: labels.fees, num: true, render: (row) => amount(row.outcome?.fees) },
            { key: "fills", label: labels.fills, num: true, render: (row) => row.outcome ? count(row.outcome.fills) : "—" },
            { key: "open", label: labels.open, num: true, render: (row) => row.outcome ? count(row.outcome.open_positions) : "—" },
            { key: "refusals", label: labels.refusals, num: true, render: (row) => refusals(row.outcome) },
          ]}
        />
      )}
      {summary?.shadow && <p>{summary.comparable ? labels.comparable : labels.insufficient}</p>}
    </section>
  );
}

export function StrategyRun({ run }: { run?: StrategyExecution | null }) {
  if (!run?.enabled)
    return (
      <Panel title="Strategy on each scan">
        <Empty title="Strategy execution is off">
          It is turned on in the planner settings.
        </Empty>
      </Panel>
    );
  const s = run.strategy;
  const author = s?.author
    ? [sentenceCase(s.author.lane || "Unknown lane"), s.author.model]
        .filter(Boolean)
        .join(" · ")
    : null;
  const fallback = s?.basis === "fallback";
  const reference = (name?: string) =>
    name ? plain(name, REFERENCE_STRATEGY_LABELS) : null;
  const accepted = s?.summary
    ? `${count(s.summary.trades)} trades · ${amount(s.summary.net_pnl)} after costs`
    : null;
  return (
    <Panel
      title="Strategy on each scan"
      note={
        <Status value={run.state}>{plain(run.state, STRATEGY_LABELS)}</Status>
      }
    >
      <Facts
        items={[
          [
            "Strategy",
            fallback ? (
              reference(s?.name)
            ) : s ? (
              <ShortId key="id" value={s.strategy_id} />
            ) : (
              "None"
            ),
          ],
          ...(s?.basis
            ? ([["Kind", plain(s.basis, STRATEGY_LABELS)]] as Fact[])
            : []),
          ...(s && !fallback
            ? ([
                ["Written by", author],
                ["Accepted result", accepted],
                ["Tested on", s.tape_dates?.join(", ") || null],
              ] as Fact[])
            : []),
          ...(run.reason
            ? ([
                [
                  fallback ? "Why a fallback runs" : "Why none is running",
                  plain(run.reason, STRATEGY_LABELS),
                ],
              ] as Fact[])
            : []),
          [
            "Fallbacks",
            run.fallback?.length
              ? run.fallback.map((name) => reference(name)).join(", ")
              : "None configured",
          ],
          [
            "Calls",
            `${count(run.calls)} · ${count(run.failures)} failed · ${count(run.skipped)} skipped`,
          ],
          [
            "Last call",
            run.last_call_at_ms ? datetime(run.last_call_at_ms) : "—",
          ],
          [
            "Last failure",
            run.last_failure ? plain(run.last_failure, STRATEGY_LABELS) : "—",
          ],
        ]}
      />
      {run.forward_shadow?.state && <ForwardTest report={run.forward_shadow} />}
      <h3>Recent actions</h3>
      <Table
        compact
        rows={run.actions || []}
        columns={[
          { key: "at", label: "Time", render: (a) => datetime(a.at_ms) },
          {
            key: "instrument",
            label: "Instrument",
            render: (a) => instrumentName(a.instrument_key),
          },
          {
            key: "side",
            label: "Side",
            render: (a) => (a.side ? plain(a.side) : "—"),
          },
          {
            key: "units",
            label: "Units",
            num: true,
            render: (a) => count(a.units),
          },
          {
            key: "outcome",
            label: "Outcome",
            render: (a) => strategyOutcome(a.outcome),
          },
        ]}
        empty="No strategy actions yet"
      />
      <h3>Refused by the execution gate</h3>
      <Table
        compact
        rows={run.refusals || []}
        columns={[
          { key: "at", label: "Time", render: (r) => datetime(r.at_ms) },
          {
            key: "instrument",
            label: "Instrument",
            render: (r) => instrumentName(r.action?.instrument_key),
          },
          {
            key: "reason",
            label: "Why",
            render: (r) => strategyOutcome(r.reason),
          },
        ]}
        empty="No refusals"
      />
      <h3>Fills</h3>
      <Table
        compact
        rows={run.fills || []}
        columns={[
          { key: "at", label: "Time", render: (f) => datetime(f.at) },
          {
            key: "instrument",
            label: "Instrument",
            render: (f) =>
              f.instrument?.symbol || instrumentName(f.instrument?.key),
          },
          {
            key: "side",
            label: "Side",
            render: (f) => (f.buy ? "Buy" : "Sell"),
          },
          {
            key: "units",
            label: "Units",
            num: true,
            render: (f) => count(f.units),
          },
          {
            key: "price",
            label: "Price",
            num: true,
            render: (f) => amount(f.price),
          },
        ]}
        empty="No strategy fills yet"
      />
    </Panel>
  );
}
