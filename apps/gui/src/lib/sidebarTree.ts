import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { projectMembers, standaloneSessions, type ProjectMember } from "./projectView.ts";

export type SidebarGroup = {
  key: string;
  name: string;
  members: ProjectMember[];
};

/** Domain views own membership and stable ordering; this adds presentation preferences. */
export function sidebarGroups(
  agents: Agent[],
  runs: RunDetail[],
  projects: readonly { key: string; name: string }[],
  pins: ReadonlySet<string>,
  order: readonly string[] = [],
): SidebarGroup[] {
  const pinned = new Set([...pins].filter((k) => k.startsWith("s:")).map((k) => k.slice(2)));
  const members = projectMembers(agents, runs, pinned);
  const assigned = new Set([...members.values()].flat().map((m) => m.agent.id));
  const rank = (key: string) => (order.includes(key) ? order.indexOf(key) : order.length);
  const taskTitles = new Map(runs.flatMap((d) => d.tasks).map((task) => [`${task.key} ${task.title}`.trim(), task.title]));
  const clean = (m: ProjectMember): ProjectMember => ({ ...m, summary: taskTitles.get(m.summary ?? "") ?? null });
  const groups = projects
    .map((p) => ({
      ...p,
      members: (members.get(p.key) ?? []).filter((m) => !m.agent.archived_at).map(clean),
    }))
    .sort((a, b) => rank(a.key) - rank(b.key) || a.key.localeCompare(b.key));
  const standalone = standaloneSessions(agents)
    .filter((a) => !assigned.has(a.id) && !a.archived_at)
    .sort((a, b) => Number(pinned.has(b.id)) - Number(pinned.has(a.id)));
  if (standalone.length)
    groups.push({
      key: "__chats",
      name: "",
      members: standalone.map((agent) => ({
        agent,
        project: "",
        status: agent.status,
        summary: null,
      })),
    });
  return groups;
}

export function searchGroups(groups: readonly SidebarGroup[], query: string): SidebarGroup[] {
  const q = query.trim().toLocaleLowerCase();
  if (!q) return [...groups];
  return groups
    .map((g) => ({
      ...g,
      members: g.members.filter((m) => [m.agent.name, m.agent.topic, m.summary].some((s) => s?.toLocaleLowerCase().includes(q))),
    }))
    .filter((g) => g.members.length);
}

/** Search reveals matches temporarily without modifying persisted folds. */
export const groupClosed = (collapsed: ReadonlySet<string>, key: string, searching: boolean) => !searching && collapsed.has(key);

export function capped<T>(items: T[], keep: (item: T) => boolean, cap: number, all: boolean): T[] {
  return all ? items : items.filter((item, i) => i < Math.max(0, cap) || keep(item));
}

/** Filesystem paths and old role-only labels do not describe useful work. */
export function summaryText(value: string | null | undefined): string {
  const text = value?.trim() ?? "";
  return /^(?:[A-Za-z]:[\\/]|\/|\\\\|(?:主导|worker|reviewer|planner)$)/i.test(text) ? "" : text;
}
