import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode, type RefObject } from "react";
import { shortcut, shortcutPlatform } from "../lib/shortcuts";
import type { Agent } from "@bindings/Agent";
import type { Capabilities } from "@bindings/Capabilities";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { RunStatus } from "@bindings/RunStatus";
import type { Handoff } from "@bindings/Handoff";
import type { ModelSwitch } from "@bindings/ModelSwitch";
import type { TranscriptUsage } from "@bindings/TranscriptUsage";
import { useDelivery, view } from "../activity";
import { api, errorText } from "../api";
import { TurnFiles } from "../attach";
import { HandoffLine } from "../handoff";
import { dismiss as dismissMessage, retry as retryMessage, useOutbox, type Outgoing } from "../outbox";
import { ACTIVE_RUN, useRunTree, useRunsLedBy } from "../App";
import { AgentImageContext, Avatar, ErrorBox, FileScopeProvider, Icon, loadAgentImage, Mark, type ImageLoad, OneLine, StatusChip, Vendor, agentTitle, agentTone, beyondFirstLine, displayName, firstLine, firstSentence, roleShape, time, useAgents, useFileScope, vendorName } from "../components";
import { Image as ImageIcon } from "lucide-react";
import { extractToolImagePaths, extractToolImages } from "../lib/toolimages";
import { ToolImages } from "../toolimages";
import { StepProducts } from "../stepproducts";
import { outcomeOf } from "../lib/stepOutcome";
import { ChangeStat, DiffView } from "../diffview";
import { parseDiff, tailLines, type DiffFile } from "../lib/diff";
import { errorHint } from "../lib/errors";
import { isModerationError } from "../lib/moderation";
import { fields, plainText, toolChanges, toolError, toolFailed, toolShowsStat } from "../lib/toolcall";
import { effortLabel, prettyModel } from "../lib/models";
import { clock, range } from "../lib/time";
import { InfoRail, RAIL_ROOM, SIDE_ROOM, useRail } from "../rail";
import { ScrollThumb } from "../scrollthumb";
import { TurnMap, type MapEntry } from "../turnmap";
import { focusSession, isTermTab, openTool, toggleTools, useLayout } from "../panes";
import { useToolCards } from "../extensions/defaults";
import { cardFor } from "../extensions/toolcards";
import { ToolCardView } from "../extensions/ui";
import { shownTab } from "../lib/docks";
import { terminalsAvailable, toggleTerminal, useTabShows } from "../terminal";
import { useWidths } from "../lib/widths";
import { useSidebar } from "../titlebar";
import { planWaitsForPerson } from "../lib/policy";
import { runState, taskState } from "../lib/taskState";
import { QUIET_AFTER, actionOf, duration, failed, liveActivity, liveKey, liveNow, quietSeconds, stepLine, workGroups, type ActionTool, type Step } from "../lib/workGroups";
import { TurnLive, type LiveTurn } from "../liveTurn";
import { PeerMessage, StateWord, useArmed } from "../parts";
import { useSession, turnExtras, type TranscriptItem, type TranscriptTurn } from "../session";
import { reasonText } from "../lib/delivery";
import { dismissNotice, useNotices } from "../notices";
import { QuestionCards, pending, useQuestions } from "../questions";
import { executorOf } from "../lib/projectView";
import "./sessiontools.css";
import { workDecisions, type WorkDecision } from "../lib/workNotes";
import { NeedsYouStrip, RunStrip } from "../strips";
import { useHubData } from "../hub";
import { ContextUsageButton, useCompact } from "../contextusage";
import { homeOf, isChat, wsKey } from "../workspace";
import { ProfileEditor } from "./Agents";
import { Composer, QueuePanel } from "./SessionComposer";
import { AgentText, Bubble, fromPerson } from "./SessionMessage";
import { SteerItem, TurnEnded } from "./SessionSteer";
import { SessionModeration } from "./SessionModeration";

const started = (turn: TranscriptTurn) => !!turn.started_at || !!turn.completed_at;

/**
 * Whether a turn is drawn in the conversation. A person's own message keeps its row from
 * submit to end, queued or running or withdrawn, so it never moves to 接下来 and back
 * (docs/tasks/DELIVERY-AUDIT.md §2, §8 P0-1).
 */
const inConversation = (turn: TranscriptTurn) => started(turn) || fromPerson(turn) || !!turnExtras(turn).withdrawn;

/** What 接下来 lists: orchestration messages, waiting; the person's own stay in the chat. */
const inNextUp = (turn: TranscriptTurn) => !started(turn) && !fromPerson(turn) && !turnExtras(turn).withdrawn;

/**
 * The latest started turn, when it failed because the gateway's content moderation refused the
 * context: its error item says so, or the agent's own error does while it is in error. A later
 * turn that went through means the reminder has done its job and is gone.
 */
function moderationTurn(turns: TranscriptTurn[], status: Agent["status"], error?: string): TranscriptTurn | undefined {
  const last = turns.filter((turn) => !!turn.started_at).at(-1);
  if (!last) return undefined;
  const fromItems = last.items.some((item) => item.type === "error" && isModerationError(item.message));
  return fromItems || (status === "error" && isModerationError(error)) ? last : undefined;
}

/** Which of a session's active tasks it is on now: running, then review / verification, then the rest. */
function rankTask(status: RunDetail["tasks"][number]["status"]): number {
  return status === "running" ? 0 : status === "reviewing" || status === "verifying" ? 1 : 2;
}

/**
 * What the pane's own header needs to know about the side slot: whether the rail is a drawer
 * (too narrow to sit beside the conversation), whether it is open, and how to open it. A header
 * that carries the run's facts uses this for its 面板 button; nothing is moved back into the
 * conversation when the rail does not fit.
 */
const SideSlot = createContext<{ narrow: boolean; open: boolean; toggle: () => void }>({
  narrow: false,
  open: false,
  toggle: () => {},
});

export const useSideSlot = () => useContext(SideSlot);

/**
 * One round of a work group, marked where it began in the continuous conversation
 * (docs/tasks/G1.md §3.5): `n` counts from 1, `at` is the round's `created_at`.
 */
export type RoundMark = { id: string; n: number; title: string; status: RunStatus; at: string };

/**
 * A conversation with one agent. The run page shows its lead's conversation through this
 * view (docs/tasks/W1.md §8): `header` replaces the agent's header, `top` comes before the
 * messages, `bottom` after them, and `side` takes the rail beside them — or, when the window
 * is too narrow for that, a drawer the header opens. `side` never joins the conversation.
 * `rounds` draws a separator where each of a work group's rounds began.
 */
export function SessionView({
  name,
  onClose,
  questionId,
  header,
  top,
  bottom,
  side,
  sideLabel = "run 的面板",
  rounds,
}: {
  name: string;
  questionId?: string;
  onClose?: () => void;
  header?: ReactNode;
  top?: ReactNode;
  bottom?: ReactNode;
  side?: ReactNode;
  /** What the side slot's drawer calls itself when the rail does not fit. */
  sideLabel?: string;
  rounds?: RoundMark[];
}) {
  const { transcript, error, reload } = useSession(name);
  const tree = useRunTree();
  const agents = useAgents();
  const [draft, setDraft] = useState("");
  const agentId = transcript?.agent.id;
  const ledRuns = useRunsLedBy(agentId);
  const execution = useHubData(
    () => agentId ? api.agentExecution(agentId) : Promise.resolve(null),
    [agentId],
    (e) => ["agent.updated", "agent.status_changed", "task.status_changed", "task.handed_off", "run.status_changed"].includes(e.type),
  );
  const questions = useQuestions();
  const questionArea = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!questionId || !questions.data || !agentId) return;
    const index = pending(questions.data.items).findIndex((item) => item.id === questionId && item.from === agentId);
    if (index < 0) return;
    const area = questionArea.current;
    if (!area) return;
    let done = false;
    const locate = () => {
      if (done) return;
      const card = area.querySelectorAll<HTMLElement>(".asks > .ask")[index];
      if (!card) {
        // 3 is questions.tsx's FIT; update both if its visible-card limit changes.
        if (index >= 3) area.querySelector<HTMLButtonElement>(".tools-more")?.click();
        return;
      }
      done = true;
      card.id = `human-item-${questionId}`;
      card.scrollIntoView({ block: "nearest" });
      const input = card.querySelector<HTMLElement>("input, textarea");
      if (!input) card.tabIndex = -1;
      (input ?? card).focus();
    };
    const observer = new MutationObserver(locate);
    observer.observe(area, { childList: true, subtree: true });
    locate();
    return () => observer.disconnect();
  }, [questionId, questions.data, agentId]);
  const outgoing = useOutbox(agentId, transcript?.turns);
  const { hidden: railOff } = useRail();
  // The run's side slot: a rail beside the conversation while it has room, else a drawer the
  // header opens. The plain info rail needs more room, since nothing waits on it.
  const [drawer, setDrawer] = useState(false);
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

  // Wide enough for a rail again: the drawer has nothing left to do.
  const rail = !railOff && width >= (side ? SIDE_ROOM : RAIL_ROOM);
  useEffect(() => {
    if (rail) setDrawer(false);
  }, [rail]);

  // A drawer that no longer has to be one (the window widened) is not a drawer left open.
  const drawerRef = useRef<HTMLElement | null>(null);
  useEffect(() => {
    if (!drawer) return;
    drawerRef.current?.focus();
    const close = () => setDrawer(false);
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    window.addEventListener("keydown", onKey);
    // Opening a task in the tool column replaces the drawer: it has served its purpose.
    window.addEventListener("hashchange", close);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("hashchange", close);
    };
  }, [drawer]);

  if (!transcript) {
    // Worker and reviewer clones are removed once their task ends.
    const gone = !!error && /not found/i.test(error);
    return (
      <section className="pane">
        {/* The run page's header does not wait for the lead's conversation. */}
        {header}
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
  // A plan waits in the lead's own conversation and in the chat that started the Work: the
  // person said 开始 there, so that is where they pick who runs each task (AGENT-WORKSPACE §6.1).
  // A run the policy approves on its own never gets a card, even while a stale
  // `awaiting_approval` status is still on record (lib/policy.ts planWaitsForPerson).
  const pendingPlans = runs.filter(
    (d) => (d.run.planner === agent.id || d.run.origin_chat_id === agent.id) && d.run.status === "awaiting_approval" && planWaitsForPerson(d.run),
  );
  const liveRuns = runs.filter((d) => d.run.planner === agent.id && ACTIVE_RUN.includes(d.run.status) && d.run.status !== "awaiting_approval");
  // First iteration: recent runs plus all runs led by this agent. Older assigned tasks may be
  // absent; fetching every run's detail for every session and event would be too expensive.
  const associated = [...new Map([...runs, ...(ledRuns.data ?? [])].map((d) => [d.run.id, d])).values()];
  const tasks = associated.flatMap((d) => d.tasks.filter((t) => t.assignee === agent.id || t.reviewer === agent.id).map((t) => ({ run: d.run, task: t })));
  const queued = turns.filter(inNextUp);
  const poisoned = moderationTurn(turns, agent.status, transcript.error);
  const led = ledRuns.data ?? [];
  const recruits = everyone.filter((a) => (a.config as Record<string, unknown> | null)?.spawned_by_agent === agent.id && !a.archived_at);
  // The turn running now, and (when it works a task for a run) that task's counts: both are in
  // the header as well as on the live line under the turn (docs/TODO.md §四 ②).
  const runningTurn = turns.findLast((turn) => !!turn.started_at && !turn.completed_at);
  const workingTask = tasks
    .filter(({ task }) => taskState(task).bucket === "active")
    .sort((a, b) => rankTask(a.task.status) - rankTask(b.task.status) || b.task.updated_at.localeCompare(a.task.updated_at))[0];
  const live: LiveTurn | undefined = runningTurn
    ? {
        agentId: agent.id,
        since: Date.parse(runningTurn.started_at!),
        items: runningTurn.items,
        ...(workingTask ? { task: workingTask.task, siblings: runs.find((d) => d.run.id === workingTask.run.id)?.tasks } : {}),
      }
    : undefined;
  // Who @ offers: this conversation's recruits, then the other agents working in its project.
  const mates = [
    ...recruits,
    ...(isChat(agent) ? [] : everyone.filter((a) => a.id !== agent.id && !a.archived_at && !recruits.includes(a) && wsKey(homeOf(a)) === wsKey(homeOf(agent)))),
  ];

  return (
    <SideSlot.Provider value={{ narrow: !rail, open: drawer, toggle: () => setDrawer((d) => !d) }}>
      <section className={`pane ${rail ? "with-rail" : ""} ${side ? "run-side" : ""}`} ref={measure} onPointerDownCapture={() => focusSession(name)} onFocusCapture={() => focusSession(name)}>
        {header ?? <Header agent={agent} capabilities={capabilities} usage={usage} driver={transcript.driver} tasks={tasks} live={live} rail={rail} onClose={onClose} />}
        <WorkContext name={name} agent={agent} tasks={tasks} led={led} agents={everyone} execution={execution.data?.execution} />
        <ExecutionBar agent={agent} execution={execution.data?.execution} reload={execution.reload} />
        <NoticeBar agent={agent} />
        <Conversation agent={agent} turns={turns.filter(inConversation)} switches={transcript.switches ?? []} handoffs={handoffsOf(ledRuns.data ?? [])} works={workDecisions(questions.data?.items ?? [], agent.id)} agents={everyone} tree={runs} top={top} rounds={rounds}>
          {outgoing.map((m) => (
            <Sending key={m.key} message={m} agent={agent} />
          ))}
          {bottom}
        </Conversation>
        {rail && side && (
          <aside className="rail rail-run" aria-label={sideLabel}>
            {side}
          </aside>
        )}
        {rail && !side && <InfoRail agent={agent} usage={usage} turns={turns} tasks={tasks} led={led} agents={everyone} recruits={recruits} spend={transcript.conversation_cost_usd} />}
        {/* No room beside the conversation: the same panels, over it, opened from the header. */}
        {!rail && side && drawer && (
          <>
            <div className="rail-scrim" onClick={() => setDrawer(false)} />
            <aside className="rail rail-run rail-drawer" role="dialog" aria-label={sideLabel} ref={drawerRef} tabIndex={-1}>
              <div className="rail-drawer-head">
                <strong>{sideLabel}</strong>
                <span className="spacer" />
                <button className="ghost icon" onClick={() => setDrawer(false)} aria-label="关闭面板" title="关闭面板">
                  <Icon name="close" size={14} />
                </button>
              </div>
              {side}
            </aside>
          </>
        )}
        <div className="dock">
          {/* One strip is enough: the pane beside leaves it to the main one. A run page passes its
              own strip (`top`), scoped to that run, so the dock's list would add every other run's
              「告诉你」 (docs/tasks/SELF.md B3). */}
          {!onClose && !side && <div className="dock-asks" ref={questionArea}><QuestionCards agents={everyone} />{questionId && !questions.loading && (!questions.data || !pending(questions.data.items).some((item) => item.id === questionId && item.from === agent.id)) && <p role="alert" className="question-missing">找不到这条待办：它可能已处理、已过期或不属于此会话。{questions.error && ` ${questions.error}`}</p>}</div>}
          {!onClose && <NeedsYouStrip agents={everyone} tree={runs} />}
          <RunStrip runs={liveRuns} agents={everyone} />
          <QueuePanel agent={agent} queued={queued} delivery={transcript.delivery} />
          {agent.status === "error" && transcript.error && !isModerationError(transcript.error) && (
            <div className="error-bar" role="alert" title={transcript.error}>
              <strong>{errorHint(transcript.error, transcript.exit)}</strong>
              <span className="mono small">{transcript.error}</span>
            </div>
          )}
          {poisoned && !agent.archived_at && <SessionModeration agent={agent} turn={poisoned} />}
          {agent.archived_at ? (
            <div className="archived-bar">
              <Icon name="archive" size={14} />
              <span>这个对话已归档，它招来的队友也一起停下了。恢复后可以继续。</span>
              <button onClick={() => void api.archiveAgent(agent.id, false)}>恢复对话</button>
            </div>
          ) : (
            <Composer agent={agent} capabilities={capabilities} driver={transcript.driver} delivery={transcript.delivery} draft={draft} setDraft={setDraft} plans={pendingPlans} onPlanDone={reload} mates={mates} />
          )}
        </div>
      </section>
    </SideSlot.Provider>
  );
}

type Execution = Awaited<ReturnType<typeof api.agentExecution>>["execution"];

function WorkContext({ name, agent, tasks, led, agents, execution: x }: { name: string; agent: Agent; tasks: { run: Run; task: RunDetail["tasks"][number] }[]; led: RunDetail[]; agents: Agent[]; execution?: Execution }) {
  const current = tasks.find(({ task }) => task.id === x?.task_id);
  const lead = led.find(({ run }) => run.id === x?.run_id);
  const source = current ? executorOf(current.task, agents, current.task.reviewer === agent.id ? "reviewer" : "worker") : undefined;
  if (!source?.fresh && !lead?.run.lead_note && !tasks.length && !led.length) return null;
  return <div className="session-work-context">
    {source?.fresh && <span>{source.template ? `使用 ${displayName(source.template)} 的配置创建` : "新建的执行会话"}</span>}
    {lead?.run.lead_note && <span>{lead.run.lead_note}</span>}
    {(tasks.length > 0 || led.length > 0) && <details><summary>关联工作</summary><div className="session-work-list">
      {led.map(({ run }) => <button key={run.id} onClick={() => { focusSession(name); openTool("task", { agent: name, run: run.id }); }}>{run.title} · 计划</button>)}
      {tasks.map(({ run, task }) => <button key={task.id} onClick={() => { focusSession(name); openTool("task", { agent: name, run: run.id, task: task.id }); }}>{task.key} {task.title} · {run.title}</button>)}
    </div></details>}
  </div>;
}

/**
 * What the session does for a run right now (docs/tasks/S1.md §3.3): 正在执行 run X · 任务 y,
 * with 中断 — the person's messages still wait their turn behind it, so this is how they take
 * the session back. Nothing when no run holds it.
 */
function ExecutionBar({ agent, execution: x, reload }: { agent: Agent; execution?: Execution; reload: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  if (!x) return null;
  async function interrupt() {
    setBusy(true);
    setError(undefined);
    try {
      await api.interruptExecution(agent.id);
      reload();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }
  const doing = x.role === "lead" ? "正在主导" : x.role === "reviewer" ? "正在审查" : "正在执行";
  return (
    <div className="exec-bar" role="status">
      <Icon name="branch" size={14} />
      <span>
        {doing} <a href={`#/runs/${x.run_id}`}>{x.run_title}</a>
        {x.task_key && x.task_id && (
          <>
            {" · 任务 "}
            <a href={`#/runs/${x.run_id}/tasks/${x.task_id}`}>
              <code className="bare">#{x.task_key}</code>
            </a>
          </>
        )}
        <span className="muted"> · {x.fresh ? "为它新开的执行会话" : "本人，带着自己的上下文"}；你发的消息会排在这一轮后面</span>
      </span>
      <span className="spacer" />
      {error && <span className="error small">{error}</span>}
      {x.task_id && (
        <button disabled={busy} onClick={() => void interrupt()} title="中断这一轮：执行记为被人打断，会话回到工作区，主导收到通知后改派">
          中断执行
        </button>
      )}
    </div>
  );
}

/**
 * What the session lost on the way back (src/notices.ts), in the hub's words: kept at the top
 * until the person closes each one, rather than folded under an old message.
 */
function NoticeBar({ agent }: { agent: Agent }) {
  const notices = useNotices(agent.id);
  if (!notices.length) return null;
  return (
    <div className="notice-bar" role="alert">
      {notices.map((n) => (
        <div key={n.id} className="notice-row">
          <span className="notice-text">{n.message}</span>
          <span className="notice-at">{clock(n.at)}</span>
          <button className="ghost icon" onClick={() => dismissNotice(agent.id, n.id)} title="知道了，关掉这条提示" aria-label="关掉这条提示">
            <Icon name="close" size={13} />
          </button>
        </div>
      ))}
    </div>
  );
}

/**
 * The command that carries this session on in its own CLI, in a terminal, by the same
 * session id (docs/TODO.md §十五). The hub writes it; when it cannot be done, the copy
 * button is off and the hub's reason is shown.
 */
function ResumePanel({ agent, onClose }: { agent: Agent; onClose: () => void }) {
  const now = useHubData(
    () => api.resumeCommand(agent.id),
    [agent.id],
    (e) => (e.type === "agent.updated" && e.data.agent.id === agent.id) || (e.type === "agent.status_changed" && e.data.id === agent.id),
  );
  const [copied, setCopied] = useState(false);
  const r = now.data;
  async function copy() {
    if (!r?.command) return;
    try {
      await navigator.clipboard.writeText(r.command);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // The command is shown; it can be copied by hand.
    }
  }
  return (
    <div className="resume-pop">
      <div className="resume-head">
        <strong>在终端里接着聊</strong>
        <span className="muted small">用同一个 session id 恢复原生 CLI 会话{r?.shell === "powershell" ? "，PowerShell 写法" : ""}</span>
        <span className="spacer" />
        <button className="ghost icon" onClick={onClose} title="收起" aria-label="收起">
          <Icon name="close" size={14} />
        </button>
      </div>
      {now.error && <ErrorBox error={now.error} />}
      {!r ? (
        !now.error && <p className="muted small">读取中…</p>
      ) : (
        <>
          {r.command ? <pre className="resume-command">{r.command}</pre> : <p className="resume-why">{r.unavailable}</p>}
          <div className="resume-actions">
            <button className="small" disabled={!r.command} onClick={() => void copy()} title={r.unavailable ?? "复制到剪贴板，粘贴到终端运行"}>
              <span className="row" style={{ gap: 5 }}>
                <Icon name={copied ? "check" : "copy"} size={13} />
                {copied ? "已复制" : "复制原生恢复命令"}
              </span>
            </button>
            {r.session_id && <span className="muted small mono">session {r.session_id}</span>}
          </div>
          {r.command && r.env.length > 0 && (
            <p className="muted small">
              带上的环境：{r.env.map((e) => e.name).join("、")}。只有账号配置目录和代理，不含令牌。
            </p>
          )}
          {r.notes.map((n) => (
            <p key={n} className="resume-note">
              {n}
            </p>
          ))}
        </>
      )}
    </div>
  );
}


function Header({
  agent,
  capabilities,
  usage,
  driver,
  tasks,
  live,
  rail,
  onClose,
}: {
  agent: Agent;
  capabilities: Capabilities | null;
  usage: TranscriptUsage | null;
  driver: string | null;
  tasks: { run: Run; task: RunDetail["tasks"][number] }[];
  /** The turn running now, shown under the title as well as on its live line below. */
  live?: LiveTurn;
  /** The info rail is showing the model, context and tasks: the header leaves them out. */
  rail: boolean;
  onClose?: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [profile, setProfile] = useState(false);
  const [resume, setResume] = useState(false);
  const compact = useCompact(agent);
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
        {/* Always an entry: the meter opens usage and 压缩上下文; over half full the header keeps
            a conspicuous button of its own. */}
        {!rail && <ContextUsageButton agent={agent} usage={usage} variant="topbar" />}
        {agent.status === "idle" && usage && usage.size > 0 && usage.used / usage.size >= 0.5 && (
          <button className="ghost small" disabled={compact.busy} onClick={() => void compact.compact()} title="让 agent 把历史压缩成摘要，腾出上下文" aria-label="压缩上下文">
            <span className="row" style={{ gap: 5 }}>
              <Icon name="shrink" size={13} />
              {/* Compacting is under way: that word stays even on a narrow topbar. */}
              <span className={compact.busy ? undefined : "topbar-label"}>{compact.busy ? "压缩中…" : "压缩上下文"}</span>
            </span>
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
        <button
          className={`ghost small resume-toggle ${resume ? "on" : ""}`}
          onClick={() => setResume(!resume)}
          title="复制原生恢复命令：在终端里用同一个 session id 接着聊"
          aria-label="终端恢复"
          aria-expanded={resume}
        >
          <span className="row" style={{ gap: 5 }}>
            <Icon name="terminal" size={13} />
            <span className="topbar-label">终端恢复</span>
          </span>
        </button>
        {agent.plugin !== "external" && (
          <button className="ghost small graph-toggle" onClick={() => openTool("graph", { agent: agent.name })} title="在右侧工具列看这个会话检出的提交图：HEAD 和本地分支" aria-label="提交图">
            <span className="row" style={{ gap: 5 }}>
              <Icon name="branch" size={13} />
              <span className="topbar-label">提交图</span>
            </span>
          </button>
        )}
        <ToolsToggle />
        {onClose && (
          <button className="ghost icon" onClick={onClose} title="关闭这一栏" aria-label="关闭这一栏">
            <Icon name="close" size={15} />
          </button>
        )}
      </div>
      {live && <TurnLive {...live} />}
      {profile && (
        <div className="profile-pop">
          <ProfileEditor agent={agent} onDone={() => setProfile(false)} />
        </div>
      )}
      {resume && <ResumePanel agent={agent} onClose={() => setResume(false)} />}
      {(error || compact.error) && (
        <div style={{ padding: "8px 20px 0" }}>
          {error && <ErrorBox error={error} />}
          {compact.error && <ErrorBox error={compact.error} />}
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

/** A message sent from here, while the hub wakes the agent up and takes it (or why it did not). */
function Sending({ message, agent }: { message: Outgoing; agent: Agent }) {
  // The row's state changes in place (P0-1). "Waking" is about the process; whether the message
  // waits, and why, is the hub's own delivery state (P1-4) — an external agent is never woken.
  const delivery = useDelivery(agent.id);
  const state =
    message.steered ? "已插入本轮，等模型读取"
    : message.queued ? `排队中 · ${reasonText(message.queued)}`
    : delivery?.state === "held" ? `等 ${displayName(agent)} 自己来取`
    : delivery?.state === "starting" || agent.status === "stopped" || agent.status === "starting" || agent.status === "error" ? `正在唤起 ${displayName(agent)}…`
    : "发送中…";
  return (
    <div className="turn sending">
      {(message.text || !message.attachments?.length) && <div className="bubble">{message.text}</div>}
      {!!message.attachments?.length && <div className="turn-note small muted">附件：{message.attachments.map((a) => a.name).join("、")}</div>}
      {message.error ? (
        <div className="turn-note danger row" style={{ alignSelf: "flex-end", gap: 8 }} title={message.errorRaw ?? message.error}>
          <span>{message.outcomeUnknown ? message.error : `没发出去：${message.error}`}</span>
          <button className="ghost small" title="用同一条消息重试，hub 不会重复发送" onClick={() => retryMessage(message.key)}>
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
          <span className="muted small">{state}</span>
        </div>
      )}
    </div>
  );
}

/**
 * A person's message that has not started: it waits, and this row says why — no other panel
 * holds a copy of it (P0-1). "立即发送" promotes this very message (same id) instead of
 * withdrawing it and sending another (P1-5); a failure leaves it waiting.
 */
function WaitingTurn({ turn, agent }: { turn: TranscriptTurn; agent: Agent }) {
  const [error, setError] = useState<string>();
  const extras = turnExtras(turn);
  if (extras.withdrawn) return <div className="turn-note" style={{ alignSelf: "flex-end" }}>— 已撤回，没有发给 {displayName(agent)}</div>;
  const act = (action: () => Promise<unknown>) => {
    setError(undefined);
    action().catch((err) => setError(errorText(err)));
  };
  return (
    <div className="turn-note row" style={{ alignSelf: "flex-end", gap: 8 }}>
      <span>排队中{extras.queued_reason ? ` · ${reasonText(extras.queued_reason)}` : ""}</span>
      <button className="ghost small" title="打断当前这一轮，先发这条（还是同一条消息）" onClick={() => act(() => api.sendMessageNow(agent.id, turn.message_id))}>
        立即发送
      </button>
      <button className="ghost small" onClick={() => act(() => api.withdrawMessage(agent.id, turn.message_id))}>
        撤回
      </button>
      {error && <span className="danger small" title={error}>{error}</span>}
    </div>
  );
}

/**
 * The conversation's scroll box and whether it follows the bottom. A part that grows on its own
 * (the live line under a running turn, once its timer has something new to say) uses this, since
 * only the conversation itself is re-rendered when a transcript item arrives.
 */
const Follow = createContext<{ el: RefObject<HTMLDivElement | null>; stick: RefObject<boolean> } | null>(null);

function Conversation({
  agent,
  turns,
  switches,
  handoffs,
  works = [],
  agents,
  tree,
  top,
  rounds,
  children,
}: {
  agent: Agent;
  turns: TranscriptTurn[];
  switches: ModelSwitch[];
  /** The handoffs of the tasks in every run this agent leads. */
  handoffs: Handoff[];
  /** What became of the Work this conversation proposed. */
  works?: WorkDecision[];
  agents: Agent[];
  tree: RunDetail[];
  top?: ReactNode;
  /** A work group's rounds, marked where each began (docs/tasks/G1.md §3.5). */
  rounds?: RoundMark[];
  children?: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  // The run page shows its lead's conversation inside the run's scope: paths there are the run's.
  const outerScope = useFileScope();
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
          {top}
          {turns.length === 0 && !top && (
            <div className="empty">
              <h1>和 {displayName(agent)} 开始对话</h1>
              <p>
                {agent.role} · {agent.plugin}
                {agent.model ? `/${agent.model}` : ""} · <code>{agent.cwd}</code>
              </p>
            </div>
          )}
          <FileScopeProvider value={outerScope ?? { kind: "agent", agent: agent.id }}>
          <AgentImageContext.Provider value={agent.id}>
          <CwdContext.Provider value={agent.cwd}>
          <Follow.Provider value={{ el: ref, stick }}>
            {byTime(turns, [...switches, ...handoffs, ...works, ...(rounds ?? []).map((r) => ({ ...r, round_id: r.id }))]).map((x) =>
              "message_id" in x ? (
                <Turn key={x.message_id} turn={x} agent={agent} agents={agents} tree={tree} />
              ) : "round_id" in x ? (
                <RoundDivider key={x.round_id} round={x} />
              ) : "task_key" in x ? (
                <HandoffLine key={x.id} handoff={x} agents={agents} />
              ) : "run_id" in x ? (
                <WorkNote key={x.id} work={x} />
              ) : (
                <SwitchNote key={x.at} change={x} />
              ),
            )}
          </Follow.Provider>
          </CwdContext.Provider>
          </AgentImageContext.Provider>
          </FileScopeProvider>
          {children}
        </div>
      </div>
      <ScrollThumb scroller={ref} />
      <TurnMap scroller={ref} entries={entries} />
    </div>
  );
}

/** Every handoff of the tasks in `runs` (the ones a lead leads): orchestration lines in its conversation. */
function handoffsOf(runs: RunDetail[]): Handoff[] {
  return runs.flatMap((d) => d.tasks.flatMap((t) => t.handoffs));
}

/**
 * Turns, model switches and handoffs in the order they happened (a switch before the message
 * it applies to; a handoff after the turn that caused it).
 */
function byTime(turns: TranscriptTurn[], notes: Note[]): (TranscriptTurn | Note)[] {
  const sorted = notes.slice().sort((a, b) => a.at.localeCompare(b.at));
  const out: (TranscriptTurn | Note)[] = [];
  let i = 0;
  for (const turn of turns) {
    while (i < sorted.length && sorted[i].at <= turn.sent_at) out.push(sorted[i++]);
    out.push(turn);
  }
  return out.concat(sorted.slice(i));
}

type Note = ModelSwitch | Handoff | WorkDecision | (RoundMark & { round_id: string });

/**
 * Where a round of the work group began: 第 N 轮 · title · its state. The id is what the
 * rounds pane scrolls to, so a round picked there comes into view here.
 */
function RoundDivider({ round }: { round: RoundMark & { round_id?: string } }) {
  const state = runState(round.status);
  return (
    <div className="round-note" id={`round-${round.id}`} data-round={round.id} role="separator" aria-label={`第 ${round.n} 轮 · ${round.title}`}>
      <span className="round-rule" aria-hidden="true" />
      <span className="round-label">第 {round.n} 轮</span>
      <span className="round-title" title={round.title}>
        {round.title}
      </span>
      <span className={`round-state ${state.tone}`}>
        <span aria-hidden="true">{state.glyph}</span> {state.word}
      </span>
      <span className="round-at" title={time(round.at)}>
        {clock(round.at)}
      </span>
      <span className="round-rule" aria-hidden="true" />
    </div>
  );
}

/** A Work this conversation proposed: started (a link to its run) or not. */
function WorkNote({ work }: { work: WorkDecision }) {
  const title = firstLine(work.title);
  return (
    <div className="switch-note" title={`${time(work.at)} · 下一条消息会告诉 agent`}>
      <Icon name="branch" size={13} />
      {work.run_id ? (
        <a href={`#/runs/${work.run_id}`}>开始了 Work「{title}」</a>
      ) : (
        <span>没有开始 Work「{title}」</span>
      )}
      <span className="switch-at">{clock(work.at)}</span>
    </div>
  );
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

function Turn({ turn, agent, agents, tree }: { turn: TranscriptTurn; agent: Agent; agents: Agent[]; tree: RunDetail[] }) {
  const running = !!turn.started_at && !turn.completed_at;
  // Which step the conversation shows live, as Steps reports it: the live line leaves that step's
  // command and output out, so the same lines are not written twice (see ShownLive).
  const [shownLive, setShownLive] = useState<string | null>(null);
  const cards = useToolCards();
  // A tool an extension draws itself is a block of its own, never folded into a step summary.
  const blocks = workGroups(turn.items, (item) => !!cardFor(item, cards));
  return (
    <div className="turn" data-turn={turn.message_id}>
      {fromPerson(turn) ? (
        <>
          {(turn.input || !turn.attachments?.length) && <Bubble turn={turn} tree={tree} />}
          <TurnFiles agent={agent.id} files={turn.attachments ?? []} />
          {!turn.started_at && <WaitingTurn turn={turn} agent={agent} />}
        </>
      ) : (
        <PeerMessage turn={turn} agents={agents} tree={tree} />
      )}
      {turn.items.length > 0 && (
        <div className="author">
          <Avatar agent={agent} size={22} />
          <span className="who">{displayName(agent)}</span>
          <span className="what">{agentTitle(agent)}</span>
        </div>
      )}
      <TurnEnded.Provider value={!!turn.completed_at}>
      <ShownLive.Provider value={{ id: shownLive, set: setShownLive }}>
        {blocks.map((block, i) =>
          block.kind === "steps" ? (
            <Steps key={i} steps={block.steps} closed={block.closed} live={running && i === blocks.length - 1 ? setShownLive : undefined} />
          ) : (
            <Item key={i} item={block.item} />
          ),
        )}
        {running && <LiveLine since={turn.started_at!} items={turn.items} />}
      </ShownLive.Provider>
      </TurnEnded.Provider>
      {turn.completed_at && <TurnFoot turn={turn} />}
      {turn.completed_at && turn.diff && <TurnChanges turn={turn} agent={agent} />}
    </div>
  );
}

function Item({ item }: { item: TranscriptItem }) {
  const cards = useToolCards();
  const card = cardFor(item, cards);
  if (card && item.type === "tool_call") return <ToolCardView card={card} call={item} />;
  const action = actionOf(item);
  if (action && item.type === "tool_call") return <ActionCard action={action} call={item} />;
  switch (item.type) {
    case "output":
      return <AgentText text={item.text} />;
    case "error":
      // The gateway refused the whole context, not this request: retrying re-sends the same
      // history and burns the same tokens, so the line says that instead of "报错".
      if (isModerationError(item.message))
        return (
          <OneLine tone="warn" icon={<Icon name="info" size={13} />} title="上下文已被污染" summary={firstLine(item.message)}>
            <div className="moderation-why-text">
              <p>网关的内容审查拦下了这一轮：它扫的是整段上下文，不只是你最后那句话。触发的内容还在历史里，原样重试只会再烧一次 token。</p>
              <p>可以新开一个会话，或先把历史压缩成摘要再继续；仍然要发也可以，只是很可能再被拒一次。</p>
              <pre className="moderation-raw">{item.message}</pre>
            </div>
          </OneLine>
        );
      // One line; the whole message opens under it (a stack trace can be long).
      return (
        <OneLine tone="danger" icon={<Icon name="close" size={13} />} title="报错" summary={firstLine(item.message)}>
          {beyondFirstLine(item.message)}
        </OneLine>
      );
    case "warning":
      return (
        <OneLine tone="warn" icon={<Icon name="info" size={13} />} title="警告" summary={firstLine(item.message)}>
          {beyondFirstLine(item.message)}
        </OneLine>
      );
    default:
      return <SteerItem item={item} />;
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

/**
 * A call that changed (or tried to change) the team or the code, as one line: what was asked and
 * what the hub did — refused calls say why, in the hub's words. This is the record to trust over
 * the agent's summary, so the whole reason and the call's arguments open under the line.
 */
function ActionCard({ action, call }: { action: ActionTool; call: Extract<TranscriptItem, { type: "tool_call" }> }) {
  const args = fields(call.detail?.arguments);
  const result = fields(call.detail?.result);
  const raw = toolError(call);
  const refused = toolFailed(call) || /^(conflict|not found|invalid|unsupported)|not recruited/i.test(raw ?? "");
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
  // The hub's words without its machine prefix; the first sentence of them is the gist.
  const reason = raw?.replace(/^(conflict|not found|invalid|unsupported)\s*:\s*/i, "");
  // What a completed call answered, when it answered in words rather than a JSON payload (for a
  // recruit, the name and model above already are the result).
  const answered = Object.keys(result).length ? undefined : plainText(call.detail?.result);
  const summary = (running ? ["进行中", what] : refused ? [what, firstSentence(reason)] : [what, action !== "agent_spawn" && answered && firstSentence(answered)])
    .filter(Boolean)
    .join(" · ");
  const detail =
    (refused && reason) || call.detail?.arguments ? (
      <>
        {refused && reason && <div className="action-reason">{reason}</div>}
        {call.detail?.arguments && (
          <>
            <div className="muted tiny">参数</div>
            <pre className="out">{call.detail.arguments}</pre>
          </>
        )}
      </>
    ) : undefined;
  return (
    <OneLine
      className="action-line"
      tone={running ? "running" : refused ? "danger" : "ok"}
      icon={running ? <span className="dot accent pulse" /> : <Icon name={refused ? "close" : "check"} size={13} />}
      title={ACTION_LABEL[action]}
      summary={summary}
    >
      {detail}
    </OneLine>
  );
}

/** Steps shown while a run is open: the newest few, the rest behind "N 个更早的步骤". */
const OPEN_STEPS = 3;

/**
 * The tool call whose live output the conversation already shows. `Steps` reports it while it
 * shows its newest steps; the live line under the turn then leaves that call's command and output
 * out, so the same lines are never written twice (the step is where they belong).
 */
const ShownLive = createContext<{ id: string | null; set: (id: string | null) => void }>({ id: null, set: () => {} });

/**
 * A run of tool calls and reasoning. Closed runs read as one summary line (failures stay
 * listed under it); open ones show the newest steps. A click overrides either.
 *
 * `live` is set on the turn's last block while the turn runs: the newest step's live output is on
 * screen here, and Steps says so, so the live line does not repeat it.
 */
function Steps({ steps, closed, live }: { steps: Step[]; closed: boolean; live?: (id: string | null) => void }) {
  const [manual, setManual] = useState<boolean>();
  const expanded = manual ?? !closed;
  const [all, setAll] = useState(false);
  const hidden = expanded ? (all || steps.length <= OPEN_STEPS ? 0 : steps.length - OPEN_STEPS) : steps.length;
  // What `live` reports is whether the newest step is on screen: the block open (not folded to
  // its summary) and the newest step past the "+N 个更早的步骤" fold.
  const newest = steps[steps.length - 1];
  const shown = expanded && steps.length - 1 >= hidden && newest?.type === "tool_call" ? newest.tool_call_id : null;
  useEffect(() => {
    live?.(shown);
  }, [live, shown]);

  if (!expanded) {
    const bad = steps.filter(failed);
    // Named by what it produced, with what it left under the line (§2.7).
    return (
      <div className="steps">
        <button className="steps-summary" onClick={() => setManual(true)} aria-expanded={false} title="展开这一轮做了什么">
          <span className="disclosure" aria-hidden="true">
            ▸
          </span>
          <span className={bad.length ? "fail" : "pass"}>{bad.length ? "✕" : "✓"}</span>
          {outcomeOf(steps).title}
        </button>
        <StepProducts steps={steps} />
        {bad.map((s, i) => (
          <StepRow key={i} step={s} />
        ))}
      </div>
    );
  }
  return (
    <div className="steps">
      {closed && (
        <button className="steps-summary" onClick={() => setManual(false)} aria-expanded={true}>
          <span className="disclosure" aria-hidden="true">
            ▾
          </span>
          {outcomeOf(steps).title}
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
  // Failed, or refused with its reason in the detail: either way nothing was changed.
  const bad = toolFailed(step);
  const code = d?.exit_code;
  const cwd = useContext(CwdContext);
  // A failed edit carries the diff the model meant to make: showing +N −M would claim a change
  // that never happened (docs/TODO.md §九).
  const changes = toolChanges(step).flatMap((c) => parseDiff(c.diff, c.path));
  const hasImages = (!!d?.result && extractToolImages(d.result).images.length > 0) || (!!d?.arguments && extractToolImagePaths(d.arguments).length > 0);
  if (bad) {
    // One line, like a result card: ✕, the call, and why it failed; the detail opens below.
    const reason = firstLine(hasImages ? extractToolImages(toolError(step) ?? "").text : toolError(step));
    const more = !!(d || step.output);
    return (
      <div className="tool-step failed">
        <OneLine
          tone="danger"
          icon={<><Icon name="close" size={13} />{hasImages && <span className="tool-image-indicator" aria-label="含图片"><ImageIcon size={14} /></span>}</>}
          title={stepLine(step, cwd)}
          summary={[code != null && code !== 0 ? `退出 ${code}` : "", reason || "失败"].filter(Boolean).join(" · ")}
        >
          {more ? <ToolDetailView step={step} changes={changes} /> : undefined}
        </OneLine>
      </div>
    );
  }
  const tone = running ? "accent pulse" : "ok";
  const live = running && step.output ? tailLines(step.output, LIVE_LINES) : undefined;
  const more = !!(d || step.output);
  return (
    <div className="tool-step">
      <button className="tool" onClick={() => more && setOpen(!open)} aria-expanded={more ? open : undefined} disabled={!more} title={step.title}>
        <span className={`dot ${tone}`} />
        <span className="title">{stepLine(step, cwd)}</span>
        {hasImages && <span className="tool-image-indicator" aria-label="含图片"><ImageIcon size={14} /></span>}
        {code != null && code !== 0 ? <span className="chip danger tiny">退出 {code}</span> : null}
        {d?.duration_ms != null && <span className="muted tiny">{took(d.duration_ms)}</span>}
        {toolShowsStat(step) && changes.length > 0 && <ChangeStat files={changes} />}
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
  const agent = useContext(AgentImageContext);
  const paths = useMemo(() => extractToolImagePaths(d?.arguments ?? ""), [d?.arguments]);
  const [loaded, setLoaded] = useState<Record<string, ImageLoad>>({});
  useEffect(() => {
    if (!agent) return;
    let active = true;
    setLoaded({});
    for (const path of paths) void loadAgentImage(agent, path).then((value) => {
      if (active) setLoaded((old) => ({ ...old, [path]: value }));
    });
    return () => { active = false; };
  }, [agent, paths]);
  const output = d?.output ?? step.output;
  const shown = output != null ? tailLines(output, OPEN_LINES) : undefined;
  // A failed call says why, first: detail.error when there is one, else the tool's own words.
  const rawError = toolFailed(step) ? toolError(step) : undefined;
  const error = rawError ? extractToolImages(rawError).text : undefined;
  // The result may be the very words shown above as the reason: one copy is enough.
  const extracted = d?.result != null ? extractToolImages(d.result) : undefined;
  const resultImages = (extracted?.images ?? []).map((src, i) => ({ src, alt: `${step.title} 图片 ${i + 1}`, caption: paths.filter((path) => loaded[path]?.src === src).join("、") || undefined }));
  const pathImages = paths.flatMap((path) => loaded[path]?.src && !extracted?.images.includes(loaded[path].src!) ? [{ src: loaded[path].src!, alt: `${step.title} 图片`, caption: path }] : []);
  const result = extracted?.text ? tailLines(extracted.text, OPEN_LINES).text : undefined;
  return (
    <div className="tool-detail">
      {d?.command && <pre className="cmd">$ {d.command}</pre>}
      {d?.cwd && <div className="muted tiny mono">{d.cwd}</div>}
      {error && <div className="item-error">{error}</div>}
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
          <ToolImages images={pathImages} />
          {paths.map((path) => loaded[path]?.error ? <div className="tool-image-error" key={path}>{path}：{loaded[path].error}</div> : !loaded[path] ? <div className="tool-image-loading" key={path}>{path}：加载中…</div> : null)}
        </>
      )}
      {d?.result != null && (
        <>
          <div className="tool-result-heading"><span className="muted tiny">结果</span><button className="ghost tiny" onClick={() => void navigator.clipboard.writeText(d.result!)}>复制原始结果</button></div>
          <ToolImages images={resultImages} />
          {result && result !== error && <pre className="out">{result}</pre>}
        </>
      )}
    </div>
  );
}

/** "70ms" under a second, else like `duration`. */
const took = (ms: number) => (ms < 1000 ? `${ms}ms` : duration(ms));

/** The docks' switches, in each conversation's header. */
function ToolsToggle() {
  const { state, peek } = useLayout();
  const { right, bottom } = state.docks;
  const { docked } = useSidebar();
  const float = useWidths(docked).float;
  const shows = useTabShows();
  const bottomShows = !float && bottom.open && bottom.tabs.some(shows);
  const below = shownTab({ ...bottom, tabs: bottom.tabs.filter(shows) });
  return (
    <>
      {/* Only what is folded. Open, a dock's own header has the button that folds it away: one place, like Codex. */}
      {terminalsAvailable() && !bottomShows && (
        <button className="ghost icon" onClick={() => toggleTerminal(below && isTermTab(below) ? below : null, bottomShows)} title={`终端（${shortcut(shortcutPlatform, "Ctrl+`", true)}）`} aria-label="终端">
          <Icon name="terminal" size={16} />
        </button>
      )}
      {!(right.open || (float && peek)) && (
        <button className="ghost icon" onClick={toggleTools} title="工具列：改动、文件、任务、提交图" aria-label="工具列">
          <Icon name="panel" size={16} />
        </button>
      )}
    </>
  );
}

/**
 * The bottom of a running turn: `••• 42s · 运行命令`, and under it what it is producing now (the
 * newest step, a running command's last lines) so a person can see it move. With nothing new for
 * `QUIET_AFTER` seconds the line says how long, instead of pretending to work.
 */
function LiveLine({ since, items }: { since: string; items: TranscriptItem[] }) {
  const cwd = useContext(CwdContext);
  const follow = useContext(Follow);
  const [now, setNow] = useState(Date.now());
  // When the turn was last seen to produce something. Only time on this page counts: the hub
  // sends items, not when each one arrived.
  const key = liveKey(items);
  const [seen, setSeen] = useState(() => ({ key, at: Date.now() }));
  useEffect(() => setSeen((s) => (s.key === key ? s : { key, at: Date.now() })), [key]);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  // This line grows on its own (a new tail line, the quiet notice) without the conversation
  // re-rendering, so it keeps the person at the bottom itself.
  useLayoutEffect(() => {
    const el = follow?.el.current;
    if (el && follow?.stick.current) el.scrollTop = el.scrollHeight;
  });
  const shown = useContext(ShownLive);
  const last = items[items.length - 1];
  // A running step whose command and output are already on screen (the open steps at the end of
  // the turn): the status row says enough; repeating it here would write the same output twice.
  const onScreen = last?.type === "tool_call" && shown.id === last.tool_call_id;
  const current = onScreen ? undefined : liveNow(items, cwd);
  const quiet = quietSeconds(seen.at, now);
  return (
    <div className="working live-turn">
      <div className="working-row">
        <span className={`busy-dots ${quiet >= QUIET_AFTER ? "still" : ""}`} aria-hidden="true">
          <i />
          <i />
          <i />
        </span>
        <span className="muted small">{duration(now - new Date(since).getTime())}</span>
        <span>· {liveActivity(items)}</span>
      </div>
      {current && (
        <div className="working-now" title={current.title}>
          {current.title}
        </div>
      )}
      {current?.output && (
        <pre className="working-out" aria-label="最新输出" title={current.output.hidden ? `前面还有 ${current.output.hidden} 行` : undefined}>
          {current.output.lines.join("\n")}
        </pre>
      )}
      {quiet >= QUIET_AFTER && <div className="muted small">已 {quiet} 秒没有新输出</div>}
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

