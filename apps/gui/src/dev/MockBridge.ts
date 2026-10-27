// A stand-in for the Electron bridge (`window.crossagents`, bridge.d.ts) in `preview:mock`: it
// answers `/v1/...` from a scenario's state and plays the scenario's event script on the event
// stream. Events change the state the same way the hub's do, so REST re-fetches agree with
// what streamed (transcripts fold with the GUI's own session.ts). Writes that matter to the
// look (sending, withdrawing, approving) are acted out; the rest answer 204.

import type { AgentExecution } from "@bindings/AgentExecution";
import type { AccountUsageResponse } from "@bindings/AccountUsageResponse";
import type { Agent } from "@bindings/Agent";
import type { CreateAgentRequest } from "@bindings/CreateAgentRequest";
import type { HumanItem } from "@bindings/HumanItem";
import type { LandMode } from "@bindings/LandMode";
import type { Note } from "@bindings/Note";
import type { PluginInfo } from "@bindings/PluginInfo";
import type { QuotaResponse } from "@bindings/QuotaResponse";
import type { ResumeCommandResponse } from "@bindings/ResumeCommandResponse";
import type { RunDetail } from "@bindings/RunDetail";
import type { RunTrace } from "@bindings/RunTrace";
import type { SendMessageRequest } from "@bindings/SendMessageRequest";
import type { TaskReviewRequest } from "@bindings/TaskReviewRequest";
import type { Transcript } from "@bindings/Transcript";
import type { UsageGroup } from "@bindings/UsageGroup";
import type { UsageResponse } from "@bindings/UsageResponse";
import type { CrossAgentsBridge, HubResponse } from "../bridge";
import type { HubEvent } from "../hub";
import { turnStat } from "../lib/diff";
import { effortOf } from "../lib/models";
import { applyEvent } from "../session";
import { capabilitiesOf, driverOf, transcript, uid } from "./fixtures";
import { mockContent, mockResolve, mockTree } from "./files.mock";

/** Names the mock hub makes up, like the real one (see crates/core/src/names.rs). */
const MOCK_NAMES = ["Nova", "Iris", "Atlas", "Orion", "Vega", "Lyra", "Mira", "Sage"];

export type MockState = {
  agents: Agent[];
  runs: RunDetail[];
  /** By agent id. */
  transcripts: Map<string, Transcript>;
  /** Unified diff of each turn that changed files, by message id. */
  diffs: Map<string, string>;
  /** Each task's whole diff against its base (review), by task id. */
  taskDiffs: Map<string, string>;
  /** Each run's branch against its base (before landing), by run id. */
  runDiffs: Map<string, string>;
  /** Run traces (docs/tasks/DOGFOOD.md §8), by run id; absent: tasks only. */
  traces: Map<string, RunTrace>;
  /** Files sent with messages, by id: type and base64 bytes. */
  files: Map<string, { mime: string; data: string }>;
  questions: HumanItem[];
  notes: Note[];
  quota: QuotaResponse;
  usage: Partial<Record<UsageGroup, UsageResponse>>;
  accountUsage?: AccountUsageResponse;
  plugins: PluginInfo[];
  /** What a mock install or upgrade does: succeed, exit non-zero, or leave nothing behind. */
  install?: "ok" | "fail" | "missing";
  /** Uncommitted files in a run's checkout, by run id: deleting it asks for force. */
  dirty?: Record<string, string[]>;
  /** The API version `/v1/info` reports; `v1` when unset. */
  api?: string;
  /** What holds each session now (docs/tasks/S1.md §3.3), by agent id. */
  executions?: Record<string, AgentExecution>;
  /** The native resume command (docs/TODO.md §十五) by agent id, when a scenario fixes it; else made up from the agent like the hub would. */
  resume?: Record<string, ResumeCommandResponse>;
  /**
   * Copied leads whose run branch is gone (docs/tasks/S1.md §7 第 15 条), by agent id: restoring
   * one rebuilds its checkout from the base and says the code did not come back.
   */
  codeLost?: Record<string, { branch: string; base: string; landed?: { mode: LandMode; reference: string } }>;
};

/**
 * The hub's own words, filled in the same way, for what the mock acts out. Keep them equal to
 * the Rust they name: the GUI shows these strings, it does not write its own.
 */
export const HUB_SAYS = {
  /** crates/orchestrator/src/delete.rs `move_out_of`: the `config.context_lost` mark. */
  contextLost: (runTitle: string, dir: string, name: string) => `run「${runTitle}」删除了，原执行目录 ${dir} 也不在了，${name} 的驱动只能在原目录延续会话`,
  /** crates/hub/src/runtime.rs `archive`, when restoring cannot carry the context on. */
  restored: (why: string) => `对话记录恢复了，但${why}：下一条消息会开新会话，不带原来的上下文`,
  /** crates/hub/src/runtime.rs `cannot_carry_on`, a driver that resumes nothing. */
  cannotResume: (name: string) => `${name} 的驱动不能延续会话`,
  /** crates/hub/src/runtime.rs `start`, once the new session is up. */
  fresh: (why: string) => `这是新会话，没有带上之前的上下文：${why}`,
  /** crates/orchestrator/src/landing.rs `lead_returned`. */
  codeLost: (branch: string, base: string, landed?: { mode: LandMode; reference: string }) =>
    `对话保留了，但原执行代码没有恢复：run 分支 ${branch} 已经删除，检出按 ${base} 重新建立` +
    (!landed ? "" : landed.mode === "pr" ? `；run 曾开过 PR（${landed.reference}），不一定已经合入 ${base}` : `；run 曾落地到 ${base}（${landed.reference}），没有核对那些改动现在是否还在`),
};

type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;
/** An event as a script writes it; the bridge numbers and stamps it. */
export type Payload = DistributiveOmit<HubEvent, "seq" | "id" | "ts" | "schema">;
/** One script step: `at` ms after the stream connects, an event, a change to the state, or a request made as the page would (`bridge`). */
export type Step = { at: number; event?: Payload; act?: (s: MockState, bridge: MockBridge) => void };

export function emptyState(): MockState {
  return {
    agents: [],
    runs: [],
    transcripts: new Map(),
    diffs: new Map(),
    taskDiffs: new Map(),
    runDiffs: new Map(),
    files: new Map(),
    traces: new Map(),
    questions: [],
    notes: [],
    quota: { accounts: [], providers: ["claude", "codex"] },
    usage: {},
    plugins: [],
  };
}

const LATENCY = 40;
/** How long a stopped agent takes to come up, as a real CLI does. */
const STARTUP = 2_500;
const json = (status: number, body?: unknown): HubResponse => ({ status, body: body === undefined ? "" : JSON.stringify(body) });
const notFound = (what: string) => json(404, { error: { code: "not_found", message: `${what} not found` } });
/** `{scope}/files/{tree|content|resolve}`; every scope answers from the same mock workspace. */
const FILES_ROUTE = /^\/v1\/(?:agents\/[^/]+|runs\/[^/]+(?:\/tasks\/[^/]+)?)\/files\/(tree|content|resolve)$/;

export class MockBridge implements CrossAgentsBridge {
  private seq = 1000;
  private events = new Set<(data: string) => void>();
  private statuses = new Set<(status: string) => void>();
  private timers: ReturnType<typeof setTimeout>[] = [];
  private started = false;

  constructor(
    readonly state: MockState,
    private script: Step[] = [],
  ) {}

  // ---- CrossAgentsBridge ----------------------------------------------------------------

  async request(method: string, path: string, body: string | null): Promise<HubResponse> {
    await new Promise((r) => setTimeout(r, LATENCY));
    // Like the hub: a message to a stopped agent is answered once its process is up.
    const to = method === "POST" ? /^\/v1\/agents\/([^/]+)\/messages$/.exec(path) : null;
    const asleep = to && this.agent(to[1]);
    if (asleep && (asleep.status === "stopped" || asleep.status === "error")) {
      this.emit({ type: "agent.status_changed", data: { id: asleep.id, from: asleep.status, to: "starting" } });
      await new Promise((r) => setTimeout(r, STARTUP));
    }
    try {
      return this.route(method, path, body ? JSON.parse(body) : undefined);
    } catch (err) {
      return json(500, { error: { code: "internal", message: `mock: ${err}` } });
    }
  }

  startEvents() {
    if (this.started) return;
    this.started = true;
    this.status("connecting");
    setTimeout(() => {
      this.status("connected");
      for (const step of this.script) this.timers.push(setTimeout(() => this.play(step), step.at));
    }, 120);
  }

  onEvent(listener: (data: string) => void) {
    this.events.add(listener);
    return () => this.events.delete(listener);
  }

  onStatus(listener: (status: string) => void) {
    this.statuses.add(listener);
    return () => this.statuses.delete(listener);
  }

  /** `?devhub` in the page URL: pretend a development hub runs instead of the shipped one. */
  private devHubOn = new URLSearchParams(location.search).has("devhub");

  async devHub() {
    return this.devHubOn ? { exe: "C:/Users/me/.crossagents/shadow/881aadafed8a0740/crossagents.exe", running: "0.1.0", shipped: "0.1.0" } : null;
  }

  async useShippedHub() {
    await new Promise((r) => setTimeout(r, 800));
    this.devHubOn = false;
  }

  async pickFolder() {
    return "C:/work/new-project";
  }

  async openPath(scope: string, path: string) {
    // No desktop to open it on in a browser.
    console.info("mock openPath", scope, path);
  }

  setTheme() {
    // No window frame to follow in a browser.
  }

  // ---- Events ---------------------------------------------------------------------------

  private status(s: string) {
    this.statuses.forEach((l) => l(s));
  }

  private play(step: Step) {
    step.act?.(this.state, this);
    if (step.event) this.emit(step.event);
  }

  /** Numbers and stamps an event, folds it into the state, and streams it. */
  emit(payload: Payload) {
    const event = { seq: ++this.seq, id: uid(), ts: new Date().toISOString(), schema: 1, ...payload } as HubEvent;
    this.fold(event);
    const data = JSON.stringify(event);
    this.events.forEach((l) => l(data));
  }

  private fold(e: HubEvent) {
    const s = this.state;
    switch (e.type) {
      case "agent.status_changed": {
        const a = s.agents.find((x) => x.id === e.data.id);
        if (a) a.status = e.data.to;
        break;
      }
      case "agent.turn_diff": {
        const t = s.transcripts.get(e.data.id);
        const turn = e.data.message_id ?? t?.turns.find((x) => x.started_at && !x.completed_at)?.message_id;
        if (turn) s.diffs.set(turn, e.data.diff);
        break;
      }
      case "task.status_changed":
      case "task.progress": {
        const task = s.runs.flatMap((d) => d.tasks).find((x) => x.id === e.data.id);
        if (task && e.type === "task.status_changed") task.status = e.data.to;
        if (task && e.type === "task.progress") task.progress = e.data.text;
        if (task) task.updated_at = e.ts;
        break;
      }
      case "task.rerouted": {
        const task = s.runs.flatMap((d) => d.tasks).find((x) => x.id === e.data.id);
        if (task) task.progress = e.data.reason;
        break;
      }
      case "task.handed_off": {
        const card = e.data.handoff;
        const task = s.runs.flatMap((d) => d.tasks).find((x) => x.id === card.task_id);
        if (task && !task.handoffs.some((h) => h.id === card.id)) {
          task.handoffs = [...task.handoffs, card];
          task.assignee = card.to;
          task.updated_at = e.ts;
        }
        break;
      }
      case "run.status_changed": {
        const r = s.runs.find((d) => d.run.id === e.data.id);
        if (r) {
          r.run.status = e.data.to;
          r.run.updated_at = e.ts;
        }
        break;
      }
      case "run.deleted":
        s.runs = s.runs.filter((d) => d.run.id !== e.data.run_id);
        break;
      case "human.asked":
        s.questions.push(e.data.item);
        break;
    }
    if (e.type.startsWith("agent.")) {
      for (const [id, t] of s.transcripts) {
        const next = applyEvent(t, e);
        if (next !== t) s.transcripts.set(id, next);
      }
    }
  }

  /** Plays events one after another, `gap` ms apart. */
  private sequence(payloads: Payload[], gap = 350) {
    payloads.forEach((p, i) => this.timers.push(setTimeout(() => this.emit(p), (i + 1) * gap)));
  }

  // ---- Requests -------------------------------------------------------------------------

  private agent(key: string): Agent | undefined {
    const k = decodeURIComponent(key);
    return this.state.agents.find((a) => a.id === k || a.name.toLowerCase() === k.toLowerCase());
  }

  private route(method: string, path: string, body: unknown): HubResponse {
    const s = this.state;
    const url = new URL(path, "http://mock");
    const p = url.pathname;
    let m: RegExpMatchArray | null;

    if (method === "GET") {
      if (p === "/v1/info") {
        // `?hubcommit=abc1234`: a hub built from another commit than the page.
        const commit = new URLSearchParams(location.search).get("hubcommit") ?? __BUILD__.commit;
        return json(200, { version: __BUILD__.version, commit, debug: false, api: s.api ?? "v1", pid: 0, home: "（mock 模式，无 hub）", started_at: new Date().toISOString() });
      }
      if (p === "/v1/agents") return json(200, { agents: s.agents });
      if ((m = p.match(/^\/v1\/agents\/([^/]+)$/))) return this.agent(m[1]) ? json(200, this.agent(m[1])) : notFound("agent");
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/execution$/))) {
        const a = this.agent(m[1]);
        if (!a) return notFound("agent");
        return json(200, { execution: s.executions?.[a.id] ?? null });
      }
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/resume-command$/))) {
        const a = this.agent(m[1]);
        return a ? json(200, this.resumeCommand(a)) : notFound("agent");
      }
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/transcript$/))) {
        const a = this.agent(m[1]);
        const t = a && s.transcripts.get(a.id);
        if (!a) return notFound(`agent ${decodeURIComponent(m[1])}`);
        const base: Transcript = t ?? { agent: a, driver: null, capabilities: null, turns: [], usage: null, switches: [], last_seq: 0 };
        return json(200, { ...base, agent: a, last_seq: this.seq });
      }
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/turns\/([^/]+)\/diff$/))) {
        const a = this.agent(m[1]);
        const mid = decodeURIComponent(m[2]);
        const turn = a && s.transcripts.get(a.id)?.turns.find((t) => t.message_id === mid);
        const diff = s.diffs.get(mid);
        if (!turn?.diff || !diff) return notFound("turn diff");
        return json(200, { message_id: mid, diff, stat: turn.diff });
      }
      if ((m = p.match(/^\/v1\/runs\/([^/]+)\/tasks\/([^/]+)\/diff$/))) {
        const found = this.task(m[1], m[2]);
        const diff = found && s.taskDiffs.get(found.task.id);
        if (!found) return notFound("task");
        if (diff === undefined) return json(409, { error: { code: "conflict", message: `#${found.task.key} has no worktree (not started, or ended)` } });
        return json(200, { base: found.run.run.branch, head: found.task.branch ?? "", diff, truncated: false });
      }
      if ((m = p.match(/^\/v1\/runs\/([^/]+)\/trace$/))) {
        const trace = s.traces.get(m[1]);
        const d = s.runs.find((x) => x.run.id === m![1]);
        if (!d) return notFound("run");
        const empty = { agents: [], turns: [], commands: [], spent: { tokens: 0, cost_usd: 0 } };
        // Without a scripted trace, each task as stored and nothing more.
        const tasks = d.tasks.map((t) => ({ task_id: t.id, key: t.key, title: t.title, status: t.status, failure: t.report.failure, lane: empty, checks: [], rerouted: [], first_failure: null }));
        return json(200, trace ?? { run_id: d.run.id, lead: empty, tasks, spent: { tokens: 0, cost_usd: 0 } });
      }
      if ((m = p.match(/^\/v1\/runs\/([^/]+)\/diff$/))) {
        const d = s.runs.find((x) => x.run.id === m![1]);
        if (!d) return notFound("run");
        if (d.run.in_place) return json(409, { error: { code: "conflict", message: "this directory is not under version control, so there is no diff" } });
        return json(200, { base: d.run.base_branch, head: d.run.branch, diff: s.runDiffs.get(d.run.id) ?? "", truncated: false });
      }
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/attachments\/([^/]+)$/))) {
        const file = s.files.get(decodeURIComponent(m[2]));
        return file ? json(200, { id: m[2], mime: file.mime, size: file.data.length, data: file.data }) : notFound("attachment");
      }
      if (p === "/v1/runs") return json(200, { runs: s.runs.map((d) => d.run) });
      if ((m = p.match(/^\/v1\/runs\/([^/]+)$/))) {
        const d = s.runs.find((x) => x.run.id === m![1]);
        return d ? json(200, d) : notFound("run");
      }
      if (p === "/v1/questions") return json(200, { items: s.questions.filter((q) => url.searchParams.get("all") || q.status === "open") });
      if (p === "/v1/notes") return json(200, { notes: s.notes });
      if (p === "/v1/quota") return json(200, s.quota);
      if (p === "/v1/usage") {
        const by = (url.searchParams.get("by") ?? "role") as UsageGroup;
        return json(200, s.usage[by] ?? { by, rows: [], total_cost_usd: 0 });
      }
      if ((m = p.match(/^\/v1\/accounts\/([^/]+)\/usage$/)))
        return json(200, s.accountUsage ?? { by: url.searchParams.get("by") ?? "day", rows: [], total: { sessions: 0, input_tokens: 0, cached_input_tokens: 0, output_tokens: 0, reasoning_output_tokens: 0 }, total_cache_hit_rate: 0 });
      if (p === "/v1/plugins") return json(200, { plugins: s.plugins });
      if ((m = p.match(FILES_ROUTE)) && m[1] !== "resolve") {
        const at = url.searchParams.get("path") ?? "";
        if (m[1] === "tree") {
          const tree = mockTree(at, url.searchParams.get("all") === "true");
          return tree ? json(200, tree) : notFound(`directory ${at}`);
        }
        const file = mockContent(at);
        return file ? json(200, file) : notFound(`file ${at}`);
      }
      return notFound(p);
    }

    if (method === "POST") {
      if ((m = p.match(FILES_ROUTE)) && m[1] === "resolve") return json(200, { entries: mockResolve((body as { paths: string[] }).paths ?? []) });
      if (p === "/v1/agents") {
        const req = body as CreateAgentRequest;
        const taken = (n: string) => s.agents.some((a) => a.name.toLowerCase() === n.toLowerCase());
        if (req.name && taken(req.name)) return json(409, { error: { code: "conflict", message: `agent name "${req.name}" is already taken` } });
        // Like the hub: no name, a made-up person's name (never a model's).
        const name = req.name || MOCK_NAMES.find((n) => !taken(n)) || `Nova ${s.agents.length}`;
        const id = uid();
        const cwd = req.cwd || `C:/Users/me/.crossagents/chats/${id}`;
        const agent: Agent = {
          id,
          ...req,
          name,
          name_source: req.name ? "chosen" : "generated",
          topic: req.topic ?? null,
          archived_at: null,
          home: cwd,
          cwd,
          status: "stopped", // as the hub makes them: started by their first message
          created_at: new Date().toISOString(),
        };
        s.agents.push(agent);
        s.transcripts.set(agent.id, transcript(agent, [], 0));
        this.emit({ type: "agent.created", data: { agent } });
        return json(201, agent);
      }
      // Starting a run needs a real planner; the preview stops here with the hub's own error shape.
      if (p === "/v1/runs") return json(400, { error: { code: "bad_request", message: "mock 预览不能真的发起 run（agent 已经建好了）" } });
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/name$/))) {
        const a = this.agent(m[1]);
        if (!a) return notFound("agent");
        const name = (body as { name: string }).name.trim();
        if (s.agents.some((x) => x !== a && x.name.toLowerCase() === name.toLowerCase()))
          return json(409, { error: { code: "conflict", message: `agent name "${name}" is already taken` } });
        a.name = name;
        a.name_source = "chosen";
        this.emit({ type: "agent.updated", data: { agent: { ...a } } });
        return json(200, a);
      }
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/settings$/))) {
        const a = this.agent(m[1]);
        if (!a) return notFound("agent");
        const config = { ...((a.config as Record<string, unknown>) ?? {}) };
        const sources = (body as { setting_sources: string | null }).setting_sources;
        if (sources === null) delete config.setting_sources;
        else config.setting_sources = sources;
        a.config = config as Agent["config"];
        this.emit({ type: "agent.updated", data: { agent: { ...a } } });
        return json(200, a);
      }
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/topic$/))) {
        const a = this.agent(m[1]);
        if (!a) return notFound("agent");
        a.topic = (body as { topic: string | null }).topic?.trim() || null;
        this.emit({ type: "agent.updated", data: { agent: { ...a } } });
        return json(200, a);
      }
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/execution\/interrupt$/))) {
        // Like the hub: the execution ends as interrupted, the task waits for its issuer, and
        // the session goes back to its workspace.
        const a = this.agent(m[1]);
        const x = a && s.executions?.[a.id];
        if (!a || !x?.task_id) return json(409, { error: { code: "conflict", message: `${a?.name ?? "it"} is not working on any task` } });
        delete s.executions![a.id];
        const task = s.runs.flatMap((d) => d.tasks).find((t) => t.id === x.task_id)!;
        const card = [...task.handoffs].reverse().find((h) => h.to === a.id);
        if (card) card.outcome = "interrupted";
        const from = task.status;
        task.assignee = null;
        task.status = "stalled";
        a.cwd = a.home;
        a.status = "idle";
        this.emit({ type: "task.status_changed", data: { run_id: task.run_id, id: task.id, key: task.key, from, to: "stalled", assignee: null } });
        this.emit({ type: "agent.updated", data: { agent: a } });
        return json(200, task);
      }
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/archive$/))) {
        const root = this.agent(m[1]);
        if (!root) return notFound("agent");
        const archived = (body as { archived: boolean }).archived;
        const recruitedBy = (a: Agent) => (a.config as Record<string, unknown> | null)?.spawned_by_agent;
        for (const a of [root, ...s.agents.filter((x) => recruitedBy(x) === root.id)]) {
          a.archived_at = archived ? new Date().toISOString() : null;
          if (archived) a.status = "stopped";
          this.emit({ type: "agent.updated", data: { agent: { ...a } } });
        }
        if (!archived) this.restored(root);
        return json(200, root);
      }
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/model$/))) {
        const a = this.agent(m[1]);
        if (!a) return notFound("agent");
        const req = body as { model: string | null; effort?: string };
        const from = { model: a.model, effort: effortOf(a.config) ?? null };
        a.model = req.model;
        if (req.effort !== undefined) {
          const config = { ...((a.config as Record<string, unknown>) ?? {}) };
          if (req.effort) config.effort = req.effort;
          else delete config.effort;
          a.config = config as Agent["config"];
        }
        this.emit({ type: "agent.updated", data: { agent: { ...a } } });
        const to = { model: a.model, effort: effortOf(a.config) ?? null };
        if (to.model !== from.model || to.effort !== from.effort) {
          s.transcripts.get(a.id)?.switches.push({ at: new Date().toISOString(), ...to, from_model: from.model, from_effort: from.effort });
          this.emit({ type: "agent.model_changed", data: { id: a.id, ...to, from_model: from.model, from_effort: from.effort } });
        }
        return json(200, a);
      }
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/messages$/))) return this.send(m[1], body as SendMessageRequest);
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/cancel$/))) return this.cancel(m[1]);
      if ((m = p.match(/^\/v1\/runs\/([^/]+)\/(approve|reject|cancel)$/))) {
        const d = s.runs.find((x) => x.run.id === m![1]);
        if (!d) return notFound("run");
        const to = m[2] === "approve" ? "running" : m[2] === "reject" ? "planning" : "cancelled";
        this.emit({ type: "run.status_changed", data: { id: d.run.id, from: d.run.status, to, note: m[2] === "reject" ? ((body as { reason?: string })?.reason ?? null) : null } });
        return json(200, d);
      }
      if ((m = p.match(/^\/v1\/runs\/([^/]+)\/tasks\/([^/]+)\/review$/))) return this.review(m[1], m[2], body as TaskReviewRequest);
      if (p === "/v1/repos/init") return json(200, body);
      if ((m = p.match(/^\/v1\/questions\/([^/]+)\/(answer|dismiss)$/))) {
        const q = s.questions.find((x) => x.id === m![1]);
        if (q) {
          q.status = m[2] === "answer" ? "answered" : "dismissed";
          q.answer = (body as { answer?: string })?.answer ?? null;
          this.emit(m[2] === "answer" ? { type: "human.answered", data: { item: q } } : { type: "human.dismissed", data: { id: q.id } });
        }
        return json(m[2] === "answer" ? 200 : 204, q);
      }
      if (p === "/v1/repos/init") return json(200, body);
      if ((m = p.match(/^\/v1\/plugins\/([^/]+)\/(install|upgrade)$/)))
        return this.installCli(decodeURIComponent(m[1]), m[2] as "install" | "upgrade");
      if ((m = p.match(/^\/v1\/plugins\/([^/]+)\/models$/))) {
        const plugin = s.plugins.find((x) => x.name === decodeURIComponent(m![1]));
        if (!plugin) return json(404, { error: { code: "not_found", message: "no such plugin" } });
        const shown = (body as { shown: string[] | null }).shown;
        plugin.shown = shown;
        // As the hub does: in the vendor's own order, the first one being the default.
        plugin.models = shown ? plugin.all_models.filter((x) => shown.includes(x)) : [...plugin.all_models];
        return json(200, plugin);
      }
      return json(204);
    }

    if (method === "DELETE") {
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/messages\/([^/]+)$/))) {
        const a = this.agent(m[1]);
        const mid = decodeURIComponent(m[2]);
        const t = a && s.transcripts.get(a.id);
        const waiting = t?.turns.find((x) => x.message_id === mid && !x.started_at);
        if (!a || !waiting) return json(409, { error: { code: "conflict", message: "already started or gone" } });
        this.emit({ type: "agent.input_withdrawn", data: { id: a.id, message_id: mid } });
        return json(204);
      }
      if ((m = p.match(/^\/v1\/runs\/([^/]+)$/))) return this.deleteRun(m[1], url);
      return json(204);
    }
    return json(405, { error: { code: "bad_request", message: `${method} ${p}` } });
  }

  /** A message: queued behind a running turn, else answered by a short made-up turn. */
  private send(key: string, req: SendMessageRequest): HubResponse {
    const a = this.agent(key);
    if (!a) return notFound("agent");
    const message_id = uid();
    const busy = a.status === "running";
    // Like the hub: files are kept apart and the message carries what they are.
    const attachments = (req.attachments ?? []).map((f) => {
      const id = uid().replace(/-/g, "");
      this.state.files.set(id, { mime: f.mime, data: f.data });
      return { id, name: f.name, mime: f.mime, size: Math.floor((f.data.length * 3) / 4), path: `C:/Users/me/.crossagents/attachments/${a.id}/${id}` };
    });
    this.emit({ type: "agent.input", data: { id: a.id, message_id, text: req.text, queued: busy, from: { kind: "human" }, ...(attachments.length ? { attachments } : {}) } });
    this.freshSession(a);
    if (!busy) this.answer(a, message_id, req.text);
    return json(202, { message_id, queued: busy });
  }

  /** Like the hub's `archive` (and `lead_returned` for a copied lead): what restoring could not bring back is said. */
  restored(a: Agent) {
    const s = this.state;
    const hadSession = !!s.transcripts.get(a.id)?.turns.some((t) => t.started_at);
    const lost = (a.config as Record<string, unknown> | null)?.context_lost;
    const why = typeof lost === "string" ? lost : !capabilitiesOf(a.plugin).resume ? HUB_SAYS.cannotResume(a.name) : undefined;
    if (hadSession && why) this.emit({ type: "agent.warning", data: { id: a.id, message: HUB_SAYS.restored(why) } });
    const code = s.codeLost?.[a.id];
    if (code) {
      // The checkout is rebuilt from the base, once.
      delete s.codeLost![a.id];
      this.emit({ type: "agent.warning", data: { id: a.id, message: HUB_SAYS.codeLost(code.branch, code.base, code.landed) } });
    }
  }

  /** Like the hub's `start`: a session marked `context_lost` opens a new one, says so, and drops the mark. */
  freshSession(a: Agent) {
    const config = (a.config as Record<string, unknown> | null) ?? {};
    const lost = config.context_lost;
    if (typeof lost !== "string") return;
    const rest = { ...config };
    delete rest.context_lost;
    a.config = rest as Agent["config"];
    this.emit({ type: "agent.warning", data: { id: a.id, message: HUB_SAYS.fresh(lost) } });
    this.emit({ type: "agent.updated", data: { agent: { ...a } } });
  }

  /**
   * `GET /v1/agents/{key}/resume-command`, decided in the order the hub decides it
   * (crates/hub/src/resume.rs `command`), with made-up paths. A scenario can fix the answer.
   */
  private resumeCommand(a: Agent): ResumeCommandResponse {
    const s = this.state;
    const fixed = s.resume?.[a.id];
    if (fixed) return fixed;
    const started = !!s.transcripts.get(a.id)?.turns.some((t) => t.started_at);
    const session_id = started ? `5e55${a.id.slice(4)}` : null;
    const base: ResumeCommandResponse = { command: null, unavailable: null, shell: "powershell", cwd: a.cwd, session_id, env: [], notes: [] };
    const driver = driverOf(a.plugin);
    const lost = (a.config as Record<string, unknown> | null)?.context_lost;
    const no = (unavailable: string) => ({ ...base, unavailable });
    if (a.plugin === "external") return no(`${a.name} 是外部接入的会话，CrossAgents 不知道它的 CLI 怎么恢复`);
    if (!capabilitiesOf(a.plugin).resume) return no(`${a.name} 的驱动（${driver}）不能恢复会话：进程停了，上下文就没了`);
    if (driver === "with-native" || driver === "acp-generic") return no(`${driver} 驱动的 CLI 没有在终端里按 session id 接着聊的命令`);
    if (!session_id) return no(`${a.name} 还没开过会话，没有可以恢复的 session id`);
    if (typeof lost === "string") return no(`原来的上下文已经带不回来：${lost}`);
    if (a.archived_at && a.cwd !== a.home && s.codeLost?.[a.id])
      return no(`目录 ${a.cwd} 不在了：复制的主导在 run 结束后检出被删掉了。先在这里点「恢复对话」，hub 会重建检出，之后再复制命令`);
    const [name, program, sub] =
      driver === "codex-native"
        ? ["CODEX_HOME", "C:\\Users\\me\\AppData\\Roaming\\npm\\codex.cmd", "'resume'"]
        : driver === "codebuddy-native"
          ? ["CODEBUDDY_CONFIG_DIR", "C:\\Users\\me\\AppData\\Roaming\\npm\\codebuddy.cmd", "--resume"]
          : ["CLAUDE_CONFIG_DIR", "C:\\Users\\me\\.local\\bin\\claude.exe", "--resume"];
    const env = [
      { name, value: `C:\\Users\\me\\.crossagents\\accounts\\${a.plugin}-work` },
      { name: "HTTPS_PROXY", value: "http://127.0.0.1:7890" },
    ];
    const q = (v: string) => `'${v.replace(/'/g, "''")}'`;
    const command = [`Set-Location -LiteralPath ${q(a.cwd)}`, ...env.map((e) => `$env:${e.name} = ${q(e.value)}`), `& ${q(program)} ${sub} ${q(session_id)}`].join("; ");
    const notes = ["命令里用 $env: 设的变量会留在这个 PowerShell 窗口里"];
    if (a.status !== "stopped" && a.status !== "error") notes.unshift(`${a.name} 现在在 hub 里开着：两边同时用同一个会话会互相覆盖，先在 GUI 里停掉它，再在终端里接着聊`);
    return { ...base, command, env, notes };
  }

  private answer(a: Agent, message_id: string, text: string) {
    const id = a.id;
    const call = uid();
    const reply = `（mock）收到：「${text.slice(0, 40)}」。这是预览模式，没有真的 agent 在跑；看样式用。`;
    const diff = `diff --git a/notes/mock.md b/notes/mock.md\n--- a/notes/mock.md\n+++ b/notes/mock.md\n@@ -1 +1,2 @@\n # mock\n+${text.slice(0, 60)}\n`;
    this.sequence([
      { type: "agent.status_changed", data: { id, from: a.status, to: "running" } },
      { type: "agent.turn_started", data: { id, message_id } },
      { type: "agent.output", data: { id, stream: "thought", text: "先看一下要改哪里。" } },
      { type: "agent.tool_call", data: { id, tool_call_id: call, title: "Read notes/mock.md", kind: "read", status: "in_progress" } },
      { type: "agent.tool_call_updated", data: { id, tool_call_id: call, title: null, status: "completed" } },
      { type: "agent.output", data: { id, stream: "text", text: reply.slice(0, 18) } },
      { type: "agent.output", data: { id, stream: "text", text: reply.slice(18) } },
      { type: "agent.turn_diff", data: { id, diff, message_id, source: "hub" } },
      { type: "agent.turn_completed", data: { id, message_id, stop_reason: "end_turn" } },
      { type: "agent.status_changed", data: { id, from: "running", to: "idle" } },
    ]);
  }

  private task(run: string, key: string) {
    const d = this.state.runs.find((x) => x.run.id === decodeURIComponent(run));
    const k = decodeURIComponent(key).replace(/^#/, "");
    const task = d?.tasks.find((t) => t.id === k || t.key === k);
    return d && task ? { run: d, task } : undefined;
  }

  /** Like the hub: the person's comments reach the worker as a message; `rework` reopens the task. */
  private review(run: string, key: string, req: TaskReviewRequest): HubResponse {
    const found = this.task(run, key);
    if (!found) return notFound("task");
    const { task } = found;
    if (!task.assignee || !["running", "waiting_answer", "passed", "stalled", "reviewing"].includes(task.status))
      return json(409, { error: { code: "conflict", message: `#${task.key} is ${task.status}` } });
    if (req.rework && ["passed", "stalled", "reviewing"].includes(task.status)) {
      task.attempts += 1;
      this.emit({ type: "task.status_changed", data: { run_id: task.run_id, id: task.id, key: task.key, from: task.status, to: "running", assignee: task.assignee } });
    }
    const place = (c: TaskReviewRequest["comments"][number]) => (c.line ? (c.side === "old" ? `${c.path} (old line ${c.line})` : `${c.path}:${c.line}`) : c.path);
    const lines = req.comments.map((c) => `- ${place(c)} — ${c.text}`);
    const parts = [req.note ?? "", lines.length ? `Review comments on #${task.key}:\n${lines.join("\n")}` : "", `---\nfrom: the person, reviewing task #${task.key}`];
    this.emit({ type: "agent.input", data: { id: task.assignee, message_id: uid(), text: parts.filter(Boolean).join("\n\n"), queued: true, from: { kind: "human" } } });
    return json(200, task);
  }

  /** Like the hub: the description's command runs, and the event stream says how it went. */
  private installCli(name: string, action: "install" | "upgrade"): HubResponse {
    const plugin = this.state.plugins.find((p) => p.name === name);
    const cli = plugin?.cli;
    if (!plugin || !cli) return notFound("plugin");
    const argv = action === "install" ? cli.install_command : cli.upgrade_command;
    if (!argv?.length) return json(409, { error: { code: "conflict", message: `${name} cannot be ${action}ed here` } });
    const mode = this.state.install ?? "ok";
    const lines =
      mode === "fail"
        ? [`$ ${argv.join(" ")}`, "npm error code EACCES", "npm error syscall mkdir"]
        : mode === "missing"
          ? [`$ ${argv.join(" ")}`, `installed ${name} into a directory the hub does not look in`]
          : [`$ ${argv.join(" ")}`, "added 1 package in 3s", `${name} ${action === "install" ? "installed" : "upgraded"}`];
    this.emit({ type: "cli.install.started", data: { plugin: name, action, argv, described_by: cli.described_by } });
    lines.forEach((line, i) =>
      this.timers.push(
        setTimeout(
          () => this.emit({ type: "cli.install.output", data: { plugin: name, action, line, dropped: 0 } }),
          300 + i * 450,
        ),
      ),
    );
    const ok = mode === "ok";
    const code = mode === "fail" ? 1 : 0;
    const reason =
      mode === "fail"
        ? "it exited with code 1"
        : mode === "missing"
          ? "the command succeeded, but the hub cannot find the installed program afterwards; it looked in: PATH: demo (demo.exe)"
          : undefined;
    this.timers.push(
      setTimeout(
        () => {
          if (ok) {
            cli.installed = true;
            cli.version = action === "install" ? "1.0.6" : "0.159.0";
            cli.source = "path";
            cli.installed_via = "npm";
            cli.can_upgrade = true;
          }
          this.emit({
            type: "cli.install.finished",
            data: { plugin: name, action, ok, code, duration_ms: 2400, reason, dropped: 0 },
          });
          this.emit({ type: "plugin.changed", data: { name } });
        },
        300 + lines.length * 450 + 500,
      ),
    );
    return json(202, { plugin: name, action, argv });
  }

  private cancel(key: string): HubResponse {
    const a = this.agent(key);
    const t = a && this.state.transcripts.get(a.id);
    const live = t?.turns.find((x) => x.started_at && !x.completed_at);
    if (!a || !live) return json(409, { error: { code: "conflict", message: "not running" } });
    this.emit({ type: "agent.turn_completed", data: { id: a.id, message_id: live.message_id, stop_reason: "cancelled" } });
    this.emit({ type: "agent.status_changed", data: { id: a.id, from: "running", to: "idle" } });
    return json(204);
  }

  /**
   * Deleting a run (docs/tasks/R1.md §3): only one that ended, a dirty checkout asking for
   * force, and then the run and everything the mock keeps for it go.
   */
  private deleteRun(key: string, url: URL): HubResponse {
    const s = this.state;
    const d = s.runs.find((x) => x.run.id === decodeURIComponent(key));
    if (!d) return notFound("run");
    const { run } = d;
    if (!["completed", "failed", "cancelled"].includes(run.status))
      return json(409, { error: { code: "conflict", message: `the run is ${run.status}: cancel it first` } });
    const dirty = s.dirty?.[run.id] ?? [];
    const force = url.searchParams.get("force") === "true";
    if (dirty.length && !run.landed && !force)
      return json(409, {
        error: { code: "conflict", message: `uncommitted changes, and the run never landed:\n${dirty.join("\n")}\nCommit them, or delete it with force.`, files: dirty },
      });
    const deleteBranches = url.searchParams.get("branches") === "true" || !!run.landed;
    // Like the hub: no session is deleted. A lead still in the run's checkout is a copy made
    // for it, and is archived; everyone else already went home when the run ended.
    const found = s.agents.find((a) => a.id === run.planner);
    const copied = found && found.cwd !== found.home ? found : undefined;
    if (copied) {
      copied.archived_at = new Date().toISOString();
      copied.cwd = copied.home;
      copied.status = "stopped";
      this.emit({ type: "agent.updated", data: { agent: copied } });
    }
    s.runs = s.runs.filter((x) => x.run.id !== run.id);
    this.emit({ type: "run.deleted", data: { run_id: run.id } });
    return json(200, {
      agents: { returned: [], archived: copied ? [copied.name] : [] },
      worktrees: run.in_place ? [] : [`${run.repo}/.crossagents/worktrees/${run.id.slice(0, 8)}/integration`],
      branches: deleteBranches && !run.in_place ? [run.branch] : [],
      warnings: [],
    });
  }

  /** Attaches a finished turn's diff (fixtures build transcripts with stats already in). */
  static withDiff(s: MockState, messageId: string, diff: string | undefined) {
    if (diff && turnStat(diff)) s.diffs.set(messageId, diff);
  }}
