// Hub-managed background jobs owned by sessions.
//
// Keeps a live mirror of active/recent SessionJobs from `GET /v1/jobs` and `agent.job_updated`
// SSE events. While at least one job is running, polls `/v1/jobs` gently (every 2.5s) so live
// output tail streams smoothly without flooding SQLite's event log.

import { useSyncExternalStore } from "react";
import type { SessionJob } from "@bindings/SessionJob";
import { api } from "./api";
import { onHubEvent } from "./hub";

const EMPTY: readonly SessionJob[] = [];
const POLL_MS = 2500;

const listeners = new Set<() => void>();
const jobsById = new Map<string, SessionJob>();
let allRunningSnapshot: readonly SessionJob[] = EMPTY;
let runningByAgent = new Map<string, readonly SessionJob[]>();
let pollTimer: ReturnType<typeof setInterval> | undefined;
let started = false;
let refreshing = false;

function notify() {
  listeners.forEach((fn) => fn());
}

function recompute() {
  const running: SessionJob[] = [];
  const grouped = new Map<string, SessionJob[]>();
  for (const job of jobsById.values()) {
    if (job.status !== "running") continue;
    running.push(job);
    const list = grouped.get(job.agent_id) ?? [];
    list.push(job);
    grouped.set(job.agent_id, list);
  }
  running.sort((a, b) => b.started_at.localeCompare(a.started_at));
  for (const list of grouped.values()) {
    list.sort((a, b) => b.started_at.localeCompare(a.started_at));
  }
  allRunningSnapshot = running.length ? running : EMPTY;
  runningByAgent = grouped;
  syncPolling();
  notify();
}

function syncPolling() {
  if (allRunningSnapshot.length > 0) {
    if (!pollTimer) {
      pollTimer = setInterval(() => {
        void refreshJobs();
      }, POLL_MS);
    }
  } else if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = undefined;
  }
}

export async function refreshJobs(): Promise<void> {
  if (refreshing) return;
  refreshing = true;
  try {
    const res = await api.listJobs();
    jobsById.clear();
    for (const job of res.jobs) {
      jobsById.set(job.id, job);
    }
    recompute();
  } catch {
    // Older hubs or disconnected state: stay quiet until connected.
  } finally {
    refreshing = false;
  }
}

export function startJobs() {
  if (started) return;
  started = true;
  void refreshJobs();
  onHubEvent((e) => {
    if (e.type === "agent.job_updated") {
      jobsById.set(e.data.job.id, e.data.job);
      recompute();
    } else if (e.type === "hub.started") {
      void refreshJobs();
    } else if (e.type === "agent.removed") {
      let removed = false;
      for (const [id, job] of jobsById) {
        if (job.agent_id === e.data.id) {
          jobsById.delete(id);
          removed = true;
        }
      }
      if (removed) recompute();
    }
  });
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** All currently running hub-managed background jobs across every session. */
export function useRunningJobs(): readonly SessionJob[] {
  return useSyncExternalStore(subscribe, () => allRunningSnapshot, () => EMPTY);
}

/** Currently running hub-managed background jobs owned by `agentId`. */
export function useAgentRunningJobs(agentId?: string): readonly SessionJob[] {
  return useSyncExternalStore(
    subscribe,
    () => (agentId ? (runningByAgent.get(agentId) ?? EMPTY) : EMPTY),
    () => EMPTY,
  );
}

/** Non-hook snapshot lookup for filter/sort helpers. */
export function agentHasRunningJobs(agentId: string): boolean {
  return (runningByAgent.get(agentId)?.length ?? 0) > 0;
}
