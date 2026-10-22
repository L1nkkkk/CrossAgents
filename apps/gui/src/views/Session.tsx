import { createContext, useContext, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { Capabilities } from "@bindings/Capabilities";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { TranscriptUsage } from "@bindings/TranscriptUsage";
import { view } from "../activity";
import { api, errorText } from "../api";
import { ACTIVE_RUN, useRunTree } from "../App";
import { Avatar, ErrorBox, Icon, Mark, Prose, StatusChip, Vendor, agentTitle, agentTone, displayName, roleShape, time, useAgents, vendorName } from "../components";
import { diffStat, parseDiff, tailLines, type DiffFile } from "../lib/diff";
import { taskState } from "../lib/taskState";
import { duration, failed, liveActivity, stepLine, summarize, workGroups, type Step } from "../lib/workGroups";
import { PeerMessage, StateWord, useArmed } from "../parts";
import { useSession, type TranscriptItem, type TranscriptTurn } from "../session";
import { QuestionCards } from "../questions";
import { NeedsYouStrip, RunStrip } from "../strips";
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

  return (
    <section className="pane">
      <Header agent={agent} capabilities={capabilities} usage={usage} driver={transcript.driver} tasks={tasks} onClose={onClose} />
      <Conversation agent={agent} turns={turns.filter(started)} agents={everyone} tree={runs} />
      <div className="dock">
        {/* One strip is enough: the pane beside leaves it to the main one. */}
        {!onClose && <QuestionCards agents={everyone} />}
        {!onClose && <NeedsYouStrip agents={everyone} tree={runs} />}
        <RunStrip runs={liveRuns} agents={everyone} />
        <QueuePanel agent={agent} queued={queued} onEdit={setDraft} />
        <Composer agent={agent} capabilities={capabilities} draft={draft} setDraft={setDraft} plans={pendingPlans} onPlanDone={reload} />
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
  onClose,
}: {
  agent: Agent;
  capabilities: Capabilities | null;
  usage: TranscriptUsage | null;
  driver: string | null;
  tasks: { run: Run; task: RunDetail["tasks"][number] }[];
  onClose?: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
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
        <Avatar agent={agent} size={26} />
        <span className="who-block">
          <span className="title">{displayName(agent)}</span>
          <span className="who-sub">
            {agentTitle(agent)} · @{agent.name}
          </span>
        </span>
        <StatusChip status={agent.status} tone={agentTone(agent.status)} />
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
        <div className="spacer" />
        {usage && usage.size > 0 && <Meter usage={usage} />}
        {agent.status === "idle" && usage && usage.size > 0 && usage.used / usage.size >= 0.5 && (
          <button className="ghost small" disabled={busy} onClick={() => run(() => api.compactAgent(agent.name))} title="让 agent 把历史压缩成摘要，腾出上下文">
            压缩上下文
          </button>
        )}
        {agent.status === "running" && capabilities?.interrupt !== false && (
          <button disabled={busy} onClick={() => run(() => api.cancelAgent(agent.name))} title="中断本轮（Esc）">
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
            onClick={() => stop.click(() => run(() => api.stopAgent(agent.name)))}
            title="停止进程；下次发消息会重新启动"
          >
            {stop.armed ? "再点一次停止" : "停止"}
          </button>
        )}
        {onClose && (
          <button className="ghost icon" onClick={onClose} title="关闭这一栏" aria-label="关闭这一栏">
            <Icon name="close" size={15} />
          </button>
        )}
      </div>
      {error && (
        <div style={{ padding: "8px 20px 0" }}>
          <ErrorBox error={error} />
        </div>
      )}
    </>
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

function Conversation({ agent, turns, agents, tree, children }: { agent: Agent; turns: TranscriptTurn[]; agents: Agent[]; tree: RunDetail[]; children?: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  useLayoutEffect(() => {
    const el = ref.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });

  return (
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
          {turns.map((turn) => (
            <Turn key={turn.message_id} turn={turn} agent={agent} agents={agents} tree={tree} />
          ))}
        </CwdContext.Provider>
        {children}
      </div>
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
    <div className="turn">
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
    </div>
  );
}

function Item({ item }: { item: TranscriptItem }) {
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

function ChangeStat({ files }: { files: DiffFile[] }) {
  const { added, removed } = diffStat(files);
  if (!added && !removed) return null;
  return (
    <span className="tiny mono">
      <span className="add">+{added}</span> <span className="del">−{removed}</span>
    </span>
  );
}

/** Unified diffs, one block per file. */
function DiffView({ files }: { files: DiffFile[] }) {
  return (
    <div className="diff">
      {files.map((file, i) => (
        <div key={i} className="diff-file">
          <div className="diff-head">
            <span className="mono">{file.path || "（未命名）"}</span>
            <ChangeStat files={[file]} />
          </div>
          <pre className="diff-body">
            {file.lines.map((line, j) => (
              <div key={j} className={`dl ${line.kind}`}>
                <span className="sign">{line.kind === "add" ? "+" : line.kind === "del" ? "−" : " "}</span>
                {line.text}
              </div>
            ))}
          </pre>
        </div>
      ))}
    </div>
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
};

/** After a finished turn: how long it worked, when, and why it stopped if not normally. */
function TurnFoot({ turn }: { turn: TranscriptTurn }) {
  const [open, setOpen] = useState(false);
  const start = turn.started_at ?? turn.sent_at;
  const worked = duration(new Date(turn.completed_at!).getTime() - new Date(start).getTime());
  const reason = turn.stop_reason && turn.stop_reason !== "end_turn" ? turn.stop_reason : null;
  const files = turn.diff ? parseDiff(turn.diff) : [];
  return (
    <>
      <div className={`turn-note ${reason === "error" ? "danger" : ""}`} title={`${time(start)} → ${time(turn.completed_at!)}`}>
        {reason ? `— ${STOP_TEXT[reason] ?? reason} · ` : ""}已工作 {worked} · {clock(turn.completed_at!)}
        {files.length > 0 && (
          <button className="turn-diff" onClick={() => setOpen(!open)} aria-expanded={open}>
            · 改动 {files.length} 个文件 <ChangeStat files={files} /> {open ? "▾" : "▸"}
          </button>
        )}
      </div>
      {open && <DiffView files={files} />}
    </>
  );
}

/** "11:02", "昨天 18:40", "10-03 18:40". */
function clock(iso: string): string {
  const d = new Date(iso);
  const hm = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const days = Math.round((new Date(new Date().toDateString()).getTime() - new Date(d.toDateString()).getTime()) / 86_400_000);
  if (days <= 0) return hm;
  if (days === 1) return `昨天 ${hm}`;
  return `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")} ${hm}`;
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
  const withdraw = (turn: TranscriptTurn) => api.withdrawMessage(agent.name, turn.message_id);

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
                        await api.sendMessage(agent.name, { text: turn.input, now: true });
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
}: {
  agent: Agent;
  capabilities: Capabilities | null;
  draft: string;
  setDraft: (text: string) => void;
  plans: RunDetail[];
  onPlanDone: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [deferred, setDeferred] = useState<string[]>([]);
  const ref = useRef<HTMLTextAreaElement>(null);
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
    try {
      await api.sendMessage(agent.name, { text, now: now || undefined });
      setDraft("");
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  async function interrupt() {
    try {
      await api.cancelAgent(agent.name);
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
        <textarea
          ref={ref}
          rows={1}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onKey}
          placeholder={running ? `${displayName(agent)} 正在工作，新消息会排在本轮之后` : `给 ${displayName(agent)} 发消息`}
        />
        <div className="bar">
          <span className="chip">
            <Vendor plugin={agent.plugin} model={agent.model} />
          </span>
          {capabilities?.resume && <span className="chip" title="会话可挂起后恢复">resumable</span>}
          <div className="spacer" />
          {canInterrupt && (
            <button className="stop-btn" onClick={interrupt} title="中断本轮（Esc）" aria-label="中断本轮">
              <Icon name="stop" size={14} />
            </button>
          )}
          <button className="primary send" disabled={busy || !draft.trim()} onClick={() => send()} aria-label="发送">
            <Icon name="send" size={16} />
          </button>
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

