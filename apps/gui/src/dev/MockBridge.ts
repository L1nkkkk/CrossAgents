// A stand-in for the Electron bridge (`window.crossagents`, bridge.d.ts) in `preview:mock`: it
// answers `/v1/...` from a scenario's state and plays the scenario's event script on the event
// stream. Events change the state the same way the hub's do, so REST re-fetches agree with
// what streamed (transcripts fold with the GUI's own session.ts). Writes that matter to the
// look (sending, withdrawing, approving) are acted out; the rest answer 204.

import type { AccountUsageResponse } from "@bindings/AccountUsageResponse";
import type { Agent } from "@bindings/Agent";
import type { CreateAgentRequest } from "@bindings/CreateAgentRequest";
import type { HumanItem } from "@bindings/HumanItem";
import type { Note } from "@bindings/Note";
import type { PluginInfo } from "@bindings/PluginInfo";
import type { QuotaResponse } from "@bindings/QuotaResponse";
import type { RunDetail } from "@bindings/RunDetail";
import type { Transcript } from "@bindings/Transcript";
import type { UsageGroup } from "@bindings/UsageGroup";
import type { UsageResponse } from "@bindings/UsageResponse";
import type { CrossAgentsBridge, HubResponse } from "../bridge";
import type { HubEvent } from "../hub";
import { turnStat } from "../lib/diff";
import { effortOf } from "../lib/models";
import { applyEvent } from "../session";
import { transcript, uid } from "./fixtures";

/** Names the mock hub makes up, like the real one (see crates/core/src/names.rs). */
const MOCK_NAMES = ["Nova", "Iris", "Atlas", "Orion", "Vega", "Lyra", "Mira", "Sage"];

export type MockState = {
  agents: Agent[];
  runs: RunDetail[];
  /** By agent id. */
  transcripts: Map<string, Transcript>;
  /** Unified diff of each turn that changed files, by message id. */
  diffs: Map<string, string>;
  questions: HumanItem[];
  notes: Note[];
  quota: QuotaResponse;
  usage: Partial<Record<UsageGroup, UsageResponse>>;
  accountUsage?: AccountUsageResponse;
  plugins: PluginInfo[];
  /** The API version `/v1/info` reports; `v1` when unset. */
  api?: string;
};

type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;
/** An event as a script writes it; the bridge numbers and stamps it. */
export type Payload = DistributiveOmit<HubEvent, "seq" | "id" | "ts" | "schema">;
/** One script step: `at` ms after the stream connects, an event or a change to the state. */
export type Step = { at: number; event?: Payload; act?: (s: MockState) => void };

export function emptyState(): MockState {
  return {
    agents: [],
    runs: [],
    transcripts: new Map(),
    diffs: new Map(),
    questions: [],
    notes: [],
    quota: { accounts: [], providers: ["claude", "codex"] },
    usage: {},
    plugins: [],
  };
}

const LATENCY = 40;
const json = (status: number, body?: unknown): HubResponse => ({ status, body: body === undefined ? "" : JSON.stringify(body) });
const notFound = (what: string) => json(404, { error: { code: "not_found", message: `${what} not found` } });

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

  async pickFolder() {
    return "C:/work/new-project";
  }

  setTheme() {
    // No window frame to follow in a browser.
  }

  // ---- Events ---------------------------------------------------------------------------

  private status(s: string) {
    this.statuses.forEach((l) => l(s));
  }

  private play(step: Step) {
    step.act?.(this.state);
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
      case "run.status_changed": {
        const r = s.runs.find((d) => d.run.id === e.data.id);
        if (r) {
          r.run.status = e.data.to;
          r.run.updated_at = e.ts;
        }
        break;
      }
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
      if (p === "/v1/info") return json(200, { version: "0.6.0-mock", api: s.api ?? "v1", pid: 0, home: "（mock 模式，无 hub）", started_at: new Date().toISOString() });
      if (p === "/v1/agents") return json(200, { agents: s.agents });
      if ((m = p.match(/^\/v1\/agents\/([^/]+)$/))) return this.agent(m[1]) ? json(200, this.agent(m[1])) : notFound("agent");
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
      return notFound(p);
    }

    if (method === "POST") {
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
          cwd,
          status: "idle",
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
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/topic$/))) {
        const a = this.agent(m[1]);
        if (!a) return notFound("agent");
        a.topic = (body as { topic: string | null }).topic?.trim() || null;
        this.emit({ type: "agent.updated", data: { agent: { ...a } } });
        return json(200, a);
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
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/messages$/))) return this.send(m[1], body as { text: string; now?: boolean });
      if ((m = p.match(/^\/v1\/agents\/([^/]+)\/cancel$/))) return this.cancel(m[1]);
      if ((m = p.match(/^\/v1\/runs\/([^/]+)\/(approve|reject|cancel)$/))) {
        const d = s.runs.find((x) => x.run.id === m![1]);
        if (!d) return notFound("run");
        const to = m[2] === "approve" ? "running" : m[2] === "reject" ? "planning" : "cancelled";
        this.emit({ type: "run.status_changed", data: { id: d.run.id, from: d.run.status, to, note: m[2] === "reject" ? ((body as { reason?: string })?.reason ?? null) : null } });
        return json(200, d);
      }
      if ((m = p.match(/^\/v1\/questions\/([^/]+)\/(answer|dismiss)$/))) {
        const q = s.questions.find((x) => x.id === m![1]);
        if (q) {
          q.status = m[2] === "answer" ? "answered" : "dismissed";
          q.answer = (body as { answer?: string })?.answer ?? null;
          this.emit(m[2] === "answer" ? { type: "human.answered", data: { item: q } } : { type: "human.dismissed", data: { id: q.id } });
        }
        return json(m[2] === "answer" ? 200 : 204, q);
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
      return json(204);
    }
    return json(405, { error: { code: "bad_request", message: `${method} ${p}` } });
  }

  /** A message: queued behind a running turn, else answered by a short made-up turn. */
  private send(key: string, req: { text: string; now?: boolean }): HubResponse {
    const a = this.agent(key);
    if (!a) return notFound("agent");
    const message_id = uid();
    const busy = a.status === "running";
    this.emit({ type: "agent.input", data: { id: a.id, message_id, text: req.text, queued: busy, from: { kind: "human" } } });
    if (!busy) this.answer(a, message_id, req.text);
    return json(202, { message_id, queued: busy });
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

  private cancel(key: string): HubResponse {
    const a = this.agent(key);
    const t = a && this.state.transcripts.get(a.id);
    const live = t?.turns.find((x) => x.started_at && !x.completed_at);
    if (!a || !live) return json(409, { error: { code: "conflict", message: "not running" } });
    this.emit({ type: "agent.turn_completed", data: { id: a.id, message_id: live.message_id, stop_reason: "cancelled" } });
    this.emit({ type: "agent.status_changed", data: { id: a.id, from: "running", to: "idle" } });
    return json(204);
  }

  /** Attaches a finished turn's diff (fixtures build transcripts with stats already in). */
  static withDiff(s: MockState, messageId: string, diff: string | undefined) {
    if (diff && turnStat(diff)) s.diffs.set(messageId, diff);
  }
}
