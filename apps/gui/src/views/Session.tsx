import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { Capabilities } from "@bindings/Capabilities";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { ModelSwitch } from "@bindings/ModelSwitch";
import type { TranscriptUsage } from "@bindings/TranscriptUsage";
import { view } from "../activity";
import { api, errorText } from "../api";
import { dismiss as dismissMessage, retry as retryMessage, sendMessage, useOutbox, type Outgoing } from "../outbox";
import { ACTIVE_RUN, useRunTree } from "../App";
import { Avatar, ErrorBox, Icon, Mark, Prose, StatusChip, Vendor, agentTitle, agentTone, displayName, roleShape, time, useAgents, vendorName } from "../components";
import { ChangeStat, DiffView } from "../diffview";
import { parseDiff, tailLines, type DiffFile } from "../lib/diff";
import { errorHint } from "../lib/errors";
import { effortLabel, prettyModel } from "../lib/models";
import { clock, range } from "../lib/time";
import { ModelEffortPicks } from "../modelPick";
import { InfoRail, RAIL_ROOM, useRail } from "../rail";
import { ScrollThumb } from "../scrollthumb";
import { TurnMap, type MapEntry } from "../turnmap";
import { focusSession, openTool, toggleTools, useLayout } from "../panes";
import { taskState } from "../lib/taskState";
import { actionOf, duration, failed, liveActivity, stepLine, summarize, workGroups, type ActionTool, type Step } from "../lib/workGroups";
import { PeerMessage, StateWord, useArmed } from "../parts";
import { useSession, type TranscriptItem, type TranscriptTurn } from "../session";
import { QuestionCards } from "../questions";
import { NeedsYouStrip, RunStrip } from "../strips";
import { useHubData } from "../hub";
import { mentionsFor, useMention } from "../mention";
import { isChat, wsKey } from "../workspace";
import { ProfileEditor } from "./Agents";
import { PlanTasks } from "./Run";

/** A person's message, or one recorded before senders were. */
const fromPerson = (turn: TranscriptTurn) => !turn.from || turn.from.kind === "human";
const started = (turn: TranscriptTurn) => !!turn.started_at || !!turn.completed_at;

export function SessionView({ name, onClose }: { name: string; onClose?: () => void }) {
  const { transcript, error, reload } = useSession(name);
  const tree = useRunTree();
  const agents = useAgents();
  const [draft, setDraft] = useState("");
  const agentId = transcript?.agent.id;
  const outgoing = useOutbox(agentId, transcript?.turns);
  const { hidden: railOff } = useRail();
  const [width, setWidth] = useState(0);
  const ro = useRef<ResizeObserver | null>(null);
  // A callback ref: the section mounts once the transcript has loaded.
  const measure = useCallback((el: HTMLElement | null) => {
    ro.current?.disconnect();
    if (!el) return;
    setWidth(el.clientWidth);
    ro.current = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.current.observe(el);
  }, []);

  useEffect(() => (agentId ? view(agentId) : undefined), [agentId]);

  if (!transcript) {
    // Worker and reviewer clones are removed once their task ends.
    const gone = !!error && /not found/i.test(error);
    return (
      <section className="pane">
        <div className="page-inner">
          {gone ? (
            <div className="empty">
              <h1>这个会话已经不在了</h1>
              <p>
                <code>{name}</code> 已被移除：任务结束后，run 里的 Worker 和 Reviewer 会被清理。
              </p>
              {onClose && <button onClick={onClose}>关闭这一栏</button>}
            </div>
          ) : error ? (
            <ErrorBox error={error} />
          ) : (
            <p className="muted">加载中…</p>
          )}
        </div>
      </section>
    );
  }
  const { agent, capabilities, turns, usage } = transcript;
  const runs = tree.data ?? [];
  const everyone = agents.data ?? [];
  const pendingPlans = runs.filter((d) => d.run.planner === agent.id && d.run.status === "awaiting_approval");
  const liveRuns = runs.filter((d) => d.run.planner === agent.id && ACTIVE_RUN.includes(d.run.status) && d.run.status !== "awaiting_approval");
  const tasks = runs.flatMap((d) => d.tasks.filter((t) => t.assignee === agent.id || t.reviewer === agent.id).map((t) => ({ run: d.run, task: t })));
  const queued = turns.filter((t) => !started(t));
  const rail = !railOff && width >= RAIL_ROOM;
  const led = runs.filter((d) => d.run.planner === agent.id);
  const recruits = everyone.filter((a) => (a.config as Record<string, unknown> | null)?.spawned_by_agent === agent.id && !a.archived_at);
  // Who @ offers: this conversation's recruits, then the other agents working in its project.
  const mates = [
    ...recruits,
    ...(isChat(agent) ? [] : everyone.filter((a) => a.id !== agent.id && !a.archived_at && !recruits.includes(a) && wsKey(a.cwd) === wsKey(agent.cwd))),
  ];

  return (
    <section className={`pane ${rail ? "with-rail" : ""}`} ref={measure} onPointerDownCapture={() => focusSession(name)}>
      <Header agent={agent} capabilities={capabilities} usage={usage} driver={transcript.driver} tasks={tasks} rail={rail} onClose={onClose} />
      <Conversation agent={agent} turns={turns.filter(started)} switches={transcript.switches ?? []} agents={everyone} tree={runs}>
        {outgoing.map((m) => (
          <Sending key={m.key} message={m} agent={agent} />
        ))}
      </Conversation>
      {rail && <InfoRail agent={agent} usage={usage} turns={turns} tasks={tasks} led={led} agents={everyone} recruits={recruits} spend={transcript.conversation_cost_usd} />}
      <div className="dock">
        {/* One strip is enough: the pane beside leaves it to the main one. */}
        {!onClose && <QuestionCards agents={everyone} />}
        {!onClose && <NeedsYouStrip agents={everyone} tree={runs} />}
        <RunStrip runs={liveRuns} agents={everyone} />
        <QueuePanel agent={agent} queued={queued} onEdit={setDraft} />
        {agent.status === "error" && transcript.error && (
          <div className="error-bar" role="alert" title={transcript.error}>
            <strong>{errorHint(transcript.error, transcript.exit)}</strong>
            <span className="mono small">{transcript.error}</span>
          </div>
        )}
        {agent.archived_at ? (
          <div className="archived-bar">
            <Icon name="archive" size={14} />
            <span>这个对话已归档，它招来的队友也一起停下了。恢复后可以继续。</span>
            <button onClick={() => void api.archiveAgent(agent.id, false)}>恢复对话</button>
          </div>
        ) : (
          <Composer agent={agent} capabilities={capabilities} draft={draft} setDraft={setDraft} plans={pendingPlans} onPlanDone={reload} mates={mates} />
        )}
      </div>
    </section>
  );
}

function Header({
  agent,
  capabilities,
  usage,
  driver,
  tasks,
  rail,
  onClose,
}: {
  agent: Agent;
  capabilities: Capabilities | null;
  usage: TranscriptUsage | null;
  driver: string | null;
  tasks: { run: Run; task: RunDetail["tasks"][number] }[];
  /** The info rail is showing the model, context and tasks: the header leaves them out. */
  rail: boolean;
  onClose?: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [profile, setProfile] = useState(false);
  const stop = useArmed();

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError(undefined);
    try {
      await action();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div className="topbar">
        {agent.plugin === "external" ? (
          <Avatar agent={agent} size={26} />
        ) : (
          <button className={`avatar-btn ${profile ? "on" : ""}`} onClick={() => setProfile(!profile)} title="编辑名片：名字、头像、人设和模型" aria-expanded={profile}>
            <Avatar agent={agent} size={26} />
          </button>
        )}
        <span className="who-block">
          <AgentName agent={agent} />
          <span className="who-sub">{agent.topic ?? agentTitle(agent)}</span>
        </span>
        <StatusChip status={agent.status} tone={agentTone(agent.status)} />
        {!rail && (
          <>
            <span className="chip hide-narrow">
              <Mark shape={roleShape(agent.role)} tone={agentTone(agent.status)} size={9} />
              {agent.role}
            </span>
            <span className="chip hide-narrow">
              <Vendor plugin={agent.plugin} model={agent.model} title={driver ? `${vendorName(agent.plugin)} · driver: ${driver}` : undefined} />
            </span>
            {tasks.map(({ run, task }) => (
              <a key={task.id} className="chip" href={`#/runs/${run.id}/tasks/${task.id}`} title={`${task.title} · ${run.title}`}>
                <code className="bare">#{task.key}</code>
                <StateWord state={taskState(task)} />
              </a>
            ))}
          </>
        )}
        <div className="spacer" />
        {!rail && usage && usage.size > 0 && <Meter usage={usage} />}
        {agent.status === "idle" && usage && usage.size > 0 && usage.used / usage.size >= 0.5 && (
          <button className="ghost small" disabled={busy} onClick={() => run(() => api.compactAgent(agent.id))} title="让 agent 把历史压缩成摘要，腾出上下文">
            压缩上下文
          </button>
        )}
        {agent.status === "running" && capabilities?.interrupt !== false && (
          <button disabled={busy} onClick={() => run(() => api.cancelAgent(agent.id))} title="中断本轮（Esc）">
            <span className="row" style={{ gap: 5 }}>
              <Icon name="stop" size={13} />
              中断
            </span>
          </button>
        )}
        {agent.status !== "stopped" && (
          <button
            className={stop.armed ? "danger armed" : "ghost"}
            disabled={busy}
            onClick={() => stop.click(() => run(() => api.stopAgent(agent.id)))}
            title="停止进程；下次发消息会重新启动"
          >
            {stop.armed ? "再点一次停止" : "停止"}
          </button>
        )}
        <ToolsToggle />
        {onClose && (
          <button className="ghost icon" onClick={onClose} title="关闭这一栏" aria-label="关闭这一栏">
            <Icon name="close" size={15} />
          </button>
        )}
      </div>
      {profile && (
        <div className="profile-pop">
          <ProfileEditor agent={agent} onDone={() => setProfile(false)} />
        </div>
      )}
      {error && (
        <div style={{ padding: "8px 20px 0" }}>
          <ErrorBox error={error} />
        </div>
      )}
    </>
  );
}

/**
 * The agent's name; click to rename. A name the hub made up says so, as an invitation to give
 * it one. The agent hears its new name with its next message.
 */
function AgentName({ agent }: { agent: Agent }) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState(agent.name);
  const [error, setError] = useState<string>();
  async function save() {
    const name = value.trim();
    if (!name || name === agent.name) return setEditing(false);
    try {
      await api.renameAgent(agent.id, name);
      setEditing(false);
      setError(undefined);
    } catch (err) {
      setError(errorText(err));
    }
  }
  if (editing)
    return (
      <input
        className={`name-edit ${error ? "bad" : ""}`}
        autoFocus
        value={value}
        maxLength={32}
        title={error ?? "Enter 保存 · Esc 取消"}
        aria-label="新名字"
        onChange={(e) => setValue(e.target.value)}
        onBlur={() => void save()}
        onKeyDown={(e) => {
          if (e.key === "Enter") void save();
          if (e.key === "Escape") {
            setEditing(false);
            setError(undefined);
          }
        }}
      />
    );
  const generated = agent.name_source === "generated";
  return (
    <button
      className="title name-button"
      onClick={() => {
        setValue(agent.name);
        setEditing(true);
      }}
      title={generated ? "名字是自动起的，点一下改名" : "点一下改名"}
    >
      {displayName(agent)}
      {generated && <span className="name-auto">自动命名</span>}
    </button>
  );
}

function Meter({ usage }: { usage: TranscriptUsage }) {
  const pct = Math.min(100, Math.round((usage.used / usage.size) * 100));
  const t = usage.tokens;
  const lines = [
    `上下文 ${usage.used.toLocaleString()} / ${usage.size.toLocaleString()} tokens`,
    ...(t
      ? [
          `本会话累计 ${t.total.toLocaleString()} tokens`,
          `  输入 ${t.input.toLocaleString()}（缓存命中 ${t.input ? Math.round((t.cached_input / t.input) * 100) : 0}%）`,
          `  输出 ${t.output.toLocaleString()}（推理 ${t.reasoning_output.toLocaleString()}）`,
        ]
      : []),
    ...(usage.cost_usd != null ? [`花费 $${usage.cost_usd.toFixed(4)}`] : []),
  ];
  return (
    <span className="meter" title={lines.join("\n")}>
      <span className="track">
        <span className={`fill ${pct >= 80 ? "warn" : ""}`} style={{ width: `${pct}%`, display: "block" }} />
      </span>
      {pct}%
    </span>
  );
}

/** A message sent from here, while the hub wakes the agent up and takes it (or why it did not). */
function Sending({ message, agent }: { message: Outgoing; agent: Agent }) {
  const waking = agent.status === "stopped" || agent.status === "starting" || agent.status === "error";
  return (
    <div className="turn sending">
      <div className="bubble">{message.text}</div>
      {message.error ? (
        <div className="turn-note danger row" style={{ gap: 8 }} title={message.error}>
          <span>没发出去：{message.error}</span>
          <button className="ghost small" onClick={() => retryMessage(message.key)}>
            重试
          </button>
          <button className="ghost small" onClick={() => dismissMessage(message.key)}>
            删除
          </button>
        </div>
      ) : (
        <div className="working">
          <span className="busy-dots" aria-hidden="true">
            <i />
            <i />
            <i />
          </span>
          <span className="muted small">{waking ? `正在唤起 ${displayName(agent)}…` : "发送中…"}</span>
        </div>
      )}
    </div>
  );
}

function Conversation({
  agent,
  turns,
  switches,
  agents,
  tree,
  children,
}: {
  agent: Agent;
  turns: TranscriptTurn[];
  switches: ModelSwitch[];
  agents: Agent[];
  tree: RunDetail[];
  children?: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  // One mark per message from a person; recomputed only when those change.
  const asked = turns.filter(fromPerson);
  const key = asked.map((t) => t.message_id).join(",");
  const entries = useMemo<MapEntry[]>(() => asked.map((t) => ({ id: t.message_id, text: t.input })), [key]);

  useLayoutEffect(() => {
    const el = ref.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });

  return (
    <div className="convo-wrap">
      <div
        className="convo"
        ref={ref}
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 60;
        }}
      >
        <div className="convo-inner">
          {turns.length === 0 && (
            <div className="empty">
              <h1>和 {displayName(agent)} 开始对话</h1>
              <p>
                {agent.role} · {agent.plugin}
                {agent.model ? `/${agent.model}` : ""} · <code>{agent.cwd}</code>
              </p>
            </div>
          )}
          <CwdContext.Provider value={agent.cwd}>
            {byTime(turns, switches).map((x) =>
              "message_id" in x ? (
                <Turn key={x.message_id} turn={x} agent={agent} agents={agents} tree={tree} />
              ) : (
                <SwitchNote key={x.at} change={x} />
              ),
            )}
          </CwdContext.Provider>
          {children}
        </div>
      </div>
      <ScrollThumb scroller={ref} />
      <TurnMap scroller={ref} entries={entries} />
    </div>
  );
}

/** Turns and model switches in the order they happened (a switch before the message it applies to). */
function byTime(turns: TranscriptTurn[], switches: ModelSwitch[]): (TranscriptTurn | ModelSwitch)[] {
  const out: (TranscriptTurn | ModelSwitch)[] = [];
  let i = 0;
  for (const turn of turns) {
    while (i < switches.length && switches[i].at <= turn.sent_at) out.push(switches[i++]);
    out.push(turn);
  }
  return out.concat(switches.slice(i));
}

/** "模型 Haiku → Sonnet · 推理 默认 → 低": only what changed. The agent is told the same with its next message. */
function SwitchNote({ change }: { change: ModelSwitch }) {
  const model = (m: string | null) => (m ? prettyModel(m) : "默认模型");
  const effort = (e: string | null) => (e ? effortLabel(e) : "默认");
  const parts: string[] = [];
  if (change.model !== change.from_model) parts.push(`模型 ${model(change.from_model)} → ${model(change.model)}`);
  if (change.effort !== change.from_effort) parts.push(`推理 ${effort(change.from_effort)} → ${effort(change.effort)}`);
  return (
    <div className="switch-note" title={`${time(change.at)} · 下一条消息会告诉 agent 这次切换`}>
      <Icon name="brain" size={13} />
      <span>{parts.join(" · ")}</span>
      <span className="switch-at">{clock(change.at)}</span>
    </div>
  );
}

/** Inputs longer than this start folded. */
const FOLD_INPUT = 600;

function Bubble({ turn }: { turn: TranscriptTurn }) {
  const [open, setOpen] = useState(false);
  const long = turn.input.length > FOLD_INPUT;
  return (
    <div className="bubble" title={time(turn.sent_at)}>
      {long && !open ? turn.input.slice(0, FOLD_INPUT).trimEnd() + "…" : turn.input}
      {long && (
        <div>
          <button className="tools-more" style={{ padding: 0, marginTop: 4 }} onClick={() => setOpen(!open)}>
            {open ? "收起" : `展开全部（${turn.input.length} 字）`}
          </button>
        </div>
      )}
    </div>
  );
}

function Turn({ turn, agent, agents, tree }: { turn: TranscriptTurn; agent: Agent; agents: Agent[]; tree: RunDetail[] }) {
  const running = !!turn.started_at && !turn.completed_at;
  return (
    <div className="turn" data-turn={turn.message_id}>
      {fromPerson(turn) ? <Bubble turn={turn} /> : <PeerMessage turn={turn} agents={agents} tree={tree} />}
      {turn.items.length > 0 && (
        <div className="author">
          <Avatar agent={agent} size={22} />
          <span className="who">{displayName(agent)}</span>
          <span className="what">{agentTitle(agent)}</span>
        </div>
      )}
      {workGroups(turn.items).map((block, i) =>
        block.kind === "steps" ? <Steps key={i} steps={block.steps} closed={block.closed} /> : <Item key={i} item={block.item} />,
      )}
      {running && <LiveLine since={turn.started_at!} items={turn.items} />}
      {turn.completed_at && <TurnFoot turn={turn} />}
      {turn.completed_at && turn.diff && <TurnChanges turn={turn} agent={agent} />}
    </div>
  );
}

function Item({ item }: { item: TranscriptItem }) {
  const action = actionOf(item);
  if (action && item.type === "tool_call") return <ActionCard action={action} call={item} />;
  switch (item.type) {
    case "output":
      return <Prose text={item.text} />;
    case "error":
      return <div className="item-error">{item.message}</div>;
    case "warning":
      return <div className="item-warn">{item.message}</div>;
    default:
      return null;
  }
}

const ACTION_LABEL: Record<ActionTool, string> = {
  agent_spawn: "招队友",
  agent_retire: "让队友退出",
  agent_set_model: "换模型",
  task_accept: "合并任务",
  task_drop: "丢弃任务",
  run_land: "落地 run",
  run_finish: "结束 run",
  plan_submit: "提交计划",
};

/** The fields of a tool's JSON arguments or result, or nothing when it is not JSON. */
/** A tool result as words: the text inside an MCP `content` wrapper (Codex reports it as is), or the result itself. */
function plainText(text?: string): string | undefined {
  try {
    const content = JSON.parse(text ?? "")?.content;
    if (Array.isArray(content)) return content.map((c: { text?: unknown }) => (typeof c?.text === "string" ? c.text : "")).join("\n");
  } catch {
    // Not JSON: already plain.
  }
  return text;
}

function fields(text?: string): Record<string, unknown> {
  if (!text) return {};
  try {
    const value = JSON.parse(text);
    // MCP results wrap the payload as content[0].text.
    const inner = value?.content?.[0]?.text;
    if (typeof inner === "string") return fields(inner) ?? {};
    return value && typeof value === "object" ? value : {};
  } catch {
    return {};
  }
}

/**
 * A call that changed (or tried to change) the team or the code, as a card: what was asked, and
 * what the hub did — refused calls say why, in the hub's words. This is the record to trust
 * over the agent's summary.
 */
function ActionCard({ action, call }: { action: ActionTool; call: Extract<TranscriptItem, { type: "tool_call" }> }) {
  const args = fields(call.detail?.arguments);
  const result = fields(call.detail?.result);
  const raw = call.detail?.error ?? (typeof result === "object" && !Object.keys(result).length ? plainText(call.detail?.result) : undefined);
  const refused = call.status === "failed" || !!call.detail?.error || /^(conflict|not found|invalid|unsupported)|not recruited/i.test(raw ?? "");
  const running = call.status === "pending" || call.status === "in_progress";
  const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);
  let what = "";
  if (action === "agent_spawn") {
    const name = str(result.agent) ?? str(args.name) ?? "（未命名）";
    const model = str(result.model) ?? str(args.model);
    what = refused ? `想招 ${str(args.name) ?? "一位队友"}${str(args.model) ? `（${str(args.model)}）` : ""}` : `${name}${model ? ` · ${model}` : ""}`;
  } else if (action === "agent_retire") what = str(args.name) ?? "";
  else if (action === "agent_set_model") what = `${str(args.name) ?? ""} → ${str(args.model) ?? "默认"}`;
  else what = str(args.task_id) ?? str(args.task) ?? str(args.summary)?.slice(0, 80) ?? "";
  const state = running ? "进行中" : refused ? "被拒绝" : "完成";
  return (
    <div className={`action-card ${refused ? "refused" : running ? "running" : "done"}`}>
      <div className="action-head">
        <Icon name={refused ? "close" : action === "agent_spawn" ? "addMate" : "check"} size={14} />
        <span className="action-name">{ACTION_LABEL[action]}</span>
        {what && <span className="action-what">{what}</span>}
        <span className="action-state">{state}</span>
      </div>
      {refused && raw && <div className="action-reason">{raw.replace(/^(conflict|not found|invalid|unsupported)\s*:\s*/i, "")}</div>}
    </div>
  );
}

/** Steps shown while a run is open: the newest few, the rest behind "N 个更早的步骤". */
const OPEN_STEPS = 3;

/**
 * A run of tool calls and reasoning. Closed runs read as one summary line (failures stay
 * listed under it); open ones show the newest steps. A click overrides either.
 */
function Steps({ steps, closed }: { steps: Step[]; closed: boolean }) {
  const [manual, setManual] = useState<boolean>();
  const expanded = manual ?? !closed;
  const [all, setAll] = useState(false);

  if (!expanded) {
    const bad = steps.filter(failed);
    return (
      <div className="steps">
        <button className="steps-summary" onClick={() => setManual(true)} aria-expanded={false}>
          <span className="disclosure" aria-hidden="true">
            ▸
          </span>
          <span className={bad.length ? "fail" : "pass"}>{bad.length ? "✕" : "✓"}</span>
          {summarize(steps)}
        </button>
        {bad.map((s, i) => (
          <StepRow key={i} step={s} />
        ))}
      </div>
    );
  }
  const hidden = all || steps.length <= OPEN_STEPS ? 0 : steps.length - OPEN_STEPS;
  return (
    <div className="steps">
      {closed && (
        <button className="steps-summary" onClick={() => setManual(false)} aria-expanded={true}>
          <span className="disclosure" aria-hidden="true">
            ▾
          </span>
          {summarize(steps)}
        </button>
      )}
      {hidden > 0 && (
        <button className="tools-more" onClick={() => setAll(true)}>
          ▸ {hidden} 个更早的步骤
        </button>
      )}
      {steps.slice(hidden).map((s, i) => (
        <StepRow key={i} step={s} />
      ))}
    </div>
  );
}

function StepRow({ step }: { step: Step }) {
  if (step.type === "output") {
    return (
      <details className="thought">
        <summary>推理</summary>
        <div className="body">{step.text}</div>
      </details>
    );
  }
  if (step.type === "permission") {
    const allowed = step.decision === "allow" || step.decision === "allow_for_session";
    return (
      <div className="tool" title={`${step.kind} · ${step.decision}`}>
        <span className={allowed ? "pass" : "fail"}>{allowed ? "✓" : "✕"}</span>
        <span className="title muted">
          {allowed ? "已授权" : "已拒绝"} · {step.title}
        </span>
      </div>
    );
  }
  return <ToolStep step={step} />;
}

type ToolCall = Extract<TranscriptItem, { type: "tool_call" }>;

/** Output lines shown under a running command. */
const LIVE_LINES = 4;
/** Output lines shown when a step is opened. */
const OPEN_LINES = 400;

/** The session's working directory, for shortening paths in tool lines. */
const CwdContext = createContext<string | undefined>(undefined);

/** One tool call; a click opens what it did (output, exit code, diffs, arguments). */
function ToolStep({ step }: { step: ToolCall }) {
  const [open, setOpen] = useState(false);
  const d = step.detail;
  const running = step.status === "in_progress" || step.status === "pending";
  const tone = step.status === "completed" ? "ok" : step.status === "failed" ? "danger" : "accent pulse";
  const code = d?.exit_code;
  const live = running && step.output ? tailLines(step.output, LIVE_LINES) : undefined;
  const more = !!(d || step.output);
  const changes = d ? d.changes.flatMap((c) => parseDiff(c.diff, c.path)) : [];
  const cwd = useContext(CwdContext);
  return (
    <div className="tool-step">
      <button className="tool" onClick={() => more && setOpen(!open)} aria-expanded={more ? open : undefined} disabled={!more} title={step.title}>
        <span className={`dot ${tone}`} />
        <span className="title">{stepLine(step, cwd)}</span>
        {code != null && code !== 0 && <span className="chip danger tiny">退出 {code}</span>}
        {d?.duration_ms != null && <span className="muted tiny">{took(d.duration_ms)}</span>}
        {changes.length > 0 && <ChangeStat files={changes} />}
        {more && (
          <span className="disclosure muted" aria-hidden="true">
            {open ? "▾" : "▸"}
          </span>
        )}
      </button>
      {live && !open && <pre className="out live">{live.text}</pre>}
      {open && <ToolDetailView step={step} changes={changes} />}
    </div>
  );
}

function ToolDetailView({ step, changes }: { step: ToolCall; changes: DiffFile[] }) {
  const d = step.detail;
  const output = d?.output ?? step.output;
  const shown = output != null ? tailLines(output, OPEN_LINES) : undefined;
  return (
    <div className="tool-detail">
      {d?.command && <pre className="cmd">$ {d.command}</pre>}
      {d?.cwd && <div className="muted tiny mono">{d.cwd}</div>}
      {shown && (
        <>
          {shown.hidden > 0 && <div className="muted tiny">… 前面还有 {shown.hidden} 行</div>}
          <pre className="out">{shown.text || "（没有输出）"}</pre>
        </>
      )}
      {d?.exit_code != null && (
        <div className={`tiny ${d.exit_code === 0 ? "muted" : "fail"}`}>
          退出码 {d.exit_code}
          {d.duration_ms != null ? ` · ${took(d.duration_ms)}` : ""}
        </div>
      )}
      {changes.length > 0 && <DiffView files={changes} />}
      {d?.arguments && (
        <>
          <div className="muted tiny">参数</div>
          <pre className="out">{d.arguments}</pre>
        </>
      )}
      {d?.result && (
        <>
          <div className="muted tiny">结果</div>
          <pre className="out">{tailLines(d.result, OPEN_LINES).text}</pre>
        </>
      )}
      {d?.error && <div className="item-error">{d.error}</div>}
    </div>
  );
}

/** "70ms" under a second, else like `duration`. */
const took = (ms: number) => (ms < 1000 ? `${ms}ms` : duration(ms));

/** The tool column's switch, in each conversation's header. */
function ToolsToggle() {
  const { state } = useLayout();
  // Open, the column's own header has the button that folds it away: one place, like Codex.
  if (state.tools.open) return null;
  return (
    <button className="ghost icon" onClick={toggleTools} title="工具列：改动、任务、事件流" aria-label="工具列">
      <Icon name="panel" size={16} />
    </button>
  );
}

/** The bottom of a running turn: `••• 42s · 运行命令`. */
function LiveLine({ since, items }: { since: string; items: TranscriptItem[] }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  return (
    <div className="working">
      <span className="busy-dots" aria-hidden="true">
        <i />
        <i />
        <i />
      </span>
      <span className="muted small">{duration(now - new Date(since).getTime())}</span>
      <span>· {liveActivity(items)}</span>
    </div>
  );
}

const STOP_TEXT: Record<string, string> = {
  cancelled: "本轮已中断",
  error: "本轮出错结束",
  max_tokens: "达到输出上限",
  max_turn_requests: "达到单轮请求上限",
  refusal: "agent 拒绝了这次请求",
  interrupted: "本轮随上一个 hub 中断，结果未知，未自动重跑",
};

/** After a finished turn: how long it worked, when, and why it stopped if not normally. */
function TurnFoot({ turn }: { turn: TranscriptTurn }) {
  const start = turn.started_at ?? turn.sent_at;
  const worked = duration(new Date(turn.completed_at!).getTime() - new Date(start).getTime());
  const reason = turn.stop_reason && turn.stop_reason !== "end_turn" ? turn.stop_reason : null;
  return (
    <div className={`turn-note ${reason === "error" || reason === "interrupted" ? "danger" : ""}`} title={range(start, turn.completed_at)}>
      {reason ? `— ${STOP_TEXT[reason] ?? reason} · ` : ""}已工作 {worked} · {clock(turn.completed_at!)}
    </div>
  );
}

/** Files named on the summary line before the rest become "+N". */
const NAMED_FILES = 2;

/**
 * What a turn changed, under its reply: `✎ 改了 3 个文件 +12 −4 · src/auth.rs · src/login.tsx · +1 ▸`.
 * Opens to one row per file: its stat, "查看变更" (this turn's diff in the tool column) and
 * "复制路径". Read-only turns have no line.
 */
function TurnChanges({ turn, agent }: { turn: TranscriptTurn; agent: Agent }) {
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState<string>();
  const stat = turn.diff!;
  const named = stat.files.slice(0, NAMED_FILES);
  const rest = stat.files.length - named.length;
  const view = (file?: string) => openTool("changes", { agent: agent.name, turn: turn.message_id, file });
  const copy = async (path: string) => {
    try {
      await navigator.clipboard.writeText(path);
      setCopied(path);
      setTimeout(() => setCopied(undefined), 1500);
    } catch {
      // The path is on screen to copy by hand.
    }
  };
  return (
    <div className={`turn-changes ${open ? "open" : ""}`}>
      <button className="tch-line" onClick={() => setOpen(!open)} aria-expanded={open} title={stat.source === "hub" ? "由 hub 比较本轮前后的工作区得出" : undefined}>
        <span className="tch-pen" aria-hidden="true">
          <Icon name="diff" size={13} />
        </span>
        改了 {stat.files.length} 个文件 <ChangeStat files={[stat]} />
        {named.map((f) => (
          <span key={f.path} className="tch-file">
            · {f.path}
          </span>
        ))}
        {rest > 0 && <span className="tch-file">· +{rest}</span>}
        <span className="disclosure" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
      </button>
      {open && (
        <div className="tch-rows">
          {stat.files.map((f) => (
            <div key={f.path} className="tch-row">
              <span className="tch-path mono" title={f.path}>
                {f.path}
              </span>
              <ChangeStat files={[f]} />
              <span className="tch-actions">
                <button className="link" onClick={() => view(f.path)}>
                  查看变更
                </button>
                <button className="link" onClick={() => copy(f.path)}>
                  {copied === f.path ? "已复制" : "复制路径"}
                </button>
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

const QUEUE_OPEN = "crossagents.queueOpen";

/**
 * "接下来": messages waiting for their turn, on the composer's top edge. Your own can be sent
 * now (interrupting the current turn), edited (taken back into the composer) or removed;
 * orchestration messages are shown but left alone.
 */
function QueuePanel({ agent, queued, onEdit }: { agent: Agent; queued: TranscriptTurn[]; onEdit: (text: string) => void }) {
  const [open, setOpen] = useState(() => {
    try {
      return localStorage.getItem(QUEUE_OPEN) !== "0";
    } catch {
      return true;
    }
  });
  const [error, setError] = useState<string>();
  if (!queued.length) return null;

  const toggle = () => {
    setOpen(!open);
    try {
      localStorage.setItem(QUEUE_OPEN, open ? "0" : "1");
    } catch {
      // Remembered for this page only.
    }
  };
  const act = async (action: () => Promise<unknown>) => {
    setError(undefined);
    try {
      await action();
    } catch (err) {
      setError(errorText(err));
    }
  };
  const withdraw = (turn: TranscriptTurn) => api.withdrawMessage(agent.id, turn.message_id);

  return (
    <div className="queue">
      <button className="queue-head" onClick={toggle} aria-expanded={open}>
        <span className="disclosure" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
        接下来 · {queued.length}
        <span className="muted small">本轮结束后依次发给 {displayName(agent)}</span>
      </button>
      {open &&
        queued.map((turn) => {
          const mine = fromPerson(turn);
          return (
            <div key={turn.message_id} className="queue-row">
              <span className="queue-kind" title={mine ? "你发的" : "编排"}>
                {mine ? "你" : "编排"}
              </span>
              <span className="queue-text">{turn.input.split("\n")[0]}</span>
              {mine && (
                <span className="queue-actions">
                  <button
                    className="link"
                    title="打断当前这一轮，先发这条"
                    onClick={() =>
                      act(async () => {
                        await withdraw(turn);
                        await api.sendMessage(agent.id, { text: turn.input, now: true });
                      })
                    }
                  >
                    立即发送
                  </button>
                  <button
                    className="link"
                    onClick={() =>
                      act(async () => {
                        await withdraw(turn);
                        onEdit(turn.input);
                      })
                    }
                  >
                    编辑
                  </button>
                  <button className="link" onClick={() => act(() => withdraw(turn))}>
                    删除
                  </button>
                </span>
              )}
            </div>
          );
        })}
      {error && <ErrorBox error={error} />}
    </div>
  );
}

function Composer({
  agent,
  capabilities,
  draft,
  setDraft,
  plans,
  onPlanDone,
  mates,
}: {
  agent: Agent;
  capabilities: Capabilities | null;
  draft: string;
  setDraft: (text: string) => void;
  plans: RunDetail[];
  onPlanDone: () => void;
  /** Agents @ offers, besides the models. */
  mates: Agent[];
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [deferred, setDeferred] = useState<string[]>([]);
  const ref = useRef<HTMLTextAreaElement>(null);
  const plugins = useHubData(() => api.listPlugins().then((r) => r.plugins), [], () => false);
  const mention = useMention(ref, setDraft, mentionsFor(mates, (plugins.data ?? []).filter((p) => p.models.length > 0)));
  const running = agent.status === "running";
  const canInterrupt = running && capabilities?.interrupt !== false;
  const plan = plans.find((d) => !deferred.includes(d.run.id));

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 240)}px`;
  }, [draft, plan]);

  useEffect(() => {
    if (!plan) ref.current?.focus();
  }, [agent.id, plan]);

  async function send(now = false) {
    const text = draft.trim();
    if (!text || busy) return;
    setBusy(true);
    setError(undefined);
    // Shown at once (outbox.ts); a failure puts the text back here.
    setDraft("");
    try {
      await sendMessage(agent.id, text, { now, takeBack: true });
    } catch (err) {
      setDraft(text);
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  async function interrupt() {
    try {
      await api.cancelAgent(agent.id);
    } catch (err) {
      setError(errorText(err));
    }
  }

  function onKey(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.nativeEvent.isComposing) return;
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      // Ctrl+Shift+Enter: stop the current turn and send this instead.
      void send(e.ctrlKey && running);
    } else if (e.key === "Escape" && canInterrupt) {
      e.preventDefault();
      void interrupt();
    }
  }

  if (plan) {
    return <PlanDock key={plan.run.id} run={plan.run} onDone={onPlanDone} onReply={() => setDeferred([...deferred, plan.run.id])} />;
  }

  return (
    <div className="composer-wrap">
      {error && <ErrorBox error={error} />}
      {plans.length > 0 && (
        <button className="dock-recall" onClick={() => setDeferred([])}>
          <span className="disclosure" aria-hidden="true">
            ▴
          </span>
          Plan 待审批 · {plans[0].run.plan.length} 个任务 · 展开审批
        </button>
      )}
      <div className="composer">
        {mention.menu}
        <textarea
          ref={ref}
          rows={1}
          value={draft}
          onChange={(e) => {
            setDraft(e.target.value);
            mention.sync();
          }}
          onClick={mention.sync}
          onKeyDown={(e) => mention.onKeyDown(e) || onKey(e)}
          placeholder={running ? `${displayName(agent)} 正在工作，新消息会排在本轮之后` : `给 ${displayName(agent)} 发消息，@ 点名协作者或模型`}
        />
        <div className="bar">
          {agent.plugin === "external" ? (
            <span className="chip">
              <Vendor plugin={agent.plugin} model={agent.model} />
            </span>
          ) : (
            <ModelEffortPicks agent={agent} />
          )}
          {capabilities?.resume && <span className="chip" title="会话可挂起后恢复">resumable</span>}
          <div className="spacer" />
          {/* One button: stop while it works and nothing is typed, else send (which queues). */}
          {canInterrupt && !draft.trim() ? (
            <button className="primary send" onClick={interrupt} title="中断本轮（Esc）" aria-label="中断本轮">
              <Icon name="stop" size={14} />
            </button>
          ) : (
            <button className="primary send" disabled={busy || !draft.trim()} onClick={() => send()} aria-label="发送">
              <Icon name="send" size={16} />
            </button>
          )}
        </div>
      </div>
      <div className="composer-hint">
        {running ? "Enter 排队 · Ctrl+Shift+Enter 打断并发送 · Esc 中断" : "Enter 发送 · Shift+Enter 换行 · agent 离线时会自动启动"}
      </div>
    </div>
  );
}

/**
 * A plan waiting for approval, docked where the composer is (the draft is kept). Keys: 1
 * approve, 2 reject with a reason, 3 or Esc reply to the planner first.
 */
function PlanDock({ run, onDone, onReply }: { run: Run; onDone: () => void; onReply: () => void }) {
  const [rejecting, setRejecting] = useState(false);
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => ref.current?.focus(), []);

  async function act(action: () => Promise<unknown>) {
    setBusy(true);
    setError(undefined);
    try {
      await action();
      onDone();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }
  const approve = () => act(() => api.approveRun(run.id));

  function onKey(e: KeyboardEvent<HTMLDivElement>) {
    if (rejecting || busy || e.target !== e.currentTarget) return;
    if (e.key === "1") approve();
    else if (e.key === "2") setRejecting(true);
    else if (e.key === "3" || e.key === "Escape") onReply();
    else return;
    e.preventDefault();
  }

  return (
    <div className="composer-wrap">
      <div className="plan-dock" tabIndex={-1} ref={ref} onKeyDown={onKey} role="dialog" aria-label="Plan 待审批">
        <header>
          <StateWord state={{ glyph: "○", word: "Plan 待审批", tone: "warn", bucket: "needs_you", live: false }} />
          <span className="muted small">{run.plan.length} 个任务</span>
          <span className="spacer" />
          <a className="small" href={`#/runs/${run.id}`}>
            {run.title}
          </a>
        </header>
        <div className="plan-dock-body">
          <PlanTasks run={run} />
          {run.note && <div className="small muted plan-dock-note">上次驳回：{run.note}</div>}
        </div>
        <footer>
          {rejecting ? (
            <>
              <textarea rows={2} autoFocus placeholder="驳回理由（必填），会转给 Planner" value={reason} onChange={(e) => setReason(e.target.value)} />
              <button className="danger" disabled={busy || !reason.trim()} onClick={() => act(() => api.rejectRun(run.id, { reason: reason.trim() }))}>
                驳回
              </button>
              <button className="ghost" onClick={() => setRejecting(false)}>
                取消
              </button>
            </>
          ) : (
            <>
              <button className="primary" disabled={busy} onClick={approve}>
                <kbd>1</kbd> Approve · 开工
              </button>
              <button disabled={busy} onClick={() => setRejecting(true)}>
                <kbd>2</kbd> 驳回并说明
              </button>
              <button className="ghost" onClick={onReply}>
                <kbd>3</kbd> 先回复 Planner
              </button>
            </>
          )}
        </footer>
        {error && <ErrorBox error={error} />}
      </div>
    </div>
  );
}

