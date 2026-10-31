// Workspaces: the repositories the hub works in. The hub has no project registry yet, so they
// are derived from what it records: each run's `repo`, and each agent's `home` — the workspace
// it belongs to (docs/DESIGN.md §4.7), wherever it works now (a task's worktree, say).

import { useSyncExternalStore } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { ACTIVE_RUN, useRunTree } from "./App";
import { api } from "./api";
import { useHubData } from "./hub";
import { homeOf, isChat, projectMembers, standaloneSessions, wsKey, wsName } from "./lib/projectView";
export { homeOf, isChat, wsKey, wsName } from "./lib/projectView";
import { useAgents } from "./components";

export type Workspace = {
  key: string;
  path: string;
  name: string;
  /** Newest first. */
  runs: RunDetail[];
  agents: Agent[];
};

export const wsHref = (key: string) => `#/w/${encodeURIComponent(key)}`;

export function useWorkspaces() {
  const tree = useRunTree();
  const agents = useAgents();
  const runs = tree.data ?? [];
  const all = agents.data ?? [];
  const map = new Map<string, Workspace>();
  const get = (path: string) => {
    const key = wsKey(path);
    let ws = map.get(key);
    if (!ws) {
      ws = { key, path, name: wsName(path), runs: [], agents: [] };
      map.set(key, ws);
    }
    return ws;
  };

  for (const d of runs) get(d.run.repo).runs.push(d);
  const members = projectMembers(all, runs);
  for (const agent of all) if (!isChat(agent) && homeOf(agent)) get(homeOf(agent));
  for (const [key, rows] of members) {
    const ws = map.get(key);
    if (ws) ws.agents = rows.map((row) => row.agent);
  }
  const list = [...map.values()].sort((a, b) => a.key.localeCompare(b.key));
  return { list, standalone: standaloneSessions(all), loaded: !!tree.data && !!agents.data, tree, agents };
}

/** All project runs, including old runs and those without a source or plan. */
export function useProjectRuns(key: string) {
  return useHubData(async () => {
    if (!key) return [];
    const { runs } = await api.listRuns();
    return Promise.all(runs.filter((run) => wsKey(run.repo) === wsKey(key)).map((run) => api.getRun(run.id)));
  }, [key], (e) => e.type.startsWith("run.") || e.type === "task.status_changed" || e.type === "task.handed_off");
}

/** The group of chats outside any project; not a repository, so never offered as a workspace. */
export const CHATS = "__chats";

/**
 * A conversation rather than Work: a chat outside any project, one started in a project (chats
 * are titled by their topic; the planners and workers Work starts are not), or a teammate a
 * conversation recruited.
 */
export function isConversation(agent: Agent, byId: Map<string, Agent>): boolean {
  const seen = new Set<string>();
  for (let a: Agent | undefined = agent; a && !seen.has(a.id); ) {
    if (isChat(a) || a.topic) return true;
    seen.add(a.id);
    const by: unknown = (a.config as Record<string, unknown> | null)?.spawned_by_agent;
    a = typeof by === "string" ? byId.get(by) : undefined;
  }
  return false;
}

/** The project a conversation works in, if it has one. */
export const projectOf = (agent: Agent) => (isChat(agent) ? undefined : wsName(homeOf(agent)));

export const liveRuns =(ws: Workspace) => ws.runs.filter((d) => ACTIVE_RUN.includes(d.run.status)).length;

// ---- The selected workspace ("" = all), remembered per browser. -------------------------

const STORAGE = "crossagents.workspace";
const listeners = new Set<() => void>();
let selected = (() => {
  try {
    return localStorage.getItem(STORAGE) ?? "";
  } catch {
    return "";
  }
})();

export function selectWorkspace(key: string) {
  selected = key;
  try {
    localStorage.setItem(STORAGE, key);
  } catch {
    // Storage may be unavailable; the choice then lasts for this page only.
  }
  listeners.forEach((l) => l());
}

export function useSelectedWorkspace(): string {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => selected,
  );
}
