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
import { ContextUsageButton } from "./contextusage";
import { effortLabel, effortOf } from "./lib/models";
import { taskState } from "./lib/taskState";
import { openTool } from "./panes";
import { StateWord } from "./parts";
import type { TranscriptTurn } from "./session";
import { isChat, wsHref, wsKey, wsName } from "./workspace";

// ---- Shown / hidden, and which cards are folded. ---------------------------------------------

const HIDDEN_KEY = "crossagents.railHidden";
const FOLDED_KEY = "crossagents.railFolded";
/** Cards the person opened that are folded by default (the run's trace, its participants). */
const OPENED_KEY = "crossagents.railOpened";
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

let rail = {
  hidden: read<boolean>(HIDDEN_KEY, false),
  folded: read<string[]>(FOLDED_KEY, []),
  opened: read<string[]>(OPENED_KEY, []),
};

function update(next: typeof rail) {
  rail = next;
  write(HIDDEN_KEY, rail.hidden);
  write(FOLDED_KEY, rail.folded);
  write(OPENED_KEY, rail.opened);
  listeners.forEach((l) => l());
}

export const toggleRail = () => update({ ...rail, hidden: !rail.hidden });

/** Whether a card shows: the person's last choice wins over the card's own default. */
const isOpen = (card: string, defaultOpen: boolean) =>
  rail.folded.includes(card) ? false : rail.opened.includes(card) ? true : defaultOpen;

const toggleFold = (card: string, defaultOpen: boolean) => {
  const open = isOpen(card, defaultOpen);
  const without = (list: string[]) => list.filter((c) => c !== card);
  update({
    ...rail,
    folded: open ? [...rail.folded, card] : without(rail.folded),
    opened: open ? without(rail.opened) : [...rail.opened, card],
  });
};

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

/**
 * A run's rail carries what the person otherwise has to go looking for — its tasks, who took
 * part, the plan, the policy — so it stays beside the conversation as long as the conversation
 * keeps its own room; narrower than this, it becomes a drawer the header opens.
 */
export const SIDE_ROOM = 820;

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

  // A lead works in its run's checkout (worktrees/<run>/integration), on the run branch.
  const checkout = led
    .map((d) => d.run)
    .find((r) => !r.in_place && r.branch && wsKey(agent.cwd).includes(`/${r.branch.split("/").pop()}/integration`));

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
        {!isChat(agent) && (
          <button className="rail-line link" onClick={() => openTool("files", { agent: agent.name })} title="在工具列里浏览工作目录的文件">
            <Icon name="diff" size={14} />
            <span className="rail-main">浏览文件</span>
          </button>
        )}
        {checkout && (
          <Line icon={<Icon name="branch" size={14} />}>
            <code>{checkout.branch}</code>
          </Line>
        )}
        <Line icon={<VendorMark plugin={agent.plugin} size={14} />}>{agent.model ?? "插件默认模型"}</Line>
        <Line icon={<Icon name="brain" size={14} />}>推理：{effortName ?? "默认"}</Line>
        {typeof account === "string" && account && <Line icon={<Icon name="account" size={14} />}>{account}</Line>}
      </Card>

      <Card id="context" title="上下文">
        {usage && usage.size > 0 ? <Context usage={usage} agent={agent} /> : <ContextUsageButton agent={agent} usage={null} variant="rail" />}
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

/**
 * One panel of a rail: its heading folds it, and it says what is inside (a count, the progress)
 * beside the title. The heading lives in the panel, not in a window bar, so a rail carries many
 * sections without a second title bar.
 */
export function RailCard({
  id,
  title,
  aside,
  defaultOpen = true,
  children,
}: {
  id: string;
  title: string;
  /** What the section holds, right of its title: "2/4", "3 个任务". */
  aside?: ReactNode;
  /** Collapsed until opened (the trace, the participants). */
  defaultOpen?: boolean;
  children: ReactNode;
}) {
  // Subscribed, so a fold anywhere re-renders every panel; the rule itself is isOpen's.
  useRail();
  const open = isOpen(id, defaultOpen);
  return (
    <section className="rail-card" data-card={id}>
      <button className="rail-head" onClick={() => toggleFold(id, defaultOpen)} aria-expanded={open}>
        <span className="rail-head-title">{title}</span>
        {aside && <span className="rail-head-aside">{aside}</span>}
        <Icon name={open ? "chevron" : "chevronRight"} size={13} />
      </button>
      {open && <div className="rail-body">{children}</div>}
    </section>
  );
}

function Card({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  return (
    <RailCard id={id} title={title}>
      {children}
    </RailCard>
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

/** Context kinds as Claude Code names them, in the words this app uses. */
const PART_NAMES: Record<string, string> = {
  system_prompt: "系统提示",
  system_tools: "系统工具",
  mcp_tools: "MCP 工具",
  mcp_instructions: "MCP 说明",
  custom_agents: "自定义 agent",
  memory_files: "记忆文件",
  skills: "技能",
  messages: "消息",
  autocompact_buffer: "压缩预留",
  free_space: "空闲",
};
const partName = (id: string, name: string) => {
  const base = id.replace(/_deferred$/, "");
  const known = PART_NAMES[base];
  if (!known) return name;
  return base === id ? known : `${known}（按需）`;
};
/** One colour per kind that matters; the rest share a grey. */
const partTone = (id: string, kind: string) =>
  kind === "buffer" ? "buffer" : ["messages", "system_tools", "mcp_tools", "system_prompt", "skills"].includes(id) ? id : "other";

/** How full the context is: green while there is room, amber from 60%, red from 85%. With
 * the agent's breakdown (Claude Code), what fills it, by kind, as /context shows. Its first
 * row is the meter button: usage in words and 压缩上下文. */
function Context({ usage, agent }: { usage: TranscriptUsage; agent: Agent }) {
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
  const parts = usage.parts ?? [];
  const inWindow = parts
    .filter((x) => x.kind !== "deferred")
    .sort((a, b) => (a.kind === b.kind ? b.tokens - a.tokens : order(a.kind) - order(b.kind)));
  const deferred = parts.filter((x) => x.kind === "deferred");
  const share = (n: number) => `${((n / usage.size) * 100).toFixed(1)}%`;
  return (
    <>
      <ContextUsageButton agent={agent} usage={usage} variant="rail" />
      {inWindow.length > 0 ? (
        <>
          <div className="rail-bar ctx-bar" role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} aria-label="上下文已用">
            {inWindow
              .filter((x) => x.kind !== "free")
              .map((x) => (
                <span key={x.id} className={`ctx-${partTone(x.id, x.kind)}`} style={{ width: `${(x.tokens / usage.size) * 100}%` }} title={partName(x.id, x.name)} />
              ))}
          </div>
          <dl className="rail-rows ctx-parts">
            {[...inWindow, ...deferred].map((x) => (
              <div key={x.id} className={x.kind === "deferred" ? "deferred" : ""}>
                <dt>
                  <i className={`ctx-dot ctx-${partTone(x.id, x.kind)}`} />
                  {partName(x.id, x.name)}
                </dt>
                <dd>
                  {short(x.tokens)}
                  <span className="ctx-share">{x.kind === "deferred" ? "—" : share(x.tokens)}</span>
                </dd>
              </div>
            ))}
          </dl>
        </>
      ) : (
        <div className={`quota-bar rail-bar ${tone}`} role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} aria-label="上下文已用">
          <span style={{ width: `${pct}%` }} />
        </div>
      )}
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

/** Used shares first, then the buffer, then free space. */
const order = (kind: string) => ["used", "buffer", "free"].indexOf(kind);
