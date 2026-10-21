// What sits above the composer, top to bottom: the "需要你" strip (other agents waiting on
// you), then the run strip in a planner's session (its run's tasks).

import { useEffect, useRef, useState } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import { markSeen, useUnread, useViewing } from "./activity";
import { Avatar, displayName, sessionHref } from "./components";
import { BUCKET_LABEL, groupTasks, progress, runState } from "./lib/taskState";
import { StateWord, TaskCard } from "./parts";

type Tone = "warn" | "danger" | "quiet";
type Chip = { agent: Agent; word: string; tone: Tone; key: string; done: boolean; href: string };

/** Chips shown before the rest go behind "+N". */
const FIT = 4;

/** Every agent that waits on you, most urgent first; the open ones are left out. */
function attention(agents: Agent[], tree: RunDetail[], unread: ReturnType<typeof useUnread>, viewing: string[]): Chip[] {
  const chips = new Map<string, Chip>();
  const add = (chip: Chip) => {
    if (!viewing.includes(chip.agent.id) && !chips.has(chip.agent.id)) chips.set(chip.agent.id, chip);
  };
  const byId = (id: string) => agents.find((a) => a.id === id);
  for (const d of tree) {
    const planner = d.run.status === "awaiting_approval" ? byId(d.run.planner) : undefined;
    if (planner) add({ agent: planner, word: "待审批", tone: "warn", key: `plan:${d.run.id}:${d.run.updated_at}`, done: false, href: sessionHref(planner.name) });
  }
  for (const agent of agents) {
    if (agent.status === "error") add({ agent, word: "出错", tone: "danger", key: `error:${agent.id}`, done: false, href: sessionHref(agent.name) });
  }
  for (const [id, mark] of Object.entries(unread)) {
    const agent = byId(id);
    if (!agent) continue;
    add({ agent, word: mark.kind === "error" ? "出错" : "完成", tone: mark.kind === "error" ? "danger" : "quiet", key: `unread:${id}:${mark.at}`, done: mark.kind === "done", href: sessionHref(agent.name) });
  }
  const rank = (c: Chip) => (c.tone === "danger" ? 0 : c.tone === "warn" ? 1 : 2);
  return [...chips.values()].sort((a, b) => rank(a) - rank(b));
}

/** Dismissed needs stay hidden until they change (their key changes). */
const dismissed = new Set<string>();

export function NeedsYouStrip({ agents, tree }: { agents: Agent[]; tree: RunDetail[] }) {
  const unread = useUnread();
  const viewing = useViewing();
  const [, refresh] = useState(0);
  const [menu, setMenu] = useState<"done" | "more" | null>(null);
  const chips = attention(agents, tree, unread, viewing).filter((c) => !dismissed.has(c.key));
  if (!chips.length) return null;

  const done = chips.filter((c) => c.done);
  const rest = chips.filter((c) => !c.done);
  // Two or more finished turns are one chip.
  const merged = done.length >= 2;
  const lead = merged ? rest : chips;
  const shown = lead.slice(0, merged ? FIT - 1 : FIT);
  const hidden = lead.slice(shown.length);

  const dismiss = (c: Chip) => {
    if (c.done) markSeen(c.agent.id);
    else dismissed.add(c.key);
    refresh((n) => n + 1);
  };

  return (
    <div className="needs-you" role="region" aria-label="需要你">
      {shown.map((c) => (
        <NeedChip key={c.agent.id} chip={c} onDismiss={() => dismiss(c)} />
      ))}
      {merged && (
        <Popover open={menu === "done"} onToggle={(o) => setMenu(o ? "done" : null)} label={`${done.length} 个已完成`} className="need quiet">
          {done.map((c) => (
            <a key={c.agent.id} className="pop-item" href={c.href} onClick={() => setMenu(null)}>
              <Avatar agent={c.agent} size={18} />
              {displayName(c.agent)}
              <span className="muted">完成</span>
            </a>
          ))}
          <button
            className="pop-item"
            onClick={() => {
              done.forEach((c) => markSeen(c.agent.id));
              setMenu(null);
            }}
          >
            全部清除
          </button>
        </Popover>
      )}
      {hidden.length > 0 && (
        <Popover open={menu === "more"} onToggle={(o) => setMenu(o ? "more" : null)} label={`+${hidden.length}`} className="need quiet">
          {hidden.map((c) => (
            <a key={c.agent.id} className="pop-item" href={c.href} onClick={() => setMenu(null)}>
              <Avatar agent={c.agent} size={18} />
              {displayName(c.agent)}
              <span className={c.tone === "quiet" ? "muted" : c.tone}>{c.word}</span>
            </a>
          ))}
        </Popover>
      )}
    </div>
  );
}

function NeedChip({ chip, onDismiss }: { chip: Chip; onDismiss: () => void }) {
  return (
    <span
      className={`need ${chip.tone}`}
      onAuxClick={(e) => {
        if (e.button === 1) onDismiss();
      }}
    >
      <a href={chip.href} title={`${displayName(chip.agent)} · ${chip.word}`}>
        <Avatar agent={chip.agent} size={16} />
        <span className="need-name">{displayName(chip.agent)}</span>
        <span className="need-word">{chip.word}</span>
      </a>
      <button className="need-x" onClick={onDismiss} aria-label={`不再提示 ${displayName(chip.agent)}`}>
        ×
      </button>
    </span>
  );
}

/** A label that opens a small menu above it; Esc or a click outside closes it. */
function Popover({ open, onToggle, label, className, children }: { open: boolean; onToggle: (open: boolean) => void; label: string; className: string; children: React.ReactNode }) {
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent | KeyboardEvent) => {
      if (e instanceof KeyboardEvent ? e.key === "Escape" : !ref.current?.contains(e.target as Node)) onToggle(false);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", close);
    };
  }, [open]);
  return (
    <span className="pop-anchor" ref={ref}>
      <button className={className} onClick={() => onToggle(!open)} aria-expanded={open}>
        {label}
      </button>
      {open && <div className="pop">{children}</div>}
    </span>
  );
}

const RUN_OPEN = "crossagents.runStrip";

function isOpen(runId: string): boolean {
  try {
    return localStorage.getItem(`${RUN_OPEN}.${runId}`) === "1";
  } catch {
    return false;
  }
}

/**
 * A planner's live runs, one line each — `▸ ◯ title · 1/3 已合并 · 2 进行中 ● 进行中` — opening
 * upwards (▴) to the tasks as cards, grouped by bucket.
 */
export function RunStrip({ runs, agents }: { runs: RunDetail[]; agents: Agent[] }) {
  const [, refresh] = useState(0);
  if (!runs.length) return null;
  return (
    <div className="run-strip">
      {runs.map(({ run, tasks }) => {
        const open = isOpen(run.id) && tasks.length > 0;
        const toggle = () => {
          try {
            localStorage.setItem(`${RUN_OPEN}.${run.id}`, open ? "0" : "1");
          } catch {
            // Remembered for this page only.
          }
          refresh((n) => n + 1);
        };
        return (
          <div key={run.id} className={`rs ${open ? "open" : ""}`}>
            {open && (
              <div className="rs-body">
                {groupTasks(tasks).map((g) => (
                  <div key={g.bucket} className="rs-group">
                    <div className="rs-label">
                      {BUCKET_LABEL[g.bucket]} · {g.tasks.length}
                    </div>
                    {g.tasks.map((t) => (
                      <TaskCard key={t.id} task={t} run={run} agents={agents} siblings={tasks} />
                    ))}
                  </div>
                ))}
              </div>
            )}
            <div className="rs-line">
              <button className="rs-toggle" onClick={toggle} disabled={!tasks.length} aria-expanded={open}>
                <span className="disclosure" aria-hidden="true">
                  {open ? "▴" : "▸"}
                </span>
                <span className="rs-title">{run.title}</span>
                <span className="rs-progress">{tasks.length ? progress(tasks) : "还没有任务"}</span>
              </button>
              <StateWord state={runState(run.status)} />
              <a className="rs-link" href={`#/runs/${run.id}`}>
                run
              </a>
            </div>
          </div>
        );
      })}
    </div>
  );
}
