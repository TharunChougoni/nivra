import React, { useEffect, useRef, useState } from "react";
import { Download, FileUp, Layers } from "lucide-react";
import Chart from "./Chart";
import { amount, count, numeric } from "./data";
import { plain } from "./labels";
import { Ledger } from "./TradeShared";
import { errorMessage } from "./types";
import type {
  Instrument as UniverseInstrument,
  ReplayPayload,
  ReplayResult,
} from "./types";
import {
  Button,
  Empty,
  Facts,
  KpiStrip,
  Notice,
  Panel,
  ShortId,
  useWorkspace,
} from "./ui";

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
