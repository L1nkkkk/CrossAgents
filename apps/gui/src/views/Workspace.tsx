import { useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { Handoff } from "@bindings/Handoff";
import type { PlannedTask } from "@bindings/PlannedTask";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { Task } from "@bindings/Task";
import { ACTIVE_RUN } from "../App";
import { Avatar, CopyCommand, Dot, ErrorBox, Icon, Vendor, agentTitle, agentTone, ago, displayName, isAgent, sessionHref, time, useAgents } from "../components";
import { KIND_LABEL, OUTCOME_LABEL, OUTCOME_TONE, freshLabel } from "../handoff";
import { executorOf, shortTitle } from "../lib/projectView";
import { checksPassed, runState, taskState, type State, type StateTone } from "../lib/taskState";
import { StateWord } from "../parts";
import { selectWorkspace, useProjectRuns, useWorkspaces, type Workspace } from "../workspace";
import "./workspace.css";

/**
 * A project's overview (docs/AGENT-WORKSPACE.md §3, §5): 计划 / 成员 / 动态 / 结果. The plan is
 * every run the project has — old ones without a source conversation, ones whose source is
 * archived, and ones that have not submitted a plan — each showing its goal, its coordinator and
 * each task's people, dependencies and state. The members are the project's sessions and what each
 * one did this time. The activity is built from the records the hub already sends (handoffs, run
 * and task state), not from polling every conversation. The results keep delivery, verification
 * and landing apart: a run that ended without landing is not the same as one that landed.
 */
export function WorkspaceView({ wsKey }: { wsKey: string }) {
  const { list, loaded } = useWorkspaces();
  const ws = list.find((w) => w.key === wsKey);

  if (!ws) {
    return <div className="page-inner">{loaded ? <div className="empty">没有这个工作区：{wsKey}</div> : <p className="muted">加载中…</p>}</div>;
  }
  return <Overview ws={ws} />;
}

const TABS = [
  { key: "plan", label: "计划" },
  { key: "members", label: "成员" },
  { key: "activity", label: "动态" },
  { key: "results", label: "结果" },
] as const;
type TabKey = (typeof TABS)[number]["key"];
const TAB_STORAGE = "crossagents.workspace.tab";

function storedTab(): TabKey {
  try {
    const saved = localStorage.getItem(TAB_STORAGE);
    if (TABS.some((tab) => tab.key === saved)) return saved as TabKey;
  } catch {
    // Storage may be unavailable; the first view is the default then.
  }
  return "plan";
}

function Overview({ ws }: { ws: Workspace }) {
  const project = useProjectRuns(ws.key);
  const allAgents = useAgents();
  const [tab, setTab] = useState<TabKey>(storedTab);
  const tabRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const runs = project.data ?? [];
  // Members come from the project's derived list (workspace.ts / projectView.ts); the full agent
  // list only resolves names, the coordinator and an archived source conversation.
  const members = ws.agents;
  const agents = allAgents.data ?? members;

  const needsMerge = runs.filter((d) => d.run.status === "completed" && !d.run.landed && !d.run.in_place).length;

  function choose(next: TabKey) {
    setTab(next);
    try {
      localStorage.setItem(TAB_STORAGE, next);
    } catch {
      // The choice then lasts for this page only.
    }
  }

  function onTabKey(e: KeyboardEvent<HTMLButtonElement>, index: number) {
    const step = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : e.key === "Home" ? -index : e.key === "End" ? TABS.length - 1 - index : 0;
    if (!step) return;
    e.preventDefault();
    const next = (index + step + TABS.length) % TABS.length;
    choose(TABS[next].key);
    tabRefs.current[next]?.focus();
  }

  return (
    <>
      <div className="topbar">
        <Icon name="folder" size={16} />
        <span className="title">{ws.name}</span>
        <span className="ws-path mono" title={ws.path}>
          {ws.path}
        </span>
        <div className="spacer" />
        <a className="button primary" href="#/" onClick={() => selectWorkspace(ws.key)} title="在这个工作区里发起 run">
          <Icon name="plus" size={14} />
          新建 run
        </a>
      </div>
      <div className="page">
        <div className="page-inner ws-page">
          <div className="ws-tabs" role="tablist" aria-label="项目视角">
            {TABS.map((item, i) => (
              <button
                key={item.key}
                ref={(el) => {
                  tabRefs.current[i] = el;
                }}
                id={`ws-tab-${item.key}`}
                className="ws-tab"
                role="tab"
                aria-selected={tab === item.key}
                aria-controls="ws-panel"
                tabIndex={tab === item.key ? 0 : -1}
                onClick={() => choose(item.key)}
                onKeyDown={(e) => onTabKey(e, i)}
              >
                {item.label}
                {item.key === "results" && needsMerge > 0 && (
                  <span className="ws-tab-badge" title={`${needsMerge} 个 run 完成但还没落地`} aria-label={`${needsMerge} 个 run 完成但还没落地`}>
                    {needsMerge}
                  </span>
                )}
              </button>
            ))}
          </div>

          <div id="ws-panel" role="tabpanel" aria-labelledby={`ws-tab-${tab}`} className="ws-panel">
            <ErrorBox error={project.error} />
            {!project.data && project.loading ? (
              <p className="muted">加载这个项目的 run…</p>
            ) : tab === "plan" ? (
              <PlanView runs={runs} agents={agents} />
            ) : tab === "members" ? (
              <MembersView members={members} runs={runs} agents={agents} />
            ) : tab === "activity" ? (
              <ActivityView runs={runs} agents={agents} />
            ) : (
              <ResultsView runs={runs} agents={agents} />
            )}
          </div>
        </div>
      </div>
    </>
  );
}

// ---- 计划 ------------------------------------------------------------------------------------

function PlanView({ runs, agents }: { runs: RunDetail[]; agents: Agent[] }) {
  const noPlan = runs.filter((d) => !d.tasks.length && !d.run.plan.length).length;
  const ordered = runs
    .slice()
    .sort(
      (a, b) =>
        Number(ACTIVE_RUN.includes(b.run.status)) - Number(ACTIVE_RUN.includes(a.run.status)) ||
        b.run.updated_at.localeCompare(a.run.updated_at),
    );

  return ordered.length ? (
    <>
      <p className="ws-count-line">
        {runs.length} 个 run{noPlan > 0 && ` · ${noPlan} 个还没提交计划`}
      </p>
      <div className="ws-runs">
        {ordered.map((detail) => (
          <PlanRun key={detail.run.id} detail={detail} agents={agents} />
        ))}
      </div>
    </>
  ) : (
    <div className="empty">这个项目还没有 run。</div>
  );
}

/** One word for where a run ended up: failed, unlanded and landed each say so, never just 「完成」. */
function runOutcome(run: Run): State {
  const state = runState(run.status);
  const kind = resultKind(run);
  if (kind === "pending") return state;
  const meta = RESULT_META[kind];
  return { ...state, glyph: meta.glyph as State["glyph"], tone: meta.tone, word: kind === "landed" && run.landed?.mode === "pr" ? "已开 PR" : meta.word };
}

/** One row per run. A run still in progress starts open; a finished one stays a line until asked. */
function PlanRun({ detail, agents }: { detail: RunDetail; agents: Agent[] }) {
  const { run, tasks } = detail;
  const origin = run.origin_chat_id ? agents.find((a) => a.id === run.origin_chat_id) : undefined;
  const { executor, template } = executorOf(run, agents, "worker");
  const lead = executor ?? template;
  const merged = tasks.filter((task) => task.status === "merged").length;
  const total = tasks.length || run.plan.length;

  return (
    <details className="ws-run" open={ACTIVE_RUN.includes(run.status)}>
      <summary className="ws-run-head">
        <Icon name="chevronRight" size={13} />
        <a className="ws-run-name" href={`#/runs/${run.id}`} title={run.title} onClick={(e) => e.stopPropagation()}>
          {shortTitle(run.title)}
        </a>
        {lead && <Avatar agent={lead} size={16} />}
        <span className="ws-run-count" title={tasks.length ? `${merged}/${total} 个任务已合并` : `计划 ${total} 个任务`}>
          {total > 0 && (tasks.length ? `${merged}/${total}` : total)}
        </span>
        <span className="ws-run-state">
          <StateWord state={runOutcome(run)} />
        </span>
        <span className="ws-run-when" title={`创建于 ${time(run.created_at)}，最后更新 ${time(run.updated_at)}`}>
          {ago(run.updated_at)}
        </span>
      </summary>

      <div className="ws-run-body">
        {run.request.trim() !== run.title.trim() && (
          <p className="ws-goal" title={run.request}>
            {run.request}
          </p>
        )}
        {run.note && <p className={`ws-note ${run.status === "failed" ? "danger" : ""}`}>{run.note}</p>}

        <div className="ws-run-meta">
          <span className="ws-field">
            <span className="ws-label">协调</span>
            <Actor work={run} agents={agents} copied={!!run.lead_note} />
          </span>
          <span className="ws-field">
            <span className="ws-label">分支</span>
            {run.in_place ? <span className="muted">直接改文件</span> : <BranchName run={run} />}
          </span>
          {origin?.archived_at && (
            <span className="ws-field">
              <span className="ws-label">来源</span>
              <a href={sessionHref(origin)} title="来源会话已归档；打开它仍能看记录并恢复">
                {displayName(origin)}（已归档）
              </a>
            </span>
          )}
          {!run.origin_chat_id && <span className="ws-field muted">没有来源会话</span>}
        </div>

        {tasks.length > 0 ? (
          <ul className="ws-tasks">
            {tasks.map((task) => (
              <TaskLine key={task.id} run={run} task={task} siblings={tasks} agents={agents} />
            ))}
          </ul>
        ) : run.plan.length > 0 ? (
          <ul className="ws-tasks">
            {run.plan.map((task) => (
              <PlannedLine key={task.key} run={run} task={task} agents={agents} />
            ))}
          </ul>
        ) : (
          <p className="ws-no-plan">还没提交计划。提交后这里会列出每个任务的负责人、评审者、依赖和状态。</p>
        )}
      </div>
    </details>
  );
}

function BranchName({ run }: { run: Run }) {
  return (
    <span className="ws-branch" title={`${run.branch} → ${run.base_branch}`}>
      <code>{run.branch}</code>
      <span className="muted" aria-hidden="true">
        →
      </span>
      <code>{run.base_branch}</code>
    </span>
  );
}

function TaskLine({ run, task, siblings, agents }: { run: Run; task: Task; siblings: Task[]; agents: Agent[] }) {
  const reviewer = !!(task.reviewer ?? task.reviewer_template);
  const progress = task.progress && !task.status.match(/merged|failed|cancelled/) ? task.progress : "";
  return (
    <li className="ws-task">
      <span className="ws-task-key" title={`#${task.key}`}>
        #{task.key}
      </span>
      <a className="ws-task-title" href={`#/runs/${run.id}/tasks/${task.id}`} title="查看规格、交付和执行记录">
        {task.title}
      </a>
      <span className="ws-task-who">
        <Actor work={task} agents={agents} />
      </span>
      <span className="ws-task-state">
        <StateWord state={taskState(task, siblings)} />
      </span>
      {(reviewer || task.depends_on.length > 0 || progress) && (
        <div className="ws-task-meta">
          {reviewer && (
            <span className="ws-field">
              <span className="ws-label">评审</span>
              <Actor work={task} agents={agents} side="reviewer" />
            </span>
          )}
          {task.depends_on.length > 0 && (
            <span className="ws-field">
              <span className="ws-label">依赖</span>
              {task.depends_on.map((key) => `#${key}`).join("、")}
            </span>
          )}
          {progress && (
            <span className="ws-progress" title={progress}>
              {progress}
            </span>
          )}
        </div>
      )}
    </li>
  );
}

function PlannedLine({ run, task, agents }: { run: Run; task: PlannedTask; agents: Agent[] }) {
  const open = ["planning", "awaiting_approval", "paused"].includes(run.status);
  return (
    <li className="ws-task">
      <span className="ws-task-key" title={`#${task.key}`}>
        #{task.key}
      </span>
      <a className="ws-task-title" href={`#/runs/${run.id}`} title="打开 run 看计划和执行记录">
        {task.title}
      </a>
      <span className="ws-task-who">
        <PlannedActor name={task.assignee} fresh={task.fresh} agents={agents} />
      </span>
      <span className="ws-task-state state muted">{open ? "还没开始" : "没有任务记录"}</span>
      {(task.reviewer || task.depends_on.length > 0) && (
        <div className="ws-task-meta">
          {task.reviewer && (
            <span className="ws-field">
              <span className="ws-label">评审</span>
              <PlannedActor name={task.reviewer} fresh={task.reviewer_fresh} agents={agents} />
            </span>
          )}
          {task.depends_on.length > 0 && (
            <span className="ws-field">
              <span className="ws-label">依赖</span>
              {task.depends_on.map((key) => `#${key}`).join("、")}
            </span>
          )}
        </div>
      )}
    </li>
  );
}

// ---- Who does the work ----------------------------------------------------------------------

/** A task's or run's actual executor, and where its session came from: itself, or a copy of a teammate's configuration. */
function Actor({
  work,
  agents,
  side = "worker",
  copied = false,
}: {
  work: Task | Run;
  agents: Agent[];
  side?: "worker" | "reviewer";
  copied?: boolean;
}) {
  const { executor, template, fresh } = executorOf(work, agents, side);
  const who = executor ?? template;
  if (!who) return <span className="muted">未记录</span>;
  const isRun = "planner" in work;
  const note = isRun
    ? copied
      ? "复制的主导 · 独立执行会话"
      : "本次协调者"
    : template && executor && template.id !== executor.id
      ? `使用 ${displayName(template)} 的配置新建`
      : template && !executor
        ? fresh
          ? `由 ${displayName(template)} 的配置新建 · 会话已不在`
          : "本人执行 · 会话记录已不在"
        : side === "reviewer"
          ? "本人"
          : fresh
            ? "独立执行会话"
            : "本人执行";
  return (
    <a
      className={`ws-actor ${who.archived_at ? "archived" : ""}`}
      href={sessionHref(who)}
      title={`${displayName(who)} · ${agentTitle(who)} · ${note}${who.archived_at ? " · 已归档" : ""} · 打开会话`}
    >
      <Avatar agent={who} size={16} />
      <span>{displayName(who)}</span>
    </a>
  );
}

/** A planned task's teammate, by the name the plan used: a new session of them, or themselves. */
function PlannedActor({ name, fresh, agents }: { name: string | null; fresh: boolean; agents: Agent[] }) {
  if (!name) return <span className="muted">默认 worker</span>;
  const who = agents.find((a) => isAgent(a, name));
  if (!who) return <span className="muted">{name}（没找到会话）</span>;
  return (
    <a className="ws-actor" href={sessionHref(who)} title={`${displayName(who)} · ${agentTitle(who)} · ${fresh ? "计划新会话" : "计划本人"} · 打开会话`}>
      <Avatar agent={who} size={16} />
      <span>{displayName(who)}</span>
    </a>
  );
}

// ---- 成员 ------------------------------------------------------------------------------------

const AGENT_WORD: Record<string, string> = {
  running: "执行中",
  starting: "启动中",
  idle: "空闲",
  stopped: "已停止",
  error: "出错",
};

type Duty =
  | { kind: "lead"; run: Run }
  | { kind: "work"; run: Run; task: Task }
  | { kind: "review"; run: Run; task: Task }
  | { kind: "history"; run: Run; task: Task; hand: Handoff }
  | { kind: "borrowed"; run: Run; task: Task; who: string | null; asReviewer: boolean }
  | { kind: "plan"; run: Run; plan: PlannedTask; asReviewer: boolean };

/** What this session does in each run of the project — read from run, plan and task, never from its role. */
function dutiesOf(member: Agent, runs: RunDetail[], agents: Agent[]): Duty[] {
  const duties: Duty[] = [];
  for (const { run, tasks } of runs) {
    if (run.planner === member.id) duties.push({ kind: "lead", run });
    for (const task of tasks) {
      const current = task.assignee === member.id;
      let worked = current;
      if (current) duties.push({ kind: "work", run, task });
      else if (!task.assignee && !task.worker_fresh && task.worker_template === member.id) {
        // An old record with no assignee, but it says the teammate did the task itself.
        duties.push({ kind: "work", run, task });
        worked = true;
      } else if (task.worker_fresh && task.worker_template === member.id) {
        duties.push({ kind: "borrowed", run, task, who: task.assignee, asReviewer: false });
        worked = true;
      }
      if (task.reviewer === member.id) duties.push({ kind: "review", run, task });
      else if (task.reviewer_fresh && task.reviewer_template === member.id) {
        duties.push({ kind: "borrowed", run, task, who: task.reviewer, asReviewer: true });
      }
      // The task's current assignee is not recorded once a merged execution session is gone:
      // the hand it took is, and that is the session's own record (never its template's).
      if (!worked) {
        const hand = task.handoffs.findLast((h) => h.to === member.id);
        if (hand) duties.push({ kind: "history", run, task, hand });
      }
    }
    if (!tasks.length) {
      for (const plan of run.plan) {
        if (plan.assignee && agents.find((a) => isAgent(a, plan.assignee))?.id === member.id) duties.push({ kind: "plan", run, plan, asReviewer: false });
        if (plan.reviewer && agents.find((a) => isAgent(a, plan.reviewer))?.id === member.id) duties.push({ kind: "plan", run, plan, asReviewer: true });
      }
    }
  }
  return duties;
}

function MembersView({ members, runs, agents }: { members: Agent[]; runs: RunDetail[]; agents: Agent[] }) {
  const active = members.filter((a) => !a.archived_at);
  const archived = members.filter((a) => a.archived_at);
  return (
    <>
      {active.length ? (
        <div className="ws-members">
          {active.map((member) => (
            <MemberCard key={member.id} member={member} runs={runs} agents={agents} />
          ))}
        </div>
      ) : (
        <div className="empty">这个项目还没有成员。</div>
      )}
      {archived.length > 0 && (
        <details className="ws-archived">
          <summary>已归档成员 · {archived.length}（不占用日常列表，记录仍可打开）</summary>
          <div className="ws-members">
            {archived.map((member) => (
              <MemberCard key={member.id} member={member} runs={runs} agents={agents} />
            ))}
          </div>
        </details>
      )}
    </>
  );
}

function MemberCard({ member, runs, agents }: { member: Agent; runs: RunDetail[]; agents: Agent[] }) {
  const duties = dutiesOf(member, runs, agents);
  const tone = agentTone(member.status);
  return (
    <article className="ws-member">
      <div className="ws-member-head">
        <Avatar agent={member} size={28} status={!member.archived_at} />
        <div className="ws-member-id">
          <a className="ws-member-name" href={sessionHref(member)} title={`打开 ${displayName(member)} 的会话`}>
            {displayName(member)}
          </a>
          <span className="ws-member-sub">
            {agentTitle(member)} · <Vendor plugin={member.plugin} model={member.model} />
          </span>
        </div>
        <span className="spacer" />
        <span className="ws-agent-state" title={member.archived_at ? `已归档 ${time(member.archived_at)}` : undefined}>
          <Dot tone={tone} />
          {AGENT_WORD[member.status] ?? member.status}
        </span>
      </div>
      {member.archived_at && (
        <p className="ws-member-note">
          已归档 · <a href={sessionHref(member)}>打开会话可恢复</a>
        </p>
      )}
      {duties.length > 0 ? (
        <ul className="ws-duties">
          {duties.map((duty, i) => (
            <li className="ws-duty" key={i}>
              <DutyLine duty={duty} member={member} agents={agents} />
            </li>
          ))}
        </ul>
      ) : (
        <p className="ws-member-empty">这次没有分工。</p>
      )}
    </article>
  );
}

/** How an execution session came to be, when it is not the teammate itself. */
function executorNote(task: Task, agents: Agent[], side: "worker" | "reviewer"): string | undefined {
  const { executor, template, fresh } = executorOf(task, agents, side);
  if (template && executor && template.id !== executor.id) return `使用 ${displayName(template)} 的配置新建`;
  if (!fresh) return side === "reviewer" ? "本人评审" : "本人执行";
  return undefined;
}

function DutyLine({ duty, member, agents }: { duty: Duty; member: Agent; agents: Agent[] }) {
  switch (duty.kind) {
    case "lead":
      return (
        <>
          <span className="ws-duty-kind">协调</span>
          <a href={`#/runs/${duty.run.id}`} title="打开 run">{duty.run.title}</a>
          <span className="muted small">本次协调者</span>
        </>
      );
    case "work": {
      const note = executorNote(duty.task, agents, "worker");
      return (
        <>
          <span className="ws-duty-kind">实现</span>
          <a href={`#/runs/${duty.run.id}/tasks/${duty.task.id}`} title="查看规格、交付和执行记录">
            #{duty.task.key} {duty.task.title}
          </a>
          <StateWord state={taskState(duty.task)} />
          {note && <span className="muted">· {note}</span>}
        </>
      );
    }
    case "review": {
      const note = executorNote(duty.task, agents, "reviewer");
      return (
        <>
          <span className="ws-duty-kind">评审</span>
          <a href={`#/runs/${duty.run.id}/tasks/${duty.task.id}`} title="查看规格、交付和执行记录">
            #{duty.task.key} {duty.task.title}
          </a>
          <StateWord state={taskState(duty.task)} />
          {note && <span className="muted">· {note}</span>}
        </>
      );
    }
    case "history":
      return (
        <>
          <span className="ws-duty-kind">执行过</span>
          <a href={`#/runs/${duty.run.id}/tasks/${duty.task.id}`} title="查看规格、交付和执行记录">
            #{duty.task.key} {duty.task.title}
          </a>
          <span className="chip">{freshLabel(duty.hand)}</span>
          {duty.hand.outcome && <span className={`chip ${OUTCOME_TONE[duty.hand.outcome]}`}>{OUTCOME_LABEL[duty.hand.outcome]}</span>}
          {duty.hand.fresh && duty.hand.template && <span className="muted">用 {duty.hand.template} 的配置新建</span>}
        </>
      );
    case "borrowed": {
      const who = duty.who ? agents.find((a) => a.id === duty.who) : undefined;
      return (
        <>
          <span className="ws-duty-kind">配置</span>
          <span>执行会话</span>
          {who ? (
            <a href={sessionHref(who)} title={`${displayName(who)} 的会话`}>
              {displayName(who)}
            </a>
          ) : (
            <span className="muted">（已不在）</span>
          )}
          <span className="muted">{duty.asReviewer ? "评审" : "执行"}</span>
          <a href={`#/runs/${duty.run.id}/tasks/${duty.task.id}`}>#{duty.task.key}</a>
          <span className="muted">用的是 {displayName(member)} 的配置</span>
        </>
      );
    }
    case "plan": {
      const open = ["planning", "awaiting_approval", "paused"].includes(duty.run.status);
      return (
        <>
          <span className="ws-duty-kind">{duty.asReviewer ? "计划评审" : "计划实现"}</span>
          <a href={`#/runs/${duty.run.id}`}>#{duty.plan.key} {duty.plan.title}</a>
          <span className="muted">{open ? "还没开始" : "没有任务记录"}</span>
        </>
      );
    }
  }
}

// ---- 动态 ------------------------------------------------------------------------------------

type TimelineEvent = {
  key: string;
  at: string;
  tone: StateTone;
  glyph: string;
  main: ReactNode;
  note?: ReactNode;
  run: Run;
  task?: Task;
};

/** The project's collaboration events, from the records already loaded: run lifecycle, each hand, each task's last state. */
function timelineOf(runs: RunDetail[], agents: Agent[]): TimelineEvent[] {
  const events: TimelineEvent[] = [];
  for (const { run, tasks } of runs) {
    events.push({ key: `${run.id}-created`, at: run.created_at, tone: "muted", glyph: "○", run, main: <>创建「{run.title}」</> });
    const runWord = runState(run.status);
    const landing = run.status === "completed" ? (run.landed ? (run.landed.mode === "pr" ? " · 已开 PR" : " · 已落地") : run.in_place ? " · 直接写在文件夹" : " · 还没落地") : "";
    events.push({ key: `${run.id}-state`, at: run.updated_at, tone: runWord.tone, glyph: runWord.glyph, run, main: <>run {runWord.word}{landing}</> });
    if (run.landed) {
      events.push({
        key: `${run.id}-landed`,
        at: run.landed.at,
        tone: "ok",
        glyph: "✓",
        run,
        main:
          run.landed.mode === "pr" ? (
            <>开了 PR</>
          ) : (
            <>落地到 <code>{run.base_branch}</code></>
          ),
        note: (
          <>
            <code>{run.landed.reference}</code> · {run.landed.by === "person" ? "你" : run.landed.by}
          </>
        ),
      });
    }
    for (const task of tasks) {
      const state = taskState(task, tasks);
      events.push({
        key: `${task.id}-state`,
        at: task.updated_at,
        tone: state.tone,
        glyph: state.glyph,
        run,
        task,
        main: <>#{task.key} {task.title} · {state.word}</>,
        note: task.progress && !task.status.match(/merged|failed|cancelled/) ? <span className="ws-event-note">{task.progress}</span> : undefined,
      });
      for (const hand of task.handoffs) {
        const from = hand.from ? agents.find((a) => a.id === hand.from) : undefined;
        const to = agents.find((a) => a.id === hand.to);
        const tone: StateTone = hand.kind === "reroute" ? "warn" : hand.kind === "escalate" ? "danger" : "accent";
        events.push({
          key: hand.id,
          at: hand.at,
          tone,
          glyph: "→",
          run,
          task,
          main: (
            <>
              {from ? displayName(from) : hand.from_name ?? "?"} 把 #{hand.task_key} 交给{" "}
              {to ? <a href={sessionHref(to)}>{displayName(to)}</a> : hand.to_name}（{KIND_LABEL[hand.kind]}）
            </>
          ),
          note: (
            <>
              <span className="chip">{freshLabel(hand)}</span>
              {hand.outcome && <span className={`chip ${OUTCOME_TONE[hand.outcome]}`}>{OUTCOME_LABEL[hand.outcome]}</span>}
              {hand.reason && <span className="ws-event-note">{hand.reason}</span>}
            </>
          ),
        });
      }
    }
  }
  return events.sort((a, b) => b.at.localeCompare(a.at)).slice(0, 60);
}

function ActivityView({ runs, agents }: { runs: RunDetail[]; agents: Agent[] }) {
  const events = timelineOf(runs, agents);
  return (
    <>
      {events.length ? (
        <ol className="ws-events">
          {events.map((event) => (
            <li key={event.key} className="ws-event">
              <span className="ws-event-time" title={time(event.at)}>
                {ago(event.at)}
              </span>
              <span className={`ws-event-mark ${event.tone}`} aria-hidden="true">
                {event.glyph}
              </span>
              <div className="ws-event-body">
                <div className="ws-event-main">{event.main}</div>
                <div className="ws-event-sub">
                  <a href={`#/runs/${event.run.id}`} title="打开 run">{event.run.title}</a>
                  {event.task && (
                    <a href={`#/runs/${event.run.id}/tasks/${event.task.id}`} title="查看规格、交付和执行记录">
                      任务 #{event.task.key}
                    </a>
                  )}
                  {event.note}
                </div>
              </div>
            </li>
          ))}
        </ol>
      ) : (
        <div className="empty">这个项目还没有协作记录。</div>
      )}
    </>
  );
}

// ---- 结果 ------------------------------------------------------------------------------------

type ResultKind = "unlanded" | "landed" | "in_place" | "failed" | "cancelled" | "pending";

const RESULT_META: Record<Exclude<ResultKind, "pending">, { label: string; word: string; tone: StateTone; glyph: string; hint: string }> = {
  unlanded: {
    label: "完成，但还没落地",
    word: "完成 · 未落地",
    tone: "warn",
    glyph: "○",
    hint: "这些 run 已完成，结果在各自的 run 分支上，还没进基准分支；确认后自己合并，hub 不会替你合并。",
  },
  failed: {
    label: "失败",
    word: "失败",
    tone: "danger",
    glyph: "✕",
    hint: "失败不是完成：下面写明失败原因，执行记录在 run 页。",
  },
  cancelled: {
    label: "已取消",
    word: "已取消",
    tone: "muted",
    glyph: "✕",
    hint: "取消的 run 没有完成，也不会自动落地。",
  },
  landed: {
    label: "已落地 / 已开 PR",
    word: "已落地",
    tone: "ok",
    glyph: "✓",
    hint: "结果已经合并进基准分支，或开好了 PR（PR 还没合入时，改动不在基准分支）。",
  },
  in_place: {
    label: "直接写在文件夹里",
    word: "写在文件夹",
    tone: "ok",
    glyph: "✓",
    hint: "这个项目没有 git：改动直接写在文件夹里，没有分支可合并。",
  },
};

const RESULT_ORDER: ResultKind[] = ["unlanded", "failed", "cancelled", "landed", "in_place", "pending"];

function resultKind(run: Run): ResultKind {
  if (run.status === "failed") return "failed";
  if (run.status === "cancelled") return "cancelled";
  if (run.status === "completed") {
    if (run.landed) return "landed";
    if (run.in_place) return "in_place";
    return "unlanded";
  }
  return "pending";
}

function ResultsView({ runs, agents }: { runs: RunDetail[]; agents: Agent[] }) {
  const groups = RESULT_ORDER.map((kind) => ({ kind, items: runs.filter((d) => resultKind(d.run) === kind) }));
  const ordered = groups.map((group) =>
    group.kind === "pending"
      ? { ...group, items: group.items.slice().sort((a, b) => b.run.updated_at.localeCompare(a.run.updated_at)) }
      : { ...group, items: group.items.slice().sort((a, b) => (b.run.landed?.at ?? b.run.updated_at).localeCompare(a.run.landed?.at ?? a.run.updated_at)) },
  );
  return (
    <>
      <div className="ws-results">
        {ordered
          .filter(({ items }) => items.length > 0)
          .map(({ kind, items }) => {
            const meta = kind === "pending" ? undefined : RESULT_META[kind];
            return (
              <section className="ws-result-group" key={kind}>
                <h2 className="ws-result-heading">
                  {meta?.label ?? "还没有结果"}
                  <span className="ws-count">{items.length}</span>
                </h2>
                {kind === "unlanded" && <p className="ws-group-hint">{meta?.hint}</p>}
                <div className="ws-results-list">
                  {items.map((detail) => (
                    <ResultRow key={detail.run.id} detail={detail} agents={agents} kind={kind} />
                  ))}
                </div>
              </section>
            );
          })}
        {!runs.length && <div className="empty">这个项目还没有结果。</div>}
      </div>
    </>
  );
}

function ResultRow({ detail, agents, kind }: { detail: RunDetail; agents: Agent[]; kind: ResultKind }) {
  const { run, tasks } = detail;
  const meta = kind === "pending" ? undefined : RESULT_META[kind];
  const state = kind === "pending" ? runState(run.status) : meta!;
  const summary = run.summary;
  const taskChecks = checksPassed(tasks.flatMap((task) => task.report.checks));
  const merged = tasks.filter((task) => task.status === "merged").length;
  const failedTasks = tasks.filter((task) => task.status === "failed").length;
  const origin = run.origin_chat_id ? agents.find((a) => a.id === run.origin_chat_id) : undefined;

  return (
    <article className={`ws-result ${kind}`}>
      <div className="ws-result-row">
        <a className="ws-result-title" href={`#/runs/${run.id}`} title="打开 run：任务、交接、追溯和执行记录">
          {shortTitle(run.title)}
        </a>
        <span className={`ws-outcome ${state.tone}`}>
          <span className="ws-glyph" aria-hidden="true">
            {kind === "pending" ? runState(run.status).glyph : meta!.glyph}
          </span>
          {kind === "pending" ? runState(run.status).word : kind === "landed" && run.landed?.mode === "pr" ? "已开 PR" : meta!.word}
        </span>
        {origin?.archived_at && (
          <a className="chip" href={sessionHref(origin)} title="来源会话已归档；打开它仍能看记录并恢复">
            来源已归档 · {displayName(origin)}
          </a>
        )}
      </div>

      {summary && <p className="ws-summary">{summary}</p>}

      <dl className="ws-facts">
        <dt>交付</dt>
        <dd>
          {tasks.length ? (
            <>
              {merged}/{tasks.length} 个任务已合并
              {failedTasks > 0 && <span className="ws-bad"> · {failedTasks} 个失败</span>}
            </>
          ) : run.plan.length ? (
            `计划 ${run.plan.length} 个任务，还没有任务记录`
          ) : (
            "还没有任务"
          )}
        </dd>
        <dt>验证</dt>
        <dd>
          {!run.checks.length && !taskChecks && (kind === "unlanded" || kind === "landed" || kind === "in_place") ? (
            <span className="ws-bad">没有运行验收</span>
          ) : (
            <>
              {checksText(run)}
              {taskChecks && <span className="muted"> · 任务验收 {taskChecks}</span>}
            </>
          )}
        </dd>
        <dt>落地</dt>
        <dd>
          <LandingLine run={run} kind={kind} />
        </dd>
        <dt>分支</dt>
        <dd>
          {run.in_place ? <span className="muted">没有分支（直接改文件）</span> : <BranchName run={run} />}
        </dd>
      </dl>

      {run.checks.length > 0 && (
        <details className="ws-check-details">
          <summary className="small muted">
            项目检查 {run.checks.filter((check) => check.passed).length}/{run.checks.length}
          </summary>
          {run.checks.map((check, i) => (
            <details className="check-result" key={i} open={!check.passed}>
              <summary>
                <span className={check.passed ? "pass" : "fail"}>{check.passed ? "✓" : "✗"}</span>
                <code>{check.command}</code>
                <span className="muted small">exit {check.exit_code ?? "—"}</span>
              </summary>
              <pre>{check.output || "（无输出）"}</pre>
            </details>
          ))}
        </details>
      )}

      {run.note && <p className={`ws-note ${kind === "failed" ? "danger" : ""}`}>{run.note}</p>}
      {kind === "cancelled" && run.origin_chat_id && (
        <p className="ws-member-empty">
          来源会话仍在：<a href={sessionHref(run.origin_chat_id)}>打开来源</a>
        </p>
      )}
    </article>
  );
}

function checksText(run: Run): string {
  const state = run.checks_state;
  if (state?.state === "not_configured") return "没有配置项目检查";
  if (state?.state === "not_applicable") return `项目检查不适用（声明 ${state.declared} 条）`;
  if (state?.state === "ran" || run.checks.length) {
    if (!run.checks.length) return "项目检查已运行，没有结果";
    return `${run.checks.filter((check) => check.passed).length}/${run.checks.length} 项目检查通过`;
  }
  return "没有运行项目检查";
}

function LandingLine({ run, kind }: { run: Run; kind: ResultKind }) {
  const landed = run.landed;
  if (landed) {
    const who = <span className="muted"> · {landed.by === "person" ? "你" : landed.by} · {ago(landed.at)}</span>;
    if (landed.mode === "pr") {
      return (
        <>
          开了 PR：<a href={landed.reference} target="_blank" rel="noreferrer">{landed.reference}</a>
          {who}
          <span className="muted">（合入后才进 {run.base_branch}）</span>
        </>
      );
    }
    return (
      <>
        {landed.mode === "squash" ? "压缩合并进" : "合并进"} <code>{run.base_branch}</code>：<code>{landed.reference}</code>
        {who}
      </>
    );
  }
  if (run.in_place) return <span className="muted">改动直接写在 {run.repo}，没有分支可合并</span>;
  if (kind === "unlanded") {
    return (
      <span className="ws-landing-wait">
        <span>还没落地 · 结果在 <code>{run.branch}</code></span>
        <CopyCommand command={`git merge ${run.branch}`} />
      </span>
    );
  }
  if (kind === "failed" || kind === "cancelled") return <span className="muted">没有落地</span>;
  return <span className="muted">还没有结果可落地</span>;
}
