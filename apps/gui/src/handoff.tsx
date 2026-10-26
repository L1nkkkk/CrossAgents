// Handoff cards (docs/research/magpie-lancer.md §2.1): each time a task changed hands — its
// first worker, 改派, 额度改派, 升级 — what went with it. Shown as a task's chain of hands on the
// run page and as an orchestration line in the lead's conversation; both open the same card.

import { useEffect, useRef, useState } from "react";
import type { Agent } from "@bindings/Agent";
import type { Handoff } from "@bindings/Handoff";
import type { HandoffKind } from "@bindings/HandoffKind";
import { Avatar, List, Prose, displayName, isAgent, time } from "./components";
import { clock } from "./lib/time";

export const KIND_LABEL: Record<HandoffKind, string> = {
  assign: "派活",
  reassign: "改派",
  reroute: "额度改派",
  escalate: "升级",
};

const KIND_TONE: Record<HandoffKind, string> = { assign: "", reassign: "accent", reroute: "warn", escalate: "danger" };

const tokens = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k` : String(n));

/** The teammate a name on a card stands for (worker clones are gone once a task ends). */
function teammate(agents: Agent[] | undefined, name: string | null): Agent | undefined {
  return name ? agents?.find((a) => isAgent(a, name)) : undefined;
}

function Who({ name, agents }: { name: string | null; agents?: Agent[] }) {
  const agent = teammate(agents, name);
  return (
    <span className="hand-who">
      {agent && <Avatar agent={agent} size={16} />}
      {agent ? displayName(agent) : (name ?? "?")}
    </span>
  );
}

/** The card: from → to, why, the files, the last word, decisions, open questions, context. */
export function HandoffCard({ handoff: h, agents }: { handoff: Handoff; agents?: Agent[] }) {
  const first = h.kind === "assign";
  return (
    <div className="tc-body hand-card">
      <div className="hand-route">
        <Who name={h.from_name} agents={agents} />
        <span className="muted">→</span>
        <Who name={h.to_name} agents={agents} />
        <span className={`chip ${KIND_TONE[h.kind]}`}>{KIND_LABEL[h.kind]}</span>
        <span className="spacer" />
        <span className="muted small" title={time(h.at)}>
          {clock(h.at)}
        </span>
      </div>
      {h.reason && (
        <section className={`tc-section ${h.kind === "reroute" || h.kind === "escalate" ? "warn" : ""}`}>
          <h4>为什么换手</h4>
          <div className="small">{h.reason}</div>
        </section>
      )}
      <section className="tc-section">
        <h4>{first ? "允许修改的文件" : "已经改了的文件"}</h4>
        <List items={h.files} empty={first ? "不限" : "没有看到改动"} />
      </section>
      {h.summary && (
        <section className="tc-section">
          <h4>上一位最后说的</h4>
          <Prose text={h.summary} />
        </section>
      )}
      {h.decisions.length > 0 && (
        <section className="tc-section">
          <h4>做过的决定</h4>
          <ul className="plain hand-list">
            {h.decisions.map((d, i) => (
              <li key={i}>{d}</li>
            ))}
          </ul>
        </section>
      )}
      {h.open_questions.length > 0 && (
        <section className="tc-section warn">
          <h4>没回答的问题</h4>
          <ul className="plain hand-list">
            {h.open_questions.map((q, i) => (
              <li key={i}>{q}</li>
            ))}
          </ul>
        </section>
      )}
      {!first && (
        <div className="small muted">
          交接时上下文 {h.context_tokens !== null ? `${tokens(h.context_tokens)} tokens` : "未知（驱动没有报告）"}
        </div>
      )}
    </div>
  );
}

/**
 * A task's hands in turn — `Ada → Rex 派活 → Mia 额度改派` — each step opening its card. Nothing
 * when the task has not started.
 */
export function HandoffChain({ handoffs, agents, open: initial }: { handoffs: Handoff[]; agents?: Agent[]; open?: string }) {
  const [open, setOpen] = useState<string | undefined>(initial);
  if (!handoffs.length) return null;
  const shown = handoffs.find((h) => h.id === open);
  return (
    <div className="hand-chain">
      <div className="hand-steps">
        <Who name={handoffs[0].from_name} agents={agents} />
        {handoffs.map((h) => (
          <span key={h.id} className="hand-step">
            <span className="muted" aria-hidden="true">
              →
            </span>
            <button
              className={`chip ${KIND_TONE[h.kind]} ${open === h.id ? "active" : ""}`}
              onClick={() => setOpen(open === h.id ? undefined : h.id)}
              aria-expanded={open === h.id}
              title={h.reason ?? KIND_LABEL[h.kind]}
            >
              {teammate(agents, h.to_name) ? displayName(teammate(agents, h.to_name)!) : h.to_name}
              <span className="hand-kind">{KIND_LABEL[h.kind]}</span>
            </button>
          </span>
        ))}
      </div>
      {shown && (
        <div className="task-card open">
          <HandoffCard handoff={shown} agents={agents} />
        </div>
      )}
    </div>
  );
}

/** `↙ 编排 · #a 交接 Rex → Mia 额度改派  why ›` in the lead's conversation; opens the card. */
export function HandoffLine({ handoff: h, agents }: { handoff: Handoff; agents?: Agent[] }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  // A line near the bottom of the conversation opens below the fold: bring the card into view.
  useEffect(() => {
    if (open) ref.current?.scrollIntoView({ block: "nearest" });
  }, [open]);
  return (
    <div ref={ref} className={`peer ${open ? "open" : ""}`} data-handoff={h.id}>
      <button className="peer-line" onClick={() => setOpen(!open)} aria-expanded={open}>
        <span className="peer-arrow" aria-hidden="true">
          ↙
        </span>
        <span className="peer-from">编排</span>
        <span className="peer-task">
          <code>#{h.task_key}</code>
          <span>交接</span>
        </span>
        <span className="peer-from">
          <Who name={h.from_name} agents={agents} /> → <Who name={h.to_name} agents={agents} />
        </span>
        <span className={`chip ${KIND_TONE[h.kind]}`}>{KIND_LABEL[h.kind]}</span>
        <span className="peer-preview">{h.reason ?? (h.files.length ? h.files.join(" · ") : "")}</span>
        <span className="disclosure" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
      </button>
      {open && (
        <div className="peer-body">
          <div className="task-card open">
            <HandoffCard handoff={h} agents={agents} />
          </div>
        </div>
      )}
    </div>
  );
}
