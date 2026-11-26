import React, { useState } from "react";
import { ArrowUpRight } from "lucide-react";
import { count, strategyReferences } from "./data";
import { instrumentName, plain } from "./labels";
import { instrumentIndex } from "./model";
import { StrategyRun } from "./StrategyRun";
import type { StrategyReference } from "./types";
import {
  Drawer,
  Facts,
  Instrument,
  Notice,
  Panel,
  Segments,
  ShortId,
  Status,
  Table,
  useWorkspace,
} from "./ui";

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
      <StrategyRun run={state.summary?.strategy_execution} />
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
