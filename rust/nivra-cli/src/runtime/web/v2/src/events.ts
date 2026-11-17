import type { Fetch, StreamEvent, PollResponse, Connection } from "./types";
interface Frame {
  id: string | null;
  event: string;
  data: string;
}
interface EventStreamOptions {
  fetchImpl: Fetch;
  getToken: () => string;
  onEvent?: (event: StreamEvent) => void;
  onState?: (state: Connection) => void;
  onUnauthorized?: () => void;
  sleep?: (ms: number) => Promise<void>;
  sseUrl?: string;
  pollUrl?: string;
}
// SSE-over-fetch with a held long-poll fallback. Pure parser, injectable I/O.
export class SseParser {
  buffer: string;
  constructor() {
    this.buffer = "";
  }

  push(chunk: string) {
    this.buffer += String(chunk || "");
    const frames = [];
    for (;;) {
      const match = /\r?\n\r?\n/.exec(this.buffer);
      if (!match) break;
      const block = this.buffer.slice(0, match.index);
      this.buffer = this.buffer.slice(match.index + match[0].length);
      const frame = parseBlock(block);
      if (frame) frames.push(frame);
    }
    return frames;
  }
}

function parseBlock(block: string): Frame | null {
  let id = null;
  let event = "message";
  const data = [];
  for (const line of block.split(/\r?\n/)) {
    if (!line || line.startsWith(":")) continue;
    const separator = line.indexOf(":");
    const field = separator < 0 ? line : line.slice(0, separator);
    let value = separator < 0 ? "" : line.slice(separator + 1);
    if (value.startsWith(" ")) value = value.slice(1);
    if (field === "id") id = value;
    else if (field === "event") event = value || "message";
    else if (field === "data") data.push(value);
  }
  if (data.length === 0 && event === "message") return null;
  return { id, event, data: data.join("\n") };
}

export function createEventStream({
  fetchImpl,
  getToken,
  onEvent,
  onState,
  onUnauthorized,
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
  sseUrl = "/api/v1/events",
  pollUrl = "/api/v1/events",
}: EventStreamOptions) {
  let stopped = false;
  let cursor = 0;
  let epoch: string | number | null = null;
  let mode = "sse";
  let backoff = 1000;
  let controller: AbortController | null = null;

  const state = (value: Connection) => onState?.(value);
  const done = (error: unknown) => {
    if (stopped || (error instanceof Error && error.name === "AbortError"))
      return true;
    return false;
  };

  function dispatch(frame: Frame) {
    const numeric = Number(frame.id);
    if (Number.isSafeInteger(numeric) && numeric >= cursor) cursor = numeric;
    let data: StreamEvent;
    try {
      data = JSON.parse(frame.data);
    } catch {
      return;
    }
    if (!data || typeof data !== "object") return;
    if (data.epoch !== undefined) {
      if (epoch !== null && data.epoch !== epoch) cursor = 0;
      epoch = data.epoch;
    }
    if (data.gap) {
      const next = Number(data.next);
      if (Number.isSafeInteger(next)) cursor = next;
      onEvent?.({ ...data, kind: "gap" });
      return;
    }
    const seq = Number.isSafeInteger(numeric) ? numeric : Number(data.seq);
    onEvent?.({
      ...data,
      kind: data.kind || frame.event,
      event: frame.event,
      ...(Number.isSafeInteger(seq) ? { seq } : {}),
    });
  }

  async function streamOnce() {
    controller = new AbortController();
    const headers: Record<string, string> = { Accept: "text/event-stream" };
    const token = getToken();
    if (token) headers.Authorization = `Bearer ${token}`;
    if (cursor > 0) headers["Last-Event-ID"] = String(cursor);
    const response = await fetchImpl(sseUrl, {
      method: "GET",
      headers,
      cache: "no-store",
      signal: controller.signal,
    });
    if (stopped || token !== getToken()) return true;
    if (response.status === 503) {
      mode = "poll";
      state("polling");
      return false;
    }
    if (stopped || token !== getToken()) return true;
    if (response.status === 401) {
      onUnauthorized?.();
      return true;
    }
    if (!response.ok || !response.body?.getReader)
      throw new Error("stream_unavailable");
    state("live");
    backoff = 1000;
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    const parser = new SseParser();
    for (;;) {
      const { value, done: finished } = await reader.read();
      if (stopped || token !== getToken()) return true;
      if (finished) return false;
      for (const frame of parser.push(
        decoder.decode(value, { stream: true }),
      )) {
        dispatch(frame);
      }
    }
  }

  async function pollOnce() {
    controller = new AbortController();
    const headers: Record<string, string> = {};
    const token = getToken();
    if (token) headers.Authorization = `Bearer ${token}`;
    const response = await fetchImpl(`${pollUrl}?poll=1&after=${cursor}`, {
      method: "GET",
      headers,
      cache: "no-store",
      signal: controller.signal,
    });
    if (stopped || token !== getToken()) return true;
    if (response.status === 401) {
      onUnauthorized?.();
      return true;
    }
    if (!response.ok) throw new Error("poll_unavailable");
    const data: PollResponse = await response.json();
    if (stopped || token !== getToken()) return true;
    state("polling");
    backoff = 1000;
    if (data.epoch !== undefined) {
      if (epoch !== null && data.epoch !== epoch) cursor = 0;
      epoch = data.epoch;
    }
    if (data.gap) {
      const next = Number(data.next);
      if (Number.isSafeInteger(next)) cursor = next;
      onEvent?.({ ...data, kind: "gap" });
    }
    const events = Array.isArray(data.events) ? data.events : [];
    for (const event of events) {
      const seq = Number(event?.seq);
      if (Number.isSafeInteger(seq) && seq > cursor) cursor = seq;
      onEvent?.({ ...event, event: event?.kind });
    }
    if (events.length === 0) await sleep(50);
    return false;
  }

  async function run() {
    while (!stopped) {
      if (!getToken()) {
        state("locked");
        return;
      }
      try {
        const stop = mode === "poll" ? await pollOnce() : await streamOnce();
        if (stop) return;
        if (mode === "sse") {
          // A held SSE stream closes normally at the server hold boundary.
          // Reconnect without marking the feed stale; only transport errors are
          // stale, so a normal reconnect never flickers the dashboard.
          await sleep(backoff);
          backoff = Math.min(backoff * 2, 15000);
        }
      } catch (error) {
        if (done(error)) return;
        if (mode === "sse") mode = "poll";
        state("stale");
        await sleep(backoff);
        backoff = Math.min(backoff * 2, 15000);
      }
    }
  }

  return {
    start() {
      if (!stopped) void run();
    },
    stop() {
      stopped = true;
      controller?.abort();
      state("locked");
    },
    get cursor() {
      return cursor;
    },
    get mode() {
      return mode;
    },
  };
}
