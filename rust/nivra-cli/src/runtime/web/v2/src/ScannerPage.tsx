import React, { useState } from "react";
import { Search } from "lucide-react";
import {
  amount,
  count,
  datetime,
  instrumentClass,
  strategyLinks,
} from "./data";
import { classLabel, instrumentName, plain } from "./labels";
import { LivePriceChart } from "./LivePriceChart";
import { useRead } from "./reads";
import { StrategyLink } from "./TradeShared";
import type { ScannerRow } from "./types";
import type { Column } from "./ui";
import {
  Drawer,
  Facts,
  Instrument,
  KpiStrip,
  Panel,
  Segments,
  Status,
  Table,
  useWorkspace,
} from "./ui";

function columnsInstrument(): Column<ScannerRow> {
  return {
    key: "symbol",
    label: "Instrument",
    render: (r) => (
      <Instrument symbol={r.symbol} instrumentKey={r.instrument_key} />
    ),
  };
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
        <LivePriceChart
          state={state}
          label={selected?.symbol || "Price"}
          seriesKey={selected?.instrument_key || undefined}
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
