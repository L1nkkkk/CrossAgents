// A sidebar row's hover card: everything the row has no room for. Never takes the pointer, so moving
// down the list only swaps which card shows.

import { useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { Agent } from "@bindings/Agent";
import type { Task } from "@bindings/Task";
import type { TranscriptItem } from "@bindings/TranscriptItem";
import { Avatar, ModelMark, ago } from "./components";
import { t } from "./lib/i18n";
import type { SessionWork } from "./lib/projectView";
import { checksPassed, progress, taskState } from "./lib/taskState";
import type { TodoItem } from "./lib/todos";
import { stepLine } from "./lib/workGroups";
import { RowElapsed } from "./liveTurn";

const WIDTH = 300;
const GAP = 10;
/** Below this the window has no room beside the sidebar; the row's text has to do. */
const MIN_WIDTH = 180;
const PEEK_STEPS = 3;

/** A run's tasks in plan order, one segment each, coloured by the word taskState gives it. */
export function TaskSegments({ tasks }: { tasks: readonly Task[] }) {
  return (
    <span className="task-segs" aria-hidden="true">
      {tasks.map((task) => {
        const state = taskState(task, [...tasks]);
        return <i key={task.id} className={`tone-${state.tone} ${state.live ? "live" : ""}`} />;
      })}
    </span>
  );
}

export type PeekFacts = {
  agent: Agent;
  status: string;
  summary: string;
  waiting: readonly TodoItem[];
  error?: string;
  turn?: { since?: number; at?: number; items: TranscriptItem[] };
  work: SessionWork;
};

export function SessionPeek({ anchor, facts }: { anchor: DOMRect; facts: PeekFacts }) {
  const { agent, status, summary, waiting, error, turn, work } = facts;
  const card = useRef<HTMLDivElement>(null);
  const [top, setTop] = useState(anchor.top);
  const left = anchor.right + GAP;
  const width = Math.min(WIDTH, window.innerWidth - left - GAP);
  useLayoutEffect(() => {
    const height = card.current?.offsetHeight ?? 0;
    setTop(Math.max(GAP, Math.min(anchor.top - 6, window.innerHeight - height - GAP)));
  }, [anchor.top, summary, waiting.length, turn?.items.length]);
  if (width < MIN_WIDTH) return null;
  const steps = (turn?.items ?? []).filter((item) => item.type === "tool_call").slice(-PEEK_STEPS);
  const task = work.task?.task;
  return createPortal(
    <div ref={card} className="session-peek" style={{ top, left, width }} role="tooltip">
      <div className="peek-head">
        <Avatar agent={agent} size={32} />
        <div className="peek-who">
          <div className="peek-name">
            <strong>{agent.name}</strong>
            {agent.profile.title && <span>{agent.profile.title}</span>}
          </div>
          <div className="peek-model">
            <ModelMark plugin={agent.plugin} model={agent.model} size={12} />
            {agent.model || agent.plugin}
          </div>
        </div>
        <span className="peek-status">{status}</span>
      </div>
      {summary && <p className="peek-summary">{summary}</p>}
      {waiting.slice(0, 2).map((item) => (
        <p key={item.kind + item.id} className="peek-line attention">
          <span>{item.summary}</span>
          <time>{ago(item.since)}</time>
        </p>
      ))}
      {error && <p className="peek-line danger">{error}</p>}
      {turn && (
        <div className="peek-turn">
          <RowElapsed since={turn.since} at={turn.at} items={turn.items} />
          {steps.map((step, i) => (
            <code key={i}>⎿ {stepLine(step)}</code>
          ))}
        </div>
      )}
      {work.lead && work.lead.tasks.length > 0 && (
        <div className="peek-run">
          <TaskSegments tasks={work.lead.tasks} />
          <span>{progress(work.lead.tasks)}</span>
        </div>
      )}
      {task && (
        <div className="peek-task">
          <div>
            <code>#{task.key}</code> {taskState(task, work.task!.tasks).word}
            {checksPassed(task.report.checks) && " · " + checksPassed(task.report.checks)}
          </div>
          {task.progress && <div>{task.progress}</div>}
          {task.branch && <code className="peek-branch">{task.branch}</code>}
        </div>
      )}
      <div className="peek-foot">{t("sidebar.besideHint")}</div>
    </div>,
    document.body,
  );
}
