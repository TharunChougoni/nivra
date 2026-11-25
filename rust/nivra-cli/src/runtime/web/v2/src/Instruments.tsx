import type { CatalogInstrument, ContractStrike } from "./types";
import type { Column } from "./ui";
import React, { useEffect, useState } from "react";
import { amount, count, datetime } from "./data";
import { classLabel, instrumentName, plain } from "./labels";
import {
  Button,
  Drawer,
  Facts,
  Instrument,
  Panel,
  ShortId,
  Table,
  useWorkspace,
} from "./ui";
import { useRead } from "./reads";
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
  {
    key: "segment",
    label: "Segment",
    render: (row) => plain(row.segment),
  },
  { key: "kind", label: "Class", render: (row) => classLabel(row.kind) },
  {
    key: "lot_size",
    label: "Lot size",
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
        title="Instruments"
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
            Segment{" "}
            <select
              aria-label="Catalog segment"
              value={segment}
              onChange={(e) => {
                setSegment(e.target.value);
                setCursor(0);
              }}
            >
              <option value="">All segments</option>
              {Object.entries(response?.segments || {}).map(([name, n]) => (
                <option key={name} value={name}>
                  {plain(name)} ({count(n)})
                </option>
              ))}
            </select>
          </label>
          <label>
            Class{" "}
            <select
              aria-label="Catalog class"
              value={kind}
              onChange={(e) => {
                setKind(e.target.value);
                setCursor(0);
              }}
            >
              <option value="">All classes</option>
              {Object.entries(response?.kinds || {}).map(([name, n]) => (
                <option key={name} value={name}>
                  {classLabel(name)} ({count(n)})
                </option>
              ))}
            </select>
          </label>
          <input
            aria-label="Search catalog"
            placeholder="Search by symbol…"
            value={search}
            onChange={(e) => {
              setSearch(e.target.value);
              setCursor(0);
            }}
          />
        </div>
        {catalog.state === "idle" || catalog.state === "loading" ? (
          <p className="panel-note">Loading…</p>
        ) : catalog.state === "unavailable" ? (
          <p className="panel-note">
            Catalog unavailable · {plain(response?.reason)}
          </p>
        ) : catalog.state === "error" ? (
          <p className="panel-note">Could not load the catalog.</p>
        ) : (
          <>
            <Facts
              items={[
                ["Source", plain(response?.source)],
                ["Coverage", plain(response?.coverage_kind)],
                ["Received", datetime(response?.received_at_ms)],
                ["Matches", count(response?.total)],
                ["Rows read", count(response?.rows_seen)],
                ["Skipped rows", count(response?.rejected_rows)],
              ]}
            />
            <Table
              hideEmpty
              columns={catalogColumns}
              rows={rows}
              getKey={(row) => row.instrument_key}
              onSelect={setSelected}
              empty="No matches"
            />
            {response?.source_hashes && (
              <details className="raw-evidence">
                <summary>Source files</summary>
                <Facts
                  items={Object.entries(response.source_hashes).map(
                    ([name, hash]) => [
                      plain(name),
                      <ShortId key={name} value={hash} />,
                    ],
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
        title={selected?.symbol || "Instrument"}
        description={instrumentName(selected?.instrument_key)}
      >
        <Facts
          items={[
            ["Class", classLabel(selected?.kind)],
            ["Segment", plain(selected?.segment)],
            ["Exchange", selected?.exchange],
            ["Type", selected?.instrument_type],
            [
              "Underlying",
              selected?.underlying_key
                ? instrumentName(selected.underlying_key)
                : null,
            ],
            ["Expiry", datetime(selected?.expiry_ms)],
            [
              "Strike",
              selected?.strike == null
                ? "Not reported"
                : String(selected.strike),
            ],
            [
              "Lot size",
              selected?.lot_size == null
                ? "Not reported"
                : String(selected.lot_size),
            ],
            ["Tick", selected ? catalogTick(selected) : null],
            [
              "Cash trading",
              selected?.cash_execution_verified ? "Verified" : "Not verified",
            ],
            [
              "Margin",
              selected?.margin?.display_only &&
              selected.margin.instrument_key === selected.instrument_key
                ? amount(selected.margin.total_margin)
                : "Not captured",
            ],
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
      "Ambiguous"
    ) : row ? (
      <>
        <Instrument
          symbol={row.symbol ?? undefined}
          instrumentKey={row.instrument_key}
        />
        <small>
          Lot size{" "}
          {row.lot_size == null ? "not reported" : String(row.lot_size)}
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
      label: "Strike",
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
        <p className="panel-note">Loading…</p>
      ) : chain.state === "unavailable" ? (
        <p className="panel-note">
          Contract chain unavailable · {plain(response?.reason)}
        </p>
      ) : chain.state === "error" ? (
        <p className="panel-note">Could not load the chain.</p>
      ) : (
        <>
          <Facts
            items={[
              ["Source", plain(response?.source)],
              ["Received", datetime(response?.received_at_ms)],
              ["Strikes", count(response?.total)],
              ["Unusable contracts", count(response?.unavailable_strikes)],
            ]}
          />
          <Table
            columns={columns}
            rows={response?.items || []}
            getKey={(row) => String(row.strike)}
            empty="No strikes captured"
          />
          <p className="panel-note">
            Futures: {count(response?.futures_total)}
            {response?.futures_truncated ? " · list cut off" : ""}
          </p>
          <Table
            columns={catalogColumns}
            rows={response?.futures || []}
            getKey={(row) => row.instrument_key}
            empty="No futures captured"
          />
        </>
      )}
    </Panel>
  );
}
export default function Instruments() {
  return <VerifiedCatalog />;
}
