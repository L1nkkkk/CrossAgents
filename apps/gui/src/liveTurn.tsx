// A session's running turn, where its conversation is not: the session header and the sidebar
// row (docs/TODO.md §四 ②). The three facts are the same ones the live line under the turn
// shows — how long this turn has run, what it does now, and how long since it last moved — from
// the same pure functions (lib/workGroups.ts `liveProgress`), so the places never disagree.
//
// Nothing here invents a progress figure: a turn without a known step count says what it is
// doing, and only a task's own counts (its run's tasks, its acceptance checks) are shown.

import { useEffect, useState } from "react";
import type { Agent } from "@bindings/Agent";
import type { Task } from "@bindings/Task";
import type { TranscriptItem } from "@bindings/TranscriptItem";
import { noteTurn, pulseKnown, usePulse, useRateSamples } from "./activity";
import { api } from "./api";
import { StateWord } from "./parts";
import { checksPassed, progress, taskState } from "./lib/taskState";
import { formatRate, tokenRate } from "./lib/tokenRate";
import { liveKey, liveProgress } from "./lib/workGroups";

/** Seconds-resolution time, so a duration counts up while the turn runs. */
function useNow(): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  return now;
}

/** When this page last saw the turn produce something: the key changes only on new output. */
function useSeen(items: TranscriptItem[]): number {
  const key = liveKey(items);
  const [seen, setSeen] = useState(() => ({ key, at: Date.now() }));
  useEffect(() => setSeen((s) => (s.key === key ? s : { key, at: Date.now() })), [key]);
  return seen.at;
}

/**
 * How fast the running turn writes, `42 tok/s`, over the last ~10 seconds of the agent's
 * `agent.usage` counts. Nothing at all when it is not known (a driver without token counts, a
 * turn just started) or nothing came out lately: the pulsing dot beside it still says it runs.
 */
function useTokenRate(agentId: string | undefined, now: number): string | undefined {
  const samples = useRateSamples(agentId);
  return samples ? formatRate(tokenRate(samples, now)) : undefined;
}

export type LiveTurn = {
  /** The session, whose output speed the line shows when its driver reports tokens. */
  agentId?: string;
  /** The turn's start, ms. */
  since: number;
  items: TranscriptItem[];
  /** The task this session works on for a run now, and the run's tasks for its progress. */
  task?: Task;
  siblings?: Task[];
};

/**
 * The session header's line: `••• 2m 31s · 运行命令 · #p1 运行中 · 3/5 已合并 · 2/3 检查通过`.
 * A quiet turn says how long in place of nothing else, in warning colour.
 */
export function TurnLive({ agentId, since, items, task, siblings }: LiveTurn) {
  const now = useNow();
  const at = useSeen(items);
  const rate = useTokenRate(agentId, now);
  const p = liveProgress(items, { since, at, now });
  const run = task && siblings?.length ? progress(siblings) : "";
  const checks = task ? checksPassed(task.report.checks) : "";
  const line = [p.elapsed && `已跑 ${p.elapsed}`, rate, p.activity, task && `#${task.key}`, task?.progress, run, checks, p.quietText].filter(Boolean).join(" · ");
  return (
    <div className={`live-bar ${p.quietText ? "quiet" : ""}`} role="group" aria-label="本轮进度" title={line}>
      <span className="busy-dots" aria-hidden="true">
        <i />
        <i />
        <i />
      </span>
      {p.elapsed && <span className="live-elapsed">{p.elapsed}</span>}
      {rate && (
        <span className="live-rate" title="最近约 10 秒的输出速度">
          {rate}
        </span>
      )}
      <span className="live-what">{p.activity}</span>
      {task && (
        <a className="chip live-task" href={`#/runs/${task.run_id}/tasks/${task.id}`} title={task.title}>
          <code className="bare">#{task.key}</code>
          <StateWord state={taskState(task, siblings)} />
        </a>
      )}
      {task?.progress && (
        <span className="live-note" title={task.progress}>
          {task.progress}
        </span>
      )}
      {run && (
        <a className="live-run" href={`#/runs/${task!.run_id}`} title="这个 run 的进度">
          {run}
        </a>
      )}
      {checks && <span className="live-checks">{checks}</span>}
      {p.quietText && <span className="live-quiet">{p.quietText}</span>}
    </div>
  );
}

/**
 * A sidebar row's suffix: `2m 31s · 运行命令`, or, once quiet, just `5 分钟没动静` — the number
 * worth noticing when someone wonders whether a session is stuck. The title carries all three.
 */
export function RowLive({ since, at, items }: { since?: number; at?: number; items: TranscriptItem[] }) {
  const now = useNow();
  const p = liveProgress(items, { since, at: at ?? now, now });
  const text = p.quietText ?? [p.elapsed, p.activity].filter(Boolean).join(" · ");
  const full = [p.elapsed, p.activity, p.quietText].filter(Boolean).join(" · ");
  return (
    <span className={`row-live ${p.quietText ? "quiet" : ""}`} title={full}>
      {text}
    </span>
  );
}

/**
 * The end of a sidebar row's first line: how long the turn has run, or how long it has been
 * quiet; beside the pulsing dot, how fast it writes when that is known (`42 tok/s`).
 */
export function RowElapsed({ agentId, since, at, items }: { agentId?: string; since?: number; at?: number; items: TranscriptItem[] }) {
  const now = useNow();
  const rate = useTokenRate(agentId, now);
  const p = liveProgress(items, { since, at: at ?? now, now });
  return (
    <span className={`member-elapsed ${p.quietText ? "quiet" : ""}`} title={[p.elapsed, rate, p.activity, p.quietText].filter(Boolean).join(" · ")}>
      <i className="working-dot" />
      {rate && !p.quietText && <span className="member-rate">{rate}</span>}
      {p.quietText ?? p.elapsed}
    </span>
  );
}

/**
 * The row of an agent whose status says it is running, with the facts its pulse has. Undefined
 * while there is nothing live to show (the turn ended, or this page has not seen it yet).
 */
export function useRowLive(agent: Agent): { since?: number; at?: number; items: TranscriptItem[] } | undefined {
  const pulse = usePulse(agent.id);
  if (agent.status !== "running" || !pulse || pulse.over) return undefined;
  return pulse;
}

/** Agents already asked for their transcript: one request per running turn, not per render. */
const asked = new Set<string>();

/**
 * A running turn that began before the page loaded has no start in the event stream. Its
 * transcript does, so ask for it once: the row then shows the real elapsed time instead of
 * nothing. A failed request is not retried while that turn keeps running; the next turn is
 * asked about again.
 */
export function usePulseSeeds(agents: Agent[]) {
  const running = agents.filter((a) => a.status === "running" && !pulseKnown(a.id));
  const key = running.map((a) => a.id).join("|");
  useEffect(() => {
    // A session that is no longer running may run again later: let that turn be asked about too.
    for (const agent of agents) if (agent.status !== "running") asked.delete(agent.id);
    for (const agent of running) {
      if (asked.has(agent.id)) continue;
      asked.add(agent.id);
      api.transcript(agent.id).then(
        (t) => {
          const turn = t.turns.findLast((x) => x.started_at && !x.completed_at);
          if (turn?.started_at) noteTurn(agent.id, Date.parse(turn.started_at), turn.items);
        },
        () => {},
      );
    }
    // Only the set of running ids matters; `running` and `agents` are rebuilt every render.
  }, [key]);
}
