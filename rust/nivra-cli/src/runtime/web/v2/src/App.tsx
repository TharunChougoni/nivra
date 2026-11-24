import React, { useEffect, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import {
  ArrowUpRight,
  ChevronsUpDown,
  Command,
  LockKeyhole,
  Menu,
  Moon,
  PanelLeftClose,
  RefreshCw,
  Search,
  Sun,
  X,
  ChevronRight,
  ArrowRight,
  Link2,
} from "lucide-react";
import {
  clock,
  datetime,
  route as parseRoute,
  sessionKey,
  sessions,
} from "./data";
import {
  Context,
  Button,
  Drawer,
  Empty,
  IconButton,
  NAV,
  Notice,
  Status,
  useWorkspace,
} from "./ui";
import { PAGES } from "./Pages";
import { PROJECTION_NAMES } from "./types";
function Brand() {
  return (
    <a href="#/overview" className="brand" aria-label="Nivra overview">
      <span className="brand-mark">i↗</span>
      <span>
        <span className="wordmark">
          <b>ind</b>
          <span>nivra</span>
        </span>
        <small>OPERATIONS WORKSPACE</small>
      </span>
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
        <span className="caption">PRIVATE PAPER WORKSPACE</span>
      </div>
      <main className="login-content">
        <span className="eyebrow">IND-Nivra / OWNER ACCESS</span>
        <h1>
          Every decision.
          <br />
          <span>The evidence behind it.</span>
        </h1>
        <p>
          Connect to your paper workspace to inspect the account, model
          conversations and original market observations.
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
        Paper only · authenticated evidence · client-side workspace
      </div>
    </div>
  );
}
export default function App() {
  const [state, app] = useWorkspace(),
    [route, setRoute] = useState(() => parseRoute(location.hash)),
    [command, setCommand] = useState(false),
    [query, setQuery] = useState(""),
    [mobile, setMobile] = useState(false),
    [compact, setCompact] = useState(false),
    [source, setSource] = useState(false),
    [now, setNow] = useState(Date.now()),
    [theme, setTheme] = useState(
      () => document.documentElement.dataset.theme || "dark",
    );
  useEffect(() => {
    const change = () => {
      setRoute(parseRoute(location.hash));
      setMobile(false);
      document.querySelector(".workspace-content")?.scrollTo?.(0, 0);
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
  const title = NAV.find(([id]) => id === route.name)?.[1] || "Page not found";
  const live = state.meta?.live_enabled === true || state.meta?.mode === "live";
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
      icon.href =
        "data:image/svg+xml," +
        encodeURIComponent(
          '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="8" r="7" fill="#df566d"/></svg>',
        );
    } else icon?.remove();
  }, [live, title]);
  function toggleTheme() {
    const next = theme === "dark" ? "light" : "dark";
    document.documentElement.dataset.theme = next;
    setTheme(next);
    try {
      localStorage.setItem("nivraTheme", next);
    } catch {
      /* optional preference */
    }
  }
  if (!state.meta) return <Login />;
  const record = app.record(),
    records = sessions(state, state.meta),
    Page = PAGES[route.name];
  const nav = (
    <>
      <div className="sidebar-top">
        <Brand />
        <IconButton
          label={compact ? "Expand navigation" : "Collapse navigation"}
          icon={PanelLeftClose}
          onClick={() => setCompact(!compact)}
        />
      </div>
      <div className="nav-caption">WORKSPACE</div>
      <nav aria-label="Workspace">
        {NAV.map(([id, text, Icon]) => (
          <a
            href={"#/" + id}
            key={id}
            className={route.name === id ? "active" : ""}
            aria-current={route.name === id ? "page" : undefined}
            title={compact ? text : undefined}
          >
            <Icon size={17} />
            <span>{text}</span>
            {route.name === id && <span className="active-indicator" />}
          </a>
        ))}
      </nav>
      <div className="sidebar-spacer" />
      <div className="sidebar-bottom">
        <div className="owner-avatar">P</div>
        <div>
          <strong>{state.meta.account || "Account not reported"}</strong>
          <small>
            {state.meta.broker?.data || "Broker unknown"} ·{" "}
            {state.meta.version || "Version unknown"}
          </small>
        </div>
        <LockKeyhole size={13} />
      </div>
    </>
  );
  return (
    <div className={"workspace-shell " + (compact ? "nav-compact" : "")}>
      <aside className="workspace-sidebar">{nav}</aside>
      <div className="workspace-body">
        <header className="workspace-topbar">
          <div className="topbar-left">
            <IconButton
              label="Open navigation"
              icon={Menu}
              onClick={() => setMobile(true)}
            />
            <span>Workspace</span>
            <ChevronRight size={12} />
            <strong>{title}</strong>
          </div>
          <button className="global-search" onClick={() => setCommand(true)}>
            <Search size={13} />
            <span>Jump to…</span>
            <kbd>⌘ K</kbd>
          </button>
          <div className="topbar-right">
            <span className={"mode-banner " + (live ? "live" : "paper")}>
              {live
                ? "LIVE"
                : state.meta.mode === "paper" || state.meta.paper_only
                  ? "PAPER"
                  : "MODE UNKNOWN"}
            </span>
            <span
              className="broker-banner"
              title={`Market data broker: ${state.meta.broker?.data || "Not reported"}; order broker: ${state.meta.broker?.orders || "Not reported"}`}
              aria-label={`Market data broker ${state.meta.broker?.data || "not reported"}; order broker ${state.meta.broker?.orders || "not reported"}`}
            >
              Data {state.meta.broker?.data || "Not reported"} · Orders{" "}
              {state.meta.broker?.orders || "Not reported"}
            </span>
            <Status value={state.connection}>
              {state.connection === "live"
                ? "Stream connected"
                : state.connection === "polling"
                  ? "Held polling"
                  : "Stream " + state.connection}
            </Status>
            <IconButton
              label={
                theme === "dark"
                  ? "Switch to light theme"
                  : "Switch to dark theme"
              }
              icon={theme === "dark" ? Sun : Moon}
              onClick={toggleTheme}
            />
            <button
              className="icon-button lock"
              onClick={app.lock}
              aria-label="Lock workspace"
              title="Lock workspace"
            >
              <LockKeyhole size={15} />
            </button>
          </div>
        </header>
        <main className="workspace-content" id="main-content">
          <div className="page-title-row">
            <div>
              <div className="page-eyebrow">
                PAPER / {state.meta.broker?.data || "DATA SOURCE UNKNOWN"}
              </div>
              <h1>{title}</h1>
            </div>
            <div className="page-toolbar">
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
              <Button
                icon={RefreshCw}
                disabled={state.snapshotState === "loading"}
                onClick={app.refresh}
              >
                {state.snapshotState === "loading" ? "Refreshing" : "Refresh"}
              </Button>
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
                . Missing values stay unknown; no zero, healthy or complete
                state is inferred.
              </Notice>
            )
          )}
          {Page ? (
            <Page key={route.name} selectedRun={route.run} />
          ) : (
            <Empty title="Page not found">
              Choose a section from the workspace navigation.
            </Empty>
          )}
        </main>
        <footer className="workspace-statusbar">
          <button onClick={() => setSource(true)}>
            <span className={"status-pin " + state.snapshotState} />
            Snapshot{" "}
            {datetime(state.summary?.as_of_ms || state.portfolio?.as_of_ms)}
            <Link2 size={11} />
          </button>
          <span>
            Read only <i /> {clock(now)} IST
          </span>
        </footer>
      </div>
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
              Search the eight workspace sections. Use Tab and Enter to
              navigate.
            </Dialog.Description>
            <div className="command-input">
              <Search size={17} />
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
              {NAV.filter(([, label]) =>
                label.toLowerCase().includes(query.toLowerCase()),
              ).map(([id, label, Icon]) => (
                <button
                  key={id}
                  onClick={() => {
                    location.hash = "#/" + id;
                    setCommand(false);
                    setQuery("");
                  }}
                >
                  <Icon size={16} />
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
            {nav}
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
      <Drawer
        open={source}
        onClose={() => setSource(false)}
        title="Snapshot provenance"
        description="A received snapshot is not a live quote or worker heartbeat."
      >
        <dl className="fact-list">
          {[
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
          ].map(([label, value]) => (
            <div key={label}>
              <dt>{label}</dt>
              <dd>{value || "Not reported"}</dd>
            </div>
          ))}
        </dl>
      </Drawer>
    </div>
  );
}
