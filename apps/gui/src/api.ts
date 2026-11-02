// Typed client for the hub's local API (docs/DESIGN.md §11). Inside Electron requests go
// through the preload bridge to the main process, which adds the bearer token; in a plain
// browser they go to the same origin (the Vite dev server forwards them, see vite.config.ts).

import type { Account } from "@bindings/Account";
import type { AccountUsageGroup } from "@bindings/AccountUsageGroup";
import type { AccountUsageResponse } from "@bindings/AccountUsageResponse";
import type { ChangesResponse } from "@bindings/ChangesResponse";
import type { CommitGraph } from "@bindings/CommitGraph";
import type { CliInstallAccepted } from "@bindings/CliInstallAccepted";
import type { AttachmentContent } from "@bindings/AttachmentContent";
import type { ListAvatarsResponse } from "@bindings/ListAvatarsResponse";
import type { UploadAvatarsRequest } from "@bindings/UploadAvatarsRequest";
import type { UploadAvatarsResponse } from "@bindings/UploadAvatarsResponse";
import type { ImageContent } from "@bindings/ImageContent";
import type { Task } from "@bindings/Task";
import type { TaskReviewRequest } from "@bindings/TaskReviewRequest";
import type { Agent } from "@bindings/Agent";
import type { AgentSettingsRequest } from "@bindings/AgentSettingsRequest";
import type { ConnectInfo } from "@bindings/ConnectInfo";
import type { CreateAccountRequest } from "@bindings/CreateAccountRequest";
import type { HumanItem } from "@bindings/HumanItem";
import type { InitRepoRequest } from "@bindings/InitRepoRequest";
import type { LandMode } from "@bindings/LandMode";
import type { ListNotesResponse } from "@bindings/ListNotesResponse";
import type { ListQuestionsResponse } from "@bindings/ListQuestionsResponse";
import type { LoginStarted } from "@bindings/LoginStarted";
import type { QuotaResponse } from "@bindings/QuotaResponse";
import type { AgentProfile } from "@bindings/AgentProfile";
import type { ApiError as ApiErrorJson } from "@bindings/ApiError";
import type { CreateAgentRequest } from "@bindings/CreateAgentRequest";
import type { CreateRunRequest } from "@bindings/CreateRunRequest";
import type { DeleteRunResponse } from "@bindings/DeleteRunResponse";
import type { AgentExecutionResponse } from "@bindings/AgentExecutionResponse";
import type { AgentCommandsResponse } from "@bindings/AgentCommandsResponse";
import type { ResumeCommandResponse } from "@bindings/ResumeCommandResponse";
import type { HubInfo } from "@bindings/HubInfo";
import type { ListAgentsResponse } from "@bindings/ListAgentsResponse";
import type { ListPluginsResponse } from "@bindings/ListPluginsResponse";
import type { PluginInfo } from "@bindings/PluginInfo";
import type { ListRunsResponse } from "@bindings/ListRunsResponse";
import type { ListSessionCandidatesResponse } from "@bindings/ListSessionCandidatesResponse";
import type { StartChatRequest } from "@bindings/StartChatRequest";
import type { ApproveRunRequest } from "@bindings/ApproveRunRequest";
import type { RejectRunRequest } from "@bindings/RejectRunRequest";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { RunTrace } from "@bindings/RunTrace";
import type { SendMessageRequest } from "@bindings/SendMessageRequest";
import type { SendMessageResponse } from "@bindings/SendMessageResponse";
import type { Transcript } from "@bindings/Transcript";
import type { TurnDiffResponse } from "@bindings/TurnDiffResponse";
import type { UpdateRunPolicyRequest } from "@bindings/UpdateRunPolicyRequest";
import type { UsageGroup } from "@bindings/UsageGroup";
import type { AcceptCandidateResponse } from "@bindings/AcceptCandidateResponse";
import type { ListCandidatesResponse } from "@bindings/ListCandidatesResponse";
import type { ListExpertiseResponse } from "@bindings/ListExpertiseResponse";
import type { AcceptRequest, Candidate, CandidateStatus, ConflictDecision, ExpertiseCard, ExpertiseEntry, Maintenance, OwnerKind } from "./lib/expertise";
import type { UsageResponse } from "@bindings/UsageResponse";
import type { UsageSeriesGroup } from "@bindings/UsageSeriesGroup";
import type { UsageSeriesRange } from "@bindings/UsageSeriesRange";
import type { UsageSeriesResponse } from "@bindings/UsageSeriesResponse";
import type { FileTreeResponse } from "@bindings/FileTreeResponse";
import type { FileContentResponse } from "@bindings/FileContentResponse";
import type { ResolvePathsRequest } from "@bindings/ResolvePathsRequest";
import type { ResolvePathsResponse } from "@bindings/ResolvePathsResponse";

/**
 * Whose files a path is relative to: a session's working directory, a run's integration
 * checkout, or a task's worktree.
 */
export type FileScope =
  | { kind: "agent"; agent: string }
  | { kind: "run"; run: string }
  | { kind: "task"; run: string; task: string };

/** The API prefix of a scope, e.g. `/v1/runs/0193-abc/tasks/auth`. */
export function scopePath(scope: FileScope): string {
  switch (scope.kind) {
    case "agent":
      return `/v1/agents/${encodeURIComponent(scope.agent)}`;
    case "run":
      return `/v1/runs/${encodeURIComponent(scope.run)}`;
    case "task":
      return `/v1/runs/${encodeURIComponent(scope.run)}/tasks/${encodeURIComponent(scope.task)}`;
  }
}

/** A stable key for a scope, for caches. */
export function scopeKey(scope: FileScope): string {
  return scopePath(scope);
}

/**
 * The Electron preload bridge (electron/preload.cts), or the mock one in `preview:mock`;
 * absent in a plain browser. Read on use: the mock installs itself before the app starts.
 */
export const getBridge = () => (typeof window !== "undefined" ? window.crossagents : undefined);

export class ApiError extends Error {
  /** HTTP status; 0 when the hub could not be reached. */
  status: number;
  code: string;
  /** The files in the way, when the conflict is about a directory's changes (deleting a run
   * whose checkout has uncommitted work). */
  files: string[];
  constructor(status: number, code: string, message: string, files: string[] = []) {
    super(message);
    this.status = status;
    this.code = code;
    this.files = files;
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

/** Why requests are refused (the hub speaks another API version, see hub.ts); `/v1/info` still goes, to check again. */
let refusal: string | undefined;

export function refuseRequests(reason: string | undefined) {
  refusal = reason;
}

async function request<T>(method: Method, path: string, body?: unknown, opts: { text?: boolean } = {}): Promise<T> {
  if (refusal && path !== "/v1/info") throw new ApiError(0, "version_mismatch", refusal);
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
      parsed?.error.files ?? [],
    );
  }
  if (opts.text) return response.body as T;
  return (response.body ? JSON.parse(response.body) : undefined) as T;
}

const enc = encodeURIComponent;

function expertiseQuery(owner_kind: OwnerKind, owner_id: string): string {
  return `owner_kind=${owner_kind}&owner_id=${enc(owner_id)}`;
}

function treeQuery(path: string, all: boolean, ref?: string): string {
  return `path=${enc(path)}${all ? "&all=true" : ""}${ref ? `&ref=${enc(ref)}` : ""}`;
}

function contentQuery(path: string, ref?: string): string {
  return `path=${enc(path)}${ref ? `&ref=${enc(ref)}` : ""}`;
}

// One literal path per scope kind: allowed.test.cts reads every call's path from this file.
function filesTree(scope: FileScope, path = "", all = false, ref?: string): Promise<FileTreeResponse> {
  const q = treeQuery(path, all, ref);
  switch (scope.kind) {
    case "agent":
      return request<FileTreeResponse>("GET", `/v1/agents/${enc(scope.agent)}/files/tree?${q}`);
    case "run":
      return request<FileTreeResponse>("GET", `/v1/runs/${enc(scope.run)}/files/tree?${q}`);
    case "task":
      return request<FileTreeResponse>("GET", `/v1/runs/${enc(scope.run)}/tasks/${enc(scope.task)}/files/tree?${q}`);
  }
}

function filesContent(scope: FileScope, path: string, ref?: string): Promise<FileContentResponse> {
  const q = contentQuery(path, ref);
  switch (scope.kind) {
    case "agent":
      return request<FileContentResponse>("GET", `/v1/agents/${enc(scope.agent)}/files/content?${q}`);
    case "run":
      return request<FileContentResponse>("GET", `/v1/runs/${enc(scope.run)}/files/content?${q}`);
    case "task":
      return request<FileContentResponse>("GET", `/v1/runs/${enc(scope.run)}/tasks/${enc(scope.task)}/files/content?${q}`);
  }
}

/** Which of `paths` exist inside the scope's root; in input order, `path` empty when not. At most 200. */
function filesResolve(scope: FileScope, paths: string[]): Promise<ResolvePathsResponse> {
  const body = { paths } satisfies ResolvePathsRequest;
  switch (scope.kind) {
    case "agent":
      return request<ResolvePathsResponse>("POST", `/v1/agents/${enc(scope.agent)}/files/resolve`, body);
    case "run":
      return request<ResolvePathsResponse>("POST", `/v1/runs/${enc(scope.run)}/files/resolve`, body);
    case "task":
      return request<ResolvePathsResponse>("POST", `/v1/runs/${enc(scope.run)}/tasks/${enc(scope.task)}/files/resolve`, body);
  }
}

export const api = {
  info: () => request<HubInfo>("GET", "/v1/info"),

  listAgents: () => request<ListAgentsResponse>("GET", "/v1/agents"),
  listAvatars: () => request<ListAvatarsResponse>("GET", "/v1/avatars"),
  avatar: (id: string) => request<AttachmentContent>("GET", `/v1/avatars/${enc(id)}`),
  uploadAvatars: (req: UploadAvatarsRequest) => request<UploadAvatarsResponse>("POST", "/v1/avatars", req),
  deleteAvatar: (id: string) => request<void>("DELETE", `/v1/avatars/${enc(id)}`),
  createAgent: (req: CreateAgentRequest) => request<Agent>("POST", "/v1/agents", req),
  /** The workspace's sessions, most recently used first; availability is advisory (the hub
   * excludes what it knows is busy, archived or unable to move). */
  /** A workspace's sessions; without `repo`, the standalone chats. */
  sessionCandidates: (repo?: string) =>
    request<ListSessionCandidatesResponse>("GET", `/v1/session-candidates${repo ? `?repo=${enc(repo)}` : ""}`),
  /** Continues an existing conversation; `restore` brings an archived session back first. */
  chat: (key: string, req: StartChatRequest) => request<SendMessageResponse>("POST", `/v1/agents/${enc(key)}/chat`, req),
  sendMessage: (key: string, req: SendMessageRequest) =>
    request<SendMessageResponse>("POST", `/v1/agents/${enc(key)}/messages`, req),
  withdrawMessage: (key: string, messageId: string) =>
    request<void>("DELETE", `/v1/agents/${enc(key)}/messages/${enc(messageId)}`),
  cancelAgent: (key: string) => request<void>("POST", `/v1/agents/${enc(key)}/cancel`),
  compactAgent: (key: string) => request<void>("POST", `/v1/agents/${enc(key)}/compact`),
  /** The slash commands and skills the agent's own CLI brings; `known` is false until it
   * reports them (a session has started). */
  agentCommands: (key: string) => request<AgentCommandsResponse>("GET", `/v1/agents/${enc(key)}/commands`),
  stopAgent: (key: string) => request<void>("POST", `/v1/agents/${enc(key)}/stop`),
  setModel: (key: string, model: string | null, effort?: string) => request<Agent>("POST", `/v1/agents/${enc(key)}/model`, { model, effort }),
  renameAgent: (key: string, name: string) => request<Agent>("POST", `/v1/agents/${enc(key)}/name`, { name }),
  setTopic: (key: string, topic: string | null) => request<Agent>("POST", `/v1/agents/${enc(key)}/topic`, { topic }),
  archiveAgent: (key: string, archived: boolean) => request<Agent>("POST", `/v1/agents/${enc(key)}/archive`, { archived }),
  /** What the session works on for a run now (docs/tasks/S1.md §3.3). */
  agentExecution: (key: string) => request<AgentExecutionResponse>("GET", `/v1/agents/${enc(key)}/execution`),
  interruptExecution: (key: string) => request<Task>("POST", `/v1/agents/${enc(key)}/execution/interrupt`, {}),
  /** The command that carries the session on in its own CLI (docs/TODO.md §十五); read-only. */
  resumeCommand: (key: string) => request<ResumeCommandResponse>("GET", `/v1/agents/${enc(key)}/resume-command`),
  deleteAgent: (key: string) => request<void>("DELETE", `/v1/agents/${enc(key)}`),
  connectAgent: (key: string) => request<ConnectInfo>("POST", `/v1/agents/${enc(key)}/connect`),
  notes: (scope: "run" | "team", run?: string) =>
    request<ListNotesResponse>("GET", `/v1/notes?scope=${scope}${run ? `&run=${enc(run)}` : ""}`),
  updateProfile: (key: string, profile: AgentProfile) => request<Agent>("POST", `/v1/agents/${enc(key)}/profile`, profile),
  /** Which Claude settings the agent loads, from its next session. */
  agentSettings: (key: string, req: AgentSettingsRequest) => request<Agent>("POST", `/v1/agents/${enc(key)}/settings`, req),
  transcript: (key: string) => request<Transcript>("GET", `/v1/agents/${enc(key)}/transcript`),
  /** One file sent to the agent, base64. */
  attachment: (key: string, id: string) => request<AttachmentContent>("GET", `/v1/agents/${enc(key)}/attachments/${enc(id)}`),
  turnDiff: (key: string, messageId: string) =>
    request<TurnDiffResponse>("GET", `/v1/agents/${enc(key)}/turns/${enc(messageId)}/diff`),
  listPlugins: () => request<ListPluginsResponse>("GET", "/v1/plugins"),
  setPluginModels: (name: string, shown: string[] | null) => request<PluginInfo>("POST", `/v1/plugins/${enc(name)}/models`, { shown }),
  /** Runs the description's install command on this machine; progress comes on the event stream. */
  installPlugin: (name: string) => request<CliInstallAccepted>("POST", `/v1/plugins/${enc(name)}/install`),
  /** The same for its upgrade command. */
  upgradePlugin: (name: string) => request<CliInstallAccepted>("POST", `/v1/plugins/${enc(name)}/upgrade`),

  listRuns: () => request<ListRunsResponse>("GET", "/v1/runs"),
  getRun: (key: string) => request<RunDetail>("GET", `/v1/runs/${enc(key)}`),
  createRun: (req: CreateRunRequest) => request<Run>("POST", "/v1/runs", req),
  approveRun: (key: string, req: ApproveRunRequest = { policy: null, max_parallel: null, assignees: null }) =>
    request<RunDetail>("POST", `/v1/runs/${enc(key)}/approve`, req),
  rejectRun: (key: string, req: RejectRunRequest) =>
    request<RunDetail>("POST", `/v1/runs/${enc(key)}/reject`, req),
  cancelRun: (key: string) => request<RunDetail>("POST", `/v1/runs/${enc(key)}/cancel`),
  /** Deletes a run that ended; `branches` also deletes its branches, `force` goes past a
   * checkout with uncommitted work. Answers with what it cleaned up. */
  deleteRun: (key: string, opts: { branches?: boolean; force?: boolean } = {}) =>
    request<DeleteRunResponse>("DELETE", `/v1/runs/${enc(key)}?branches=${!!opts.branches}&force=${!!opts.force}`),
  landRun: (key: string, mode: LandMode) => request<RunDetail>("POST", `/v1/runs/${enc(key)}/land`, { mode }),
  /** How the run continues by itself from now on, and whether its allowance starts over. */
  updateRunPolicy: (key: string, req: UpdateRunPolicyRequest) =>
    request<RunDetail>("POST", `/v1/runs/${enc(key)}/policy`, req),
  finishRun: (key: string) => request<RunDetail>("POST", `/v1/runs/${enc(key)}/finish`, {}),
  /** Everything a task changed (its worktree against its base), for review. */
  taskDiff: (run: string, task: string) => request<ChangesResponse>("GET", `/v1/runs/${enc(run)}/tasks/${enc(task)}/diff`),
  /** What landing the run would bring into its base branch. */
  runDiff: (run: string) => request<ChangesResponse>("GET", `/v1/runs/${enc(run)}/diff`),
  /** The commit graph of the run's branches (base, run branch, tasks), newest first in topo order. */
  runGraph: (run: string, limit?: number) =>
    request<CommitGraph>("GET", `/v1/runs/${enc(run)}/graph${limit ? `?limit=${limit}` : ""}`),
  /** The commit graph of the session's checkout (HEAD and local branches). */
  agentGraph: (agent: string, limit?: number) =>
    request<CommitGraph>("GET", `/v1/agents/${enc(agent)}/graph${limit ? `?limit=${limit}` : ""}`),
  /** What one commit of the run's graph changed, against its first parent. */
  runCommitDiff: (run: string, sha: string) =>
    request<ChangesResponse>("GET", `/v1/runs/${enc(run)}/commits/${enc(sha)}/diff`),
  /** The same for a commit of the session's checkout. */
  agentCommitDiff: (agent: string, sha: string) =>
    request<ChangesResponse>("GET", `/v1/agents/${enc(agent)}/commits/${enc(sha)}/diff`),
  /** What happened in a run, by task, from its events. */
  runTrace: (run: string) => request<RunTrace>("GET", `/v1/runs/${enc(run)}/trace`),
  /** The person's comments, to the task's worker as its next turn; `rework` sends the task back. */
  reviewTask: (run: string, task: string, req: TaskReviewRequest) => request<Task>("POST", `/v1/runs/${enc(run)}/tasks/${enc(task)}/review`, req),
  /** `git init` and a first commit in `path`, so later runs there get worktrees. */
  initRepo: (path: string) => request<InitRepoRequest>("POST", "/v1/repos/init", { path } satisfies InitRepoRequest),

  usage: (by: UsageGroup) => request<UsageResponse>("GET", `/v1/usage?by=${by}`),
  /** Token use and cost day by day, across every account (`source: "all"`) or one.
   * `granularity` says whether days count calls or sessions; see the response's notes. */
  usageSeries: (range: UsageSeriesRange = "30d", group: UsageSeriesGroup = "total", source = "all") =>
    request<UsageSeriesResponse>(
      "GET",
      `/v1/usage/series?range=${range}&group=${group}&source=${enc(source)}`,
    ),

  /** One owner's expertise profile (person: owner_id "me"; project: the repo key). */
  expertise: async (owner_kind: OwnerKind, owner_id: string): Promise<ExpertiseEntry[]> =>
    (await request<ListExpertiseResponse>("GET", `/v1/expertise?${expertiseQuery(owner_kind, owner_id)}`)).entries,
  /** Candidates by status; with an owner, only that owner's. */
  expertiseCandidates: async (status: CandidateStatus = "pending", owner?: { kind: OwnerKind; id: string }): Promise<Candidate[]> =>
    (
      await request<ListCandidatesResponse>(
        "GET",
        `/v1/expertise/candidates?status=${status}${owner ? `&${expertiseQuery(owner.kind, owner.id)}` : ""}`,
      )
    ).candidates,
  /** Every conflict of the candidate needs a verdict, else 400 and nothing changes; always adds
   * a new entry (keep_old too), replace/merge supersede the old one. */
  acceptCandidate: (id: string, decisions: ConflictDecision[]) =>
    request<AcceptCandidateResponse>("POST", `/v1/expertise/candidates/${enc(id)}/accept`, { decisions } satisfies AcceptRequest),
  rejectCandidate: (id: string) => request<Candidate>("POST", `/v1/expertise/candidates/${enc(id)}/reject`),
  expertiseCard: (owner_kind: OwnerKind, owner_id: string) =>
    request<ExpertiseCard>("GET", `/v1/expertise/card?${expertiseQuery(owner_kind, owner_id)}`),
  expertiseMaintenance: (owner_kind: OwnerKind, owner_id: string) =>
    request<Maintenance>("GET", `/v1/expertise/maintenance?${expertiseQuery(owner_kind, owner_id)}`),
  /** The profile as Markdown (text/markdown); empty when it has no entries. */
  expertiseExport: (owner_kind: OwnerKind, owner_id: string) =>
    request<string>("GET", `/v1/expertise/export?${expertiseQuery(owner_kind, owner_id)}`, undefined, { text: true }),

  questions: (all = false) => request<ListQuestionsResponse>("GET", `/v1/questions${all ? "?all=true" : ""}`),
  answer: (id: string, answer: string) => request<HumanItem>("POST", `/v1/questions/${enc(id)}/answer`, { answer }),
  dismiss: (id: string) => request<void>("POST", `/v1/questions/${enc(id)}/dismiss`),

  quota: () => request<QuotaResponse>("GET", "/v1/quota"),
  createAccount: (req: CreateAccountRequest) => request<Account>("POST", "/v1/accounts", req),
  deleteAccount: (id: string) => request<void>("DELETE", `/v1/accounts/${enc(id)}`),
  accountUsage: (id: string, by: AccountUsageGroup) =>
    request<AccountUsageResponse>("GET", `/v1/accounts/${enc(id)}/usage?by=${by}`),
  login: (id: string) => request<LoginStarted>("POST", `/v1/accounts/${enc(id)}/login`),

  /** One directory of the scope's root, lazily; `all` lists gitignored entries too. */
  filesTree,
  /** A file's text (up to 1 MiB), or `binary` without it. */
  filesContent,
  filesResolve,
  /** An image a tool call named by path (a `ViewImage` tool's `image_path`, say), base64:
   * show it as `data:${mime};base64,${data}`, as an attachment is shown. The hub serves only
   * png, jpg, jpeg, gif, webp, bmp and svg, at most 10 MB, inside the agent's working
   * directory or the system temp directory; anything else is an ApiError with the reason. */
  agentImage: (key: string, path: string) =>
    request<ImageContent>("GET", `/v1/agents/${enc(key)}/files/image?path=${enc(path)}`),
};

export function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
