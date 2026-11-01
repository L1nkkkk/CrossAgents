import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { lastWork, projectMembers, standaloneSessions, wsKey, type ProjectMember } from "./projectView.ts";

/** A sidebar row: the member, and when the work its summary names last moved. */
export type SidebarMember = ProjectMember & { lastAt: string | null };

export type SidebarGroup = {
  key: string;
  name: string;
  members: SidebarMember[];
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
  // Newest first, and only pins move a row: a recent session is not buried under old ones behind
  // 更多, and neither streaming nor a status change reorders the list.
  const newestFirst = (a: Agent, b: Agent) =>
    Number(pinned.has(b.id)) - Number(pinned.has(a.id)) || b.created_at.localeCompare(a.created_at) || a.id.localeCompare(b.id);
  const withWork = (m: ProjectMember, project: readonly RunDetail[]): SidebarMember => {
    const work = lastWork(m.agent.id, project);
    return { ...m, summary: work?.text ?? null, lastAt: work?.at || null };
  };
  const groups = projects
    .map((p) => {
      const project = runs.filter((d) => wsKey(d.run.repo) === p.key);
      return {
        ...p,
        members: (members.get(p.key) ?? [])
          .filter((m) => !m.agent.archived_at)
          .sort((a, b) => newestFirst(a.agent, b.agent))
          .map((m) => withWork(m, project)),
      };
    })
    .sort((a, b) => rank(a.key) - rank(b.key) || a.key.localeCompare(b.key));
  const standalone = standaloneSessions(agents)
    .filter((a) => !assigned.has(a.id) && !a.archived_at)
    .sort(newestFirst);
  if (standalone.length)
    groups.push({
      key: "__chats",
      name: "",
      members: standalone.map((agent) => ({
        agent,
        project: "",
        status: agent.status,
        summary: null,
        lastAt: null,
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

/** How long since `iso`, in the few characters a row's end has: 刚刚, 40 分, 3 时, 2 天. */
export function shortSince(iso: string, now: number): string {
  const minutes = (now - Date.parse(iso)) / 60_000;
  if (!Number.isFinite(minutes)) return "";
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${Math.floor(minutes)} 分`;
  if (minutes < 60 * 24) return `${Math.floor(minutes / 60)} 时`;
  return `${Math.floor(minutes / (60 * 24))} 天`;
}

/** Filesystem paths and old role-only labels do not describe useful work. */
export function summaryText(value: string | null | undefined): string {
  const text = value?.trim() ?? "";
  return /^(?:[A-Za-z]:[\\/]|\/|\\\\|(?:主导|worker|reviewer|planner)$)/i.test(text) ? "" : text;
}
