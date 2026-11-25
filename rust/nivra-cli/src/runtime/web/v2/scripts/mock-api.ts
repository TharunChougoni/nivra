// DEV-ONLY Vite plugin: answers the dashboard's GET /api/v1/* routes from the
// deterministic fixtures in ./mock-data.ts so the real frontend renders a full
// trading day. Registered only for `vite serve` with NIVRA_MOCK=1; it is never
// part of `vite build`, so the committed bundle is unaffected.
//
// Routes the real backend no longer serves (approvals, session, runs, blobs,
// prompts) answer 404 here too.
import { createHash } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import type { Plugin } from "vite";
import {
  agentAt,
  alertsAt,
  chainAt,
  createMockData,
  healthAt,
  instrumentsAt,
  metaFor,
  projections,
  quoteAt,
  replayExampleResult,
  seriesAt,
  sessionsAt,
  toolsAt,
  transcriptAt,
  type MockData,
} from "./mock-data";

const TOKEN = /^[0-9a-fA-F]{64}$/;
const EPOCH = "mock";
const HEARTBEAT_MS = 15_000;
const BATCH_MS = 2_000;
const SUMMARY_MS = 10_000;
const LIVE_RUN_PERIOD_MS = 60_000;

function send(
  res: ServerResponse,
  status: number,
  body: unknown,
  extra: Record<string, string> = {},
) {
  const text = JSON.stringify(body);
  res.writeHead(status, {
    "Content-Type": "application/json",
    "Cache-Control": "no-store",
    "Content-Length": Buffer.byteLength(text),
    ...extra,
  });
  res.end(text);
}
const fail = (res: ServerResponse, status: number, error: string) =>
  send(res, status, { v: 1, error });

/** Content hash of a projection, ignoring loop clocks (as the real backend does). */
const CLOCKS = new Set([
  "as_of_ms",
  "observed_at_ms",
  "captured_at_ms",
  "checked_at_ms",
]);
const revisionOf = (body: unknown) =>
  createHash("sha1")
    .update(JSON.stringify(body, (k, v) => (CLOCKS.has(k) ? undefined : v)))
    .digest("hex")
    .slice(0, 16);

export function mockApi(options: { seed?: number } = {}): Plugin {
  const data: MockData = createMockData(options.seed);
  const boot = Date.now();
  /** Virtual exchange clock: starts mid-afternoon and runs in real time to the close. */
  const virtualNow = () =>
    Math.min(data.startNowMs + (Date.now() - boot), data.closeMs - 10_000);
  let eventSeq = 0;
  const ring: { seq: number; frame: string; event: Record<string, unknown> }[] =
    [];
  const subscribers = new Set<(frame: string) => void>();

  const revisions = (now: number, wall: number) => ({
    portfolio: revisionOf(projections.portfolio(data, now)),
    fills: revisionOf(projections.fills(data, now)),
    plans: revisionOf(projections.plans(data, now)),
    scanner: revisionOf(projections.scanner(data, now, "prospects")),
    sessions: revisionOf(sessionsAt(data, now)),
    health: revisionOf(healthAt(data, now, wall)),
    alerts: revisionOf(alertsAt(data, now)),
    agent: revisionOf(agentAt(data, now, 1)),
    agent_turns: revisionOf(agentAt(data, now, 1).total),
  });

  function publish(name: string, body: Record<string, unknown>) {
    const seq = ++eventSeq;
    const event = { ...body, seq, epoch: EPOCH };
    const frame = `id: ${seq}\nevent: ${name}\ndata: ${JSON.stringify(name === "summary_changed" ? { etag: `"mock-${seq}"`, revisions: body.revisions } : event)}\n\n`;
    ring.push({ seq, frame, event });
    if (ring.length > 256) ring.shift();
    subscribers.forEach((write) => write(frame));
  }

  // A fake live pi turn every minute: started, a few activity events, ended.
  let liveRun = 0;
  function startLiveRun() {
    const id = `pi-live-${String(++liveRun).padStart(3, "0")}`;
    const lane = ["decision", "review"][liveRun % 2];
    const base = { archive_run: id, run: id, execution_authority: false };
    const at = () => virtualNow();
    publish("run_started", {
      ...base,
      kind: "run_started",
      role: lane,
      model: "gpt-6.1-sol",
      at_ms: at(),
    });
    const steps: [number, Record<string, unknown>][] = [
      [
        2_500,
        {
          kind: "text_delta",
          text: "Reading the position book and the latest receipts.",
        },
      ],
      [5_000, { kind: "tool_call", tool: "positions", status: "running" }],
      [
        7_500,
        {
          kind: "tool_result",
          tool: "positions",
          status: "ok",
          duration_ms: 18,
        },
      ],
      [
        10_000,
        {
          kind: "text_delta",
          text: "No candidate clears the entry bar; holding the book.",
        },
      ],
    ];
    for (const [delay, body] of steps)
      setTimeout(
        () => publish("run_event", { ...base, ...body, at_ms: at() }),
        delay,
      );
    setTimeout(
      () =>
        publish("run_ended", {
          ...base,
          kind: "run_ended",
          outcome: "abstain",
          at_ms: at(),
        }),
      12_500,
    );
  }

  function handle(req: IncomingMessage, res: ServerResponse, next: () => void) {
    const url = new URL(req.url ?? "/", "http://mock.local");
    if (!url.pathname.startsWith("/api/")) return next();
    if (!url.pathname.startsWith("/api/v1/"))
      return fail(res, 404, "not_found");
    const auth = String(req.headers.authorization ?? "");
    const match = /^Bearer (\S+)$/.exec(auth);
    if (!match || !TOKEN.test(match[1]))
      return fail(res, 401, "authentication_required");
    const route = url.pathname.slice("/api/v1/".length).replace(/\/$/, "");
    const q = url.searchParams;
    const num = (k: string) =>
      q.has(k) && Number.isFinite(Number(q.get(k)))
        ? Number(q.get(k))
        : undefined;
    const now = virtualNow();
    const wall = Date.now();

    if (req.method === "POST" && route === "backtest")
      return send(res, 200, replayExampleResult(data));
    if (req.method !== "GET") return fail(res, 405, "method_not_allowed");

    switch (route) {
      case "meta":
        return send(res, 200, metaFor(data));
      case "summary":
        return send(res, 200, {
          ...projections.summary(data, now, wall),
          revisions: revisions(now, wall),
        });
      case "portfolio":
        return send(res, 200, projections.portfolio(data, now));
      case "positions": {
        const items = projections.positions(data, now);
        return send(res, 200, {
          v: 1,
          total: items.length,
          positions: items,
          items,
          next: null,
        });
      }
      case "orders": {
        const plan = projections.plans(data, now);
        const items = [...(plan.actions ?? [])].reverse();
        return send(res, 200, {
          ...plan,
          total: items.length,
          orders: items,
          items,
          next: null,
        });
      }
      case "fills":
        return send(
          res,
          200,
          projections.fills(data, now, num("cursor"), num("limit")),
        );
      case "plans":
        return send(res, 200, projections.plans(data, now));
      case "scanner": {
        const segment = q.get("segment") ?? "prospects";
        if (!["prospects", "missing", "screens"].includes(segment))
          return fail(res, 400, "scanner_segment_invalid");
        return send(res, 200, projections.scanner(data, now, segment));
      }
      case "instruments":
        return send(
          res,
          200,
          instrumentsAt(data, now, {
            scope: q.get("scope") ?? undefined,
            market_segment: q.get("market_segment") ?? undefined,
            kind: q.get("kind") ?? undefined,
            q: q.get("q") ?? undefined,
            cursor: num("cursor"),
            limit: num("limit"),
          }),
        );
      case "instruments/chain":
        return send(
          res,
          200,
          chainAt(data, now, {
            underlying_key: q.get("underlying_key") ?? undefined,
            expiry_ms: num("expiry_ms"),
            market_segment: q.get("market_segment") ?? undefined,
            cursor: num("cursor"),
            limit: num("limit"),
          }),
        );
      case "sessions":
        return send(res, 200, sessionsAt(data, now));
      case "tools":
        return send(res, 200, toolsAt(data, now, num("limit")));
      case "health":
        return send(res, 200, healthAt(data, now, wall));
      case "alerts":
        return send(res, 200, alertsAt(data, now));
      case "agent":
        return send(
          res,
          200,
          agentAt(data, now, num("limit") ?? 50, num("cursor") ?? 0),
        );
      case "agent/transcript": {
        const lane = q.get("lane") ?? "";
        if (!/^[a-z][a-z0-9_]{0,31}$/.test(lane))
          return fail(res, 400, "transcript_lane_invalid");
        const limit = num("limit") ?? 20,
          cursor = num("cursor") ?? 0;
        if (!Number.isInteger(limit) || limit < 1 || limit > 50)
          return fail(res, 400, "transcript_limit_invalid");
        if (!Number.isInteger(cursor) || cursor < 0 || cursor > 100_000)
          return fail(res, 400, "transcript_cursor_invalid");
        return send(res, 200, transcriptAt(data, now, { lane, cursor, limit }));
      }
      case "series":
        return send(
          res,
          200,
          seriesAt(data, now, {
            kind: q.get("kind") ?? undefined,
            instrument: q.get("instrument") ?? undefined,
            from: num("from"),
            to: num("to"),
            limit: num("limit"),
          }),
        );
      case "backtest/example":
        return send(res, 200, data.example);
      case "events":
        return events(req, res, q, now);
      default:
        return fail(res, 404, "not_found");
    }
  }

  function events(
    req: IncomingMessage,
    res: ServerResponse,
    q: URLSearchParams,
    _now: number,
  ) {
    if (q.get("poll") === "1") {
      const after = Number(q.get("after") ?? 0) || 0;
      const reply = () => {
        const rows = ring.filter((r) => r.seq > after).map((r) => r.event);
        send(res, 200, {
          v: 1,
          events: rows,
          next: eventSeq,
          gap: false,
          epoch: EPOCH,
        });
      };
      if (ring.some((r) => r.seq > after)) return reply();
      const timer = setTimeout(reply, 1_000);
      req.on("close", () => clearTimeout(timer));
      return;
    }
    res.writeHead(200, {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-store",
      Connection: "keep-alive",
      "X-Accel-Buffering": "no",
    });
    res.write(": connected\n\n");
    const last = Number(req.headers["last-event-id"] ?? 0) || 0;
    for (const r of ring) if (r.seq > last) res.write(r.frame);
    const write = (frame: string) => {
      if (!res.writableEnded) res.write(frame);
    };
    subscribers.add(write);
    let batchSeq = 0;
    let sent = new Map<string, string>();
    const batch = setInterval(() => {
      const t = virtualNow();
      const rows = data.equities.map((e, i) => ({
        instrument_key: e.key,
        last_price: quoteAt(data, e.key, t).toFixed(2),
        receipt_ms: t - 120 - i * 17,
      }));
      // First frame is a full snapshot; later frames carry only moved rows
      // and idle intervals send nothing.
      const full = sent.size === 0;
      const items = rows.filter(
        (r) => full || sent.get(r.instrument_key) !== r.last_price,
      );
      if (!items.length) return;
      for (const r of items) sent.set(r.instrument_key, r.last_price);
      const body = {
        v: 1,
        batch_seq: ++batchSeq,
        as_of_ms: t,
        session: data.session,
        full,
        items,
        price_kind: "receipt_price",
        execution_authority: false,
        source: "receipt_derived_feature_store",
        note: "Display evidence only; receipt prices are not executable quotes.",
      };
      write(`event: price_batch\ndata: ${JSON.stringify(body)}\n\n`);
    }, BATCH_MS);
    const heartbeat = setInterval(() => write(": heartbeat\n\n"), HEARTBEAT_MS);
    req.on("close", () => {
      subscribers.delete(write);
      clearInterval(batch);
      clearInterval(heartbeat);
    });
  }

  return {
    name: "nivra-mock-api",
    apply: "serve",
    configureServer(server) {
      const summaryTimer = setInterval(
        () =>
          publish("summary_changed", {
            kind: "summary_changed",
            revisions: revisions(virtualNow(), Date.now()),
          }),
        SUMMARY_MS,
      );
      const runTimer = setInterval(startLiveRun, LIVE_RUN_PERIOD_MS);
      setTimeout(startLiveRun, 8_000);
      server.httpServer?.on("close", () => {
        clearInterval(summaryTimer);
        clearInterval(runTimer);
      });
      server.middlewares.use((req, res, next) => handle(req, res, next));
    },
  };
}
