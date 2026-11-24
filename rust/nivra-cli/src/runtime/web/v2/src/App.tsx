import React, { useEffect, useMemo, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import {
  ArrowRight,
  ArrowUpRight,
  LockKeyhole,
  Menu,
  PanelLeftClose,
  PanelLeftOpen,
  RefreshCw,
  Search,
  X,
} from "lucide-react";
import {
  clock,
  datetime,
  duration,
  finance,
  numeric,
  observation,
  route as parseRoute,
  sessionKey,
  sessions,
} from "./data";
import { instrumentLabel, modeOf } from "./model";
import type { Mode } from "./model";
import {
  Button,
  Drawer,
  Empty,
  Facts,
  IconButton,
  NAV,
  NAV_GROUPS,
  Notice,
  useWorkspace,
} from "./ui";
import { PAGES } from "./Pages";
import { PROJECTION_NAMES } from "./types";
import type { Connection, SummaryResponse, WorkspaceState } from "./types";

function Brand() {
  return (
    <a href="#/overview" className="brand" aria-label="Nivra overview">
      nivra <b>/ ops</b>
    </a>
  );
}
function Login() {
  const [state, app] = useWorkspace(),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  async function submit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setBusy(true);
    setError("");
    const form = e.currentTarget;
    const token = new FormData(form).get("token");
    form.reset();
    try {
      await app.authenticate(token);
    } catch {
      setError(
        "Connection refused or unavailable. Check the dashboard token and retry.",
      );
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="login-layout">
      <div className="login-brand">
        <Brand />
        <span>PRIVATE WORKSPACE</span>
      </div>
      <main className="login-content">
        <span className="eyebrow">NIVRA / OWNER ACCESS</span>
        <h1>
          Every decision.
          <br />
          <span>The evidence behind it.</span>
        </h1>
        <p>
          Connect to inspect the account, pi lane turns and original market
          observations.
        </p>
        <form onSubmit={submit} aria-label="Connect to Nivra">
          <label>
            Dashboard token
            <input
              name="token"
              id="token"
              type="password"
              autoComplete="off"
              required
              minLength={32}
              placeholder="Enter this installation’s token"
            />
          </label>
          <Notice error>{error}</Notice>
          <Button
            className="button-primary"
            disabled={busy || (!!state.token && !state.meta)}
          >
            {busy || state.token ? "Connecting…" : "Unlock workspace"}
            <ArrowRight size={14} />
          </Button>
        </form>
        <small>
          The token stays in this browser tab. Broker and model credentials do
          not belong here.
        </small>
      </main>
      <div className="login-footer">
        <span>Authenticated evidence · client-side workspace</span>
      </div>
    </div>
  );
}

type Tone = "pos" | "warn" | "neg" | "off";
const STREAM: Record<Connection, [Tone, string]> = {
  live: ["pos", "SSE"],
  polling: ["warn", "long-poll"],
  stale: ["neg", "stale"],
  locked: ["off", "locked"],
};
function feedTone(summary: SummaryResponse | null): Tone {
  const feed = summary?.market_feed;
  if (!feed) return "off";
  const age = numeric(feed.last_tick_age_ms);
  if (/fail|error|down|disconnect/i.test(feed.state || "")) return "neg";
  if (feed.fallback === true || (age !== null && age > 15_000)) return "warn";
  if (/stale|degraded|reconnect/i.test(feed.state || "")) return "warn";
  return age === null && !feed.state ? "off" : "pos";
}
function ModePill({ mode }: { mode: Mode }) {
  return (
    <span className={"mode-pill " + mode.kind}>
      {mode.kind === "paper"
        ? "PAPER"
        : mode.kind === "live"
          ? "LIVE"
          : "MODE UNKNOWN"}
    </span>
  );
}
function StatusBar({
  mode,
  now,
  onNav,
  onProvenance,
}: {
  mode: Mode;
  now: number;
  onNav: () => void;
  onProvenance: () => void;
}) {
  const [state, app] = useWorkspace(),
    { meta, summary } = state;
  const [stream, streamLabel] = STREAM[state.connection];
  const alerts = numeric(summary?.alert_count ?? summary?.alerts?.length);
  const tick = numeric(summary?.market_feed?.last_tick_age_ms);
  const orders = meta?.broker?.orders;
  const account = mode.kind === "live" ? mode.account : meta?.account;
  return (
    <header className="statusbar">
      <IconButton
        className="nav-toggle"
        label="Open navigation"
        icon={Menu}
        onClick={onNav}
      />
      <Brand />
      <ModePill mode={mode} />
      <span className="kv hide-narrow">
        data <span>{meta?.broker?.data || "Not reported"}</span>
      </span>
      <span
        className="kv hide-narrow"
        title="The broker identity that orders are bound to"
      >
        orders <span>{orders || "Not reported"}</span>
      </span>
      <span className="kv hide-narrow">
        account <span>{account || "Not reported"}</span>
      </span>
      <button className="kv row-link hide-narrow" onClick={onProvenance}>
        session{" "}
        <span>
          {[summary?.session, summary?.state?.phase]
            .filter(Boolean)
            .join(" · ") || "Not reported"}
        </span>
      </button>
      <span className="spacer" />
      <span className="kv" title="Age of the last market tick received">
        <i className={"dot " + feedTone(summary)} />{" "}
        <em className="kv-label">feed</em>
        <span>{tick === null ? "—" : duration(tick)}</span>
      </span>
      <span className="kv">
        <i className={"dot " + stream} /> <em className="kv-label">stream</em>{" "}
        <span>{streamLabel}</span>
      </span>
      <a className="kv hide-narrow" href="#/health">
        <i
          className={
            "dot " + (alerts === null ? "off" : alerts ? "warn" : "pos")
          }
        />{" "}
        alerts <span>{alerts === null ? "—" : alerts}</span>
      </a>
      <time
        className="kv hide-narrow"
        title="Display only; never a receipt time"
      >
        {clock(now)} IST
      </time>
      <IconButton
        label="Refresh"
        icon={RefreshCw}
        disabled={state.snapshotState === "loading"}
        onClick={() => void app.refresh()}
      />
      <IconButton
        label="Lock workspace"
        icon={LockKeyhole}
        onClick={app.lock}
      />
    </header>
  );
}
function Ticker() {
  const [state, app] = useWorkspace();
  const items = useMemo(() => {
    const rows = Array.isArray(state.scanner?.items) ? state.scanner.items : [];
    return rows.slice(0, 16).flatMap((row) => {
      const key = row.instrument_key;
      const seen = observation(row);
      const quote = key ? state.quotes[key] : undefined;
      const price =
        quote && quote.at >= (seen?.at ?? 0) ? quote.price : seen?.value;
      if (!key || price === undefined) return [];
      const bps = numeric(row.sample_change_bps);
      return [
        {
          key,
          label: instrumentLabel({ ...row.instrument, symbol: row.symbol })
            .primary,
          price,
          change: bps === null ? null : bps / 100,
        },
      ];
    });
  }, [state.scanner, state.quotes]);
  if (!items.length) return null;
  return (
    <div className="ticker num" aria-label="Receipt prices">
      {items.map((item) => (
        <button
          key={item.key}
          className={item.key === state.watch ? "on" : ""}
          title="Show this instrument on the overview chart"
          onClick={() => {
            app.watch(item.key);
            location.hash = "#/overview";
          }}
        >
          <b>{item.label}</b>
          {item.price.toLocaleString("en-IN", { maximumFractionDigits: 2 })}
          {item.change !== null && (
            <span
              className={item.change < 0 ? "neg" : item.change > 0 ? "pos" : ""}
            >
              {item.change > 0 ? "+" : item.change < 0 ? "\u2212" : ""}
              {Math.abs(item.change).toFixed(2)}%
            </span>
          )}
        </button>
      ))}
    </div>
  );
}
/** Counts only where the projection is loaded; unknown shows nothing. */
function navCounts(
  state: WorkspaceState,
  app: { record: () => ReturnType<typeof sessions>[number] | null },
) {
  const f = finance(app.record());
  const fills = numeric(state.fills?.total ?? state.fills?.items?.length);
  const turns = numeric(state.agent?.total);
  const alerts = numeric(state.summary?.alert_count);
  return {
    portfolio:
      f.positionsKnown && fills !== null
        ? `${f.positions.length}/${fills}`
        : null,
    lanes:
      turns === null
        ? null
        : state.agent?.truncated
          ? `${state.agent.turns?.length ?? 0}+`
          : String(turns),
    health: alerts ? String(alerts) : null,
  } as Record<string, string | null>;
}
function NavList({
  route,
  compact,
  counts,
  onCollapse,
}: {
  route: string;
  compact?: boolean;
  counts: Record<string, string | null>;
  onCollapse?: () => void;
}) {
  return (
    <nav className="nav" aria-label="Workspace">
      {NAV_GROUPS.map(({ group, items }) => (
        <React.Fragment key={group}>
          <h6>{group}</h6>
          {items.map(({ id, label, icon: Icon }) => (
            <a
              href={"#/" + id}
              key={id}
              className={route === id ? "active" : ""}
              aria-current={route === id ? "page" : undefined}
              title={compact ? label : undefined}
            >
              <Icon size={15} />
              <span className="label">{label}</span>
              {counts[id] && <small>{counts[id]}</small>}
            </a>
          ))}
        </React.Fragment>
      ))}
      {onCollapse && (
        <div className="nav-foot">
          <button
            className="nav-collapse"
            onClick={onCollapse}
            aria-label={compact ? "Expand navigation" : "Collapse navigation"}
          >
            {compact ? (
              <PanelLeftOpen size={15} />
            ) : (
              <PanelLeftClose size={15} />
            )}
            <span>Collapse</span>
          </button>
        </div>
      )}
    </nav>
  );
}
const NAV_KEY = "nivraNavCompact";
function readCompact() {
  try {
    return localStorage.getItem(NAV_KEY) === "1";
  } catch {
    return false;
  }
}
export default function App() {
  const [state, app] = useWorkspace(),
    [route, setRoute] = useState(() => parseRoute(location.hash)),
    [command, setCommand] = useState(false),
    [query, setQuery] = useState(""),
    [mobile, setMobile] = useState(false),
    [compact, setCompact] = useState(readCompact),
    [source, setSource] = useState(false),
    [now, setNow] = useState(Date.now());
  useEffect(() => {
    const change = () => {
      setRoute(parseRoute(location.hash));
      setMobile(false);
      document.querySelector(".content")?.scrollTo?.(0, 0);
    };
    const key = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setCommand((v) => !v);
      }
    };
    addEventListener("hashchange", change);
    addEventListener("keydown", key);
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => {
      removeEventListener("hashchange", change);
      removeEventListener("keydown", key);
      clearInterval(timer);
    };
  }, []);
  useEffect(() => {
    if (!state.meta) {
      setCommand(false);
      setMobile(false);
      setSource(false);
      setQuery("");
    }
  }, [state.meta]);
  const nav = NAV.find(({ id }) => id === route.name),
    title = nav?.label || "Page not found";
  const mode = modeOf(state.meta, state.summary),
    live = mode.kind === "live";
  // During controller startup the v1 view is not published yet: health and
  // summary report `starting` while every projection is still `*_not_reported`.
  // That is a loading state, not missing evidence, so retry until it publishes.
  const starting =
    state.summary?.status === "starting" ||
    state.summary?.state?.phase === "starting";
  useEffect(() => {
    if (!starting) return;
    const timer = setInterval(() => void app.refresh(), 1000);
    return () => clearInterval(timer);
  }, [starting, app]);
  useEffect(() => {
    document.title = (live ? "LIVE · " : "Nivra · ") + title;
    let icon = document.querySelector<HTMLLinkElement>('link[rel="icon"]');
    if (live) {
      if (!icon) {
        icon = document.createElement("link");
        icon.rel = "icon";
        document.head.append(icon);
      }
      const fill = getComputedStyle(document.documentElement)
        .getPropertyValue("--live")
        .trim();
      icon.href =
        "data:image/svg+xml," +
        encodeURIComponent(
          `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="7" fill="${fill}"/></svg>`,
        );
    } else icon?.remove();
  }, [live, title]);
  function toggleCompact() {
    const next = !compact;
    setCompact(next);
    try {
      localStorage.setItem(NAV_KEY, next ? "1" : "0");
    } catch {
      /* optional preference */
    }
  }
  if (!state.meta) return <Login />;
  const record = app.record(),
    records = sessions(state, state.meta),
    Page = route.name === "missing" ? undefined : PAGES[route.name];
  const counts = navCounts(state, app);
  return (
    <div
      className={
        "shell" + (compact ? " nav-compact" : "") + (live ? " live" : "")
      }
    >
      <StatusBar
        mode={mode}
        now={now}
        onNav={() => setMobile(true)}
        onProvenance={() => setSource(true)}
      />
      <Ticker />
      <NavList
        route={route.name}
        compact={compact}
        counts={counts}
        onCollapse={toggleCompact}
      />
      <main className="content" id="main-content">
        <div className="page-head">
          <h1>{title}</h1>
          <p>{nav?.blurb}</p>
          <div className="page-tools">
            <button
              className="kv row-link"
              onClick={() => setCommand(true)}
              aria-label="Open command palette"
            >
              <Search size={12} /> Jump to <kbd>Ctrl K</kbd>
            </button>
            {records.length > 0 && (
              <label className="session-select">
                <span>Session</span>
                <select
                  aria-label="Selected session"
                  value={sessionKey(record)}
                  onChange={(e) => app.selectSession(e.target.value)}
                >
                  {records.map((r) => (
                    <option key={sessionKey(r)} value={sessionKey(r)}>
                      {r.id?.date} · {r.id?.account} · {r.id?.broker}
                    </option>
                  ))}
                </select>
              </label>
            )}
          </div>
        </div>
        <Notice error>{state.error}</Notice>
        {starting ? (
          <Notice>
            Controller starting: the v1 evidence view is not published yet.
            Missing values stay unknown; this page retries automatically.
          </Notice>
        ) : (
          state.snapshotState === "unavailable" && (
            <Notice>
              Unavailable v1 evidence:{" "}
              {state.issues
                .map((issue) => `${issue.source} (${issue.reason})`)
                .join(" · ") || "unknown source"}
              . Missing values stay unknown; no zero, healthy or complete state
              is inferred.
            </Notice>
          )
        )}
        {Page ? (
          <Page key={route.name} />
        ) : (
          <Empty title="Page not found">
            Choose a section from the workspace navigation.
          </Empty>
        )}
      </main>
      <Dialog.Root
        open={command}
        onOpenChange={(v) => {
          setCommand(v);
          if (!v) setQuery("");
        }}
      >
        <Dialog.Portal>
          <div className="dialog-overlay" aria-hidden="true" />
          <Dialog.Content className="command-dialog">
            <Dialog.Title className="sr-only">
              Jump to a workspace section
            </Dialog.Title>
            <Dialog.Description className="sr-only">
              Search the workspace sections. Use Tab and Enter to navigate.
            </Dialog.Description>
            <div className="command-input">
              <Search size={15} />
              <input
                autoFocus
                placeholder="Where do you want to go?"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
              <Dialog.Close asChild>
                <button aria-label="Close command palette">
                  <kbd>esc</kbd>
                </button>
              </Dialog.Close>
            </div>
            <div className="command-options">
              {NAV.filter(({ label }) =>
                label.toLowerCase().includes(query.toLowerCase()),
              ).map(({ id, label, icon: Icon }) => (
                <button
                  key={id}
                  onClick={() => {
                    location.hash = "#/" + id;
                    setCommand(false);
                    setQuery("");
                  }}
                >
                  <Icon size={14} />
                  {label}
                  <ArrowUpRight size={12} />
                </button>
              ))}
            </div>
            <div className="command-footer">
              Workspace navigation <span>↵ Open</span>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
      <Dialog.Root open={mobile} onOpenChange={setMobile}>
        <Dialog.Portal>
          <div className="dialog-overlay" aria-hidden="true" />
          <Dialog.Content className="mobile-nav">
            <Dialog.Title className="sr-only">
              Workspace navigation
            </Dialog.Title>
            <Dialog.Description className="sr-only">
              Select a section.
            </Dialog.Description>
            <Dialog.Close
              className="mobile-close"
              aria-label="Close navigation"
            >
              <X size={16} />
            </Dialog.Close>
            <NavList route={route.name} counts={counts} />
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
      <Drawer
        open={source}
        onClose={() => setSource(false)}
        title="Snapshot provenance"
        description="A received snapshot is not a live quote or worker heartbeat."
      >
        <Facts
          items={[
            ["Broker", record?.id?.broker || state.summary?.broker?.data],
            ["Account", record?.id?.account || state.summary?.account],
            ["Session", record?.id?.date || state.summary?.session],
            [
              "Source checkpoint",
              datetime(state.summary?.as_of_ms || state.portfolio?.as_of_ms),
            ],
            ["Browser received", datetime(state.received)],
            ["Transport", state.connection],
            [
              "v1 projections",
              PROJECTION_NAMES.map(
                (name) => `${name}:${state[`${name}State`]}`,
              ).join(" · "),
            ],
            [
              "Chart series",
              state.series?.price?.length || state.series?.equity?.length
                ? `${state.series.price.length} price · ${state.series.equity.length} equity · ${state.series.priceCached || state.series.equityCached ? "stale cache" : "read-through"}`
                : "Not loaded",
            ],
            ["Mode", state.meta.mode],
            ["Execution gate", "Original quote source and receipt · 2 seconds"],
          ]}
        />
      </Drawer>
    </div>
  );
}
