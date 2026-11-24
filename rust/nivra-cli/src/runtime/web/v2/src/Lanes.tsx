import type { ToolsResponse, AgentResponse, AgentTurn } from "./types";
import React, { useEffect, useState } from "react";
import { amount, count, datetime, duration } from "./data";
import { Empty, Notice, Panel, Status, Table, useWorkspace } from "./ui";
function useToolCalls(date: string) {
  const [state, app] = useWorkspace(),
    [result, setResult] = useState<{
      value: ToolsResponse | null;
      state: "idle" | "loading" | "ready" | "error" | "unavailable";
    }>({ value: null, state: "idle" }),
    token = state.token;
  useEffect(() => {
    let active = true;
    if (!token) {
      setResult({ value: null, state: "idle" });
      return;
    }
    setResult({ value: null, state: "loading" });
    Promise.resolve(app.api.tools({ date, limit: 50 }))
      .then((value) => {
        if (active)
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
  }, [token, date]);
  return result;
}
function WebToolActivity() {
  const [state] = useWorkspace();
  const date =
    state.summary?.session || state.sessions?.items?.[0]?.id?.date || "";
  const calls = useToolCalls(date),
    rows = calls.value?.calls || [];
  return (
    <Panel
      title="Web tool activity"
      note="Every recorded orchestrator web_search, web_fetch and news_lookup call with its original query or URL, latency, bytes and outcome. Display evidence only; no order authority."
    >
      {(calls.state === "idle" || calls.state === "loading") && (
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
const LANE_TURNS_POLL_MS = 15_000;

function turnOutcome(t: AgentTurn) {
  if (t.verdict)
    return (t.verdict.escalate ? "Escalate · " : "Pass · ") + t.verdict.reason;
  if (t.plan)
    return (
      (t.plan.status === "validated" ? "" : (t.plan.status || "plan") + " · ") +
      (t.plan.stance || "") +
      (t.plan.actions ? ` · ${count(t.plan.actions)} actions` : "")
    );
  return t.error || "—";
}

/** The pi lane turns from the book's own journals: newest first. */
export function LaneTurns() {
  const [state, app] = useWorkspace(),
    token = state.token,
    [result, setResult] = useState<{
      value: AgentResponse | null;
      state: "loading" | "ready" | "error";
    }>({ value: null, state: "loading" });
  useEffect(() => {
    let active = true;
    if (!token) return;
    const load = () =>
      Promise.resolve(app.api.agent({ limit: 100 }))
        .then((value) => active && setResult({ value, state: "ready" }))
        .catch(() => active && setResult((v) => ({ ...v, state: "error" })));
    void load();
    const timer = window.setInterval(load, LANE_TURNS_POLL_MS);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [token]);
  const turns = result.value?.turns || [],
    lanes = result.value?.usage?.lanes || {},
    triggers = result.value?.triggers || {};
  return (
    <Panel
      title="Lane turns"
      note="pi decision, gate, research and review turns from this session's journal, with each tool call and outcome. Display evidence only; no order authority."
      actions={
        <span className="caption">
          {count(result.value?.total ?? 0)} turns · gate{" "}
          {count(triggers.gate_escalated ?? 0)} escalated /{" "}
          {count(triggers.gate_declined ?? 0)} passed
        </span>
      }
    >
      {result.state === "error" && (
        <Notice error>The lane journal is unavailable.</Notice>
      )}
      <div className="lane-usage">
        {Object.entries(lanes).map(([lane, u]) => (
          <span key={lane} className="caption">
            {lane}: {count(u.calls ?? 0)} calls · {amount(u.cost_usd ?? 0)} USD
          </span>
        ))}
      </div>
      <Table
        getKey={(t) => t.turn_id}
        rows={turns}
        empty={
          result.state === "loading"
            ? "Loading lane turns…"
            : "No lane turns yet"
        }
        columns={[
          {
            key: "at_ms",
            label: "Time · IST",
            render: (t) => datetime(t.at_ms),
          },
          { key: "lane", label: "Lane", render: (t) => t.lane },
          {
            key: "model",
            label: "Model",
            render: (t) =>
              t.fallback ? `${t.model} (fallback)` : t.model || "—",
          },
          {
            key: "trigger",
            label: "Trigger",
            render: (t) =>
              [
                t.trigger?.reason || t.trigger?.kind,
                ...(t.trigger?.instruments || []).map(
                  (k) => k.split("|")[1] || k,
                ),
              ]
                .filter(Boolean)
                .join(" · "),
          },
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
                  {(i ? " " : "") + c.name + (c.ok ? "" : "✗")}
                </span>
              )),
          },
          { key: "outcome", label: "Outcome", render: turnOutcome },
          {
            key: "status",
            label: "Status",
            render: (t) => <Status value={t.status} />,
          },
          {
            key: "elapsed_ms",
            label: "Took",
            render: (t) => duration(t.elapsed_ms),
          },
          {
            key: "cost",
            label: "Cost",
            render: (t) => amount(t.usage?.cost_usd ?? 0),
          },
        ]}
      />
    </Panel>
  );
}
export default function Lanes() {
  return (
    <>
      <LaneTurns />
      <WebToolActivity />
    </>
  );
}
