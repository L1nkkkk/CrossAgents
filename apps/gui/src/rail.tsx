// The cards floating on the right of a conversation, like Claude Code's and Codex's: where the
// agent works and on what model, how full its context is, what it changed, its tasks and the
// team it leads. Shown when the conversation is wide enough, unless turned off (视图 → 信息栏,
// Ctrl+I); a card folds by its title. Both are remembered on this device.

import { useSyncExternalStore, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { TranscriptUsage } from "@bindings/TranscriptUsage";
import { Avatar, Dot, Icon, VendorMark, agentTone, displayName, sessionHref, statusLabel } from "./components";
import { effortLabel, effortOf } from "./lib/models";
import { taskState } from "./lib/taskState";
import { openTool } from "./panes";
import { StateWord } from "./parts";
import type { TranscriptTurn } from "./session";
import { isChat, wsHref, wsKey, wsName } from "./workspace";

// ---- Shown / hidden, and which cards are folded. ---------------------------------------------

const HIDDEN_KEY = "crossagents.railHidden";
const FOLDED_KEY = "crossagents.railFolded";
const listeners = new Set<() => void>();

function read<T>(key: string, fallback: T): T {
  try {
    return JSON.parse(localStorage.getItem(key) ?? "null") ?? fallback;
  } catch {
    return fallback;
  }
}

function write(key: string, value: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Then it lasts for this page only.
  }
}

let rail = { hidden: read<boolean>(HIDDEN_KEY, false), folded: read<string[]>(FOLDED_KEY, []) };

function update(next: typeof rail) {
  rail = next;
  write(HIDDEN_KEY, rail.hidden);
  write(FOLDED_KEY, rail.folded);
  listeners.forEach((l) => l());
}

export const toggleRail = () => update({ ...rail, hidden: !rail.hidden });
const toggleFold = (card: string) =>
  update({ ...rail, folded: rail.folded.includes(card) ? rail.folded.filter((c) => c !== card) : [...rail.folded, card] });

export const useRail = () =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => rail,
  );

// ---- The rail. -----------------------------------------------------------------------------

/** A conversation at least this wide has room for the rail beside it. */
export const RAIL_ROOM = 1060;

const short = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${n}`);

/** The folder a path is in, with the home directory as `~`. */
function parentOf(path: string): string {
  const parts = path.replace(/\\/g, "/").replace(/\/+$/, "").split("/");
  parts.pop();
  const dir = parts.join("/");
  return dir.replace(/^[A-Za-z]:\/Users\/[^/]+|^\/(?:home|Users)\/[^/]+/, "~");
}

export function InfoRail({
  agent,
  usage,
  turns,
  tasks,
  led,
  agents,
  recruits = [],
  spend,
}: {
  agent: Agent;
  usage: TranscriptUsage | null;
  turns: TranscriptTurn[];
  tasks: { run: Run; task: RunDetail["tasks"][number] }[];
  /** Runs this agent plans. */
  led: RunDetail[];
  agents: Agent[];
  /** Teammates this conversation recruited (chat mode). */
  recruits?: Agent[];
  /** What the conversation spent, its recruits included. */
  spend?: number | null;
}) {
  const effort = effortOf(agent.config);
  const effortName = effort ? effortLabel(effort) : undefined;
  const account = (agent.config as Record<string, unknown> | null)?.account;

  // Everything the session changed, over all its turns.
  const files = new Set<string>();
  let added = 0;
  let removed = 0;
  for (const t of turns) {
    if (!t.diff) continue;
    t.diff.files.forEach((f) => files.add(f.path));
    added += t.diff.added;
    removed += t.diff.removed;
  }

  // Who works in the runs it leads, each by the task it has: worker clones share a name.
  const team = led.flatMap((d) =>
    d.tasks.flatMap((task) => {
      const a = agents.find((x) => x.id === task.assignee);
      return a && a.id !== agent.id ? [{ agent: a, task, run: d.run }] : [];
    }),
  );

  return (
    <aside className="rail" aria-label="会话信息">
      <Card id="env" title="环境">
        {isChat(agent) ? (
          <Line icon={<Icon name="chat" size={14} />}>对话（不在项目里）</Line>
        ) : (
          <a className="rail-line link" href={wsHref(wsKey(agent.cwd))} title={agent.cwd}>
            <Icon name="folder" size={14} />
            <span className="rail-main">{wsName(agent.cwd)}</span>
            <span className="rail-dim">{parentOf(agent.cwd)}</span>
          </a>
        )}
        <Line icon={<VendorMark plugin={agent.plugin} size={14} />}>{agent.model ?? "插件默认模型"}</Line>
        <Line icon={<Icon name="brain" size={14} />}>推理：{effortName ?? "默认"}</Line>
        {typeof account === "string" && account && <Line icon={<Icon name="account" size={14} />}>{account}</Line>}
      </Card>

      <Card id="context" title="上下文">
        {usage && usage.size > 0 ? <Context usage={usage} /> : <p className="rail-empty">还没有报告用量</p>}
        {recruits.length > 0 && spend != null && (
          <div className="rail-spend" title="这个对话和它招来的队友一共花了多少（各自报告的费用之和）">
            <span>对话花费（含 {recruits.length} 位队友）</span>
            <span className="rail-spend-v">${spend.toFixed(2)}</span>
          </div>
        )}
      </Card>

      <Card id="changes" title="改动">
        {files.size ? (
          <button className="rail-line link" onClick={() => openTool("changes", { agent: agent.name })} title="在工具列里看每一轮的改动">
            <Icon name="diff" size={14} />
            <span className="rail-main">{files.size} 个文件</span>
            <span className="add">+{added}</span>
            <span className="del">−{removed}</span>
          </button>
        ) : (
          <p className="rail-empty">还没有改动文件</p>
        )}
      </Card>

      {tasks.length > 0 && (
        <Card id="tasks" title="任务">
          {tasks.map(({ run, task }) => (
            <a key={task.id} className="rail-line link" href={`#/runs/${run.id}/tasks/${task.id}`} title={`${task.title} · ${run.title}`}>
              <code className="bare">#{task.key}</code>
              <span className="rail-main">{task.title}</span>
              <StateWord state={taskState(task)} />
            </a>
          ))}
        </Card>
      )}

      {recruits.length > 0 && (
        <Card id="recruits" title="队友">
          {recruits.map((a) => (
            <a key={a.id} className="rail-line link" href={sessionHref(a)} title={`${displayName(a)} · ${a.plugin}${a.model ? " · " + a.model : ""} · ${statusLabel(a.status)}`}>
              <Avatar agent={a} size={16} />
              <span className="rail-main">
                {displayName(a)} <span className="rail-dim">{a.model ?? a.plugin}</span>
              </span>
              <Dot tone={agentTone(a.status)} title={statusLabel(a.status)} />
            </a>
          ))}
        </Card>
      )}

      {team.length > 0 && (
        <Card id="team" title="团队">
          {team.map(({ agent: a, task, run }) => (
            <a key={task.id} className="rail-line link" href={sessionHref(a)} title={`${task.title} · ${run.title}`}>
              <Avatar agent={a} size={16} />
              <span className="rail-main">
                {displayName(a)} <span className="rail-dim">#{task.key} {task.title}</span>
              </span>
              <Dot tone={agentTone(a.status)} title={statusLabel(a.status)} />
            </a>
          ))}
        </Card>
      )}
    </aside>
  );
}

function Card({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  const { folded } = useRail();
  const open = !folded.includes(id);
  return (
    <section className="rail-card">
      <button className="rail-head" onClick={() => toggleFold(id)} aria-expanded={open}>
        {title}
        <Icon name={open ? "chevron" : "chevronRight"} size={13} />
      </button>
      {open && <div className="rail-body">{children}</div>}
    </section>
  );
}

function Line({ icon, children }: { icon: ReactNode; children: ReactNode }) {
  return (
    <div className="rail-line">
      {icon}
      <span className="rail-main">{children}</span>
    </div>
  );
}

/** How full the context is: green while there is room, amber from 60%, red from 85%. */
function Context({ usage }: { usage: TranscriptUsage }) {
  const pct = Math.min(100, Math.round((usage.used / usage.size) * 100));
  const tone = pct >= 85 ? "danger" : pct >= 60 ? "warn" : "ok";
  const t = usage.tokens;
  const rows: [string, string][] = t
    ? [
        ["输入", short(t.input)],
        ["缓存命中", `${t.input ? Math.round((t.cached_input / t.input) * 100) : 0}%`],
        ["输出", short(t.output)],
        ["推理", short(t.reasoning_output)],
      ]
    : [];
  if (usage.cost_usd != null) rows.push(["花费", `$${usage.cost_usd.toFixed(2)}`]);
  return (
    <>
      <div className="rail-context">
        <span className="rail-main">{pct}% 已用</span>
        <span className="rail-dim">
          {short(usage.used)} / {short(usage.size)}
        </span>
      </div>
      <div className={`quota-bar rail-bar ${tone}`} role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} aria-label="上下文已用">
        <span style={{ width: `${pct}%` }} />
      </div>
      {rows.length > 0 && (
        <dl className="rail-rows">
          {rows.map(([k, v]) => (
            <div key={k}>
              <dt>{k}</dt>
              <dd>{v}</dd>
            </div>
          ))}
        </dl>
      )}
    </>
  );
}
