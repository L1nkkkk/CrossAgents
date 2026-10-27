// Workspaces: the repositories the hub works in. The hub has no project registry yet, so they
// are derived from what it records: each run's `repo`, and each agent's `home` — the workspace
// it belongs to (docs/DESIGN.md §4.7), wherever it works now (a task's worktree, say).

import { useSyncExternalStore } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { ACTIVE_RUN, runAgents, useRunTree } from "./App";
import { useAgents } from "./components";

export type Workspace = {
  key: string;
  path: string;
  name: string;
  /** Newest first. */
  runs: RunDetail[];
  agents: Agent[];
};

/** The workspace an agent belongs to; its cwd in records from before homes. */
export const homeOf = (agent: Agent) => agent.home || agent.cwd;

/** Paths compare case-insensitively with either slash (Windows). */
export const wsKey = (path: string) => path.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();

export const wsName = (path: string) => path.replace(/\\/g, "/").replace(/\/+$/, "").split("/").pop() || path;

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

  const placed = new Set<string>();
  for (const d of runs) {
    const ws = get(d.run.repo);
    ws.runs.push(d);
    for (const agent of runAgents(d, all)) {
      if (placed.has(agent.id)) continue;
      placed.add(agent.id);
      ws.agents.push(agent);
    }
  }
  // Every conversation is listed under 对话, in a project or not; a project group holds only
  // its Work (runs, and the planners and workers they use). The project still counts as one
  // the hub works in, so it is offered when starting something.
  const chats: Workspace = { key: CHATS, path: "", name: "对话", runs: [], agents: [] };
  const byId = new Map(all.map((a) => [a.id, a]));
  for (const agent of all) {
    if (placed.has(agent.id)) continue;
    const group = isConversation(agent, byId) ? chats : get(homeOf(agent));
    if (!isChat(agent)) get(homeOf(agent));
    group.agents.push(agent);
  }

  // Busiest first, then by name; chats outside any project come first, like Claude Code's.
  const list = [...map.values()].sort((a, b) => liveRuns(b) - liveRuns(a) || a.name.localeCompare(b.name));
  if (chats.agents.length) list.unshift(chats);
  return { list, loaded: !!tree.data && !!agents.data, tree, agents };
}

/** The group of chats outside any project; not a repository, so never offered as a workspace. */
export const CHATS = "__chats";

/**
 * A chat outside any project: the hub made its directory, `<home>/chats/<id>`. Older chats were
 * named by the agent's name, which may since have changed; under the default home
 * (`.crossagents`) any directory there is one.
 */
export function isChat(agent: Agent): boolean {
  const parts = homeOf(agent).replace(/\\/g, "/").replace(/\/+$/, "").split("/");
  const [home, chats, last] = parts.slice(-3);
  if (chats !== "chats") return false;
  return home === ".crossagents" || last === agent.id || last.toLowerCase() === agent.name.toLowerCase();
}

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
