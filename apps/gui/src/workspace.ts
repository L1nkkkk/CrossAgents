// Workspaces: the repositories the hub works in. The hub has no project registry yet, so they
// are derived from what it records: each run's `repo`, and each agent's `cwd`. An agent that
// belongs to a run counts under that run's repo (worker clones live in worktrees elsewhere).

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
  const chats: Workspace = { key: CHATS, path: "", name: "对话", runs: [], agents: [] };
  for (const agent of all) {
    if (placed.has(agent.id)) continue;
    if (isChat(agent)) chats.agents.push(agent);
    else get(agent.cwd).agents.push(agent);
  }

  // Busiest first, then by name; chats outside any project come first, like Claude Code's.
  const list = [...map.values()].sort((a, b) => liveRuns(b) - liveRuns(a) || a.name.localeCompare(b.name));
  if (chats.agents.length) list.unshift(chats);
  return { list, loaded: !!tree.data && !!agents.data, tree, agents };
}

/** The group of chats outside any project; not a repository, so never offered as a workspace. */
export const CHATS = "__chats";

/** A chat outside any project: the hub made its directory, `<home>/chats/<name>`. */
export function isChat(agent: Agent): boolean {
  const parts = agent.cwd.replace(/\\/g, "/").replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] === agent.name && parts[parts.length - 2] === "chats";
}

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
