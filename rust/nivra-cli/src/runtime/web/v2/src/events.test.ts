import type { Fetch, StreamEvent, Connection } from "./types";
import { test } from "node:test";
import assert from "node:assert/strict";
import { createEventStream, SseParser } from "./events";

async function waitFor(check: () => unknown, timeout = 2000) {
  const started = Date.now();
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() - started > timeout) throw new Error("timed out");
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

test("SSE parser handles split chunks, comments and ids", () => {
  const parser = new SseParser();
  assert.deepEqual(parser.push(": heartbeat\n\n"), []);
  assert.deepEqual(
    parser.push('id: 7\nevent: run_started\ndata: {"kind":"run_started"'),
    [],
  );
  assert.deepEqual(parser.push("}\n\n"), [
    { id: "7", event: "run_started", data: '{"kind":"run_started"}' },
  ]);
  assert.deepEqual(parser.push("data: first\ndata: second\n\n"), [
    { id: null, event: "message", data: "first\nsecond" },
  ]);
  assert.deepEqual(parser.push(": comment only\n\n"), []);
  assert.deepEqual(parser.push('id: 9\r\ndata: {"x":1}\r\n\r\n'), [
    { id: "9", event: "message", data: '{"x":1}' },
  ]);
});

test("event stream falls back to held long-poll after a 503", async () => {
  const calls: { url: string; options: RequestInit }[] = [];
  const states: Connection[] = [];
  const events: StreamEvent[] = [];
  let secondPoll = false;
  const fetch: Fetch = async (url, options = {}) => {
    calls.push({ url, options });
    if (calls.length === 1) return new Response(null, { status: 503 });
    if (!secondPoll) {
      secondPoll = true;
      return new Response(
        JSON.stringify({
          v: 1,
          events: [
            {
              seq: 4,
              kind: "run_started",
              run: "harness-digest-1",
              role: "digest",
            },
          ],
          next: 4,
          gap: false,
          epoch: "fixture",
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Promise<Response>(() => {});
  };
  const stream = createEventStream({
    fetchImpl: fetch,
    getToken: () => "fixture-token",
    onState: (state) => states.push(state),
    onEvent: (event) => events.push(event),
    sleep: async () => {},
  });
  stream.start();
  await waitFor(() => events.length === 1);
  assert.equal(calls[0].url, "/api/v1/events");
  assert.match(calls[1].url, /^\/api\/v1\/events\?poll=1&after=0$/);
  assert.equal(
    new Headers(calls[1].options.headers).get("Authorization"),
    "Bearer fixture-token",
  );
  assert.ok(states.includes("polling"));
  assert.equal(events[0].kind, "run_started");
  assert.equal(events[0].run, "harness-digest-1");
  stream.stop();
});

test("SSE reconnect sends Last-Event-ID and keeps the cursor", async () => {
  const calls: { url: string; options: RequestInit }[] = [];
  const events: StreamEvent[] = [];
  const encoder = new TextEncoder();
  const first = new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(
          encoder.encode(
            'id: 5\nevent: run_event\ndata: {"kind":"tool_call","seq":5}\n\n',
          ),
        );
        controller.close();
      },
    }),
    { headers: { "Content-Type": "text/event-stream" } },
  );
  const fetch: Fetch = async (url, options = {}) => {
    calls.push({ url, options });
    return calls.length === 1 ? first : new Promise<Response>(() => {});
  };
  const stream = createEventStream({
    fetchImpl: fetch,
    getToken: () => "fixture-token",
    onEvent: (event) => events.push(event),
    sleep: async () => {},
  });
  stream.start();
  await waitFor(() => calls.length >= 2);
  assert.equal(new Headers(calls[1].options.headers).get("Last-Event-ID"), "5");
  assert.equal(events[0].kind, "tool_call");
  assert.equal(events[0].seq, 5);
  stream.stop();
});

test("a 401 locks instead of retrying", async () => {
  let unauthorized = false;
  const stream = createEventStream({
    fetchImpl: async () => new Response(null, { status: 401 }),
    getToken: () => "expired",
    onUnauthorized: () => {
      unauthorized = true;
    },
    sleep: async () => {},
  });
  stream.start();
  await waitFor(() => unauthorized);
  assert.equal(unauthorized, true);
  stream.stop();
});
