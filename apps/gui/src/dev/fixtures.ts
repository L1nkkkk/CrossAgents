// Shared made-up entities for `npm run preview:mock` (docs/tasks/V6-GUI.md §3): the team, the
// repositories, and builders for agents, runs, tasks and transcripts. Each view's own data is in
// its `views/*.mock.ts`; scenarios.ts puts them together.

import type { Agent } from "@bindings/Agent";
import type { AgentProfile } from "@bindings/AgentProfile";
import type { AgentStatus } from "@bindings/AgentStatus";
import type { Capabilities } from "@bindings/Capabilities";
import type { DriverKind } from "@bindings/DriverKind";
import type { MessageSource } from "@bindings/MessageSource";
import type { PlannedTask } from "@bindings/PlannedTask";
import type { Role } from "@bindings/Role";
import type { Run } from "@bindings/Run";
import type { RunStatus } from "@bindings/RunStatus";
import type { Task } from "@bindings/Task";
import type { TaskStatus } from "@bindings/TaskStatus";
import type { ToolDetail } from "@bindings/ToolDetail";
import type { ToolStatus } from "@bindings/ToolStatus";
import type { Transcript } from "@bindings/Transcript";
import type { TranscriptItem } from "@bindings/TranscriptItem";
import type { TranscriptTurn } from "@bindings/TranscriptTurn";
import type { TurnDiffStat } from "@bindings/TurnDiffStat";
import { turnStat } from "../lib/diff";

// ---- Time and ids -------------------------------------------------------------------------

/** `minutes` ago, as the hub writes times. */
export const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

let counter = 0;
/** A fresh UUID, as the hub's ids are. */
export function uid(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) return crypto.randomUUID();
  counter += 1;
  const hex = (Date.now().toString(16).padStart(12, "0") + counter.toString(16).padStart(20, "0")).slice(-32);
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

// ---- Repositories -------------------------------------------------------------------------

export const REPO = "C:/work/shop";
export const NOTES_REPO = "C:/work/notes";

// ---- Agents -------------------------------------------------------------------------------

const CAPS: Record<DriverKind, Capabilities> = {
  "codex-native": {
    streaming: true, interrupt: true, mcp: true, multi_session: true, resume: true, move_cwd: true, fork: true, rollback: true,
    revert_files: false, set_model: true, structured_diff: true, usage: true, rate_limits: true, permission_callback: true, compaction: true,
    attachments: { image: true, pdf: false, text: true },
  },
  "claude-native": {
    streaming: true, interrupt: true, mcp: true, multi_session: false, resume: true, move_cwd: true, fork: true, rollback: false,
    revert_files: false, set_model: true, structured_diff: false, usage: true, rate_limits: true, permission_callback: true, compaction: true,
    attachments: { image: true, pdf: true, text: true },
  },
  "codebuddy-native": {
    streaming: true, interrupt: true, mcp: true, multi_session: false, resume: true, move_cwd: true, fork: true, rollback: false,
    revert_files: false, set_model: true, structured_diff: false, usage: true, rate_limits: true, permission_callback: true, compaction: true,
    attachments: { image: true, pdf: true, text: true },
  },
  "with-native": {
    streaming: true, interrupt: true, mcp: true, multi_session: false, resume: true, move_cwd: false, fork: false, rollback: false,
    revert_files: false, set_model: true, structured_diff: false, usage: true, rate_limits: false, permission_callback: false, compaction: false,
    attachments: { image: true, pdf: false, text: true },
  },
  "acp-generic": {
    streaming: true, interrupt: true, mcp: true, multi_session: false, resume: false, move_cwd: false, fork: false, rollback: false,
    revert_files: false, set_model: false, structured_diff: false, usage: true, rate_limits: false, permission_callback: true, compaction: false,
    attachments: null,
  },
};

export const capabilitiesOf = (plugin: string): Capabilities => CAPS[driverOf(plugin)];
export const driverOf = (plugin: string): DriverKind =>
  plugin === "codex" ? "codex-native" : plugin === "claude" ? "claude-native" : plugin === "codebuddy" ? "codebuddy-native" : plugin === "with" ? "with-native" : "acp-generic";

type AgentInit = {
  name: string;
  plugin: "claude" | "codex" | "codebuddy" | "mock";
  model?: string;
  role: Role;
  status?: AgentStatus;
  cwd?: string;
  /** The workspace it belongs to; its cwd unless that is a run's worktree. */
  home?: string;
  profile?: Partial<AgentProfile>;
  createdMinutesAgo?: number;
  /** A conversation title (chat mode). */
  topic?: string;
  /** The conversation that recruited it (chat mode). */
  recruitedBy?: string;
};

export function agent(init: AgentInit): Agent {
  return {
    id: uid(),
    // The name is what people read ("Ada"); routes and lookups go by id or by name, any case.
    name: init.profile?.display_name ?? init.name,
    name_source: "chosen",
    topic: init.topic ?? null,
    archived_at: null,
    plugin: init.plugin,
    driver: null,
    model: init.model ?? null,
    role: init.role,
    home: init.home ?? (init.cwd && !init.cwd.includes("/worktrees/") ? init.cwd : REPO),
    cwd: init.cwd ?? REPO,
    config: init.recruitedBy ? { spawned_by_agent: init.recruitedBy } : {},
    status: init.status ?? "idle",
    created_at: ago(init.createdMinutesAgo ?? 60 * 24 * 3),
    profile: { title: null, avatar: null, color: null, persona: null, ...init.profile, display_name: null },
  };
}

/** The standing team in REPO: a planner, two workers, a reviewer. */
export function team() {
  return {
    ada: agent({ name: "ada", plugin: "claude", model: "claude-opus-4-1", role: "planner", profile: { display_name: "Ada", title: "架构师", color: "indigo" } }),
    rex: agent({ name: "rex", plugin: "codex", model: "gpt-5-codex", role: "worker", profile: { display_name: "Rex", title: "Coder", color: "sky" } }),
    mia: agent({ name: "mia", plugin: "claude", model: "claude-sonnet-4-5", role: "worker", profile: { display_name: "Mia", title: "设计师", color: "rose" } }),
    sam: agent({ name: "sam", plugin: "claude", model: "claude-opus-4-1", role: "reviewer", profile: { display_name: "Sam", title: "安全审核专员", color: "amber" } }),
  };
}

/** A task's worker: a fresh clone of a teammate, as the orchestrator makes them. */
export function clone(of: Agent, suffix: string, status: AgentStatus, cwd: string): Agent {
  return { ...agent({ name: `${of.name}-${suffix}`, plugin: of.plugin as AgentInit["plugin"], model: of.model ?? undefined, role: of.role, status, cwd, createdMinutesAgo: 40 }), profile: { ...of.profile } };
}

// ---- Runs and tasks -----------------------------------------------------------------------

export function planned(key: string, title: string, goal: string, assignee: string | null, deps: string[] = [], allow: string[] = []): PlannedTask {
  return {
    key,
    title,
    spec: { goal, scope: { allow, deny: [] }, interfaces: [], acceptance: ["cargo test -p auth", "npm test"], references: [] },
    depends_on: deps,
    assignee,
    reviewer: null,
    fresh: true,
    restore: false,
    reviewer_fresh: true,
  };
}

export function run(init: { title: string; request: string; planner: Agent; worker: Agent; team: Agent[]; status: RunStatus; plan: PlannedTask[]; minutesAgo: number }): Run {
  return {
    id: uid(),
    title: init.title,
    request: init.request,
    planner: init.planner.id,
    status: init.status,
    config: {
      worker: init.worker.id,
      escalate: null,
      reviewer: null,
      auto_approve: false,
      max_parallel: 3,
      max_attempts: 2,
      team: init.team.map((a) => a.id),
      policy: {
        continuation: null,
        plan_approval: "required",
        land: "manual",
        human_ask: "allow",
        ask_timeout_minutes: 30,
        budget: { max_tokens: null, max_usd: 8, max_minutes: 120 },
        spawn: false,
        max_agents: 6,
        spawn_models: null,
        delegate: false,
        approvals: "guard",
      },
    },
    repo: REPO,
    base_branch: "main",
    branch: "crossagents/oauth-login",
    in_place: false,
    continuation: null,
    lead_note: null,
    note: null,
    plan: init.plan,
    landed: null,
    summary: null,
    checks: [],
    checks_state: null,
    created_at: ago(init.minutesAgo),
    updated_at: ago(Math.max(0, init.minutesAgo - 30)),
  };
}

export function task(r: Run, p: PlannedTask, init: { status: TaskStatus; assignee?: Agent; template?: Agent; minutesAgo: number; summary?: string; failure?: string; diffStat?: string; progress?: string; checks?: Task["report"]["checks"]; interrupted?: boolean; unchecked?: boolean; forced?: string }): Task {
  return {
    id: uid(),
    run_id: r.id,
    key: p.key,
    title: p.title,
    spec: p.spec,
    depends_on: p.depends_on,
    issuer: r.planner,
    assignee: init.assignee?.id ?? null,
    reviewer: null,
    worker_template: init.template?.id ?? null,
    reviewer_template: null,
    worker_fresh: true,
    worker_restore: false,
    reviewer_fresh: true,
    status: init.status,
    attempts: 1,
    escalated: false,
    nudges: 0,
    branch: `crossagents/oauth-login/${p.key}`,
    worktree: `${REPO}/.crossagents/worktrees/${p.key}`,
    report: {
      summary: init.summary ?? null,
      checks: init.checks ?? [],
      diff_stat: init.diffStat ?? null,
      failure: init.failure ?? null,
      question: null,
      question_message: null,
      review: null,
      rework: [],
      verification: null,
      verified_sha: init.checks?.length ? "9f2c1ab" : null,
      interrupted: init.interrupted ?? false,
      unchecked: init.unchecked ?? !init.checks?.length,
      forced: init.forced ?? null,
    },
    parent: null,
    progress: init.progress ?? null,
    handoffs: [],
    created_at: ago(init.minutesAgo),
    updated_at: ago(Math.max(0, init.minutesAgo - 5)),
  };
}

// ---- Transcripts --------------------------------------------------------------------------

export const tool = (title: string, kind: string, status: ToolStatus = "completed", detail?: Partial<ToolDetail>, output?: string): TranscriptItem => ({
  type: "tool_call",
  tool_call_id: uid(),
  title,
  kind,
  status,
  ...(output !== undefined ? { output } : {}),
  ...(detail ? { detail: { changes: [], ...detail } } : {}),
});
export const say = (text: string): TranscriptItem => ({ type: "output", stream: "text", text });
export const think = (text: string): TranscriptItem => ({ type: "output", stream: "thought", text });

/** A finished (or, with `running`, running) turn; `diff` is the unified diff it made. */
export function turn(init: {
  input: string;
  from?: MessageSource;
  minutesAgo: number;
  took?: number;
  items: TranscriptItem[];
  diff?: string;
  source?: TurnDiffStat["source"];
  running?: boolean;
  queued?: boolean;
}): { turn: TranscriptTurn; diff?: string } {
  const started = ago(init.minutesAgo);
  const stat = init.diff ? turnStat(init.diff, init.source ?? "driver") : undefined;
  return {
    turn: {
      message_id: uid(),
      input: init.input,
      from: init.from ?? { kind: "human" },
      sent_at: started,
      started_at: init.queued ? null : started,
      completed_at: init.queued || init.running ? null : ago(Math.max(0, init.minutesAgo - (init.took ?? 2))),
      stop_reason: init.queued || init.running ? null : "end_turn",
      items: init.items,
      ...(stat ? { diff: stat } : {}),
    },
    diff: init.diff,
  };
}

/** A /context-like breakdown of `used` tokens, shaped like Claude Code 2.1.308 answers it. */
export function contextParts(used: number, size: number) {
  const fixed = { system_prompt: 2_848, system_tools: 14_200, mcp_tools: 3_100, skills: 2_505 };
  const messages = Math.max(0, used - Object.values(fixed).reduce((a, b) => a + b, 0));
  const buffer = 33_000;
  return [
    { id: "system_prompt", name: "System prompt", tokens: fixed.system_prompt, kind: "used" },
    { id: "system_tools", name: "System tools", tokens: fixed.system_tools, kind: "used" },
    { id: "system_tools_deferred", name: "System tools (deferred)", tokens: 20_034, kind: "deferred" },
    { id: "mcp_tools", name: "MCP tools", tokens: fixed.mcp_tools, kind: "used" },
    { id: "skills", name: "Skills", tokens: fixed.skills, kind: "used" },
    { id: "messages", name: "Messages", tokens: messages, kind: "used" },
    { id: "autocompact_buffer", name: "Autocompact buffer", tokens: buffer, kind: "buffer" },
    { id: "free_space", name: "Free space", tokens: size - used - buffer, kind: "free" },
  ];
}

export function transcript(a: Agent, turns: { turn: TranscriptTurn }[], used = 38_000): Transcript {
  return {
    agent: a,
    driver: driverOf(a.plugin),
    capabilities: capabilitiesOf(a.plugin),
    turns: turns.map((t) => t.turn),
    switches: [],
    usage: {
      used,
      size: 200_000,
      cost_usd: a.plugin === "claude" ? 0.4213 : null,
      tokens: { input: used * 3, cached_input: used * 2, output: Math.round(used / 4), reasoning_output: Math.round(used / 10), total: used * 3 + Math.round(used / 4) },
      // Claude Code says what fills the window (get_context_usage); the others do not.
      ...(a.plugin === "claude" ? { parts: contextParts(used, 200_000) } : {}),
    },
    last_seq: 0,
  };
}

/** A unified diff of one file: `remove` lines out, `add` lines in, under a hunk at `line`. */
export function fileDiff(path: string, line: number, remove: string[], add: string[], context: string[] = []): string {
  const isNew = remove.length === 0 && context.length === 0 && line === 0;
  const head = [
    `diff --git a/${path} b/${path}`,
    isNew ? "new file mode 100644" : "index 3f2a1c4..9b8e7d2 100644",
    isNew ? "--- /dev/null" : `--- a/${path}`,
    `+++ b/${path}`,
    `@@ -${isNew ? 0 : line},${remove.length + context.length} +${isNew ? 1 : line},${add.length + context.length} @@`,
  ];
  return [...head, ...context.map((c) => ` ${c}`), ...remove.map((r) => `-${r}`), ...add.map((a) => `+${a}`), ""].join("\n");
}
