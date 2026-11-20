import type {
  Fetch,
  Query,
  MetaResponse,
  SummaryResponse,
  PortfolioResponse,
  FillsResponse,
  PlansResponse,
  ScannerResponse,
  InstrumentCatalogResponse,
  ContractChainResponse,
  SessionsResponse,
  HealthResponse,
  ApprovalsResponse,
  AlertsResponse,
  SessionResponse,
  SeriesResponse,
  ReplayExample,
  ReplayInput,
  ReplayResponse,
  RunsResponse,
  RunResponse,
  ToolsResponse,
  AgentResponse,
} from "./types";
// Authenticated JSON/text reads with ETag revalidation. No mutations exist here.
export class ApiError extends Error {
  code: string;
  status: number;
  constructor(code: string, status: number) {
    super(code);
    this.name = "ApiError";
    this.code = code;
    this.status = status;
  }
}

function query(parameters?: Query) {
  const parts = [];
  for (const [key, value] of Object.entries(parameters || {})) {
    if (value === null || value === undefined || value === "") continue;
    parts.push(
      `${encodeURIComponent(key)}=${encodeURIComponent(String(value))}`,
    );
  }
  return parts.length ? `?${parts.join("&")}` : "";
}

export interface ApiOptions {
  fetchImpl: Fetch;
  getToken: () => string;
  onUnauthorized?: () => void;
}
export function createApi({ fetchImpl, getToken, onUnauthorized }: ApiOptions) {
  const cache = new Map<string, { etag: string | null; body: unknown }>();
  let generation = 0;

  async function get<T>(path: string, { json = true } = {}): Promise<T> {
    const requestGeneration = generation;
    const headers: Record<string, string> = {};
    const token = getToken();
    if (token) headers.Authorization = `Bearer ${token}`;
    const cached = cache.get(path);
    if (cached?.etag) headers["If-None-Match"] = cached.etag;
    const response = await fetchImpl(path, {
      method: "GET",
      headers,
      cache: "no-store",
    });
    if (token !== getToken() || requestGeneration !== generation)
      throw new ApiError("session_changed", 0);
    if (response.status === 401) {
      onUnauthorized?.();
      throw new ApiError("authentication_required", 401);
    }
    let body = cached?.body;
    if (response.status !== 304) {
      body = json ? await response.json() : await response.text();
    }
    if (!response.ok && response.status !== 304) {
      throw new ApiError(
        responseError(body, "request_failed"),
        response.status,
      );
    }
    if (token !== getToken() || requestGeneration !== generation)
      throw new ApiError("session_changed", 0);
    const etag = response.headers?.get?.("etag") || cached?.etag || null;
    if (response.status !== 304 || !cached) cache.set(path, { etag, body });
    return body as T;
  }

  return {
    meta: () => get<MetaResponse>("/api/v1/meta"),
    summary: () => get<SummaryResponse>("/api/v1/summary"),
    portfolio: () => get<PortfolioResponse>("/api/v1/portfolio"),
    fills: (parameters?: Query) =>
      get<FillsResponse>("/api/v1/fills" + query(parameters)),
    plans: (parameters?: Query) =>
      get<PlansResponse>("/api/v1/plans" + query(parameters)),
    scanner: (parameters?: Query) =>
      get<ScannerResponse>("/api/v1/scanner" + query(parameters)),
    instruments: (parameters?: Query) =>
      get<InstrumentCatalogResponse>("/api/v1/instruments" + query(parameters)),
    instrumentChain: (parameters: Query) =>
      get<ContractChainResponse>(
        "/api/v1/instruments/chain" + query(parameters),
      ),
    sessions: (parameters?: Query) =>
      get<SessionsResponse>("/api/v1/sessions" + query(parameters)),
    health: () => get<HealthResponse>("/api/v1/health"),
    approvals: () => get<ApprovalsResponse>("/api/v1/approvals"),
    alerts: (parameters?: Query) =>
      get<AlertsResponse>("/api/v1/alerts" + query(parameters)),
    session: (date?: string) =>
      get<SessionResponse>("/api/v1/session" + query({ date })),
    tools: (parameters?: Query) =>
      get<ToolsResponse>("/api/v1/tools" + query(parameters)),
    agent: (parameters?: Query) =>
      get<AgentResponse>("/api/v1/agent" + query(parameters)),
    series: (parameters?: Query) =>
      get<SeriesResponse>("/api/v1/series" + query(parameters)),
    example: () => get<ReplayExample>("/api/v1/backtest/example"),
    replay: async (input: ReplayInput): Promise<ReplayResponse> => {
      const requestGeneration = generation;
      const token = getToken();
      const response = await fetchImpl("/api/v1/backtest", {
        method: "POST",
        cache: "no-store",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify(input),
      });
      if (token !== getToken() || requestGeneration !== generation)
        throw new ApiError("session_changed", 0);
      if (response.status === 401) onUnauthorized?.();
      const body: unknown = await response.json();
      if (token !== getToken() || requestGeneration !== generation)
        throw new ApiError("session_changed", 0);
      if (!response.ok)
        throw new ApiError(
          responseError(body, "replay_unavailable"),
          response.status,
        );
      return body as ReplayResponse;
    },
    runs: (parameters?: Query) =>
      get<RunsResponse>("/api/v1/runs" + query(parameters)),
    run: (id: string) =>
      get<RunResponse>(`/api/v1/runs/${encodeURIComponent(id)}`),
    blob: (id: string, name: string, parameters?: Query) =>
      get<string>(
        `/api/v1/runs/${encodeURIComponent(id)}/blobs/${encodeURIComponent(name)}` +
          query(parameters),
        { json: false },
      ),
    prompt: (sha: string) =>
      get<string>(`/api/v1/prompts/${encodeURIComponent(sha)}`, {
        json: false,
      }),
    clear: () => {
      generation++;
      cache.clear();
    },
  };
}

function responseError(body: unknown, fallback: string): string {
  return body !== null &&
    typeof body === "object" &&
    "error" in body &&
    typeof body.error === "string"
    ? body.error
    : fallback;
}
export type Api = ReturnType<typeof createApi>;
