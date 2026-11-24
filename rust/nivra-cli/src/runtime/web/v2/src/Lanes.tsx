import type { AgentTurn } from "./types";
import type { Column } from "./ui";
import React, { useEffect, useMemo, useState } from "react";
import {
  amount,
  clock,
  count,
  datetime,
  duration,
  numeric,
  usd,
  words,
} from "./data";
import { LANE_ORDER, laneStats, turnFailed, turnOutcome } from "./model";
import {
  Drawer,
  Empty,
  Facts,
  LaneSwatch,
  Notice,
  Panel,
  Segments,
  Status,
  Table,
  Tag,
  useWorkspace,
} from "./ui";
import Bars from "./charts/Bars";
import LaneTimeline, { laneColor, sessionRange } from "./charts/LaneTimeline";
import { TimeChart } from "./charts/TimeChart";
import { useRead, useRevision } from "./reads";

const FALLBACK_POLL_MS = 60_000;
const PAGE = 100;
const tokens = (t: AgentTurn) =>
  t.usage ? `${count(t.usage.input)} / ${count(t.usage.output)}` : "—";
const triggerText = (t: AgentTurn) =>
  [
    t.trigger?.reason || t.trigger?.kind,
    ...(t.trigger?.instruments || []).map((k) => k.split("|")[1] || k),
  ]
    .filter(Boolean)
    .join(" · ") || "—";

function WebToolActivity() {
  const [state] = useWorkspace();
  const date =
    state.summary?.session || state.sessions?.items?.[0]?.id?.date || "";
  const calls = useRead(
      (api) => api.tools({ date, limit: 50 }),
      date + "|" + useRevision("agent"),
    ),
    rows = calls.value?.calls || [];
  return (
    <Panel
      title="Web tool activity"
      note="Every recorded web_search, web_fetch and news_lookup call with its original query or URL, latency, bytes and outcome. Display evidence only; no order authority."
    >
      {(calls.state === "idle" ||
        (calls.state === "loading" && !calls.value)) && (
        <p className="panel-note">Loading recorded web tool calls…</p>
      )}
      {(calls.state === "error" || calls.state === "unavailable") && (
        <Notice error>
          The web tool record is unavailable for this session.
        </Notice>
      )}
      {calls.state === "ready" && rows.length === 0 && (
        <Empty>No web tool calls were recorded for this session.</Empty>
      )}
      {rows.length > 0 && (
        <Table
          getKey={(r) => `${r.run}:${r.turn}:${r.call}`}
          rows={rows}
          columns={[
            {
              key: "tool",
              label: "Tool",
              render: (r) => r.tool || "Not reported",
            },
            {
              key: "query",
              label: "Query / URL",
              render: (r) => r.query || r.url || "—",
            },
            {
              key: "latency_ms",
              label: "Latency",
              render: (r) => duration(r.latency_ms),
            },
            {
              key: "bytes",
              label: "Bytes",
              render: (r) => count(r.bytes),
            },
            {
              key: "status",
              label: "Outcome",
              render: (r) => <Status value={r.status} />,
            },
            {
              key: "at_ms",
              label: "Recorded · IST",
              render: (r) => datetime(r.at_ms),
            },
          ]}
        />
      )}
      {calls.state === "ready" && (
        <div className="table-footer">
          <span>{count(calls.value?.total ?? rows.length)} recorded calls</span>
          <span>
            Queries and URLs are untrusted model text; credentials are never
            recorded.
          </span>
        </div>
      )}
    </Panel>
  );
}

function LaneCards({
  turns,
  loadedOf,
}: {
  turns: AgentTurn[];
  loadedOf: number | null;
}) {
  const [state] = useWorkspace();
  const stats = useMemo(
    () => laneStats(turns, state.agent?.usage),
    [turns, state.agent],
  );
  return (
    <Panel
      title="Lanes"
      note={
        loadedOf !== null && loadedOf > turns.length
          ? `latency and failures from the ${count(turns.length)} newest of ${count(loadedOf)} turns`
          : `${count(turns.length)} turns loaded · tokens and cost from API usage totals`
      }
    >
      <div className="lane-cards">
        {stats.map((l) => (
          <div className="lane-card" key={l.lane}>
            <div className="top">
              <LaneSwatch lane={l.lane} />
              <span className="num muted">{count(l.turns)} turns</span>
            </div>
            {(
              [
                [
                  "failed · fallbacks",
                  `${count(l.failed)} · ${count(l.fallbacks)}`,
                ],
                [
                  "latency p50 / p95",
                  `${duration(l.p50_ms)} / ${duration(l.p95_ms)}`,
                ],
                ["tokens in / out", `${count(l.input)} / ${count(l.output)}`],
                ["cache read", count(l.cache_read)],
                ["cost", usd(l.cost_usd)],
                ["calls", count(l.calls)],
                [
                  "last",
                  l.last
                    ? `${clock(l.last.at_ms)} · ${turnOutcome(l.last)}`
                    : "—",
                ],
              ] as [string, string][]
            ).map(([k, v]) => (
              <div className="kv-row" key={k}>
                <span>{k}</span>
                <span title={v}>{v}</span>
              </div>
            ))}
          </div>
        ))}
      </div>
    </Panel>
  );
}
/** Gate turns → escalated → decision turns → validated plans → actions → fills; recorded counts only. */
function GateFunnel({ turns }: { turns: AgentTurn[] }) {
  const [state] = useWorkspace();
  const gate = turns.filter((t) => t.lane === "gate"),
    decisions = turns.filter((t) => t.lane === "decision");
  const escalated =
    numeric(state.agent?.triggers?.gate_escalated) ??
    (gate.length ? gate.filter((t) => t.verdict?.escalate).length : null);
  const plans = decisions.filter((t) => t.plan);
  const stages: [string, number | null][] = [
    ["gate turns", gate.length || null],
    ["escalated", escalated],
    ["decision turns", decisions.length || null],
    [
      "validated plans",
      plans.length
        ? plans.filter((t) => t.plan?.status === "validated").length
        : null,
    ],
    [
      "actions",
      plans.length
        ? plans.reduce((n, t) => n + (t.plan?.actions ?? 0), 0)
        : null,
    ],
    ["fills", numeric(state.fills?.total ?? state.fills?.items?.length)],
  ];
  const known = stages.filter((s): s is [string, number] => s[1] !== null);
  return (
    <Panel title="Gate funnel" note="recorded counts only">
      {known.length ? (
        <div className="panel-body">
          <Bars items={known.map(([label, value]) => ({ label, value }))} />
        </div>
      ) : (
        <Empty title="No funnel counts recorded" />
      )}
    </Panel>
  );
}
function CostChart({ turns, lanes }: { turns: AgentTurn[]; lanes: string[] }) {
  const priced = useMemo(
    () =>
      turns
        .filter(
          (t) =>
            numeric(t.at_ms) !== null && numeric(t.usage?.cost_usd) !== null,
        )
        .sort((a, b) => a.at_ms! - b.at_ms!),
    [turns],
  );
  const x = useMemo(() => priced.map((t) => t.at_ms! / 1000), [priced]);
  const panes = useMemo(
    () => [
      {
        height: 170,
        format: (v: number) => "$" + v.toFixed(2),
        yRange: (_min: number, max: number): [number, number] => [
          0,
          max * 1.1 || 1,
        ],
        series: lanes.map((lane) => {
          let total = 0;
          return {
            label: lane,
            color: laneColor(lane),
            step: true,
            width: 1.4,
            data: priced.map((t) => {
              if (t.lane === lane) total += Number(t.usage!.cost_usd);
              return total;
            }),
          };
        }),
      },
    ],
    [priced, lanes],
  );
  if (priced.length < 2)
    return (
      <Empty title="Not enough priced turns">
        Cumulative cost needs at least two turns with recorded cost.
      </Empty>
    );
  return (
    <TimeChart
      x={x}
      panes={panes}
      ariaLabel="Cumulative pi cost by lane"
      tooltip={(i) => ({
        title: clock(x[i] * 1000) + " IST",
        rows: panes[0].series.map(
          (s) => [s.label, usd(s.data[i])] as [string, string],
        ),
      })}
    />
  );
}
const EDGES = [100, 250, 500, 1000, 2500, 5000, 10000, 30000, 100000];
const edgeLabel = (ms: number) => (ms < 1000 ? `${ms}` : `${ms / 1000}s`);
function LatencyHistograms({
  turns,
  lanes,
}: {
  turns: AgentTurn[];
  lanes: string[];
}) {
  return (
    <div className="latency-grid">
      {lanes.map((lane) => {
        const counts = new Array(EDGES.length + 1).fill(0);
        for (const t of turns) {
          const ms = numeric(t.elapsed_ms);
          if (t.lane !== lane || ms === null) continue;
          const i = EDGES.findIndex((e) => ms < e);
          counts[i < 0 ? EDGES.length : i]++;
        }
        return (
          <div key={lane}>
            <LaneSwatch lane={lane} />
            <Bars
              orientation="vertical"
              color={laneColor(lane)}
              height={54}
              items={counts.map((value, i) => ({
                label: i < EDGES.length ? edgeLabel(EDGES[i]) : "∞",
                value,
                title: `${i ? edgeLabel(EDGES[i - 1]) : "0"} – ${i < EDGES.length ? edgeLabel(EDGES[i]) + (i < EDGES.length ? "" : "") : "∞"}: ${value} turns`,
              }))}
            />
          </div>
        );
      })}
    </div>
  );
}
function TurnDrawer({
  turn,
  onClose,
}: {
  turn: AgentTurn | null;
  onClose: () => void;
}) {
  return (
    <Drawer
      open={!!turn}
      onClose={onClose}
      title={turn ? `${turn.lane} turn` : "Turn"}
      description={turn?.turn_id}
    >
      {turn && (
        <>
          <Facts
            items={[
              ["Time", datetime(turn.at_ms)],
              ["Lane", turn.lane],
              ["Status", words(turn.status)],
              ["Model", turn.model],
              ["Provider", turn.provider],
              [
                "Fallback",
                turn.fallback
                  ? `${turn.fallback.to?.id ?? "unknown"} · ${words(turn.fallback.reason)}`
                  : "None",
              ],
              ["Latency", duration(turn.elapsed_ms)],
              ["Tokens in / out", tokens(turn)],
              ["Cache read", count(turn.usage?.cache_read)],
              ["Cost", usd(turn.usage?.cost_usd)],
              ["Trigger", triggerText(turn)],
              [
                "Plan",
                turn.plan
                  ? [
                      turn.plan.status,
                      turn.plan.stance,
                      turn.plan.actions != null
                        ? `${turn.plan.actions} actions`
                        : null,
                    ]
                      .filter(Boolean)
                      .join(" · ")
                  : "—",
              ],
              ["Plan error", turn.plan?.error],
              [
                "Verdict",
                turn.verdict
                  ? `${turn.verdict.escalate ? "escalate" : "pass"} · ${turn.verdict.reason}`
                  : "—",
              ],
              ["Error", turn.error],
            ]}
          />
          <Panel title="Tool calls">
            <Table
              rows={turn.tool_calls || []}
              getKey={(c, i) => `${c.name}:${i}`}
              empty="No tool calls recorded"
              columns={[
                { key: "name", label: "Tool" },
                {
                  key: "ok",
                  label: "Result",
                  render: (c) =>
                    c.ok ? <Status value="ok" /> : <Status value="failed" />,
                },
                { key: "error", label: "Error", render: (c) => c.error || "—" },
              ]}
            />
          </Panel>
          <details className="raw-evidence">
            <summary>Inspect raw turn record</summary>
            <pre>{JSON.stringify(turn, null, 2)}</pre>
          </details>
        </>
      )}
    </Drawer>
  );
}
function TurnTable({
  turns,
  lanes,
  onSelect,
  selected,
}: {
  turns: AgentTurn[];
  lanes: string[];
  onSelect: (t: AgentTurn) => void;
  selected: AgentTurn | null;
}) {
  const [lane, setLane] = useState("all"),
    [shown, setShown] = useState(PAGE);
  const rows = turns.filter((t) => lane === "all" || t.lane === lane);
  const columns: Column<AgentTurn>[] = [
    { key: "at_ms", label: "Time", num: true, render: (t) => clock(t.at_ms) },
    { key: "lane", label: "Lane", render: (t) => <LaneSwatch lane={t.lane} /> },
    {
      key: "model",
      label: "Model",
      render: (t) => (
        <span className="row">
          {t.model || "—"}
          {t.fallback && (
            <Tag tone="warn" title={t.fallback.reason}>
              fallback
            </Tag>
          )}
        </span>
      ),
    },
    { key: "trigger", label: "Trigger", render: triggerText },
    {
      key: "tools",
      label: "Tools",
      render: (t) =>
        (t.tool_calls || []).map((c, i) => (
          <span
            key={i}
            className={c.ok ? "tool-ok" : "tool-failed"}
            title={c.error || ""}
          >
            {c.name + (c.ok ? "" : " ✗")}
          </span>
        )),
    },
    { key: "outcome", label: "Outcome", render: turnOutcome },
    {
      key: "elapsed_ms",
      label: "Latency",
      num: true,
      render: (t) => duration(t.elapsed_ms),
    },
    { key: "tokens", label: "In / out", num: true, render: tokens },
    {
      key: "cost",
      label: "Cost",
      num: true,
      render: (t) => usd(t.usage?.cost_usd),
    },
    {
      key: "status",
      label: "Status",
      render: (t) => <Status value={turnFailed(t) ? "failed" : t.status} />,
    },
  ];
  return (
    <Panel
      title="Turns"
      note={`${count(rows.length)} loaded · newest first · select a row for the full record`}
      actions={
        <Segments
          value={lane}
          onChange={(v) => {
            setLane(v);
            setShown(PAGE);
          }}
          label="Lane filter"
          items={[["all", "all"], ...lanes.map((l) => [l, l])]}
        />
      }
    >
      <Table
        rows={rows.slice(0, shown)}
        columns={columns}
        getKey={(t) => t.turn_id}
        selected={selected?.turn_id}
        onSelect={onSelect}
        empty="No lane turns yet"
      />
      {rows.length > shown && (
        <div className="panel-foot">
          <span>
            {count(shown)} of {count(rows.length)} shown
          </span>
          <button className="button" onClick={() => setShown(shown + PAGE)}>
            Show more
          </button>
        </div>
      )}
    </Panel>
  );
}
export default function Lanes() {
  const [state, app] = useWorkspace(),
    [laneFilter, setLaneFilter] = useState("all"),
    [turn, setTurn] = useState<AgentTurn | null>(null);
  const turns = state.agent?.turns ?? [],
    total = numeric(state.agent?.total);
  // The controller reloads on revision change and run_ended. A slow fallback
  // covers a backend that sends neither, and only while the tab is visible.
  useEffect(() => {
    if (!state.agent && state.agentState === "idle") void app.loadAgent();
    const timer = setInterval(() => {
      if (!document.hidden) void app.loadAgent();
    }, FALLBACK_POLL_MS);
    return () => clearInterval(timer);
  }, [app]);
  const lanes = useMemo(() => {
    const seen = new Set([...LANE_ORDER, ...turns.map((t) => t.lane)]);
    return [...seen];
  }, [turns]);
  const range = useMemo(
    () => sessionRange(state.summary?.session, turns),
    [state.summary?.session, turns],
  );
  const shownLanes = lanes.filter(
    (l) => laneFilter === "all" || l === laneFilter,
  );
  return (
    <>
      {(state.agentState === "error" || state.agentState === "unavailable") && (
        <Notice error>
          The pi lane journal is{" "}
          {state.agentState === "error" ? "unavailable" : "not reported"}.
        </Notice>
      )}
      <LaneCards turns={turns} loadedOf={total} />
      <Panel
        title="Timeline"
        note="x = session time IST · height = log latency · hollow = failed · ◆ gate escalated"
        actions={
          <Segments
            value={laneFilter}
            onChange={setLaneFilter}
            label="Lane filter"
            items={[["all", "all"], ...lanes.map((l) => [l, l])]}
          />
        }
      >
        {turns.length ? (
          <div className="panel-body">
            <LaneTimeline
              turns={turns}
              lanes={shownLanes}
              range={range}
              now={numeric(state.summary?.as_of_ms)}
              rowHeight={laneFilter === "all" ? 40 : 90}
              onSelect={setTurn}
            />
          </div>
        ) : (
          <Empty title="No lane turns recorded">
            Turns appear after an eligible pi lane runs.
          </Empty>
        )}
      </Panel>
      <div className="cols-2">
        <GateFunnel turns={turns} />
        <Panel
          title="Latency distribution"
          note="turns per log-spaced latency bucket (ms)"
        >
          <div className="panel-body">
            <LatencyHistograms
              turns={turns}
              lanes={lanes.filter((l) => turns.some((t) => t.lane === l))}
            />
          </div>
        </Panel>
      </div>
      <Panel
        title="Cumulative cost by lane"
        note="stepped · USD · from recorded per-turn cost"
      >
        <CostChart
          turns={turns}
          lanes={lanes.filter((l) => turns.some((t) => t.lane === l))}
        />
      </Panel>
      <TurnTable
        turns={turns}
        lanes={lanes}
        onSelect={setTurn}
        selected={turn}
      />
      <WebToolActivity />
      <TurnDrawer turn={turn} onClose={() => setTurn(null)} />
    </>
  );
}
