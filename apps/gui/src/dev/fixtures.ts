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
    streaming: true, interrupt: true, mcp: true, multi_session: true, resume: true, fork: true, rollback: true,
    revert_files: false, set_model: true, structured_diff: true, usage: true, rate_limits: true, permission_callback: true, compaction: true,
  },
  "claude-native": {
    streaming: true, interrupt: true, mcp: true, multi_session: false, resume: true, fork: true, rollback: false,
    revert_files: false, set_model: true, structured_diff: false, usage: true, rate_limits: true, permission_callback: true, compaction: true,
  },
  "acp-generic": {
    streaming: true, interrupt: true, mcp: true, multi_session: false, resume: false, fork: false, rollback: false,
    revert_files: false, set_model: false, structured_diff: false, usage: true, rate_limits: false, permission_callback: true, compaction: false,
  },
};

export const capabilitiesOf = (plugin: string): Capabilities => CAPS[driverOf(plugin)];
export const driverOf = (plugin: string): DriverKind => (plugin === "codex" ? "codex-native" : plugin === "claude" ? "claude-native" : "acp-generic");

type AgentInit = {
  name: string;
  plugin: "claude" | "codex" | "mock";
  model?: string;
  role: Role;
  status?: AgentStatus;
  cwd?: string;
  profile?: Partial<AgentProfile>;
  createdMinutesAgo?: number;
};

export function agent(init: AgentInit): Agent {
  return {
    id: uid(),
    name: init.name,
    plugin: init.plugin,
    driver: null,
    model: init.model ?? null,
    role: init.role,
    cwd: init.cwd ?? REPO,
    config: {},
    status: init.status ?? "idle",
    created_at: ago(init.createdMinutesAgo ?? 60 * 24 * 3),
    profile: { display_name: null, title: null, avatar: null, color: null, persona: null, ...init.profile },
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
        plan_approval: "required",
        land: "manual",
        human_ask: "allow",
        ask_timeout_minutes: 30,
        budget: { max_tokens: null, max_usd: 8, max_minutes: 120 },
        spawn: false,
        max_agents: 6,
        delegate: false,
      },
    },
    repo: REPO,
    base_branch: "main",
    branch: "crossagents/oauth-login",
    note: null,
    plan: init.plan,
    landed: null,
    summary: null,
    created_at: ago(init.minutesAgo),
    updated_at: ago(Math.max(0, init.minutesAgo - 30)),
  };
}

export function task(r: Run, p: PlannedTask, init: { status: TaskStatus; assignee?: Agent; template?: Agent; minutesAgo: number; summary?: string; failure?: string; diffStat?: string; progress?: string; checks?: Task["report"]["checks"] }): Task {
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
    },
    parent: null,
    progress: init.progress ?? null,
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

export function transcript(a: Agent, turns: { turn: TranscriptTurn }[], used = 38_000): Transcript {
  return {
    agent: a,
    driver: driverOf(a.plugin),
    capabilities: capabilitiesOf(a.plugin),
    turns: turns.map((t) => t.turn),
    usage: {
      used,
      size: 200_000,
      cost_usd: a.plugin === "claude" ? 0.4213 : null,
      tokens: { input: used * 3, cached_input: used * 2, output: Math.round(used / 4), reasoning_output: Math.round(used / 10), total: used * 3 + Math.round(used / 4) },
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
