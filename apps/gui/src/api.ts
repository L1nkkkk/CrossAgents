// Typed client for the hub's local API (docs/DESIGN.md §11). Inside Electron requests go
// through the preload bridge to the main process, which adds the bearer token; in a plain
// browser they go to the same origin (the Vite dev server forwards them, see vite.config.ts).

import type { Account } from "@bindings/Account";
import type { AccountUsageGroup } from "@bindings/AccountUsageGroup";
import type { AccountUsageResponse } from "@bindings/AccountUsageResponse";
import type { Agent } from "@bindings/Agent";
import type { ConnectInfo } from "@bindings/ConnectInfo";
import type { CreateAccountRequest } from "@bindings/CreateAccountRequest";
import type { HumanItem } from "@bindings/HumanItem";
import type { LandMode } from "@bindings/LandMode";
import type { ListNotesResponse } from "@bindings/ListNotesResponse";
import type { ListQuestionsResponse } from "@bindings/ListQuestionsResponse";
import type { LoginStarted } from "@bindings/LoginStarted";
import type { QuotaResponse } from "@bindings/QuotaResponse";
import type { AgentProfile } from "@bindings/AgentProfile";
import type { ApiError as ApiErrorJson } from "@bindings/ApiError";
import type { CreateAgentRequest } from "@bindings/CreateAgentRequest";
import type { CreateRunRequest } from "@bindings/CreateRunRequest";
import type { HubInfo } from "@bindings/HubInfo";
import type { ListAgentsResponse } from "@bindings/ListAgentsResponse";
import type { ListPluginsResponse } from "@bindings/ListPluginsResponse";
import type { PluginInfo } from "@bindings/PluginInfo";
import type { ListRunsResponse } from "@bindings/ListRunsResponse";
import type { RejectRunRequest } from "@bindings/RejectRunRequest";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { SendMessageRequest } from "@bindings/SendMessageRequest";
import type { SendMessageResponse } from "@bindings/SendMessageResponse";
import type { Transcript } from "@bindings/Transcript";
import type { TurnDiffResponse } from "@bindings/TurnDiffResponse";
import type { UsageGroup } from "@bindings/UsageGroup";
import type { UsageResponse } from "@bindings/UsageResponse";

/**
 * The Electron preload bridge (electron/preload.cts), or the mock one in `preview:mock`;
 * absent in a plain browser. Read on use: the mock installs itself before the app starts.
 */
export const getBridge = () => (typeof window !== "undefined" ? window.crossagents : undefined);

export class ApiError extends Error {
  /** HTTP status; 0 when the hub could not be reached. */
  status: number;
  code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

type Method = "GET" | "POST" | "DELETE";

async function send(method: Method, path: string, body?: unknown): Promise<{ status: number; body: string }> {
  const json = body === undefined ? null : JSON.stringify(body);
  const bridge = getBridge();
  if (bridge) {
    try {
      return await bridge.request(method, path, json);
    } catch (err) {
      throw new ApiError(0, "unreachable", err instanceof Error ? err.message : String(err));
    }
  }
  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers: json === null ? {} : { "content-type": "application/json" },
      body: json,
    });
  } catch (err) {
    throw new ApiError(0, "unreachable", String(err));
  }
  return { status: response.status, body: await response.text() };
}

async function request<T>(method: Method, path: string, body?: unknown): Promise<T> {
  const response = await send(method, path, body);
  if (response.status >= 400) {
    let parsed: ApiErrorJson | undefined;
    try {
      parsed = JSON.parse(response.body);
    } catch {
      // Not an API error body.
    }
    throw new ApiError(
      response.status,
      parsed?.error.code ?? "http",
      parsed?.error.message ?? (response.body || `HTTP ${response.status}`),
    );
  }
  return (response.body ? JSON.parse(response.body) : undefined) as T;
}

const enc = encodeURIComponent;

export const api = {
  info: () => request<HubInfo>("GET", "/v1/info"),

  listAgents: () => request<ListAgentsResponse>("GET", "/v1/agents"),
  createAgent: (req: CreateAgentRequest) => request<Agent>("POST", "/v1/agents", req),
  sendMessage: (key: string, req: SendMessageRequest) =>
    request<SendMessageResponse>("POST", `/v1/agents/${enc(key)}/messages`, req),
  withdrawMessage: (key: string, messageId: string) =>
    request<void>("DELETE", `/v1/agents/${enc(key)}/messages/${enc(messageId)}`),
  cancelAgent: (key: string) => request<void>("POST", `/v1/agents/${enc(key)}/cancel`),
  compactAgent: (key: string) => request<void>("POST", `/v1/agents/${enc(key)}/compact`),
  stopAgent: (key: string) => request<void>("POST", `/v1/agents/${enc(key)}/stop`),
  setModel: (key: string, model: string | null, effort?: string) => request<Agent>("POST", `/v1/agents/${enc(key)}/model`, { model, effort }),
  renameAgent: (key: string, name: string) => request<Agent>("POST", `/v1/agents/${enc(key)}/name`, { name }),
  setTopic: (key: string, topic: string | null) => request<Agent>("POST", `/v1/agents/${enc(key)}/topic`, { topic }),
  archiveAgent: (key: string, archived: boolean) => request<Agent>("POST", `/v1/agents/${enc(key)}/archive`, { archived }),
  deleteAgent: (key: string) => request<void>("DELETE", `/v1/agents/${enc(key)}`),
  connectAgent: (key: string) => request<ConnectInfo>("POST", `/v1/agents/${enc(key)}/connect`),
  notes: (scope: "run" | "team", run?: string) =>
    request<ListNotesResponse>("GET", `/v1/notes?scope=${scope}${run ? `&run=${enc(run)}` : ""}`),
  updateProfile: (key: string, profile: AgentProfile) => request<Agent>("POST", `/v1/agents/${enc(key)}/profile`, profile),
  transcript: (key: string) => request<Transcript>("GET", `/v1/agents/${enc(key)}/transcript`),
  turnDiff: (key: string, messageId: string) =>
    request<TurnDiffResponse>("GET", `/v1/agents/${enc(key)}/turns/${enc(messageId)}/diff`),
  listPlugins: () => request<ListPluginsResponse>("GET", "/v1/plugins"),
  setPluginModels: (name: string, shown: string[] | null) => request<PluginInfo>("POST", `/v1/plugins/${enc(name)}/models`, { shown }),

  listRuns: () => request<ListRunsResponse>("GET", "/v1/runs"),
  getRun: (key: string) => request<RunDetail>("GET", `/v1/runs/${enc(key)}`),
  createRun: (req: CreateRunRequest) => request<Run>("POST", "/v1/runs", req),
  approveRun: (key: string) => request<RunDetail>("POST", `/v1/runs/${enc(key)}/approve`),
  rejectRun: (key: string, req: RejectRunRequest) =>
    request<RunDetail>("POST", `/v1/runs/${enc(key)}/reject`, req),
  cancelRun: (key: string) => request<RunDetail>("POST", `/v1/runs/${enc(key)}/cancel`),
  landRun: (key: string, mode: LandMode) => request<RunDetail>("POST", `/v1/runs/${enc(key)}/land`, { mode }),
  finishRun: (key: string) => request<RunDetail>("POST", `/v1/runs/${enc(key)}/finish`, {}),

  usage: (by: UsageGroup) => request<UsageResponse>("GET", `/v1/usage?by=${by}`),

  questions: (all = false) => request<ListQuestionsResponse>("GET", `/v1/questions${all ? "?all=true" : ""}`),
  answer: (id: string, answer: string) => request<HumanItem>("POST", `/v1/questions/${enc(id)}/answer`, { answer }),
  dismiss: (id: string) => request<void>("POST", `/v1/questions/${enc(id)}/dismiss`),

  quota: () => request<QuotaResponse>("GET", "/v1/quota"),
  createAccount: (req: CreateAccountRequest) => request<Account>("POST", "/v1/accounts", req),
  deleteAccount: (id: string) => request<void>("DELETE", `/v1/accounts/${enc(id)}`),
  accountUsage: (id: string, by: AccountUsageGroup) =>
    request<AccountUsageResponse>("GET", `/v1/accounts/${enc(id)}/usage?by=${by}`),
  login: (id: string) => request<LoginStarted>("POST", `/v1/accounts/${enc(id)}/login`),
};

export function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
