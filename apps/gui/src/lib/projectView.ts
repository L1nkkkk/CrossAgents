import type { Agent } from "@bindings/Agent";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { Task } from "@bindings/Task";
import { runState } from "./taskState.ts";

export const homeOf = (agent: Agent) => agent.home || agent.cwd;
export const wsKey = (path: string) => path.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
export const wsName = (path: string) => path.replace(/\\/g, "/").replace(/\/+$/, "").split("/").pop() || path;

export function isChat(agent: Agent): boolean {
  const parts = homeOf(agent).replace(/\\/g, "/").replace(/\/+$/, "").split("/");
  const [home, chats, last] = parts.slice(-3);
  return chats === "chats" && (home === ".crossagents" || last === agent.id || last.toLowerCase() === agent.name.toLowerCase());
}

/** A title made from the request often opens with a whole file path; the file's name says enough in a one-line row. */
export function shortTitle(title: string): string {
  return title.replace(/(?:[A-Za-z]:[\\/]|\\\\)\S*[\\/]/g, "");
}

/**
 * What this session did last in these runs, newest first: a run it coordinated, a task it took or
 * reviewed. Read from the records, so it survives a restart and needs no transcript.
 */
export function lastWork(agentId: string, runs: readonly RunDetail[]): { text: string; at: string } | null {
  let last: { text: string; at: string } | null = null;
  const consider = (text: string | null | undefined, at: string | null | undefined) => {
    if (text && (!last || (at ?? "") > last.at)) last = { text, at: at ?? "" };
  };
  for (const { run, tasks } of runs) {
    for (const task of tasks) {
      const took = task.assignee === agentId || task.handoffs?.some((hand) => hand.to === agentId);
      if (took) consider(task.title, task.updated_at);
    }
    for (const task of tasks) if (task.reviewer === agentId) consider(`评审：${task.title}`, task.updated_at);
    if (run.planner === agentId) consider(run.title && shortTitle(run.title), run.updated_at);
  }
  return last;
}

/** The run this session leads now, and the task it last took or reviewed; what its row can add. */
export type SessionWork = { lead: RunDetail | null; task: { task: Task; tasks: Task[] } | null };
export function sessionWork(agentId: string, runs: readonly RunDetail[]): SessionWork {
  const out: SessionWork = { lead: null, task: null };
  for (const detail of runs) {
    if (detail.run.planner === agentId && runState(detail.run.status).bucket !== "ended" && detail.run.created_at > (out.lead?.run.created_at ?? "")) out.lead = detail;
    for (const task of detail.tasks) {
      if ((task.assignee === agentId || task.reviewer === agentId) && task.updated_at > (out.task?.task.updated_at ?? "")) out.task = { task, tasks: detail.tasks };
    }
  }
  return out;
}

export type Executor = { executor: Agent | undefined; template: Agent | undefined; fresh: boolean };
/** The recorded executor and configuration source. Missing records remain unknown. */
export function executorOf(work: Task | Run, agents: Agent[], side: "worker" | "reviewer" = "worker"): Executor {
  const find = (id: string | null) => agents.find((a) => a.id === id);
  if ("planner" in work) {
    // lead_note records a copied lead but does not identify its template.
    return { executor: find(work.planner), template: undefined, fresh: !!work.lead_note };
  }
  const reviewer = side === "reviewer";
  return { executor: find(reviewer ? work.reviewer : work.assignee), template: find(reviewer ? work.reviewer_template : work.worker_template), fresh: reviewer ? work.reviewer_fresh : work.worker_fresh };
}

export type ProjectMember = { agent: Agent; project: string; summary: string | null; status: Agent["status"] };
/** Projects keyed by normalized path. Pins are agent IDs; no activity affects order. */
export function projectMembers(agents: Agent[], runs: RunDetail[], pinned: ReadonlySet<string> = new Set()): Map<string, ProjectMember[]> {
  const groups = new Map<string, Map<string, ProjectMember>>();
  const tasks = runs.flatMap((d) => d.tasks.map((task) => ({ task, project: wsKey(d.run.repo) })))
    .sort((a, b) => a.task.created_at.localeCompare(b.task.created_at) || a.task.id.localeCompare(b.task.id));
  const add = (agent: Agent, project: string) => {
    if (!project) return;
    let group = groups.get(project);
    if (!group) groups.set(project, group = new Map());
    if (group.has(agent.id)) return;
    const relevant = tasks.filter((t) => t.project === project);
    const current = relevant.find(({ task }) => task.assignee === agent.id) ?? relevant.find(({ task }) => task.reviewer === agent.id);
    group.set(agent.id, { agent, project, status: agent.status, summary: current ? `${current.task.key} ${current.task.title}`.trim() : agent.profile.title });
  };
  for (const agent of agents) if (!isChat(agent)) add(agent, wsKey(homeOf(agent)));
  const byId = new Map(agents.map((a) => [a.id, a]));
  for (const { run, tasks: runTasks } of runs) {
    for (const id of [run.planner, ...runTasks.flatMap((t) => [t.assignee, t.reviewer])]) {
      const agent = id ? byId.get(id) : undefined;
      if (agent) add(agent, wsKey(run.repo));
    }
  }
  return new Map([...groups].sort(([a], [b]) => a.localeCompare(b)).map(([key, members]) => [key, [...members.values()].sort((a, b) => Number(pinned.has(b.agent.id)) - Number(pinned.has(a.agent.id)) || a.agent.created_at.localeCompare(b.agent.created_at) || a.agent.id.localeCompare(b.agent.id))]));
}

/** Sessions outside any project: chats, and records with no home or cwd, which belong nowhere else. */
export function standaloneSessions(agents: Agent[]): Agent[] {
  return agents.filter((a) => isChat(a) || !homeOf(a)).slice().sort((a, b) => a.created_at.localeCompare(b.created_at) || a.id.localeCompare(b.id));
}
