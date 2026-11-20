import type {
  RunResponse,
  StreamEvent,
  RunSummary,
  ModelOutput,
  TranscriptEvent,
  ToolsResponse,
  AgentResponse,
  AgentTurn,
} from "./types";
import React, { useEffect, useRef, useState } from "react";
import {
  Bot,
  ChevronLeft,
  ChevronDown,
  FileCode2,
  Terminal,
  AlertCircle,
  Check,
  Copy,
} from "lucide-react";
import {
  amount,
  count,
  datetime,
  duration,
  identityLabels,
  identities,
  runHref,
  words,
} from "./data";
import {
  Button,
  Empty,
  Instrument,
  Notice,
  Panel,
  Segments,
  Status,
  Table,
  useWorkspace,
} from "./ui";
const pretty = (v: unknown) =>
  typeof v === "string" ? v : JSON.stringify(v ?? null, null, 2);
function inline(text: string) {
  return String(text)
    .split(/(`[^`]+`|\*\*[^*]+\*\*)/)
    .map((v, i) =>
      v.startsWith("`") ? (
        <code key={i}>{v.slice(1, -1)}</code>
      ) : v.startsWith("**") ? (
        <strong key={i}>{v.slice(2, -2)}</strong>
      ) : (
        v
      ),
    );
}
function Prose({ text }: { text?: string }) {
  return (
    <div className="prose">
      {String(text || "")
        .split(/(```[\s\S]*?```)/)
        .map((v, i) =>
          v.startsWith("```") ? (
            <pre key={i}>{v.slice(3, -3).replace(/^\w+\n/, "")}</pre>
          ) : (
            v
              .split("\n\n")
              .filter(Boolean)
              .map((p, j) => <p key={`${i}-${j}`}>{inline(p)}</p>)
          ),
        )}
    </div>
  );
}
function LazyEvidence({
  label,
  event,
  id,
  kind = "blob",
  initial,
}: {
  label: string;
  event: Pick<TranscriptEvent, "sha" | "blob" | "bytes">;
  id: string;
  kind?: "blob" | "prompt";
  initial?: string;
}) {
  const [state, app] = useWorkspace(),
    [value, setValue] = useState(initial),
    [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [offset, setOffset] = useState(0),
    [more, setMore] = useState(true);
  const alive = useRef(true);
  useEffect(
    () => () => {
      alive.current = false;
    },
    [],
  );
  const size = state.meta?.limits?.blob_slice_max_bytes || 262144;
  async function load() {
    if (busy) return;
    setBusy(true);
    setError("");
    const token = app.getSnapshot().token;
    try {
      const text =
        kind === "prompt"
          ? await app.api.prompt(event.sha || "")
          : await app.api.blob(id, event.blob || "", { offset, len: size });
      if (!alive.current || token !== app.getSnapshot().token) return;
      setValue((v) => (v || "") + text);
      setOffset(offset + size);
      setMore(
        kind !== "prompt" && new TextEncoder().encode(text).length >= size,
      );
    } catch {
      if (alive.current) setError("Archived evidence could not be read.");
    } finally {
      if (alive.current) setBusy(false);
    }
  }
  return (
    <details
      className="evidence-fold"
      onToggle={(e) => {
        if (e.currentTarget.open && value === undefined) void load();
      }}
    >
      <summary>
        <FileCode2 size={13} />
        {label}
        <span>{event.bytes != null ? count(event.bytes) + " B" : ""}</span>
      </summary>
      <Notice error>{error}</Notice>
      {value !== undefined ? (
        <pre>{typeof value === "string" ? value : pretty(value)}</pre>
      ) : (
        <p className="muted p-4">
          {busy ? "Reading archive…" : "Expand to load archived evidence."}
        </p>
      )}
      {value !== undefined && event.blob && more && (
        <Button onClick={load} disabled={busy}>
          {busy ? "Loading…" : "Load next slice"}
        </Button>
      )}
    </details>
  );
}
function Output({ output }: { output?: ModelOutput }) {
  const [raw, setRaw] = useState(false);
  const actions = Array.isArray(output?.actions) ? output.actions : null;
  return (
    <section className="model-output">
      <div className="flex justify-between gap-3">
        <h3>Model output</h3>
        <Button onClick={() => setRaw(!raw)}>
          {raw ? "Structured view" : "Raw JSON"}
        </Button>
      </div>
      <p className="caption">
        Model intent · validation and fills are separate records
      </p>
      {actions && !raw ? (
        actions.length ? (
          <Table
            columns={[
              { key: "instrument_key", label: "Instrument" },
              { key: "side", label: "Side" },
              { key: "units", label: "Units" },
              {
                key: "max_price",
                label: "Limit",
                render: (v) => amount(v.max_price),
              },
              { key: "rationale", label: "Rationale" },
            ]}
            rows={actions}
          />
        ) : (
          <p className="abstention">
            <Check size={15} /> No actions proposed. Inspect the rationale
            below.
          </p>
        )
      ) : null}
      {(!actions || raw || actions.length === 0) && <pre>{pretty(output)}</pre>}
    </section>
  );
}
export function Transcript({ id, onBack }: { id: string; onBack: () => void }) {
  const [state, app] = useWorkspace(),
    [detail, setDetail] = useState<RunResponse | null>(null),
    [error, setError] = useState(""),
    [limit, setLimit] = useState(200),
    [live, setLive] = useState<StreamEvent[]>([]);
  const generation = useRef(0),
    seen = useRef(0);
  const ended = useRef(false);
  async function load() {
    const request = ++generation.current,
      token = app.getSnapshot().token;
    try {
      const v = await app.api.run(id);
      if (request === generation.current && token === app.getSnapshot().token) {
        setDetail(v);
        setLive([]);
        setError(v.unavailable ? "Run archive unavailable." : "");
      }
    } catch {
      if (request === generation.current && token === app.getSnapshot().token)
        setError("Run archive unavailable.");
    }
  }
  useEffect(() => {
    setDetail(null);
    setError("");
    setLive([]);
    setLimit(200);
    ended.current = false;
    seen.current = state.revision;
    void load();
    return () => {
      generation.current++;
    };
  }, [id]);
  useEffect(() => {
    if (!detail?.running) return;
    const events = state.events.slice(
      Math.max(0, state.events.length - (state.revision - seen.current)),
    );
    seen.current = state.revision;
    for (const event of events) {
      if ((event.archive_run || event.run) !== id) continue;
      if (event.kind === "run_ended" && !ended.current) {
        ended.current = true;
        void load();
      } else setLive((v) => [...v, event].slice(-200));
    }
  }, [state.revision, detail?.running, id]);
  const events = detail?.events || [],
    start = events.find((v) => v.kind === "run_started"),
    end = events.find((v) => v.kind === "run_ended");
  return (
    <div className="transcript">
      <div className="transcript-toolbar">
        <Button icon={ChevronLeft} onClick={onBack}>
          Archive
        </Button>
        <span>
          {detail?.legacy
            ? "LEGACY · PARTIAL"
            : detail?.running
              ? "RUNNING · DISPLAY HINTS"
              : "ARCHIVED TRANSCRIPT"}
        </span>
      </div>
      <header className="transcript-title">
        <div className="flex items-center gap-3">
          <Bot size={19} />
          <h2>
            {words(start?.role)}{" "}
            <span>/ {start?.model || "Model not reported"}</span>
          </h2>
        </div>
        <code>{id}</code>
        <div className="run-facts">
          <span>{datetime(start?.at_ms)}</span>
          <span>{duration(end?.duration_ms)}</span>
          <span>
            {count(end?.tokens_in)} in / {count(end?.tokens_out)} out
          </span>
          <span>
            {end?.cost_estimate_inr == null
              ? "Cost unknown"
              : amount(end.cost_estimate_inr)}
          </span>
          <Status
            value={
              detail?.running
                ? "running"
                : end?.outcome || (detail ? "interrupted" : undefined)
            }
          />
        </div>
      </header>
      <Notice error>{error}</Notice>
      {!detail && !error && <Empty title="Opening the original archive…" />}
      {events.slice(0, limit).map((event, index) => {
        const key = event.seq ?? index;
        if (event.kind === "input")
          return (
            <LazyEvidence
              key={key}
              label="Input received"
              event={event}
              id={id}
              initial={
                event.blob ? undefined : pretty(event.input ?? event.summary)
              }
            />
          );
        if (event.kind === "system_prompt")
          return (
            <LazyEvidence
              key={key}
              label={
                "System prompt · " + (event.sha?.slice(0, 10) || "unknown")
              }
              event={event}
              id={id}
              kind="prompt"
            />
          );
        if (event.kind === "assistant_text")
          return event.blob ? (
            <LazyEvidence
              key={key}
              label="Assistant message · archived source"
              event={event}
              id={id}
            />
          ) : (
            <Prose key={key} text={event.text} />
          );
        if (event.kind === "tool_call") {
          const result = events.find(
            (v) =>
              v.kind === "tool_result" &&
              v.call === event.call &&
              v.turn === event.turn,
          );
          const payload = { ...result, blob: result?.blob };
          return (
            <div key={key} className="tool-row">
              <div className="tool-heading">
                <Terminal size={13} />
                <strong>{event.name}</strong>
                <span>{duration(result?.duration_ms)}</span>
                <Status
                  value={
                    result?.status || (detail?.running ? "running" : "unknown")
                  }
                />
              </div>
              <details className="tool-args">
                <summary>
                  Arguments{event.args_truncated ? " · recorded prefix" : ""}
                </summary>
                <pre>{pretty(event.args)}</pre>
              </details>
              {(event.blob || event.args_truncated) && (
                <LazyEvidence
                  label="Full archived call source"
                  event={{
                    blob:
                      event.blob ||
                      `turn-${String(event.turn ?? 0).padStart(2, "0")}-response.json`,
                  }}
                  id={id}
                />
              )}
              {result ? (
                <LazyEvidence
                  label="Tool result"
                  event={payload}
                  id={id}
                  initial={
                    result.blob
                      ? undefined
                      : pretty(result.result ?? result.summary)
                  }
                />
              ) : (
                <p className="caption">
                  {detail?.running ? "Running…" : "No terminal result recorded"}
                </p>
              )}
            </div>
          );
        }
        if (event.kind === "provider_tool")
          return (
            <p className="provider-tool" key={key}>
              <Terminal size={12} />
              Provider tool · {event.name || event.tool || "unknown"}
            </p>
          );
        if (event.kind === "final_output")
          return event.blob ? (
            <LazyEvidence
              key={key}
              label="Model output"
              event={event}
              id={id}
            />
          ) : (
            <Output key={key} output={event.output ?? event.value} />
          );
        if (["host_note", "truncated"].includes(event.kind))
          return (
            <p key={key} className="host-note">
              {event.text || "Archive truncated by the producing version"}
            </p>
          );
        return null;
      })}
      {events.length > limit && (
        <Button onClick={() => setLimit(limit + 200)}>
          Show next 200 events
        </Button>
      )}
      {live.length > 0 && (
        <section className="live-hints">
          <h3>
            Live activity <span>Unvalidated display hints</span>
          </h3>
          {live.map((e, i) => (
            <div key={i}>
              <time>{datetime(e.at_ms)}</time>
              <span>{words(e.kind)}</span>
              {e.text && <Prose text={e.text} />}
            </div>
          ))}
        </section>
      )}
      {end && (
        <footer className="transcript-footer">
          {count(end.tool_calls)} tool calls · {count(end.tool_failures)}{" "}
          failures · original terminal record
        </footer>
      )}
    </div>
  );
}
export function ModelRoster({ compact = false }) {
  const [state] = useWorkspace();
  const approvalsUnavailable =
    state.approvalsState === "error" || state.approvalsState === "unavailable";
  return (
    <div className={compact ? "model-roster compact-roster" : "model-roster"}>
      {identities(state).map(({ id, label, role }) => (
        <div key={id} className={"model-identity " + id}>
          <div className="model-avatar">{id === "digest" ? "L" : "O"}</div>
          <div className="model-info">
            <strong>{label}</strong>
            <small>
              {role?.model ||
                (approvalsUnavailable
                  ? "Approval projection unavailable"
                  : "Model not reported")}
            </small>
            {!compact && (
              <p>
                {id === "digest"
                  ? "Compiles factual evidence for the orchestrator."
                  : "Owns paper actions, sizes, holding periods and research."}
              </p>
            )}
          </div>
          <div className="model-health">
            <Status
              value={
                role?.status || (approvalsUnavailable ? "unavailable" : null)
              }
            />
            {!compact && (
              <small>
                {role
                  ? count(role.remaining_current) + " remaining"
                  : approvalsUnavailable
                    ? "Allowance unavailable"
                    : "Allowance unknown"}
              </small>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}
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
export default function Models({ selectedRun }: { selectedRun?: string }) {
  const [state, app] = useWorkspace(),
    [rows, setRows] = useState<RunSummary[]>([]),
    [next, setNext] = useState<string | null>(null),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const [filters, setFilters] = useState({
    identity: "all",
    role: "",
    date: "",
    outcome: "",
    trigger: "",
    instrument: "",
    failed: false,
  });
  const generation = useRef(0),
    applied = useRef(0);
  const [draft, setDraft] = useState({ instrument: "", trigger: "" });
  const key = JSON.stringify(filters);
  async function load(more = false) {
    const request = ++generation.current,
      token = app.getSnapshot().token;
    setBusy(true);
    setError("");
    try {
      const { identity, ...params } = filters;
      const page = await app.api.runs({
        ...params,
        ...(identity === "all" ? {} : { identity }),
        failed: params.failed ? "1" : "",
        limit: 50,
        ...(more && next ? { cursor: next } : {}),
      });
      if (request !== generation.current || token !== app.getSnapshot().token)
        return;
      setRows((v) =>
        more
          ? [
              ...new Map(
                [...v, ...(page.runs || [])].map((r) => [r.run, r]),
              ).values(),
            ]
          : page.runs || [],
      );
      setNext(page.next ?? null);
      if (page.unavailable) setError("Archive source unavailable.");
    } catch {
      if (request === generation.current) {
        setError("Run index unavailable.");
        if (!more) setRows([]);
      }
    } finally {
      if (request === generation.current) setBusy(false);
    }
  }
  useEffect(() => {
    void load();
    return () => {
      generation.current++;
    };
  }, [key]);
  useEffect(() => {
    const last = state.events.at(-1);
    if (state.revision === applied.current) return;
    applied.current = state.revision;
    if (["run_started", "run_ended", "gap"].includes(last?.kind || ""))
      void load();
  }, [state.revision]);
  // Identity is applied by /api/v1/runs before paging, so `next` cannot skip
  // older orchestrator runs behind a page of digest rows.
  const shown = rows;
  if (selectedRun)
    return (
      <Transcript
        key={selectedRun}
        id={selectedRun}
        onBack={() => {
          location.hash = "#/models";
        }}
      />
    );
  return (
    <>
      <ModelRoster />
      <LaneTurns />
      <WebToolActivity />
      <Panel
        title="Run archive"
        note="Select a run to inspect the full conversation and tool evidence."
        actions={<span className="caption">{count(shown.length)} loaded</span>}
      >
        <div className="filter-bar">
          <Segments
            value={filters.identity}
            onChange={(identity) => setFilters({ ...filters, identity })}
            items={[
              ["all", "All identities"],
              ...Object.entries(identityLabels),
            ]}
            label="Model identity"
          />
          <div className="filter-fields">
            <label className="search-field">
              <SearchIcon />
              <input
                placeholder="Instrument key…"
                aria-label="Filter run instrument"
                value={draft.instrument}
                onChange={(e) =>
                  setDraft({ ...draft, instrument: e.target.value })
                }
                onBlur={() =>
                  setFilters({
                    ...filters,
                    instrument: draft.instrument.trim(),
                  })
                }
                onKeyDown={(e) => {
                  if (e.key === "Enter") e.currentTarget.blur();
                }}
              />
            </label>
            <input
              type="date"
              aria-label="Run date"
              value={filters.date}
              onChange={(e) => setFilters({ ...filters, date: e.target.value })}
            />
            <select
              aria-label="Run outcome"
              value={filters.outcome}
              onChange={(e) =>
                setFilters({ ...filters, outcome: e.target.value })
              }
            >
              <option value="">All outcomes</option>
              {[
                "accepted",
                "abstain",
                "failure",
                "running",
                "stopped",
                "interrupted",
              ].map((v) => (
                <option key={v}>{v}</option>
              ))}
            </select>
            <input
              aria-label="Run trigger"
              placeholder="Trigger…"
              value={draft.trigger}
              onChange={(e) => setDraft({ ...draft, trigger: e.target.value })}
              onBlur={() =>
                setFilters({ ...filters, trigger: draft.trigger.trim() })
              }
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
              }}
            />
            <label className="checkbox">
              <input
                type="checkbox"
                checked={filters.failed}
                onChange={(e) =>
                  setFilters({ ...filters, failed: e.target.checked })
                }
              />
              Failures only
            </label>
          </div>
        </div>
        <Notice error>{error}</Notice>
        <Table
          getKey={(r) => r.run}
          rows={shown}
          onSelect={(r) => {
            location.hash = runHref(r.run);
          }}
          columns={[
            {
              key: "model",
              label: "Model / run",
              render: (r) => (
                <Instrument symbol={r.model || "Unknown model"} sub={r.run} />
              ),
            },
            { key: "role", label: "Task", render: (r) => words(r.role) },
            {
              key: "started_ms",
              label: "Started · IST",
              render: (r) => datetime(r.started_ms),
            },
            {
              key: "duration_ms",
              label: "Duration",
              render: (r) => duration(r.duration_ms),
            },
            {
              key: "tokens",
              label: "Tokens · in / out",
              render: (r) => count(r.tokens_in) + " / " + count(r.tokens_out),
            },
            {
              key: "tools",
              label: "Tools",
              render: (r) => count(r.tool_calls),
            },
            {
              key: "outcome",
              label: "Outcome",
              render: (r) => (
                <Status value={r.running ? "running" : r.outcome} />
              ),
            },
          ]}
          empty={busy ? "Loading run archive…" : "No matching loaded runs"}
        />
        <div className="table-footer">
          <span>
            Identity filters apply to loaded archive pages. Legacy fields stay
            unknown.
          </span>
          {next && (
            <Button onClick={() => load(true)} disabled={busy}>
              {busy ? "Loading…" : "Load more runs"}
            </Button>
          )}
        </div>
      </Panel>
    </>
  );
}
function SearchIcon() {
  return <span className="search-glyph">⌕</span>;
}
