// A stand-in for the Electron bridge (`window.crossagents`, bridge.d.ts) in `preview:mock`: it
// answers `/v1/...` from a scenario's state and plays the scenario's event script on the event
// stream. Events change the state the same way the hub's do, so REST re-fetches agree with
// what streamed (transcripts fold with the GUI's own session.ts). Writes that matter to the
// look (sending, withdrawing, approving) are acted out; the rest answer 204.

import { usageSeriesMock } from "../views/Usage.mock";
import type { AgentCommandsResponse } from "@bindings/AgentCommandsResponse";
import type { AgentExecution } from "@bindings/AgentExecution";
import type { AccountUsageResponse } from "@bindings/AccountUsageResponse";
import type { CommitGraph } from "@bindings/CommitGraph";
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
import type { SessionCandidate } from "@bindings/SessionCandidate";
import type { StartChatRequest } from "@bindings/StartChatRequest";
import type { TaskReviewRequest } from "@bindings/TaskReviewRequest";
import type { Transcript } from "@bindings/Transcript";
import type { UsageGroup } from "@bindings/UsageGroup";
import type { UsageResponse } from "@bindings/UsageResponse";
import type { CrossAgentsBridge, HubResponse, UpdateState } from "../bridge";
import type { HubEvent } from "../hub";
import { turnStat } from "../lib/diff";
import {
  acceptCandidate,
  budgetReport,
  conflictList,
  expertiseCard,
  exportMarkdown,
  missingVerdicts,
  staleScan,
  type Candidate,
  type ConflictDecision,
  type ExpertiseEntry,
  type UsageRecord,
} from "../lib/expertise";
import { effortOf } from "../lib/models";
import { applyEvent } from "../session";
import { capabilitiesOf, contextParts, driverOf, transcript, uid } from "./fixtures";
import { mockContent, mockResolve, mockTree } from "./files.mock";

/** Names the mock hub makes up, like the real one (see crates/core/src/names.rs). */
const MOCK_NAMES = ["Nova", "Iris", "Atlas", "Orion", "Vega", "Lyra", "Mira", "Sage"];

export type MockState = {
  agents: Agent[];
  agentCommands?: Record<string, AgentCommandsResponse>;
  runs: RunDetail[];
  /** By agent id. */
  transcripts: Map<string, Transcript>;
  /** Unified diff of each turn that changed files, by message id. */
  diffs: Map<string, string>;
  /** Each task's whole diff against its base (review), by task id. */
  taskDiffs: Map<string, string>;
  /** Each run's branch against its base (before landing), by run id. */
  runDiffs: Map<string, string>;
  /** Commit graphs (`GET .../graph`), by run id or agent id; absent: no commits. */
  graphs?: Map<string, CommitGraph>;
  /** What each commit of those graphs changed against its first parent, by sha. */
  commitDiffs?: Map<string, string>;
  /** Run traces (docs/tasks/DOGFOOD.md §8), by run id; absent: tasks only. */
  traces: Map<string, RunTrace>;
  /** Files sent with messages, by id: type and base64 bytes. */
  files: Map<string, { mime: string; data: string }>;
  questions: HumanItem[];
  notes: Note[];
  quota: QuotaResponse;
  usage: Partial<Record<UsageGroup, UsageResponse>>;
  usageSeriesVariant?: "normal" | "empty" | "issues" | "session";
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
  /** A workspace's sessions as `GET /v1/session-candidates` answers; a scenario sets what it shows. */
  sessions?: SessionCandidate[];
  /**
   * Copied leads whose run branch is gone (docs/tasks/S1.md §7 第 15 条), by agent id: restoring
   * one rebuilds its checkout from the base and says the code did not come back.
   */
  codeLost?: Record<string, { branch: string; base: string; landed?: { mode: LandMode; reference: string } }>;
  /** Expertise profiles (docs EXPERTISE.md): entries of every owner, candidates, usages. */
  expertise?: ExpertiseMock;
};

export type ExpertiseMock = { entries: ExpertiseEntry[]; candidates: Candidate[]; usages: UsageRecord[] };

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
  private avatars = new Map<string, string>();
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

  /** An update that is found, downloads in a few steps and waits to be installed. */
  private update: UpdateState = { state: "latest", current: __BUILD__.version, checked_at: new Date().toISOString() };
  private updateListeners = new Set<(state: UpdateState) => void>();
  private setUpdate(state: UpdateState) {
    this.update = state;
    for (const listener of this.updateListeners) listener(state);
  }
  updates = {
    get: async () => this.update,
    check: async () => {
      const current = __BUILD__.version;
      this.setUpdate({ state: "checking", current });
      await new Promise((r) => setTimeout(r, 600));
      for (const percent of [0, 35, 80]) {
        this.setUpdate({ state: "downloading", current, version: "9.9.9", percent });
        await new Promise((r) => setTimeout(r, 500));
      }
      this.setUpdate({ state: "ready", current, version: "9.9.9" });
      return this.update;
    },
    install: () => this.setUpdate({ state: "error", current: __BUILD__.version, message: "mock: 不会真的安装", checked_at: new Date().toISOString() }),
    onState: (listener: (state: UpdateState) => void) => {
      this.updateListeners.add(listener);
      return () => {
        this.updateListeners.delete(listener);
      };
    },
  };

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

    if (p === "/v1/avatars" && method === "GET") return json(200, { avatars: [...this.avatars.keys()] });
    if (p === "/v1/avatars" && method === "POST") {
      const added: string[] = [];
      for (const image of (body as { images: string[] }).images) {
        const id = Array.from(image.slice(-48), (c) => c.charCodeAt(0).toString(16).padStart(2, "0")).join("").slice(0, 64).padEnd(64, "0");
        if (!this.avatars.has(id)) added.push(id);
        this.avatars.set(id, image);
      }
      const ids = [...this.avatars.keys()];
      let assigned = 0;
      for (const a of s.agents) {
        if (a.profile.avatar || a.profile.avatar_image || !ids.length) continue;
        a.profile = { ...a.profile, avatar_image: ids[assigned++ % ids.length] };
        this.emit({ type: "agent.updated", data: { agent: { ...a } } });
      }
      return json(200, { added, assigned });
    }
    if ((m = p.match(/^\/v1\/avatars\/([^/]+)$/))) {
      const image = this.avatars.get(m[1]);
      if (image === undefined) return notFound("avatar");
      if (method === "GET") return json(200, { id: m[1], mime: "image/webp", size: image.length, data: image });
      if (method === "DELETE") {
        this.avatars.delete(m[1]);
        for (const a of s.agents) {
          if (a.profile.avatar_image !== m[1]) continue;
          a.profile = { ...a.profile, avatar_image: null };
          this.emit({ type: "agent.updated", data: { agent: { ...a } } });
        }
        return { status: 204, body: "" };
      }
    }
    if (method === "GET" && /^\/v1\/agents\/[^/]+\/files\/image$/.test(p)) {
      const imagePath = url.searchParams.get("path") ?? "";
      if (imagePath.startsWith("/outside/")) return json(403, { error: { code: "forbidden", message: "不在工作目录或临时目录下" } });
      return json(200, { path: imagePath, mime: "image/png", size: 29699, data: "iVBORw0KGgoAAAANSUhEUgAAAKAAAABaCAIAAACwpMoFAABzyklEQVR4nATBg6IUAQBA0T4n27axY2tnZ8d62bbtutm2Xb/VOYMGdxjSYWiHYR2GdxjRYWSHUR1GdxjTYWyHcR3Gd5jQYWKHSR0md5jSYWqHaR2md5jRYWaHWR1md5jTYW6HeR3md1jQYWGHRR0Wd1jSYWmHZR2Wd1jRYWWHVR06HYQOYgepg9xB6aB20DroHYwOZgerg93B6eB28Dr4HYIO3Q5hh16HqEO/Q9wh6ZB2yDrkHYoOZYeqQ92h6dB2GOiwusOaDms7rOuwvsOGDhs7bOqwucOWDls7bOuwvcOODjs77Oqwu8OeDns77Ouwv8OBDgc7HOpwuMORDkc7HOtwvMOJDic7nOpwusOZDmc7nOtwvsOFDhc7XOpwucOVDlc7XOtwvQMdbnS42eFWh9sd7nS42+Feh/sdHnR42OFRh8cdnnR42uFZh+cdXnR42eFVh9cd3nR42+Fdh/cdPnT42OFTh88dvnT42uFbh+8dfnT42eFXh98d/nT42+Ffh0GDBYYIDBUYJjBcYITASIFRAqMFxgiMFRgnMF5ggsBEgUkCkwWmCEwVmCYwXWCGwEyBWQKzBeYIzBWYJzBfYIHAQoFFAosFlggsFVgmsFxghcBKgVUCHQFBQBSQBGQBRUAV0AR0AUPAFLAEbAFHwBXwBHyBQKArEAr0BCKBvkAskAikAplALlAIlAKVQC3QCLQCAwKrBdYIrBVYJ7BeYIPARoFNApsFtghsFdgmsF1gh8BOgV0CuwX2COwV2CewX+CAwEGBQwKHBY4IHBU4JnBc4ITASYFTAqcFzgicFTgncF7ggsBFgUsClwWuCFwVuCZwXQCBGwI3BW4J3Ba4I3BX4J7AfYEHAg8FHgk8Fngi8FTgmcBzgRcCLwVeCbwWeCPwVuCdwHuBDwIfBT4JfBb4IvBV4JvAd4EfAj8Ffgn8Fvgj8Ffgn8CgwSJDRIaKDBMZLjJCZKTIKJHRImNExoqMExkvMkFkosgkkckiU0SmikwTmS4yQ2SmyCyR2SJzROaKzBOZL7JAZKHIIpHFIktEloosE1kuskJkpcgqkY6IICKKSCKyiCKiimgiuoghYopYIraII+KKeCK+SCDSFQlFeiKRSF8kFklEUpFMJBcpREqRSqQWaURakQGR1SJrRNaKrBNZL7JBZKPIJpHNIltEtopsE9kuskNkp8gukd0ie0T2iuwT2S9yQOSgyCGRwyJHRI6KHBM5LnJC5KTIKZHTImdEzoqcEzkvckHkosglkcsiV0SuilwTuS6CyA2RmyK3RG6L3BG5K3JP5L7IA5GHIo9EHos8EXkq8kzkucgLkZcir0Rei7wReSvyTuS9yAeRjyKfRD6LfBH5KvJN5LvID5GfIr9Efov8Efkr8k9k0GCJIRJDJYZJDJcYITFSYpTEaIkxEmMlxkmMl5ggMVFiksRkiSkSUyWmSUyXmCExU2KWxGyJORJzJeZJzJdYILFQYpHEYoklEksllkksl1ghsVJilURHQpAQJSQJWUKRUCU0CV3CkDAlLAlbwpFwJTwJXyKQ6EqEEj2JSKIvEUskEqlEJpFLFBKlRCVRSzQSrcSAxGqJNRJrJdZJrJfYILFRYpPEZoktElsltklsl9ghsVNil8RuiT0SeyX2SeyXOCBxUOKQxGGJIxJHJY5JHJc4IXFS4pTEaYkzEmclzkmcl7ggcVHiksRliSsSVyWuSVyXQOKGxE2JWxK3Je5I3JW4J3Ff4oHEQ4lHEo8lnkg8lXgm8VzihcRLiVcSryXeSLyVeCfxXuKDxEeJTxKfJb5IfJX4JvFd4ofET4lfEr8l/kj8lfgnMWiwzBCZoTLDZIbLjJAZKTNKZrTMGJmxMuNkxstMkJkoM0lmsswUmaky02Smy8yQmSkzS2a2zByZuTLzZObLLJBZKLNIZrHMEpmlMstklsuskFkps0qmIyPIiDKSjCyjyKgymowuY8iYMpaMLePIuDKejC8TyHRlQpmeTCTTl4llEplUJpPJZQqZUqaSqWUamVZmQGa1zBqZtTLrZNbLbJDZKLNJZrPMFpmtMttktsvskNkps0tmt8wemb0y+2T2yxyQOShzSOawzBGZozLHZI7LnJA5KXNK5rTMGZmzMudkzstckLkoc0nmsswVmasy12SuyyBzQ+amzC2Z2zJ3ZO7K3JO5L/NA5qHMI5nHMk9knso8k3ku80Lmpcwrmdcyb2TeyryTeS/zQeajzCeZzzJfZL7KfJP5LvND5qfML5nfMn9k/sr8kxk0WGGIwlCFYQrDFUYojFQYpTBaYYzCWIVxCuMVJihMVJikMFlhisJUhWkK0xVmKMxUmKUwW2GOwlyFeQrzFRYoLFRYpLBYYYnCUoVlCssVViisVFil0FEQFEQFSUFWUBRUBU1BVzAUTAVLwVZwFFwFT8FXCBS6CqFCTyFS6CvEColCqpAp5AqFQqlQKdQKjUKrMKCwWmGNwlqFdQrrFTYobFTYpLBZYYvCVoVtCtsVdijsVNilsFthj8JehX0K+xUOKBxUOKRwWOGIwlGFYwrHFU4onFQ4pXBa4YzCWYVzCucVLihcVLikcFnhisJVhWsK1xVQuKFwU+GWwm2FOwp3Fe4p3Fd4oPBQ4ZHCY4UnCk8Vnik8V3ih8FLhlcJrhTcKbxXeKbxX+KDwUeGTwmeFLwpfFb4pfFf4ofBT4ZfCb4U/Cn8V/ikMGqwyRGWoyjCV4SojVEaqjFIZrTJGZazKOJXxKhNUJqpMUpmsMkVlqso0lekqM1RmqsxSma0yR2WuyjyV+SoLVBaqLFJZrLJEZanKMpXlKitUVqqsUumoCCqiiqQiqygqqoqmoqsYKqaKpWKrOCquiqfiqwQqXZVQpacSqfRVYpVEJVXJVHKVQqVUqVRqlUalVRlQWa2yRmWtyjqV9SobVDaqbFLZrLJFZavKNpXtKjtUdqrsUtmtskdlr8o+lf0qB1QOqhxSOaxyROWoyjGV4yonVE6qnFI5rXJG5azKOZXzKhdULqpcUrmsckXlqso1lesqqNxQualyS+W2yh2Vuyr3VO6rPFB5qPJI5bHKE5WnKs9Unqu8UHmp8krltcoblbcq71Teq3xQ+ajySeWzyheVryrfVL6r/FD5qfJL5bfKH5W/Kv9UBg3WGKIxVGOYxnCNERojNUZpjNYYozFWY5zGeI0JGhM1JmlM1piiMVVjmsZ0jRkaMzVmaczWmKMxV2OexnyNBRoLNRZpLNZYorFUY5nGco0VGis1Vml0NAQNUUPSkDUUDVVD09A1DA1Tw9KwNRwNV8PT8DUCja5GqNHTiDT6GrFGopFqZBq5RqFRalQatUaj0WoMaKzWWKOxVmOdxnqNDRobNTZpbNbYorFVY5vGdo0dGjs1dmns1tijsVdjn8Z+jQMaBzUOaRzWOKJxVOOYxnGNExonNU5pnNY4o3FW45zGeY0LGhc1Lmlc1riicVXjmsZ1DTRuaNzUuKVxW+OOxl2Nexr3NR5oPNR4pPFY44nGU41nGs81Xmi81Hil8VrjjcZbjXca7zU+aHzU+KTxWeOLxleNbxrfNX5o/NT4pfFb44/GX41/GoMG6wzRGaozTGe4zgidkTqjdEbrjNEZqzNOZ7zOBJ2JOpN0JutM0ZmqM01nus4MnZk6s3Rm68zRmaszT2e+zgKdhTqLdBbrLNFZqrNMZ7nOCp2VOqt0OjqCjqgj6cg6io6qo+noOoaOqWPp2DqOjqvj6fg6gU5XJ9Tp6UQ6fZ1YJ9FJdTKdXKfQKXUqnVqn0Wl1BnRW66zRWauzTme9zgadjTqbdDbrbNHZqrNNZ7vODp2dOrt0duvs0dmrs09nv84BnYM6h3QO6xzROapzTOe4zgmdkzqndE7rnNE5q3NO57zOBZ2LOpd0Lutc0bmqc03nug46N3Ru6tzSua1zR+euzj2d+zoPdB7qPNJ5rPNE56nOM53nOi90Xuq80nmt80bnrc47nfc6H3Q+6nzS+azzReerzjed7zo/dH7q/NL5rfNH56/OP51Bgw2GGAw1GGYw3GCEwUiDUQajDcYYjDUYZzDeYILBRINJBpMNphhMNZhmMN1ghsFMg1kGsw3mGMw1mGcw32CBwUKDRQaLDZYYLDVYZrDcYIXBSoNVBh0DwUA0kAxkA8VANdAMdAPDwDSwDGwDx8A18Ax8g8CgaxAa9Awig75BbJAYpAaZQW5QGJQGlUFt0Bi0BgMGqw3WGKw1WGew3mCDwUaDTQabDbYYbDXYZrDdYIfBToNdBrsN9hjsNdhnsN/ggMFBg0MGhw2OGBw1OGZw3OCEwUmDUwanDc4YnDU4Z3De4ILBRYNLBpcNrhhcNbhmcN0AgxsGNw1uGdw2uGNw1+CewX2DBwYPDR4ZPDZ4YvDU4JnBc4MXBi8NXhm8Nnhj8NbgncF7gw8GHw0+GXw2+GLw1eCbwXeDHwY/DX4Z/Db4Y/DX4J/BoMEmQ0yGmgwzGW4ywmSkySiT0SZjTMaajDMZbzLBZKLJJJPJJlNMpppMM5luMsNkpsksk9kmc0zmmswzmW+ywGShySKTxSZLTJaaLDNZbrLCZKXJKpOOiWAimkgmsolioppoJrqJYWKaWCa2iWPimngmvklg0jUJTXomkUnfJDZJTFKTzCQ3KUxKk8qkNmlMWpMBk9Uma0zWmqwzWW+ywWSjySaTzSZbTLaabDPZbrLDZKfJLpPdJntM9prsM9lvcsDkoMkhk8MmR0yOmhwzOW5ywuSkySmT0yZnTM6anDM5b3LB5KLJJZPLJldMrppcM7lugskNk5smt0xum9wxuWtyz+S+yQOThyaPTB6bPDF5avLM5LnJC5OXJq9MXpu8MXlr8s7kvckHk48mn0w+m3wx+WryzeS7yQ+Tnya/TH6b/DH5a/LPZNBgiyEWQy2GWQy3GGEx0mKUxWiLMRZjLcZZjLeYYDHRYpLFZIspFlMtpllMt5hhMdNilsVsizkWcy3mWcy3WGCx0GKRxWKLJRZLLZZZLLdYYbHSYpVFx0KwEC0kC9lCsVAtNAvdwrAwLSwL28KxcC08C98isOhahBY9i8iibxFbJBapRWaRWxQWpUVlUVs0Fq3FgMVqizUWay3WWay32GCx0WKTxWaLLRZbLbZZbLfYYbHTYpfFbos9Fnst9lnstzhgcdDikMVhiyMWRy2OWRy3OGFx0uKUxWmLMxZnLc5ZnLe4YHHR4pLFZYsrFlctrllct8DihsVNi1sWty3uWNy1uGdx3+KBxUOLRxaPLZ5YPLV4ZvHc4oXFS4tXFq8t3li8tXhn8d7ig8VHi08Wny2+WHy1+Gbx3eKHxU+LXxa/Lf5Y/LX4ZzFosM0Qm6E2w2yG24ywGWkzyma0zRibsTbjbMbbTLCZaDPJZrLNFJupNtNsptvMsJlpM8tmts0cm7k282zm2yywWWizyGaxzRKbpTbLbJbbrLBZabPKpmMj2Ig2ko1so9ioNpqNbmPYmDaWjW3j2Lg2no1vE9h0bUKbnk1k07eJbRKb1CazyW0Km9KmsqltGpvWZsBmtc0am7U262zW22yw2WizyWazzRabrTbbbLbb7LDZabPLZrfNHpu9Nvts9tscsDloc8jmsM0Rm6M2x2yO25ywOWlzyua0zRmbszbnbM7bXLC5aHPJ5rLNFZurNtdsrttgc8Pmps0tm9s2d2zu2tyzuW/zwOahzSObxzZPbJ7aPLN5bvPC5qXNK5vXNm9s3tq8s3lv88Hmo80nm882X2y+2nyz+W7zw+anzS+b3zZ/bP7a/LMZNNhhiMNQh2EOwx1GOIx0GOUw2mGMw1iHcQ7jHSY4THSY5DDZYYrDVIdpDtMdZjjMdJjlMNthjsNch3kO8x0WOCx0WOSw2GGJw1KHZQ7LHVY4rHRY5dBxEBxEB8lBdlAcVAfNQXcwHEwHy8F2cBxcB8/Bdwgcug6hQ88hcug7xA6JQ+qQOeQOhUPpUDnUDo1D6zDgsNphjcNah3UO6x02OGx02OSw2WGLw1aHbQ7bHXY47HTY5bDbYY/DXod9DvsdDjgcdDjkcNjhiMNRh2MOxx1OOJx0OOVw2uGMw1mHcw7nHS44XHS45HDZ4YrDVYdrDtcdcLjhcNPhlsNthzsOdx3uOdx3eODw0OGRw2OHJw5PHZ45PHd44fDS4ZXDa4c3Dm8d3jm8d/jg8NHhk8Nnhy8OXx2+OXx3+OHw0+GXw2+HPw5/Hf45DBrsMsRlqMswl+EuI1xGuoxyGe0yxmWsyziX8S4TXCa6THKZ7DLFZarLNJfpLjNcZrrMcpntMsdlrss8l/kuC1wWuixyWeyyxGWpyzKX5S4rXFa6rHLpuAguoovkIrsoLqqL5qK7GC6mi+Viuzgurovn4rsELl2X0KXnErn0XWKXxCV1yVxyl8KldKlcapfGpXUZcFntssZlrcs6l/UuG1w2umxy2eyyxWWryzaX7S47XHa67HLZ7bLHZa/LPpf9LgdcDroccjnscsTlqMsxl+MuJ1xOupxyOe1yxuWsyzmX8y4XXC66XHK57HLF5arLNZfrLrjccLnpcsvltssdl7su91zuuzxweejyyOWxyxOXpy7PXJ67vHB56fLK5bXLG5e3Lu9c3rt8cPno8snls8sXl68u31y+u/xw+enyy+W3yx+Xvy7/XAYN9hjiMdRjmMdwjxEeIz1GeYz2GOMx1mOcx3iPCR4TPSZ5TPaY4jHVY5rHdI8ZHjM9ZnnM9pjjMddjnsd8jwUeCz0WeSz2WOKx1GOZx3KPFR4rPVZ5dDwED9FD8pA9FA/VQ/PQPQwP08PysD0cD9fD8/A9Ao+uR+jR84g8+h6xR+KRemQeuUfhUXpUHrVH49F6DHis9ljjsdZjncd6jw0eGz02eWz22OKx1WObx3aPHR47PXZ57PbY47HXY5/Hfo8DHgc9Dnkc9jjicdTjmMdxjxMeJz1OeZz2OONx1uOcx3mPCx4XPS55XPa44nHV45rHdQ88bnjc9Ljlcdvjjsddj3se9z0eeDz0eOTx2OOJx1OPZx7PPV54vPR45fHa443HW493Hu89Pnh89Pjk8dnji8dXj28e3z1+ePz0+OXx2+OPx1+Pfx6DBvsM8RnqM8xnuM8In5E+o3xG+4zxGeszzme8zwSfiT6TfCb7TPGZ6jPNZ7rPDJ+ZPrN8ZvvM8ZnrM89nvs8Cn4U+i3wW+yzxWeqzzGe5zwqflT6rfDo+go/oI/nIPoqP6qP56D6Gj+lj+dg+jo/r4/n4PoFP1yf06flEPn2f2CfxSX0yn9yn8Cl9Kp/ap/FpfQZ8Vvus8Vnrs85nvc8Gn40+m3w2+2zx2eqzzWe7zw6fnT67fHb77PHZ67PPZ7/PAZ+DPod8Dvsc8Tnqc8znuM8Jn5M+p3xO+5zxOetzzue8zwWfiz6XfC77XPG56nPN57oPPjd8bvrc8rntc8fnrs89n/s+D3we+jzyeezzxOepzzOf5z4vfF76vPJ57fPG563PO5/3Ph98Pvp88vns88Xnq883n+8+P3x++vzy+e3zx+evzz+fQYMDhgQMDRgWMDxgRMDIgFEBowPGBIwNGBcwPmBCwMSASQGTA6YETA2YFjA9YEbAzIBZAbMD5gTMDZgXMD9gQcDCgEUBiwOWBCwNWBawPGBFwMqAVQGdACFADJAC5AAlQA3QAvQAI8AMsALsACfADfAC/IAgoBsQBvQCooB+QByQBKQBWUAeUASUAVVAHdAEtAEDAasD1gSsDVgXsD5gQ8DGgE0BmwO2BGwN2BawPWBHwM6AXQG7A/YE7A3YF7A/4EDAwYBDAYcDjgQcDTgWcDzgRMDJgFMBpwPOBJwNOBdwPuBCwMWASwGXA64EXA24FnA9gIAbATcDbgXcDrgTcDfgXsD9gAcBDwMeBTwOeBLwNOBZwPOAFwEvA14FvA54E/A24F3A+4APAR8DPgV8DvgS8DXgW8D3gB8BPwN+BfwO+BPwN+BfwKDBXYZ0GdplWJfhXUZ0GdllVJfRXcZ0GdtlXJfxXSZ0mdhlUpfJXaZ0mdplWpfpXWZ0mdllVpfZXeZ0mdtlXpf5XRZ0WdhlUZfFXZZ0WdplWZflXVZ0WdllVZdOF6GL2EXqIndRuqhdtC56F6OL2cXqYndxurhdvC5+l6BLt0vYpdcl6tLvEndJuqRdsi55l6JL2aXqUndpurRdBrqs7rKmy9ou67qs77Khy8Yum7ps7rKly9Yu27ps77Kjy84uu7rs7rKny94u+7rs73Kgy8Euh7oc7nKky9Eux7oc73Kiy8kup7qc7nKmy9ku57qc73Khy8Uul7pc7nKly9Uu17pc70KXG11udrnV5XaXO13udrnX5X6XB10ednnU5XGXJ12ednnW5XmXF11ednnV5XWXN13ednnX5X2XD10+dvnU5XOXL12+dvnW5XuXH11+dvnV5XeXP13+dvnXZdDgkCEhQ0OGhQwPGREyMmRUyOiQMSFjQ8aFjA+ZEDIxZFLI5JApIVNDpoVMD5kRMjNkVsjskDkhc0PmhcwPWRCyMGRRyOKQJSFLQ5aFLA9ZEbIyZFVIJ0QIEUOkEDlECVFDtBA9xAgxQ6wQO8QJcUO8ED8kCOmGhCG9kCikHxKHJCFpSBaShxQhZUgVUoc0IW3IQMjqkDUha0PWhawP2RCyMWRTyOaQLSFbQ7aFbA/ZEbIzZFfI7pA9IXtD9oXsDzkQcjDkUMjhkCMhR0OOhRwPORFyMuRUyOmQMyFnQ86FnA+5EHIx5FLI5ZArIVdDroVcDyHkRsjNkFsht0PuhNwNuRdyP+RByMOQRyGPQ56EPA15FvI85EXIy5BXIa9D3oS8DXkX8j7kQ8jHkE8hn0O+hHwN+RbyPeRHyM+QXyG/Q/6E/A35FzJocI8hPYb2GNZjeI8RPUb2GNVjdI8xPcb2GNdjfI8JPSb2mNRjco8pPab2mNZjeo8ZPWb2mNVjdo85Peb2mNdjfo8FPRb2WNRjcY8lPZb2WNZjeY8VPVb2WNWj00PoIfaQesg9lB5qD62H3sPoYfawetg9nB5uD6+H3yPo0e0R9uj1iHr0e8Q9kh5pj6xH3qPoUfaoetQ9mh5tj4Eeq3us6bG2x7oe63ts6LGxx6Yem3ts6bG1x7Ye23vs6LGzx64eu3vs6bG3x74e+3sc6HGwx6Eeh3sc6XG0x7Eex3uc6HGyx6kep3uc6XG2x7ke53tc6HGxx6Uel3tc6XG1x7Ue13vQ40aPmz1u9bjd406Puz3u9bjf40GPhz0e9Xjc40mPpz2e9Xje40WPlz1e9Xjd402Ptz3e9Xjf40OPjz0+9fjc40uPrz2+9fje40ePnz1+9fjd40+Pvz3+9Rg0OGJIxNCIYRHDI0ZEjIwYFTE6YkzE2IhxEeMjJkRMjJgUMTliSsTUiGkR0yNmRMyMmBUxO2JOxNyIeRHzIxZELIxYFLE4YknE0ohlEcsjVkSsjFgV0YkQIsQIKUKOUCLUCC1CjzAizAgrwo5wItwIL8KPCCK6EWFELyKK6EfEEUlEGpFF5BFFRBlRRdQRTUQbMRCxOmJNxNqIdRHrIzZEbIzYFLE5YkvE1ohtEdsjdkTsjNgVsTtiT8TeiH0R+yMORByMOBRxOOJIxNGIYxHHI05EnIw4FXE64kzE2YhzEecjLkRcjLgUcTniSsTViGsR1yOIuBFxM+JWxO2IOxF3I+5F3I94EPEw4lHE44gnEU8jnkU8j3gR8TLiVcTriDcRbyPeRbyP+BDxMeJTxOeILxFfI75FfI/4EfEz4lfE74g/EX8j/kUMGtxnSJ+hfYb1Gd5nRJ+RfUb1Gd1nTJ+xfcb1Gd9nQp+JfSb1mdxnSp+pfab1md5nRp+ZfWb1md1nTp+5feb1md9nQZ+FfRb1WdxnSZ+lfZb1Wd5nRZ+VfVb16fQR+oh9pD5yH6WP2kfro/cx+ph9rD52H6eP28fr4/cJ+nT7hH16faI+/T5xn6RP2ifrk/cp+pR9qj51n6ZP22egz+o+a/qs7bOuz/o+G/ps7LOpz+Y+W/ps7bOtz/Y+O/rs7LOrz+4+e/rs7bOvz/4+B/oc7HOoz+E+R/oc7XOsz/E+J/qc7HOqz+k+Z/qc7XOuz/k+F/pc7HOpz+U+V/pc7XOtz/U+9LnR52afW31u97nT526fe33u93nQ52GfR30e93nS52mfZ32e93nR52WfV31e93nT522fd33e9/nQ52OfT30+9/nS52ufb32+9/nR52efX31+9/nT52+ff30GDY4ZEjM0ZljM8JgRMSNjRsWMjhkTMzZmXMz4mAkxE2MmxUyOmRIzNWZazPSYGTEzY2bFzI6ZEzM3Zl7M/JgFMQtjFsUsjlkSszRmWczymBUxK2NWxXRihBgxRoqRY5QYNUaL0WOMGDPGirFjnBg3xovxY4KYbkwY04uJYvoxcUwSk8ZkMXlMEVPGVDF1TBPTxgzErI5ZE7M2Zl3M+pgNMRtjNsVsjtkSszVmW8z2mB0xO2N2xeyO2ROzN2ZfzP6YAzEHYw7FHI45EnM05ljM8ZgTMSdjTsWcjjkTczbmXMz5mAsxF2MuxVyOuRJzNeZazPUYYm7E3Iy5FXM75k7M3Zh7MfdjHsQ8jHkU8zjmSczTmGcxz2NexLyMeRXzOuZNzNuYdzHvYz7EfIz5FPM55kvM15hvMd9jfsT8jPkV8zvmT8zfmH8xgwYnDEkYmjAsYXjCiISRCaMSRieMSRibMC5hfMKEhIkJkxImJ0xJmJowLWF6woyEmQmzEmYnzEmYmzAvYX7CgoSFCYsSFicsSViasCxhecKKhJUJqxI6CUKCmCAlyAlKgpqgJegJRoKZYCXYCU6Cm+Al+AlBQjchTOglRAn9hDghSUgTsoQ8oUgoE6qEOqFJaBMGElYnrElYm7AuYX3ChoSNCZsSNidsSdiasC1he8KOhJ0JuxJ2J+xJ2JuwL2F/woGEgwmHEg4nHEk4mnAs4XjCiYSTCacSTiecSTibcC7hfMKFhIsJlxIuJ1xJuJpwLeF6Agk3Em4m3Eq4nXAn4W7CvYT7CQ8SHiY8Snic8CThacKzhOcJLxJeJrxKeJ3wJuFtwruE9wkfEj4mfEr4nPAl4WvCt4TvCT8Sfib8Svid8Cfhb8K/hEGDU4akDE0ZljI8ZUTKyJRRKaNTxqSMTRmXMj5lQsrElEkpk1OmpExNmZYyPWVGysyUWSmzU+akzE2ZlzI/ZUHKwpRFKYtTlqQsTVmWsjxlRcrKlFUpnRQhRUyRUuQUJUVN0VL0FCPFTLFS7BQnxU3xUvyUIKWbEqb0UqKUfkqckqSkKVlKnlKklClVSp3SpLQpAymrU9akrE1Zl7I+ZUPKxpRNKZtTtqRsTdmWsj1lR8rOlF0pu1P2pOxN2ZeyP+VAysGUQymHU46kHE05lnI85UTKyZRTKadTzqScTTmXcj7lQsrFlEspl1OupFxNuZZyPYWUGyk3U26l3E65k3I35V7K/ZQHKQ9THqU8TnmS8jTlWcrzlBcpL1NepbxOeZPyNuVdyvuUDykfUz6lfE75kvI15VvK95QfKT9TfqX8TvmT8jflX8qgwRlDMoZmDMsYnjEiY2TGqIzRGWMyxmaMyxifMSFjYsakjMkZUzKmZkzLmJ4xI2NmxqyM2RlzMuZmzMuYn7EgY2HGoozFGUsylmYsy1iesSJjZcaqjE6GkCFmSBlyhpKhZmgZeoaRYWZYGXaGk+FmeBl+RpDRzQgzehlRRj8jzkgy0owsI88oMsqMKqPOaDLajIGM1RlrMtZmrMtYn7EhY2PGpozNGVsytmZsy9iesSNjZ8aujN0ZezL2ZuzL2J9xIONgxqGMwxlHMo5mHMs4nnEi42TGqYzTGWcyzmacyzifcSHjYsaljMsZVzKuZlzLuJ5Bxo2Mmxm3Mm5n3Mm4m3Ev437Gg4yHGY8yHmc8yXia8SzjecaLjJcZrzJeZ7zJeJvxLuN9xoeMjxmfMj5nfMn4mvEt43vGj4yfGb8yfmf8yfib8S9j0OCcITlDc4blDM8ZkTMyZ1TO6JwxOWNzxuWMz5mQMzFnUs7knCk5U3Om5UzPmZEzM2dWzuycOTlzc+blzM9ZkLMwZ1HO4pwlOUtzluUsz1mRszJnVU4nR8gRc6QcOUfJUXO0HD3HyDFzrBw7x8lxc7wcPyfI6eaEOb2cKKefE+ckOWlOlpPnFDllTpVT5zQ5bc5AzuqcNTlrc9blrM/ZkLMxZ1PO5pwtOVtztuVsz9mRszNnV87unD05e3P25ezPOZBzMOdQzuGcIzlHc47lHM85kXMy51TO6ZwzOWdzzuWcz7mQczHnUs7lnCs5V3Ou5VzPIedGzs2cWzm3c+7k3M25l3M/50HOw5xHOY9znuQ8zXmW8zznRc7LnFc5r3Pe5LzNeZfzPudDzsecTzmfc77kfM35lvM950fOz5xfOb9z/uT8zfmXM2hwwZCCoQXDCoYXjCgYWTCqYHTBmIKxBeMKxhdMKJhYMKlgcsGUgqkF0wqmF8womFkwq2B2wZyCuQXzCuYXLChYWLCoYHHBkoKlBcsKlhesKFhZsKqgUyAUiAVSgVygFKgFWoFeYBSYBVaBXeAUuAVegV8QFHQLwoJeQVTQL4gLkoK0ICvIC4qCsqAqqAuagrZgoGB1wZqCtQXrCtYXbCjYWLCpYHPBloKtBdsKthfsKNhZsKtgd8Gegr0F+wr2FxwoOFhwqOBwwZGCowXHCo4XnCg4WXCq4HTBmYKzBecKzhdcKLhYcKngcsGVgqsF1wquF1Bwo+Bmwa2C2wV3Cu4W3Cu4X/Cg4GHBo4LHBU8KnhY8K3he8KLgZcGrgtcFbwreFrwreF/woeBjwaeCzwVfCr4WfCv4XvCj4GfBr4LfBX8K/hb8Kxg0uGRIydCSYSXDS0aUjCwZVTK6ZEzJ2JJxJeNLJpRMLJlUMrlkSsnUkmkl00tmlMwsmVUyu2ROydySeSXzSxaULCxZVLK4ZEnJ0pJlJctLVpSsLFlV0ikRSsQSqUQuUUrUEq1ELzFKzBKrxC5xStwSr8QvCUq6JWFJryQq6ZfEJUlJWpKV5CVFSVlSldQlTUlbMlCyumRNydqSdSXrSzaUbCzZVLK5ZEvJ1pJtJdtLdpTsLNlVsrtkT8nekn0l+0sOlBwsOVRyuORIydGSYyXHS06UnCw5VXK65EzJ2ZJzJedLLpRcLLlUcrnkSsnVkmsl10souVFys+RWye2SOyV3S+6V3C95UPKw5FHJ45InJU9LnpU8L3lR8rLkVcnrkjclb0velbwv+VDyseRTyeeSLyVfS76VfC/5UfKz5FfJ75I/JX9L/pUMGlwxpGJoxbCK4RUjKkZWjKoYXTGmYmzFuIrxFRMqJlZMqphcMaViasW0iukVMypmVsyqmF0xp2JuxbyK+RULKhZWLKpYXLGkYmnFsorlFSsqVlasquhUCBVihVQhVygVaoVWoVcYFWaFVWFXOBVuhVfhVwQV3YqwolcRVfQr4oqkIq3IKvKKoqKsqCrqiqairRioWF2xpmJtxbqK9RUbKjZWbKrYXLGlYmvFtortFTsqdlbsqthdsadib8W+iv0VByoOVhyqOFxxpOJoxbGK4xUnKk5WnKo4XXGm4mzFuYrzFRcqLlZcqrhccaXiasW1iusVVNyouFlxq+J2xZ2KuxX3Ku5XPKh4WPGo4nHFk4qnFc8qnle8qHhZ8aridcWbircV7yreV3yo+FjxqeJzxZeKrxXfKr5X/Kj4WfGr4nfFn4q/Ff8qBg2uGVIztGZYzfCaETUja0bVjK4ZUzO2ZlzN+JoJNRNrJtVMrplSM7VmWs30mhk1M2tm1cyumVMzt2ZezfyaBTULaxbVLK5ZUrO0ZlnN8poVNStrVtV0aoQasUaqkWuUGrVGq9FrjBqzxqqxa5wat8ar8WuCmm5NWNOriWr6NXFNUpPWZDV5TVFT1lQ1dU1T09YM1KyuWVOztmZdzfqaDTUbazbVbK7ZUrO1ZlvN9podNTtrdtXsrtlTs7dmX83+mgM1B2sO1RyuOVJztOZYzfGaEzUna07VnK45U3O25lzN+ZoLNRdrLtVcrrlSc7XmWs31Gmpu1NysuVVzu+ZOzd2aezX3ax7UPKx5VPO45knN05pnNc9rXtS8rHlV87rmTc3bmnc172s+1Hys+VTzueZLzdeabzXfa37U/Kz5VfO75k/N35p/NYMGNwxpGNowrGF4w4iGkQ2jGkY3jGkY2zCuYXzDhIaJDZMaJjdMaZjaMK1hesOMhpkNsxpmN8xpmNswr2F+w4KGhQ2LGhY3LGlY2rCsYXnDioaVDasaOg1Cg9ggNcgNSoPaoDXoDUaD2WA12A1Og9vgNfgNQUO3IWzoNUQN/Ya4IWlIG7KGvKFoKBuqhrqhaWgbBhpWN6xpWNuwrmF9w4aGjQ2bGjY3bGnY2rCtYXvDjoadDbsadjfsadjbsK9hf8OBhoMNhxoONxxpONpwrOF4w4mGkw2nGk43nGk423Cu4XzDhYaLDZcaLjdcabjacK3hegMNNxpuNtxquN1wp+Fuw72G+w0PGh42PGp43PCk4WnDs4bnDS8aXja8anjd8KbhbcO7hvcNHxo+Nnxq+NzwpeFrw7eG7w0/Gn42/Gr43fCn4W/Dv4ZBg1uGtAxtGdYyvGVEy8iWUS2jW8a0jG0Z1zK+ZULLxJZJLZNbprRMbZnWMr1lRsvMllkts1vmtMxtmdcyv2VBy8KWRS2LW5a0LG1Z1rK8ZUXLypZVLZ0WoUVskVrkFqVFbdFa9BajxWyxWuwWp8Vt8Vr8lqCl2xK29Fqiln5L3JK0pC1ZS95StJQtVUvd0rS0LQMtq1vWtKxtWdeyvmVDy8aWTS2bW7a0bG3Z1rK9ZUfLzpZdLbtb9rTsbdnXsr/lQMvBlkMth1uOtBxtOdZyvOVEy8mWUy2nW860nG0513K+5ULLxZZLLZdbrrRcbbnWcr2FlhstN1tutdxuudNyt+Vey/2WBy0PWx61PG550vK05VnL85YXLS9bXrW8bnnT8rblXcv7lg8tH1s+tXxu+dLyteVby/eWHy0/W361/G750/K35V/LoMEDDBlg6ADDBhg+wIgBRg4waoDRA4z5TxA8IMoNAAAU7HFq27Zt293Yye7GTn6tV9u222t1psPYDuM6jO8wocPEDpM6TO4wpcPUDtM6TO8wo8PMDrM6zO4wp8PcDvM6zO+woMPCDos6LO6wpMPSDss6LO+wosPKDqs6rO6wpsPaDus6rO+wocPGDps6bO6wpcPWDts6bO+wo8PODrs67O6wp8PeDvs67O9woMPBDoc6HO5wpMPRDsc6HO9wosPJDqc6nO5wpkOng9BB7CB1kDsoHdQOWge9g9HB7GB1sDs4HdwOXoduh16Hfge/Q9Ah7BB1iDskHdIOWYe8Q9Gh7FB1qDs0HdoOAx3OdjjX4XyHCx0udrjU4XKHKx2udrjW4XoHOtzocLPDrQ63O9zpcLfDvQ73Ozzo8LDDow6POzzp8LTDsw7PO7zo8LLDqw6vO7zp8LbDuw7vO3zo8LHDpw6fO3zp8LXDtw7fO/zo8LPDrw6/O/zp8LfDvw6DBgsMERgqMExguMAIgZECowRGC4wRGCswTmC8wASBiQKTBCYLTBGYKjBNYLrADIGZArMEZgvMEZgrME9gvsACgYUCiwQWCywRWCqwTGC5wAqBlQKrBFYLrBFYK7BOYL3ABoGNApsENgtsEdgqsE1gu8AOgZ0CuwR2C+wR2CuwT2C/wAGBgwKHBA4LHBE4KnBM4LjACYGTAqcETgucEegICAKigCQgCygCqoAmoAsYAqaAJWALOAKugCfQFegJ9AV8gUAgFIgEYoFEIBXIBHKBQqAUqARqgUagFRgQOCtwTuC8wAWBiwKXBC4LXBG4KnBN4LoAAjcEbgrcErgtcEfgrsA9gfsCDwQeCjwSeCzwROCpwDOB5wIvBF4KvBJ4LfBG4K3AO4H3Ah8EPgp8Evgs8EXgq8A3ge8CPwR+CvwS+C3wR+CvwD+BQYNFhogMFRkmMlxkhMhIkVEio0XGiIwVGScyXmSCyESRSSKTRaaITBWZJjJdZIbITJFZIrNF5ojMFZknMl9kgchCkUUii0WWiCwVWSayXGSFyEqRVSKrRdaIrBVZJ7JeZIPIRpFNIptFtohsFdkmsl1kh8hOkV0iu0X2iOwV2SeyX+SAyEGRQyKHRY6IHBU5JnJc5ITISZFTIqdFzoh0RAQRUUQSkUUUEVVEE9FFDBFTxBKxRRwRV8QT6Yr0RPoivkggEopEIrFIIpKKZCK5SCFSilQitUgj0ooMiJwVOSdyXuSCyEWRSyKXRa6IXBW5JnJdBJEbIjdFboncFrkjclfknsh9kQciD0UeiTwWeSLyVOSZyHORFyIvRV6JvBZ5I/JW5J3Ie5EPIh9FPol8Fvki8lXkm8h3kR8iP0V+ifwW+SPyV+SfyKDBEkMkhkoMkxguMUJipMQoidESYyTGSoyTGC8xQWKixCSJyRJTJKZKTJOYLjFDYqbELInZEnMk5krMk5gvsUBiocQiicUSSySWSiyTWC6xQmKlxCqJ1RJrJNZKrJNYL7FBYqPEJonNElsktkpsk9gusUNip8Quid0SeyT2SuyT2C9xQOKgxCGJwxJHJI5KHJM4LnFC4qTEKYnTEmckOhKChCghScgSioQqoUnoEoaEKWFJ2BKOhCvhSXQlehJ9CV8ikAglIolYIpFIJTKJXKKQKCUqiVqikWglBiTOSpyTOC9xQeKixCWJyxJXJK5KXJO4LoHEDYmbErckbkvckbgrcU/ivsQDiYcSjyQeSzyReCrxTOK5xAuJlxKvJF5LvJF4K/FO4r3EB4mPEp8kPkt8kfgq8U3iu8QPiZ8SvyR+S/yR+CvxT2LQYJkhMkNlhskMlxkhM1JmlMxomTEyY2XGyYyXmSAzUWaSzGSZKTJTZabJTJeZITNTZpbMbJk5MnNl5snMl1kgs1BmkcximSUyS2WWySyXWSGzUmaVzGqZNTJrZdbJrJfZILNRZpPMZpktMltltslsl9khs1Nml8xumT0ye2X2yeyXOSBzUOaQzGGZIzJHZY7JHJc5IXNS5pTMaZkzMh0ZQUaUkWRkGUVGldFkdBlDxpSxZGwZR8aV8WS6Mj2ZvowvE8iEMpFMLJPIpDKZTC5TyJQylUwt08i0MgMyZ2XOyZyXuSBzUeaSzGWZKzJXZa7JXJdB5obMTZlbMrdl7sjclbknc1/mgcxDmUcyj2WeyDyVeSbzXOaFzEuZVzKvZd7IvJV5J/Ne5oPMR5lPMp9lvsh8lfkm813mh8xPmV8yv2X+yPyV+SczaLDCEIWhCsMUhiuMUBipMEphtMIYhbEK4xTGK0xQmKgwSWGywhSFqQrTFKYrzFCYqTBLYbbCHIW5CvMU5issUFiosEhhscIShaUKyxSWK6xQWKmwSmG1whqFtQrrFNYrbFDYqLBJYbPCFoWtCtsUtivsUNipsEtht8Iehb0K+xT2KxxQOKhwSOGwwhGFowrHFI4rnFA4qXBK4bTCGYWOgqAgKkgKsoKioCpoCrqCoWAqWAq2gqPgKngKXYWeQl/BVwgUQoVIIVZIFFKFTCFXKBRKhUqhVmgUWoUBhbMK5xTOK1xQuKhwSeGywhWFqwrXFK4roHBD4abCLYXbCncU7ircU7iv8EDhocIjhccKTxSeKjxTeK7wQuGlwiuF1wpvFN4qvFN4r/BB4aPCJ4XPCl8Uvip8U/iu8EPhp8Ivhd8KfxT+KvxTGDRYZYjKUJVhKsNVRqiMVBmlMlpljMpYlXEq41UmqExUmaQyWWWKylSVaSrTVWaozFSZpTJbZY7KXJV5KvNVFqgsVFmkslhlicpSlWUqy1VWqKxUWaWyWmWNylqVdSrrVTaobFTZpLJZZYvKVpVtKttVdqjsVNmlsltlj8pelX0q+1UOqBxUOaRyWOWIylGVYyrHVU6onFQ5pXJa5YxKR0VQEVUkFVlFUVFVNBVdxVAxVSwVW8VRcVU8la5KT6Wv4qsEKqFKpBKrJCqpSqaSqxQqpUqlUqs0Kq3KgMpZlXMq51UuqFxUuaRyWeWKylWVayrXVVC5oXJT5ZbKbZU7KndV7qncV3mg8lDlkcpjlScqT1WeqTxXeaHyUuWVymuVNypvVd6pvFf5oPJR5ZPKZ5UvKl9Vvql8V/mh8lPll8pvlT8qf1X+qQwarDFEY6jGMI3hGiM0RmqM0hitMUZjrMY4jfEaEzQmakzSmKwxRWOqxjSN6RozNGZqzNKYrTFHY67GPI35Ggs0Fmos0lissURjqcYyjeUaKzRWaqzSWK2xRmOtxjqN9RobNDZqbNLYrLFFY6vGNo3tGjs0dmrs0titsUdjr8Y+jf0aBzQOahzSOKxxROOoxjGN4xonNE5qnNI4rXFGo6MhaIgakoasoWioGpqGrmFomBqWhq3haLgankZXo6fR1/A1Ao1QI9KINRKNVCPTyDUKjVKj0qg1Go1WY0DjrMY5jfMaFzQualzSuKxxReOqxjWN6xpo3NC4qXFL47bGHY27Gvc07ms80Hio8UjjscYTjacazzSea7zQeKnxSuO1xhuNtxrvNN5rfND4qPFJ47PGF42vGt80vmv80Pip8Uvjt8Yfjb8a/zQGDdYZojNUZ5jOcJ0ROiN1RumM1hmjM1ZnnM54nQk6E3Um6UzWmaIzVWeaznSdGTozdWbpzNaZozNXZ57OfJ0FOgt1Fuks1lmis1Rnmc5ynRU6K3VW6azWWaOzVmedznqdDTobdTbpbNbZorNVZ5vOdp0dOjt1duns1tmjs1dnn85+nQM6B3UO6RzWOaJzVOeYznGdEzondU7pnNY5o9PREXREHUlH1lF0VB1NR9cxdEwdS8fWcXRcHU+nq9PT6ev4OoFOqBPpxDqJTqqT6eQ6hU6pU+nUOo1OqzOgc1bnnM55nQs6F3Uu6VzWuaJzVeeaznUddG7o3NS5pXNb547OXZ17Ovd1Hug81Hmk81jnic5TnWc6z3Ve6LzUeaXzWueNzluddzrvdT7ofNT5pPNZ54vOV51vOt91fuj81Pml81vnj85fnX86gwYbDDEYajDMYLjBCIORBqMMRhuMMRhrMM5gvMEEg4kGkwwmG0wxmGowzWC6wQyDmQazDGYbzDGYazDPYL7BAoOFBosMFhssMVhqsMxgucEKg5UGqwxWG6wxWGuwzmC9wQaDjQabDDYbbDHYarDNYLvBDoOdBrsMdhvsMdhrsM9gv8EBg4MGhwwOGxwxOGpwzOC4wQmDkwanDE4bnDHoGAgGooFkIBsoBqqBZqAbGAamgWVgGzgGroFn0DXoGfQNfIPAIDSIDGKDxCA1yAxyg8KgNKgMaoPGoDUYMDhrcM7gvMEFg4sGlwwuG1wxuGpwzeC6AQY3DG4a3DK4bXDH4K7BPYP7Bg8MHho8Mnhs8MTgqcEzg+cGLwxeGrwyeG3wxuCtwTuD9wYfDD4afDL4bPDF4KvBN4PvBj8Mfhr8Mvht8Mfgr8E/g0GDTYaYDDUZZjLcZITJSJNRJqNNxpiMNRlnMt5kgslEk0kmk02mmEw1mWYy3WSGyUyTWSazTeaYzDWZZzLfZIHJQpNFJotNlpgsNVlmstxkhclKk1Umq03WmKw1WWey3mSDyUaTTSabTbaYbDXZZrLdZIfJTpNdJrtN9pjsNdlnst/kgMlBk0Mmh02OmBw1OWZy3OSEyUmTUyanTc6YdEwEE9FEMpFNFBPVRDPRTQwT08QysU0cE9fEM+ma9Ez6Jr5JYBKaRCaxSWKSmmQmuUlhUppUJrVJY9KaDJicNTlnct7kgslFk0sml02umFw1uWZy3QSTGyY3TW6Z3Da5Y3LX5J7JfZMHJg9NHpk8Nnli8tTkmclzkxcmL01embw2eWPy1uSdyXuTDyYfTT6ZfDb5YvLV5JvJd5MfJj9Nfpn8Nvlj8tfkn8mgwRZDLIZaDLMYbjHCYqTFKIvRFmMsxlqMsxhvMcFiosUki8kWUyymWkyzmG4xw2KmxSyL2RZzLOZazLOYb7HAYqHFIovFFksslloss1huscJipcUqi9UWayzWWqyzWG+xwWKjxSaLzRZbLLZabLPYbrHDYqfFLovdFnss9lrss9hvccDioMUhi8MWRyyOWhyzOG5xwuKkxSmL0xZnLDoWgoVoIVnIFoqFaqFZ6BaGhWlhWdgWjoVr4Vl0LXoWfQvfIrAILSKL2CKxSC0yi9yisCgtKovaorFoLQYszlqcszhvccHiosUli8sWVyyuWlyzuG6BxQ2Lmxa3LG5b3LG4a3HP4r7FA4uHFo8sHls8sXhq8cziucULi5cWryxeW7yxeGvxzuK9xQeLjxafLD5bfLH4avHN4rvFD4ufFr8sflv8sfhr8c9i0GCbITZDbYbZDLcZYTPSZpTNaJsxNmNtxtmMt5lgM9Fmks1kmyk2U22m2Uy3mWEz02aWzWybOTZzbebZzLdZYLPQZpHNYpslNkttltkst1lhs9Jmlc1qmzU2a23W2ay32WCz0WaTzWabLTZbbbbZbLfZYbPTZpfNbps9Nntt9tnstzlgc9DmkM1hmyM2R22O2Ry3OWFz0uaUzWmbMzYdG8FGtJFsZBvFRrXRbHQbw8a0sWxsG8fGtfFsujY9m76NbxPYhDaRTWyT2KQ2mU1uU9iUNpVNbdPYtDYDNmdtztmct7lgc9Hmks1lmys2V22u2Vy3weaGzU2bWza3be7Y3LW5Z3Pf5oHNQ5tHNo9tntg8tXlm89zmhc1Lm1c2r23e2Ly1eWfz3uaDzUebTzafbb7YfLX5ZvPd5ofNT5tfNr9t/tj8tflnM2iwwxCHoQ7DHIY7jHAY6TDKYbTDGIexDuMcxjtMcJjoMMlhssMUh6kO0xymO8xwmOkwy2G2wxyHuQ7zHOY7LHBY6LDIYbHDEoelDsscljuscFjpsMphtcMah7UO6xzWO2xw2OiwyWGzwxaHrQ7bHLY77HDY6bDLYbfDHoe9Dvsc9jsccDjocMjhsMMRh6MOxxyOO5xwOOlwyuG0wxmHjoPgIDpIDrKD4qA6aA66g+FgOlgOtoPj4Dp4Dl2HnkPfwXcIHEKHyCF2SBxSh8whdygcSofKoXZoHFqHAYezDucczjtccLjocMnhssMVh6sO1xyuO+Bww+Gmwy2H2w53HO463HO47/DA4aHDI4fHDk8cnjo8c3ju8MLhpcMrh9cObxzeOrxzeO/wweGjwyeHzw5fHL46fHP47vDD4afDL4ffDn8c/jr8cxg02GWIy1CXYS7DXUa4jHQZ5TLaZYzLWJdxLuNdJrhMdJnkMtllistUl2ku011muMx0meUy22WOy1yXeS7zXRa4LHRZ5LLYZYnLUpdlLstdVrisdFnlstpljctal3Uu6102uGx02eSy2WWLy1aXbS7bXXa47HTZ5bLbZY/LXpd9LvtdDrgcdDnkctjliMtRl2Mux11OuJx0OeVy2uWMS8dFcBFdJBfZRXFRXTQX3cVwMV0sF9vFcXFdPJeuS8+l7+K7BC6hS+QSuyQuqUvmkrsULqVL5VK7NC6ty4DLWZdzLuddLrhcdLnkctnlistVl2su111wueFy0+WWy22XOy53Xe653Hd54PLQ5ZHLY5cnLk9dnrk8d3nh8tLllctrlzcub13eubx3+eDy0eWTy2eXLy5fXb65fHf54fLT5ZfLb5c/Ln9d/rkMGuwxxGOoxzCP4R4jPEZ6jPIY7THGY6zHOI/xHhM8JnpM8pjsMcVjqsc0j+keMzxmeszymO0xx2OuxzyP+R4LPBZ6LPJY7LHEY6nHMo/lHis8Vnqs8ljtscZjrcc6j/UeGzw2emzy2OyxxWOrxzaP7R47PHZ67PLY7bHHY6/HPo/9Hgc8Dnoc8jjsccTjqMcxj+MeJzxOepzyOO1xxqPjIXiIHpKH7KF4qB6ah+5heJgeloft4Xi4Hp5H16Pn0ffwPQKP0CPyiD0Sj9Qj88g9Co/So/KoPRqP1mPA46zHOY/zHhc8Lnpc8rjsccXjqsc1j+seeNzwuOlxy+O2xx2Pux73PO57PPB46PHI47HHE4+nHs88nnu88Hjp8crjtccbj7ce7zzee3zw+OjxyeOzxxePrx7fPL57/PD46fHL47fHH4+/Hv88Bg3uMqTL0C7DugzvMqLLyC6juozuMqbL2C7juozvMqHLxC6TukzuMqXL1C7TukzvMqPLzC6zuszuMqfL3C7zuszvsqDLwi6LuizusqTL0i7LuizvsqLLyi6ruqzusqbL2i7ruqzvsqHLxi6bumzusqXL1i7bumzvsqPLzi67uuzusqfL3i77uuzvcqDLwS6HuhzucqTL0S7HuhzvcqLLyS6nupzucqZLp4vQRewidZG7KF3ULloXvYvRxexidbG7OF3cLl6Xbpdel34Xv0vQJewSdYm7JF3SLlmXvEvRpexSdam7NF3aLgNdznY51+V8lwtdLna51OVylytdrna51uV6F7rc6HKzy60ut7vc6XK3y70u97s86PKwy6Muj7s86fK0y7Muz7u86PKyy6sur7u86fK2y7su77t86PKxy6cun7t86fK1y7cu37v86PKzy68uv7v86fK3y78ugwb3GNJjaI9hPYb3GNFjZI9RPUb3GNNjbI9xPcb3mNBjYo9JPSb3mNJjao9pPab3mNFjZo9ZPWb3mNNjbo95Peb3WNBjYY9FPRb3WNJjaY9lPZb3WNFjZY9VPVb3WNNjbY91Pdb32NBjY49NPTb32NJja49tPbb32NFjZ49dPXb32NNjb499Pfb3ONDjYI9DPQ73ONLjaI9jPY73ONHjZI9TPU73ONOj00PoIfaQesg9lB5qD62H3sPoYfawetg9nB5uD69Ht0evR7+H3yPoEfaIesQ9kh5pj6xH3qPoUfaoetQ9mh5tj4EeZ3uc63G+x4UeF3tc6nG5x5UeV3tc63G9Bz1u9LjZ41aP2z3u9Ljb416P+z0e9HjY41GPxz2e9Hja41mP5z1e9HjZ41WP1z3e9Hjb412P9z0+9PjY41OPzz2+9Pja41uP7z1+9PjZ41eP3z3+9Pjb41+PQYP7DOkztM+wPsP7jOgzss+oPqP7jOkzts+4PuP7TOgzsc+kPpP7TOkztc+0PtP7zOgzs8+sPrP7zOkzt8+8PvP7LOizsM+iPov7LOmztM+yPsv7rOizss+qPqv7rOmzts+6Puv7bOizsc+mPpv7bOmztc+2Ptv77Oizs8+uPrv77Omzt8++Pvv7HOhzsM+hPof7HOlztM+xPsf7nOhzss+pPqf7nOnT6SP0EftIfeQ+Sh+1j9ZH72P0MftYfew+Th+3j9en26fXp9/H7xP0CftEfeI+SZ+0T9Yn71P0KftUfeo+TZ+2z0Cfs33O9Tnf50Kfi30u9bnc50qfq32u9bnehz43+tzsc6vP7T53+tztc6/P/T4P+jzs86jP4z5P+jzt86zP8z4v+rzs86rP6z5v+rzt867P+z4f+nzs86nP5z5f+nzt863P9z4/+vzs86vP7z5/+vzt86/PoME+Q3yG+gzzGe4zwmekzyif0T5jfMb6jPMZ7zPBZ6LPJJ/JPlN8pvpM85nuM8Nnps8sn9k+c3zm+szzme+zwGehzyKfxT5LfJb6LPNZ7rPCZ6XPKp/VPmt81vqs81nvs8Fno88mn80+W3y2+mzz2e6zw2enzy6f3T57fPb67PPZ73PA56DPIZ/DPkd8jvoc8znuc8LnpM8pn9M+Z3w6PoKP6CP5yD6Kj+qj+eg+ho/pY/nYPo6P6+P5dH16Pn0f3yfwCX0in9gn8Ul9Mp/cp/ApfSqf2qfxaX0GfM76nPM573PB56LPJZ/LPld8rvpc87nug88Nn5s+t3xu+9zxuetzz+e+zwOfhz6PfB77PPF56vPM57nPC5+XPq98Xvu88Xnr887nvc8Hn48+n3w++3zx+erzzee7zw+fnz6/fH77/PH56/PPZ9DggCEBQwOGBQwPGBEwMmBUwOiAMQFjA8YFjA+YEDAxYFLA5IApAVMDpgVMD5gRMDNgVsDsgDkBcwPmBcwPWBCwMGBRwOKAJQFLA5YFLA9YEbAyYFXA6oA1AWsD1gWsD9gQsDFgU8DmgC0BWwO2BWwP2BGwM2BXwO6APQF7A/YF7A84EHAw4FDA4YAjAUcDjgUcDzgRcDLgVMDpgDMBnQAhQAyQAuQAJUAN0AL0ACPADLAC7AAnwA3wAroBvYB+gB8QBIQBUUAckASkAVlAHlAElAFVQB3QBLQBAwFnA84FnA+4EHAx4FLA5YArAVcDrgVcDyDgRsDNgFsBtwPuBNwNuBdwP+BBwMOARwGPA54EPA14FvA84EXAy4BXAa8D3gS8DXgX8D7gQ8DHgE8BnwO+BHwN+BbwPeBHwM+AXwG/A/4E/A34FzBocMiQkKEhw0KGh4wIGRkyKmR0yJiQsSHjQsaHTAiZGDIpZHLIlJCpIdNCpofMCJkZMitkdsickLkh80LmhywIWRiyKGRxyJKQpSHLQpaHrAhZGbIqZHXImpC1IetC1odsCNkYsilkc8iWkK0h20K2h+wI2RmyK2R3yJ6QvSH7QvaHHAg5GHIo5HDIkZCjIcdCjoecCDkZcirkdMiZkE6IECKGSCFyiBKihmgheogRYoZYIXaIE+KGeCHdkF5IP8QPCULCkCgkDklC0pAsJA8pQsqQKqQOaULakIGQsyHnQs6HXAi5GHIp5HLIlZCrIddCrocQciPkZsitkNshd0LuhtwLuR/yIORhyKOQxyFPQp6GPAt5HvIi5GXIq5DXIW9C3oa8C3kf8iHkY8inkM8hX0K+hnwL+R7yI+RnyK+Q3yF/Qv6G/AsZNDhiSMTQiGERwyNGRIyMGBUxOmJMxNiIcRHjIyZETIyYFDE5YkrE1IhpEdMjZkTMjJgVMTtiTsTciHkR8yMWRCyMWBSxOGJJxNKIZRHLI1ZErIxYFbE6Yk3E2oh1EesjNkRsjNgUsTliS8TWiG0R2yN2ROyM2BWxO2JPxN6IfRH7Iw5EHIw4FHE44kjE0YhjEccjTkScjDgVcTriTEQnQogQI6QIOUKJUCO0CD3CiDAjrAg7wolwI7yIbkQvoh/hRwQRYUQUEUckEWlEFpFHFBFlRBVRRzQRbcRAxNmIcxHnIy5EXIy4FHE54krE1YhrEdcjiLgRcTPiVsTtiDsRdyPuRdyPeBDxMOJRxOOIJxFPI55FPI94EfEy4lXE64g3EW8j3kW8j/gQ8THiU8TniC8RXyO+RXyP+BHxM+JXxO+IPxF/I/5FDBocMyRmaMywmOExI2JGxoyKGR0zJmZszLiY8TETYibGTIqZHDMlZmrMtJjpMTNiZsbMipkdMydmbsy8mPkxC2IWxiyKWRyzJGZpzLKY5TErYlbGrIpZHbMmZm3Mupj1MRtiNsZsitkcsyVma8y2mO0xO2J2xuyK2R2zJ2ZvzL6Y/TEHYg7GHIo5HHMk5mjMsZjjMSdiTsacijkdcyamEyPEiDFSjByjxKgxWoweY8SYMVaMHePEuDFeTDemF9OP8WOCmDAmioljkpg0JovJY4qYMqaKqWOamDZmIOZszLmY8zEXYi7GXIq5HHMl5mrMtZjrMcTciLkZcyvmdsydmLsx92LuxzyIeRjzKOZxzJOYpzHPYp7HvIh5GfMq5nXMm5i3Me9i3sd8iPkY8ynmc8yXmK8x32K+x/yI+RnzK+Z3zJ+YvzH/YgYNThiSMDRhWMLwhBEJIxNGJYxOGJMwNmFcwviECQkTEyYlTE6YkjA1YVrC9IQZCTMTZiXMTpiTMDdhXsL8hAUJCxMWJSxOWJKwNGFZwvKEFQkrE1YlrE5Yk7A2YV3C+oQNCRsTNiVsTtiSsDVhW8L2hB0JOxN2JexO2JOwN2Ffwv6EAwkHEw4lHE44knA04VjC8YQTCScTTiWcTjiT0EkQEsQEKUFOUBLUBC1BTzASzAQrwU5wEtwEL6Gb0EvoJ/gJQUKYECXECUlCmpAl5AlFQplQJdQJTUKbMJBwNuFcwvmECwkXEy4lXE64knA14VrC9QQSbiTcTLiVcDvhTsLdhHsJ9xMeJDxMeJTwOOFJwtOEZwnPE14kvEx4lfA64U3C24R3Ce8TPiR8TPiU8DnhS8LXhG8J3xN+JPxM+JXwO+FPwt+EfwmDBqcMSRmaMixleMqIlJEpo1JGp4xJGZsyLmV8yoSUiSmTUianTEmZmjItZXrKjJSZKbNSZqfMSZmbMi9lfsqClIUpi1IWpyxJWZqyLGV5yoqUlSmrUlanrElZm7IuZX3KhpSNKZtSNqdsSdmasi1le8qOlJ0pu1J2p+xJ2ZuyL2V/yoGUgymHUg6nHEk5mnIs5XjKiZSTKadSTqecSemkCCliipQipygpaoqWoqcYKWaKlWKnOCluipfSTeml9FP8lCAlTIlS4pQkJU3JUvKUIqVMqVLqlCalTRlIOZtyLuV8yoWUiymXUi6nXEm5mnIt5XoKKTdSbqbcSrmdciflbsq9lPspD1IepjxKeZzyJOVpyrOU5ykvUl6mvEp5nfIm5W3Ku5T3KR9SPqZ8Svmc8iXla8q3lO8pP1J+pvxK+Z3yJ+Vvyr+UQYMzhmQMzRiWMTxjRMbIjFEZozPGZIzNGJcxPmNCxsSMSRmTM6ZkTM2YljE9Y0bGzIxZGbMz5mTMzZiXMT9jQcbCjEUZizOWZCzNWJaxPGNFxsqMVRmrM9ZkrM1Yl7E+Y0PGxoxNGZsztmRszdiWsT1jR8bOjF0ZuzP2ZOzN2JexP+NAxsGMQxmHM45kHM04lnE840TGyYxTGaczzmR0MoQMMUPKkDOUDDVDy9AzjAwzw8qwM5wMN8PL6Gb0MvoZfkaQEWZEGXFGkpFmZBl5RpFRZlQZdUaT0WYMZJzNOJdxPuNCxsWMSxmXM65kXM24lnE9g4wbGTczbmXczriTcTfjXsb9jAcZDzMeZTzOeJLxNONZxvOMFxkvM15lvM54k/E2413G+4wPGR8zPmV8zviS8TXjW8b3jB8ZPzN+ZfzO+JPxN+NfxqDBOUNyhuYMyxmeMyJnZM6onNE5Y3LG5ozLGZ8zIWdizqScyTlTcqbmTMuZnjMjZ2bOrJzZOXNy5ubMy5mfsyBnYc6inMU5S3KW5izLWZ6zImdlzqqc1TlrctbmrMtZn7MhZ2POppzNOVtytuZsy9mesyNnZ86unN05e3L25uzL2Z9zIOdgzqGcwzlHco7mHMs5nnMi52TOqZzTOWdyOjlCjpgj5cg5So6ao+XoOUaOmWPl2DlOjpvj5XRzejn9HD8nyAlzopw4J8lJc7KcPKfIKXOqnDqnyWlzBnLO5pzLOZ9zIedizqWcyzlXcq7mXMu5nkPOjZybObdybufcybmbcy/nfs6DnIc5j3Ie5zzJeZrzLOd5zouclzmvcl7nvMl5m/Mu533Oh5yPOZ9yPud8yfma8y3ne86PnJ85v3J+5/zJ+ZvzL2fQ4IIhBUMLhhUMLxhRMLJgVMHogjEFYwvGFYwvmFAwsWBSweSCKQVTC6YVTC+YUTCzYFbB7II5BXML5hXML1hQsLBgUcHigiUFSwuWFSwvWFGwsmBVweqCNQVrC9YVrC/YULCxYFPB5oItBVsLthVsL9hRsLNgV8Hugj0Fewv2FewvOFBwsOBQweGCIwVHC44VHC84UXCy4FTB6YIzBZ0CoUAskArkAqVALdAK9AKjwCywCuwCp8At8Aq6Bb2CfoFfEBSEBVFBXJAUpAVZQV5QFJQFVUFd0BS0BQMFZwvOFZwvuFBwseBSweWCKwVXC64VXC+g4EbBzYJbBbcL7hTcLbhXcL/gQcHDgkcFjwueFDwteFbwvOBFwcuCVwWvC94UvC14V/C+4EPBx4JPBZ8LvhR8LfhW8L3gR8HPgl8Fvwv+FPwt+FcwaHDJkJKhJcNKhpeMKBlZMqpkdMmYkrEl40rGl0womVgyqWRyyZSSqSXTSqaXzCiZWTKrZHbJnJK5JfNK5pcsKFlYsqhkccmSkqUly0qWl6woWVmyqmR1yZqStSXrStaXbCjZWLKpZHPJlpKtJdtKtpfsKNlZsqtkd8mekr0l+0r2lxwoOVhyqORwyZGSoyXHSo6XnCg5WXKq5HTJmZJOiVAilkglcolSopZoJXqJUWKWWCV2iVPilngl3ZJeSb/ELwlKwpKoJC5JStKSrCQvKUrKkqqkLmlK2pKBkrMl50rOl1wouVhyqeRyyZWSqyXXSq6XUHKj5GbJrZLbJXdK7pbcK7lf8qDkYcmjksclT0qeljwreV7youRlyauS1yVvSt6WvCt5X/Kh5GPJp5LPJV9KvpZ8K/le8qPkZ8mvkt8lf0r+lvwrGTS4YkjF0IphFcMrRlSMrBhVMbpiTMXYinEV4ysmVEysmFQxuWJKxdSKaRXTK2ZUzKyYVTG7Yk7F3Ip5FfMrFlQsrFhUsbhiScXSimUVyytWVKysWFWxumJNxdqKdRXrKzZUbKzYVLG5YkvF1optFdsrdlTsrNhVsbtiT8Xein0V+ysOVBysOFRxuOJIxdGKYxXHK05UnKw4VXG64kxFp0KoECukCrlCqVArtAq9wqgwK6wKu8KpcCu8im5Fr6Jf4VcEFWFFVBFXJBVpRVaRVxQVZUVVUVc0FW3FQMXZinMV5ysuVFysuFRxueJKxdWKaxXXK6i4UXGz4lbF7Yo7FXcr7lXcr3hQ8bDiUcXjiicVTyueVTyveFHxsuJVxeuKNxVvK95VvK/4UPGx4lPF54ovFV8rvlV8r/hR8bPiV8Xvij8Vfyv+VQwaXDOkZmjNsJrhNSNqRtaMqhldM6ZmbM24mvE1E2om1kyqmVwzpWZqzbSa6TUzambWzKqZXTOnZm7NvJr5NQtqFtYsqllcs6Rmac2ymuU1K2pW1qyqWV2zpmZtzbqa9TUbajbWbKrZXLOlZmvNtprtNTtqdtbsqtlds6dmb82+mv01B2oO1hyqOVxzpOZozbGa4zUnak7WnKo5XXOmplMj1Ig1Uo1co9SoNVqNXmPUmDVWjV3j1Lg1Xk23plfTr/FrgpqwJqqJa5KatCaryWuKmrKmqqlrmpq2ZqDmbM25mvM1F2ou1lyquVxzpeZqzbWa6zXU3Ki5WXOr5nbNnZq7Nfdq7tc8qHlY86jmcc2Tmqc1z2qe17yoeVnzquZ1zZuatzXvat7XfKj5WPOp5nPNl5qvNd9qvtf8qPlZ86vmd82fmr81/2oGDW4Y0jC0YVjD8IYRDSMbRjWMbhjTMLZhXMP4hgkNExsmNUxumNIwtWFaw/SGGQ0zG2Y1zG6Y0zC3YV7D/IYFDQsbFjUsbljSsLRhWcPyhhUNKxtWNaxuWNOwtmFdw/qGDQ0bGzY1bG7Y0rC1YVvD9oYdDTsbdjXsbtjTsLdhX8P+hgMNBxsONRxuONJwtOFYw/GGEw0nG041nG4409BpEBrEBqlBblAa1AatQW8wGswGq8FucBrcBq+h29Br6Df4DUFD2BA1xA1JQ9qQNeQNRUPZUDXUDU1D2zDQcLbhXMP5hgsNFxsuNVxuuNJwteFaw/UGGm403Gy41XC74U7D3YZ7DfcbHjQ8bHjU8LjhScPThmcNzxteNLxseNXwuuFNw9uGdw3vGz40fGz41PC54UvD14ZvDd8bfjT8bPjV8LvhT8Pfhn8Ngwa3DGkZ2jKsZXjLiJaRLaNaRreMaRnbMq5lfMuEloktk1omt0xpmdoyrWV6y4yWmS2zWma3zGmZ2zKvZX7LgpaFLYtaFrcsaVnasqxlecuKlpUtq1pWt6xpWduyrmV9y4aWjS2bWja3bGnZ2rKtZXvLjpadLbtadrfsadnbsq9lf8uBloMth1oOtxxpOdpyrOV4y4mWky2nWk63nGnptAgtYovUIrcoLWqL1qK3GC1mi9VitzgtbovX0m3ptfRb/JagJWyJWuKWpCVtyVrylqKlbKla6pampW0ZaDnbcq7lfMuFlostl1out1xpudpyreV6Cy03Wm623Gq53XKn5W7LvZb7LQ9aHrY8annc8qTlacuzluctL1petrxqed3ypuVty7uW9y0fWj62fGr53PKl5WvLt5bvLT9afrb8avnd8qflb8u/lkGDBxgywNABhg0wfIARA4wcYNQAowcYM8DYAcYNMH6ACQNMHGDSAJMHmDLA1AH+V3QWWGKDUBTtcuru7u7ublN3d3cNIYQQbiBGvZ26e7fVM2yBA1/fu6MiRkeMiRgbMS5ifMSEiIkRkyImR0yJmBoxLWJ6xIyImRGzImZHzImYGzEvYn7EgoiFEYsiFkcsiVgasSxiecSKiJURqyJWR6yJWBuxLmJ9xIaIjRGbIjZHbInYGrEtYntER8SOiJ0RuyJ2R+yJ2BuxL2J/xIGIgxGHIg5HHIk4GnEs4njEiYiTEaciTkeciTgbcS7ifMSFiIsRlyIuR1yJuBpxLeJ6xI2ImxG3Im5H3Im4G3Ev4n7Eg4iHEVGEiIgjZEQSoSLSCB2RRZiIPMJGEFFEuAgfUUZUEXVEE9FGhIhHEY8jnkQ8jXgW8TziRcTLiFcRnRGvI95EvI14F/E+4kPEx4hPEZ8jvkR8jfgW8T3iR8TPiF8RvyP+RPyN+BfRrbugh6CnoJegt6CPoK+gn6C/YIBgoGCQYLBgiGCoYJhguGCEYKRglGC0YIxgrGCcYLxggmCiYJJgsmCKYKpgmmC6YIZgpmCWYLZgjmCuYJ5gvmCBYKFgkWCxYIlgqWCZYLlghWClYJVgtWCNYK1gnWC9YINgo2CTYLNgi2CrYJtgu6BDsEOwU7BLsFuwR7BXsE+wX3BAcFBwSHBYcERwVHBMcFxwQnBScEpwWnBGcFZwTnBecEFwUXBJcFlwRXBVcE1wXXBDcFNwS3BbcEdwV3BPcF/wQPBQEAmEIBZIQSJQglSgBZnACHKBFSAoBE7gBaWgEtSCRtAKguCR4LHgieCp4JngueCF4KXglaBT8FrwRvBW8E7wXvBB8FHwSfBZ8EXwVfBN8F3wQ/BT8EvwW/BH8FfwT9Cte0yPmJ4xvWJ6x/SJ6RvTL6Z/zICYgTGDYgbHDIkZGjMsZnjMiJiRMaNiRseMiRkbMy5mfMyEmIkxk2Imx0yJmRozLWZ6zIyYmTGzYmbHzImZGzMvZn7MgpiFMYtiFscsiVkasyxmecyKmJUxq2JWx6yJWRuzLmZ9zIaYjTGbYjbHbInZGrMtZntMR8yOmJ0xu2J2x+yJ2RuzL2Z/zIGYgzGHYg7HHIk5GnMs5njMiZiTMadiTseciTkbcy7mfMyFmIsxl2Iux1yJuRpzLeZ6zI2YmzG3Ym7H3Im5G3Mv5n7Mg5iHMVGMiIljZEwSo2LSGB2TxZiYPMbGEFPEuBgfU8ZUMXVME9PGhJhHMY9jnsQ8jXkW8zzmRczLmFcxnTGvY97EvI15F/M+5kPMx5hPMZ9jvsR8jfkW8z3mR8zPmF8xv2P+xPyN+RfTrbukh6SnpJekt6SPpK+kn6S/ZIBkoGSQZLBkiGSoZJhkuGSEZKRklGS0ZIxkrGScZLxkgmSiZJJksmSKZKpkmmS6ZIZkpmSWZLZkjmSuZJ5kvmSBZKFkkWSxZIlkqWSZZLlkhWSlZJVktWSNZK1knWS9ZINko2STZLNki2SrZJtku6RDskOyU7JLsluyR7JXsk+yX3JAclBySHJYckRyVHJMclxyQnJSckpyWnJGclZyTnJeckFyUXJJcllyRXJVck1yXXJDclNyS3JbckdyV3JPcl/yQPJQEkmEJJZISSJRklSiJZnESHKJlSApJE7iJaWkktSSRtJKguSR5LHkieSp5JnkueSF5KXklaRT8lryRvJW8k7yXvJB8lHySfJZ8kXyVfJN8l3yQ/JT8kvyW/JH8lfyT9Kte0KPhJ4JvRJ6J/RJ6JvQL6F/woCEgQmDEgYnDEkYmjAsYXjCiISRCaMSRieMSRibMC5hfMKEhIkJkxImJ0xJmJowLWF6woyEmQmzEmYnzEmYmzAvYX7CgoSFCYsSFicsSViasCxhecKKhJUJqxJWJ6xJWJuwLmF9woaEjQmbEjYnbEnYmrAtYXtCR8KOhJ0JuxJ2J+xJ2JuwL2F/woGEgwmHEg4nHEk4mnAs4XjCiYSTCacSTiecSTibcC7hfMKFhIsJlxIuJ1xJuJpwLeF6wo2Emwm3Em4n3Em4m3Av4X7Cg4SHCVGCSIgTZEKSoBLSBJ2QJZiEPMEmkFAkuASfUCZUCXVCk9AmhIRHCY8TniQ8TXiW8DzhRcLLhFcJnQmvE94kvE14l/A+4UPCx4RPCZ8TviR8TfiW8D3hR8LPhF8JvxP+JPxN+JfQrbuih6Knopeit6KPoq+in6K/YoBioGKQYrBiiGKoYphiuGKEYqRilGK0YoxirGKcYrxigmKiYpJismKKYqpimmK6YoZipmKWYrZijmKuYp5ivmKBYqFikWKxYoliqWKZYrlihWKlYpVitWKNYq1inWK9YoNio2KTYrNii2KrYptiu6JDsUOxU7FLsVuxR7FXsU+xX3FAcVBxSHFYcURxVHFMcVxxQnFScUpxWnFGcVZxTnFecUFxUXFJcVlxRXFVcU1xXXFDcVNxS3FbcUdxV3FPcV/xQPFQESmEIlZIRaJQilShFZnCKHKFVaAoFE7hFaWiUtSKRtEqguKR4rHiieKp4pniueKF4qXilaJT8VrxRvFW8U7xXvFB8VHxSfFZ8UXxVfFN8V3xQ/FT8UvxW/FH8VfxT9Gte0qPlJ4pvVJ6p/RJ6ZvSL6V/yoCUgSmDUganDEkZmjIsZXjKiJSRKaNSRqeMSRmbMi5lfMqElIkpk1Imp0xJmZoyLWV6yoyUmSmzUmanzEmZmzIvZX7KgpSFKYtSFqcsSVmasixlecqKlJUpq1JWp6xJWZuyLmV9yoaUjSmbUjanbEnZmrItZXtKR8qOlJ0pu1J2p+xJ2ZuyL2V/yoGUgymHUg6nHEk5mnIs5XjKiZSTKadSTqecSTmbci7lfMqFlIspl1Iup1xJuZpyLeV6yo2Umym3Um6n3Em5m3Iv5X7Kg5SHKVGKSIlTZEqSolLSFJ2SpZiUPMWmkFKkuBSfUqZUKXVKk9KmhJRHKY9TnqQ8TXmW8jzlRcrLlFcpnSmvU96kvE15l/I+5UPKx5RPKZ9TvqR8TfmW8j3lR8rPlF8pv1P+pPxN+ZfSrbumh6anppemt6aPpq+mn6a/ZoBmoGaQZrBmiGaoZphmuGaEZqRmlGa0ZoxmrGacZrxmgmaiZpJmsmaKZqpmmma6ZoZmpmaWZrZmjmauZp5mvmaBZqFmkWaxZolmqWaZZrlmhWalZpVmtWaNZq1mnWa9ZoNmo2aTZrNmi2arZptmu6ZDs0OzU7NLs1uzR7NXs0+zX3NAc1BzSHNYc0RzVHNMc1xzQnNSc0pzWnNGc1ZzTnNec0FzUXNJc1lzRXNVc01zXXNDc1NzS3Nbc0dzV3NPc1/zQPNQE2mEJtZITaJRmlSjNZnGaHKN1aApNE7jNaWm0tSaRtNqguaR5rHmieap5pnmueaF5qXmlaZT81rzRvNW807zXvNB81HzSfNZ80XzVfNN813zQ/NT80vzW/NH81fzT9Ote0aPjJ4ZvTJ6Z/TJ6JvRL6N/xoCMgRmDMgZnDMkYmjEsY3jGiIyRGaMyRmeMyRibMS5jfMaEjIkZkzImZ0zJmJoxLWN6xoyMmRmzMmZnzMmYmzEvY37GgoyFGYsyFmcsyViasSxjecaKjJUZqzJWZ6zJWJuxLmN9xoaMjRmbMjZnbMnYmrEtY3tGR8aOjJ0ZuzJ2Z+zJ2JuxL2N/xoGMgxmHMg5nHMk4mnEs43jGiYyTGacyTmecyTibcS7jfMaFjIsZlzIuZ1zJuJpxLeN6xo2Mmxm3Mm5n3Mm4m3Ev437Gg4yHGVGGyIgzZEaSoTLSDJ2RZZiMPMNmkFFkuAyfUWZUGXVGk9FmhIxHGY8znmQ8zXiW8TzjRcbLjFcZnRmvM95kvM14l/E+40PGx4xPGZ8zvmR8zfiW8T3jR8bPjF8ZvzP+ZPzN+JfRrbuhh6GnoZeht6GPoa+hn6G/YYBhoGGQYbBhiGGoYZhhuGGEYaRhlGG0YYxhrGGcYbxhgmGiYZJhsmGKYaphmmG6YYZhpmGWYbZhjmGuYZ5hvmGBYaFhkWGxYYlhqWGZYblhhWGlYZVhtWGNYa1hnWG9YYNho2GTYbNhi2GrYZthu6HDsMOw07DLsNuwx7DXsM+w33DAcNBwyHDYcMRw1HDMcNxwwnDScMpw2nDGcNZwznDecMFw0XDJcNlwxXDVcM1w3XDDcNNwy3DbcMdw13DPcN/wwPDQEBmEITZIQ2JQhtSgDZnBGHKDNWAoDM7gDaWhMtSGxtAaguGR4bHhieGp4ZnhueGF4aXhlaHT8NrwxvDW8M7w3vDB8NHwyfDZ8MXw1fDN8N3ww/DT8Mvw2/DH8Nfwz9Cte06PnJ45vXJ65/TJ6ZvTL6d/zoCcgTmDcgbnDMkZmjMsZ3jOiJyROaNyRueMyRmbMy5nfM6EnIk5k3Im50zJmZozLWd6zoycmTmzcmbnzMmZmzMvZ37OgpyFOYtyFucsyVmasyxnec6KnJU5q3JW56zJWZuzLmd9zoacjTmbcjbnbMnZmrMtZ3tOR86OnJ05u3J25+zJ2ZuzL2d/zoGcgzmHcg7nHMk5mnMs53jOiZyTOadyTuecyTmbcy7nfM6FnIs5l3Iu51zJuZpzLed6zo2cmzm3cm7n3Mm5m3Mv537Og5yHOVGOyIlzZE6So3LSHJ2T5ZicPMfmkFPkuByfU+ZUOXVOk9PmhJxHOY9znuQ8zXmW8zznRc7LnFc5nTmvc97kvM15l/M+50POx5xPOZ9zvuR8zfmW8z3nR87PnF85v3P+5PzN+ZfTrbulh6WnpZelt6WPpa+ln6W/ZYBloGWQZbBliGWoZZhluGWEZaRllGW0ZYxlrGWcZbxlgmWiZZJlsmWKZaplmmW6ZYZlpmWWZbZljmWuZZ5lvmWBZaFlkWWxZYllqWWZZbllhWWlZZVltWWNZa1lnWW9ZYNlo2WTZbNli2WrZZtlu6XDssOy07LLstuyx7LXss+y33LActByyHLYcsRy1HLMctxywnLScspy2nLGctZyznLecsFy0XLJctlyxXLVcs1y3XLDctNyy3Lbcsdy13LPct/ywPLQElmEJbZIS2JRltSiLZnFWHKLtWApLM7iLaWlstSWxtJaguWR5bHlieWp5ZnlueWF5aXllaXT8tryxvLW8s7y3vLB8tHyyfLZ8sXy1fLN8t3yw/LT8svy2/LH8tfyz9KtO/SAntALekMf6Av9oD8MgIEwCAbDEBgKw2A4jICRMApGwxgYC+NgPEyAiTAJJsMUmArTYDrMgJkwC2bDHJgL82A+LICFsAgWwxJYCstgOayAlbCqC3vPmi50Nuu68Lts6EJ4sqkLA8iWLpQY27pwRHR0IU3Y2YVFYHeXtTp7u+yZ2d9l8crBLptIDndZzXG0y66K412WN5zsss3gdJf0nrNd8l3Od0kAudglI+JylxSBq13rzFzvWonkZtdaFbe7VjO42zXe5X7XiIiHXW1mRFerCtlV7qK6UmZ0V9jFdD1dLF2nAAceSqighgZaCPAIHsMTeArP4Dm8gJfwCjrhNbyBt/AO3sMH+Aif4DN8ga/wDb7DD/gJv+A3/IG/8A+6dS/oUdCzoFdB74I+BX0L+hX0LxhQMLBgUMHggiEFQwuGFQwvGFEwsmBUweiCMQVjC8YVjC+YUDCxYFLB5IIpBVMLphVML5hRMLNgVsHsgjkFcwvmFcwvWFCwsGBRweKCJQVLC5YVLC9YUbCyYFXB6oI1BWsL1hWsL9hQsLFgU8Hmgi0FWwu2FWwv6CjYUbCzYFfB7oI9BXsL9hXsLzhQcLDgUMHhgiMFRwuOFRwvOFFwsuBUwemCMwVnC84VnC+4UHCx4FLB5YIrBVcLrhVcL7hRcLPgVsHtgjsFdwvuFdwveFDwsCAqEAVxgSxIClRBWqALsgJTkBfYout6iwJX4AvKgqqgLmgK2oJQ8KjgccGTgqcFzwqeF7woeFnwqqCz4HXBm4K3Be8K3hd8KPhY8Kngc8GXgq8F3wq+F/wo+Fnwq+B3wZ+CvwX/Crp1d/Rw9HT0cvR29HH0dfRz9HcMcAx0DHIMdgxxDHUMcwx3jHCMdIxyjHaMcYx1jHOMd0xwTHRMckx2THFMdUxzTHfMcMx0zHLMdsxxzHXMc8x3LHAsdCxyLHYscSx1LHMsd6xwrHSscqx2rHGsdaxzrHdscGx0bHJsdmxxbHVsc2x3dDh2OHY6djl2O/Y49jr2OfY7DjgOOg45DjuOOI46jjmOO044TjpOOU47zjjOOs45zjsuOC46LjkuO644rjquOa47bjhuOm45bjvuOO467jnuOx44Hjoih3DEDulIHMqROrQjcxhH7rCu6/EWDufwjtJROWpH42gdwfHI8djxxPHU8czx3PHC8dLxytHpeO1443jreOd47/jg+Oj45Pjs+OL46vjm+O744fjp+OX47fjj+Ov45+jW3dPD09PTy9Pb08fT19PP098zwDPQM8gz2DPEM9QzzDPcM8Iz0jPKM9ozxjPWM84z3jPBM9EzyTPZM8Uz1TPNM90zwzPTM8sz2zPHM9czzzPfs8Cz0LPIs9izxLPUs8yz3LPCs9KzyrPas8az1rPOs96zwbPRs8mz2bPFs9WzzbPd0+HZ4dnp2eXZ7dnj2evZ59nvOeA56DnkOew54jnqOeY57jnhOek55TntOeM56znnOe+54LnoueS57Lniueq55rnuueG56bnlue2547nruee573ngeeiJPMITe6Qn8ShP6tGezGM8ucf6rq+58DiP95SeylN7Gk/rCZ5HnseeJ56nnmee554XnpeeV55Oz2vPG89bzzvPe88Hz0fPJ89nzxfPV883z3fPD89Pzy/Pb88fz1/PP0+37iU9SnqW9CrpXdKnpG9Jv5L+JQNKBpYMKhlcMqRkaMmwkuElI0pGlowqGV0ypmRsybiS8SUTSiaWTCqZXDKlZGrJtJLpJTNKZpbMKpldMqdkbsm8kvklC0oWliwqWVyypGRpybKS5SUrSlaWrCpZXbKmZG3JupL1JRtKNpZsKtlcsqVka8m2ku0lHSU7SnaW7CrZXbKnZG/JvpL9JQdKDpYcKjlccqTkaMmxkuMlJ0pOlpwqOV1ypuRsybmS8yUXSi6WXCq5XHKl5GrJtZLrJTdKbpbcKrldcqfkbsm9kvslD0oelkQloiQukSVJiSpJS3RJVmJK8hJbdgXeosSV+JKypCqpS5qStiSUPCp5XPKk5GnJs5LnJS9KXpa8KukseV3ypuRtybuS9yUfSj6WfCr5XPKl5GvJt5LvJT9Kfpb8Kvld8qfkb8m/km7dK3pU9KzoVdG7ok9F34p+Ff0rBlQMrBhUMbhiSMXQimEVwytGVIysGFUxumJMxdiKcRXjKyZUTKyYVDG5YkrF1IppFdMrZlTMrJhVMbtiTsXcinkV8ysWVCysWFSxuGJJxdKKZRXLK1ZUrKxYVbG6Yk3F2op1FesrNlRsrNhUsbliS8XWim0V2ys6KnZU7KzYVbG7Yk/F3op9FfsrDlQcrDhUcbjiSMXRimMVxytOVJysOFVxuuJMxdmKcxXnKy5UXKy4VHG54krF1YprFdcrblTcrLhVcbviTsXdinsV9yseVDysiCpERVwhK5IKVZFW6IqswlTkFbbqSquKClfhK8qKqqKuaCrailDxqOJxxZOKpxXPKp5XvKh4WfGqorPidcWbircV7yreV3yo+FjxqeJzxZeKrxXfKr5X/Kj4WfGr4nfFn4q/Ff8qunWv6VHTs6ZXTe+aPjV9a/rV9K8ZUDOwZlDN4JohNUNrhtUMrxlRM7JmVM3omjE1Y2vG1YyvmVAzsWZSzeSaKTVTa6bVTK+ZUTOzZlbN7Jo5NXNr5tXMr1lQs7BmUc3imiU1S2uW1SyvWVGzsmZVzeqaNTVra9bVrK/ZULOxZlPN5potNVtrttVsr+mo2VGzs2ZXze6aPTV7a/bV7K85UHOw5lDN4ZojNUdrjtUcrzlRc7LmVM3pmjM1Z2vO1ZyvuVBzseZSzeWaKzVXa67VXK+5UXOz5lbN7Zo7NXdr7tXcr3lQ87AmqhE1cY2sSWpUTVqja7IaU5PX2LoraS5qXI2vKWuqmrqmqWlrQs2jmsc1T2qe1jyreV7zouZlzauazprXNW9q3ta8q3lf86HmY82nms81X2q+1nyr+V7zo+Znza+a3zV/av7W/Kvp1r2hR0PPhl4NvRv6NPRt6NfQv2FAw8CGQQ2DG4Y0DG0Y1jC8YUTDyIZRDaMbxjSMbRjXML5hQsPEhkkNkxumNExtmNYwvWFGw8yGWQ2zG+Y0zG2Y1zC/YUHDwoZFDYsbljQsbVjWsLxhRcPKhlUNqxvWNKxtWNewvmFDw8aGTQ2bG7Y0bG3Y1rC9oaNhR8POhl0Nuxv2NOxt2Newv+FAw8GGQw2HG440HG041nC84UTDyYZTDacbzjScbTjXcL7hQsPFhksNlxuuNFxtuNZwveFGw82GWw23G+403G2413C/4UHDw4aoQTTEDbIhaVANaYNuyBpMQ95gm66SqGhwDb6hbKga6oamoW0IDY8aHjc8aXja8KzhecOLhpcNrxo6G143vGl42/Cu4X3Dh4aPDZ8aPjd8afja8K3he8OPhp8Nvxp+N/xp+Nvwr6Fb95YeLT1berX0bunT0relX0v/lgEtA1sGtQxuGdIytGVYy/CWES0jW0a1jG4Z0zK2ZVzL+JYJLRNbJrVMbpnSMrVlWsv0lhktM1tmtcxumdMyt2Vey/yWBS0LWxa1LG5Z0rK0ZVnL8pYVLStbVrWsblnTsrZlXcv6lg0tG1s2tWxu2dKytWVby/aWjpYdLTtbdrXsbtnTsrdlX8v+lgMtB1sOtRxuOdJytOVYy/GWEy0nW061nG4503K25VzL+ZYLLRdbLrVcbrnScrXlWsv1lhstN1tutdxuudNyt+Vey/2WBy0PW6IW0RK3yJakRbWkLbolazEteYttuwreosW1+JaypWqpW5qWtiW0PGp53PKk5WnLs5bnLS9aXra8aulsed3ypuVty7uW9y0fWj62fGr53PKl5WvLt5bvLT9afrb8avnd8qflb8u/lm7dAz0CPQO9Ar0DfQJ9A/0C/QMDAgMDgwKDA0MCQwPDAsMDIwIjA6MCowNjAmMD4wLjAxMCEwOTApMDUwJTA9MC0wMzAjMDswKzA3MCcwPzAvMDCwILA4sCiwNLAksDywLLAysCKwOrAqsDawJrA+sC6wMbAhsDmwKbA1sCWwPbAtsDHYEdgZ2BXYHdgT2BvYF9gf2BA4GDgUOBw4EjgaOBY4HjgROBk4FTgdOBM4GzgXOB84ELgYuBS4HLgSuBq4FrgeuBG4GbgVuB24E7gbuBe4H7gQeBh4EoIAJxQAaSgAqkAR3IAiaQB2zoamcUARfwgTJQBepAE2gDIfAo8DjwJPA08CzwPPAi8DLwKtAZeB14E3gbeBd4H/gQ+Bj4FPgc+BL4GvgW+B74EfgZ+BX4HfgT+Bv4F/gPYVTKKWRD0S0AAAAASUVORK5CYII=" });
    }
    if (method === "GET") {
      if (p === "/v1/info") {
        // `?hubcommit=abc1234`: a hub built from another commit than the page.
        const commit = new URLSearchParams(location.search).get("hubcommit") ?? __BUILD__.commit;
        return json(200, { version: __BUILD__.version, commit, debug: false, api: s.api ?? "v1", pid: 0, home: "（mock 模式，无 hub）", started_at: new Date().toISOString() });
      }
      if (p === "/v1/agents") return json(200, { agents: s.agents });
      if ((m = p.match(/^\/v1\/agents\/([^/]+)$/))) return this.agent(m[1]) ? json(200, this.agent(m[1])) : notFound("agent");
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/commands$/))) {
        const a = this.agent(m[1]);
        return a ? json(200, s.agentCommands?.[a.id] ?? { commands: [], known: false, note: "这个驱动不报告自带指令" }) : notFound("agent");
      }
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
      if ((m = p.match(/^\/v1\/(runs|agents)\/([^/]+)\/graph$/))) {
        const id = m[1] === "runs" ? s.runs.find((x) => x.run.id === m![2])?.run.id : this.agent(m[2])?.id;
        if (!id) return notFound(m[1] === "runs" ? "run" : "agent");
        if (m[1] === "runs" && s.runs.find((x) => x.run.id === id)!.run.in_place)
          return json(409, { error: { code: "conflict", message: "this directory is not under version control, so there is no commit graph" } });
        const graph = s.graphs?.get(id) ?? { refs: [], commits: [], truncated: false };
        const limit = Number(url.searchParams.get("limit") ?? 200);
        return json(200, { ...graph, commits: graph.commits.slice(0, limit), truncated: graph.truncated || graph.commits.length > limit });
      }
      if ((m = p.match(/^\/v1\/(runs|agents)\/([^/]+)\/commits\/([^/]+)\/diff$/))) {
        const id = m[1] === "runs" ? s.runs.find((x) => x.run.id === m![2])?.run.id : this.agent(m[2])?.id;
        const sha = decodeURIComponent(m[3]);
        const commit = id ? s.graphs?.get(id)?.commits.find((c) => c.sha === sha) : undefined;
        if (!commit) return notFound(`commit ${sha}`);
        return json(200, { base: commit.parents[0] ?? "", head: sha, diff: s.commitDiffs?.get(sha) ?? "", truncated: false });
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
      if (p === "/v1/usage/series") return json(200, usageSeriesMock(url.searchParams.get("range") ?? "7d", url.searchParams.get("group") ?? "total", url.searchParams.get("source") ?? "all", s.usageSeriesVariant ?? "normal"));
      if (p === "/v1/expertise" || p === "/v1/expertise/card" || p === "/v1/expertise/maintenance" || p === "/v1/expertise/export") {
        const x = s.expertise ?? { entries: [], candidates: [], usages: [] };
        const kind = url.searchParams.get("owner_kind") ?? "person";
        const owner = url.searchParams.get("owner_id") ?? "";
        const entries = x.entries.filter((e) => e.owner_kind === kind && e.owner_id === owner);
        const ids = new Set(entries.map((e) => e.id));
        const usages = x.usages.filter((u) => ids.has(u.entry_id));
        if (p === "/v1/expertise") return json(200, { entries });
        // Like the hub: plain Markdown, an empty body when nothing is left to export.
        if (p === "/v1/expertise/export") return { status: 200, body: exportMarkdown(entries) };
        if (p === "/v1/expertise/card") return json(200, expertiseCard(entries, usages, new Date()));
        const candidates = x.candidates.filter((c) => c.owner_kind === kind && c.owner_id === owner);
        return json(200, { budgets: budgetReport(entries), stale: staleScan(entries, usages, new Date()), conflicts: conflictList(candidates) });
      }
      if (p === "/v1/expertise/candidates") {
        const status = url.searchParams.get("status") ?? "pending";
        const kind = url.searchParams.get("owner_kind");
        const owner = url.searchParams.get("owner_id");
        const candidates = (s.expertise?.candidates ?? []).filter(
          (c) => c.status === status && (kind === null || c.owner_kind === kind) && (owner === null || c.owner_id === owner),
        );
        return json(200, { candidates });
      }
      if (p === "/v1/usage") {
        const by = (url.searchParams.get("by") ?? "role") as UsageGroup;
        return json(200, s.usage[by] ?? { by, rows: [], total_cost_usd: 0 });
      }
      if ((m = p.match(/^\/v1\/accounts\/([^/]+)\/usage$/)))
        return json(200, s.accountUsage ?? { by: url.searchParams.get("by") ?? "day", rows: [], total: { sessions: 0, input_tokens: 0, cached_input_tokens: 0, output_tokens: 0, reasoning_output_tokens: 0 }, total_cache_hit_rate: 0 });
      if (p === "/v1/plugins") return json(200, { plugins: s.plugins });
      if (p === "/v1/session-candidates") return json(200, { sessions: s.sessions ?? [] });
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
      if ((m = p.match(/^\/v1\/expertise\/candidates\/([^/]+)\/(accept|reject)$/))) {
        const x = s.expertise;
        const c = x?.candidates.find((k) => k.id === decodeURIComponent(m![1]));
        if (!x || !c) return notFound("candidate");
        if (c.status !== "pending") return json(409, { error: { code: "conflict", message: `候选已经${c.status === "accepted" ? "接受" : "拒绝"}` } });
        const now = new Date().toISOString();
        if (m[2] === "reject") {
          c.status = "rejected";
          c.decided_at = now;
          return json(200, c);
        }
        const decisions = (body as { decisions?: ConflictDecision[] } | null)?.decisions ?? [];
        const missing = missingVerdicts(c, decisions);
        if (missing.length) return json(400, { error: { code: "bad_request", message: `冲突没有裁决：${missing.join(", ")}` } });
        const newId = uid();
        x.entries = acceptCandidate(x.entries, c, decisions, newId, now);
        c.status = "accepted";
        c.decided_at = now;
        return json(200, { candidate: c, entry: x.entries.find((e) => e.id === newId) });
      }
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
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/chat$/))) {
        // Like the hub's start_chat: only a free session, an archived one with restore, and
        // the same words for both refusals.
        const a = this.agent(m[1]);
        if (!a) return notFound("agent");
        const req = body as StartChatRequest;
        if (!req.text.trim()) return json(400, { error: { code: "bad_request", message: "text must not be empty" } });
        const held = s.executions?.[a.id];
        if (held) return json(409, { error: { code: "conflict", message: `${a.name} is working on run "${held.run_title}"${held.task_key ? ` #${held.task_key}` : ""}` } });
        if (a.archived_at) {
          if (!req.restore) return json(409, { error: { code: "conflict", message: `${a.name} is archived: restore it first with restore: true` } });
          a.archived_at = null;
          this.emit({ type: "agent.updated", data: { agent: { ...a } } });
          this.restored(a);
        }
        return this.send(a.id, { text: req.text });
      }
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/messages$/))) return this.send(m[1], body as SendMessageRequest);
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/cancel$/))) return this.cancel(m[1]);
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/compact$/))) return this.compact(m[1]);
      if ((m = p.match(/^\/v1\/runs\/([^/]+)\/(approve|reject|cancel)$/))) {
        const d = s.runs.find((x) => x.run.id === m![1]);
        if (!d) return notFound("run");
        const to = m[2] === "approve" ? "running" : m[2] === "reject" ? "planning" : "cancelled";
        this.emit({ type: "run.status_changed", data: { id: d.run.id, from: d.run.status, to, note: m[2] === "reject" ? ((body as { reason?: string })?.reason ?? null) : null } });
        return json(200, d);
      }
      if ((m = p.match(/^\/v1\/runs\/([^/]+)\/policy$/))) {
        // The person's change to how the run continues by itself (crates/hub/src/api.rs).
        const d = s.runs.find((x) => x.run.id === decodeURIComponent(m![1]));
        if (!d) return notFound("run");
        const req = body as {
          continuation?: {
            enabled: boolean;
            max_attempts?: number;
            interval_seconds?: number;
            stuck_minutes?: number | null;
          };
          reset_attempts?: boolean;
        };
        if (req.continuation?.enabled) {
          const had = d.run.config.policy.continuation ?? { max_attempts: 3, interval_seconds: 60, stuck_minutes: null };
          d.run.config.policy.continuation = {
            max_attempts: req.continuation.max_attempts ?? had.max_attempts,
            interval_seconds: req.continuation.interval_seconds ?? had.interval_seconds,
            // Unstated keeps what the run had; `null` turns the quiet-wait report off.
            stuck_minutes:
              req.continuation.stuck_minutes === undefined ? had.stuck_minutes : req.continuation.stuck_minutes,
          };
        } else if (req.continuation) {
          d.run.config.policy.continuation = null;
          d.run.continuation = null;
        }
        if (req.reset_attempts && d.run.continuation) {
          d.run.continuation = { ...d.run.continuation, attempts: 0, next_at: null, exhausted_notified: false };
        }
        if (d.run.continuation) this.emit({ type: "run.continuation_changed", data: { id: d.run.id, state: d.run.continuation } });
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
      if (p === "/v1/plugins/refresh") return this.refreshClis();
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

  /** Compacting: a running agent refuses, as the hub does; otherwise the window empties out. */
  private compact(key: string): HubResponse {
    const a = this.agent(key);
    if (!a) return notFound("agent");
    if (a.status !== "idle") return json(409, { error: { code: "conflict", message: `agent ${a.name} is not idle: compact after this turn` } });
    const t = this.state.transcripts.get(a.id);
    const usage = t?.usage;
    if (!t || !usage) return json(204);
    const used = Math.round(usage.size * 0.08);
    t.usage = { ...usage, used, tokens: { input: used * 2, cached_input: used, output: Math.round(used / 5), reasoning_output: Math.round(used / 10), total: used * 2 + Math.round(used / 5) }, parts: a.plugin === "claude" ? contextParts(used, usage.size) : usage.parts };
    this.emit({ type: "agent.usage", data: { id: a.id, used, size: usage.size, cost_usd: usage.cost_usd, tokens: t.usage.tokens } });
    return json(204);
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

  /** Looking for the CLIs again: `202` at once, then each row's version is probed and the
   * event stream says the row changed, as the hub does (docs/DESIGN.md §9.3). */
  private refreshClis(): HubResponse {
    for (const plugin of this.state.plugins) {
      if (!plugin.cli) continue;
      plugin.cli.probing = true;
      this.timers.push(
        setTimeout(() => {
          if (!plugin.cli) return;
          plugin.cli.probing = false;
          this.emit({ type: "plugin.changed", data: { name: plugin.name } });
        }, 700),
      );
    }
    return json(202);
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
