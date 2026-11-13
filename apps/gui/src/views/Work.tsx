// A work group (docs/tasks/G1.md §3.5): one continuous conversation with its lead, and the
// rounds — runs — it has had so far. The conversation stays put while the rounds change; the
// separator where a round began marks it, and the 各轮 pane on the right picks one and brings
// its separator into view. Everything that belongs to a single round (tasks, checks, landing,
// the closing summary) stays on that run's page, one click away.

import { useEffect, useMemo, useState } from "react";
import type { Run } from "@bindings/Run";
import { api } from "../api";
import { ApplyButton } from "../apply";
import { ErrorBox, FileScopeProvider, Icon, OneLine, agentName, ago, firstSentence, sessionHref, time, useAgents } from "../components";
import { DeleteRunDialog } from "../deleteRun";
import { useHubData } from "../hub";
import { InlineNeeds } from "../inlineask";
import { openInRun } from "../notices";
import { openTool } from "../panes";
import { useQuestions } from "../questions";
import { RailCard } from "../rail";
import { RelayBanner } from "../relay";
import { navigate } from "../App";
import { RunModeTag, leadsInPlace } from "../runmode";
import { runState } from "../lib/taskState";
import { StateWord } from "../parts";
import { Ending, ProgressChip, RunRail, useRun } from "./Run";
import { SessionView, useSideSlot, type RoundMark } from "./Session";
import "../work.css";

/** How a round reads in the pane and in the conversation's separator. */
function marksOf(runs: Run[]): RoundMark[] {
  return runs.map((run, i) => ({ id: run.id, n: i + 1, title: run.title, status: run.status, at: run.created_at }));
}

/** What the round left behind, in a few words: landed or not, and how its checks went. */
function roundFacts(run: Run): { landed: string; checks: string } {
  const landed = run.landed ? `已落地${run.landed.mode === "pr" ? " · PR" : ""}` : leadsInPlace(run) ? "原地 · 无分支" : "未落地";
  const checks = !run.checks.length
    ? run.checks_state?.state === "not_applicable"
      ? "不需要检查"
      : "未跑检查"
    : `${run.checks.filter((c) => c.passed).length}/${run.checks.length} 检查通过`;
  return { landed, checks };
}

/** One round in the pane: the target, its state, what it left behind, what it cost, when. */
function RoundRow({
  mark,
  run,
  selected,
  cost,
  onPick,
}: {
  mark: RoundMark;
  run: Run;
  selected: boolean;
  cost: number | null | undefined;
  onPick: () => void;
}) {
  const state = runState(run.status);
  const facts = roundFacts(run);
  return (
    <li>
      <button className={`round-row ${selected ? "selected" : ""}`} onClick={onPick} aria-current={selected ? "true" : undefined} title={run.request}>
        <span className="round-row-top">
          <span className="round-n">第 {mark.n} 轮</span>
          <StateWord state={state} />
          <span className="spacer" />
          <span className="round-when" title={time(run.created_at)}>
            {ago(run.created_at)}
          </span>
        </span>
        <span className="round-row-title">{run.title}</span>
        <span className="round-row-facts">
          <span className={run.landed ? "ok" : "muted"}>{facts.landed}</span>
          <span aria-hidden="true">·</span>
          <span>{facts.checks}</span>
          {cost != null && (
            <>
              <span aria-hidden="true">·</span>
              <span>${cost.toFixed(2)}</span>
            </>
          )}
        </span>
      </button>
    </li>
  );
}

/** The 各轮 pane: every round one row, the selected one lit; picking one moves the conversation. */
function RoundsPane({
  workId,
  marks,
  runs,
  selected,
  costs,
  deleted,
}: {
  workId: string;
  marks: RoundMark[];
  runs: Run[];
  selected?: string;
  costs: Record<string, number | null>;
  deleted: { id: string; title: string; at: string }[];
}) {
  return (
    <RailCard id="rounds" title="各轮" aside={`${marks.length} 轮`}>
      <ol className="round-list">
        {marks.map((mark, i) => (
          <RoundRow
            key={mark.id}
            mark={mark}
            run={runs[i]}
            selected={selected === mark.id}
            cost={costs[mark.id]}
            onPick={() => navigate("work", workId, mark.id)}
          />
        ))}
      </ol>
      {deleted.length > 0 && (
        <div className="round-deleted">
          <span className="muted small">已删除的轮次</span>
          <ul className="plain">
            {deleted.map((d) => (
              <li key={d.id} className="round-deleted-row" title={`这一轮已经删除，只留下记录（${time(d.at)}）`}>
                {d.title}
              </li>
            ))}
          </ul>
        </div>
      )}
    </RailCard>
  );
}

/** The header's way into the rounds when the window has no room for the rail beside it. */
function RoundsToggle({ n }: { n: number }) {
  const { narrow, open, toggle } = useSideSlot();
  if (!narrow) return null;
  return (
    <button
      className={`ghost small side-toggle ${open ? "on" : ""}`}
      onClick={toggle}
      aria-label={open ? "收起各轮" : "打开各轮"}
      aria-expanded={open}
      title="这个工作组的每一轮"
    >
      <Icon name="panel" size={14} />
      各轮 {n}
    </button>
  );
}

export function WorkView({ workId, roundId }: { workId: string; roundId?: string }) {
  const detail = useHubData(
    () => api.getWork(workId),
    [workId],
    (e) => e.type.startsWith("run.") || e.type.startsWith("work."),
  );
  const agents = useAgents();
  const questions = useQuestions();
  const [deleting, setDeleting] = useState(false);
  const rounds = useMemo(() => (detail.data?.runs ?? []).slice().sort((a, b) => a.created_at.localeCompare(b.created_at)), [detail.data]);
  const marks = useMemo(() => marksOf(rounds), [rounds]);
  const ids = rounds.map((r) => r.id).join(",");
  // What each round spent; a hub that cannot answer leaves the row without a figure.
  const costs = useHubData(
    async () => {
      if (!rounds.length) return {} as Record<string, number | null>;
      const answers = await Promise.all(rounds.map((r) => api.runCost(r.id).then((c) => c.cost_usd, () => null)));
      return Object.fromEntries(rounds.map((r, i) => [r.id, answers[i]])) as Record<string, number | null>;
    },
    [ids],
    () => false,
  );
  const selected = rounds.find((r) => r.id === roundId) ?? rounds[rounds.length - 1];
  const runDetail = useRun(selected?.id ?? "");

  // Picking a round in the pane (or arriving with one in the route) brings its separator into
  // view. Without one in the route the conversation opens at its end, as a conversation does.
  useEffect(() => {
    if (!roundId) return;
    const find = () => document.getElementById(`round-${roundId}`)?.scrollIntoView({ block: "start", behavior: "smooth" });
    const frame = requestAnimationFrame(find);
    const later = setTimeout(find, 400);
    return () => {
      cancelAnimationFrame(frame);
      clearTimeout(later);
    };
  }, [roundId, marks.length]);

  if (!detail.data) {
    return <div className="page-inner">{detail.error ? <ErrorBox error={detail.error} /> : <p className="muted">加载中…</p>}</div>;
  }
  const { work, history } = detail.data;
  const deleted = history
    .filter((h) => h.deleted)
    .map((h) => ({ id: h.run_id, title: h.title, at: h.ended_at ?? h.created_at }))
    .sort((a, b) => b.at.localeCompare(a.at));
  const n = selected ? rounds.findIndex((r) => r.id === selected.id) + 1 : 0;
  const currentRun = runDetail.data?.run ?? selected;
  const tasks = runDetail.data?.tasks ?? [];
  const asks = currentRun ? openInRun(questions.data?.items ?? [], currentRun) : [];
  const allEnded = tasks.length > 0 && tasks.every((t) => ["merged", "failed", "cancelled"].includes(t.status));
  const isLastRound = !!selected && rounds[rounds.length - 1]?.id === selected.id;
  const activeRun = !!currentRun && ["planning", "awaiting_approval", "running", "paused"].includes(currentRun.status);
  const reloadAll = () => {
    detail.reload();
    runDetail.reload();
  };

  const header = (
    <div className="topbar work-topbar">
      <span className="title">{work.title}</span>
      {currentRun && (
        <span className="chip state-chip">
          <StateWord state={runState(currentRun.status)} />
        </span>
      )}
      {currentRun && <RunModeTag run={currentRun} />}
      {rounds.length > 0 && (
        <span className="chip" title={`这个工作组一共 ${rounds.length} 轮`}>
          第 {n} / {rounds.length} 轮
        </span>
      )}
      <ProgressChip tasks={tasks} />
      <div className="spacer" />
      {currentRun && <ApplyButton run={currentRun} />}
      <RoundsToggle n={rounds.length} />
      {currentRun && !currentRun.in_place && (
        <button className="ghost small graph-toggle" onClick={() => openTool("graph")} title="在右侧工具列看这一轮的分支和提交">
          <span className="row" style={{ gap: 5 }}>
            <Icon name="branch" size={13} />
            提交图
          </span>
        </button>
      )}
      <a className="chip" href={sessionHref(work.lead)} title="主导的会话">
        主导 · {agentName(agents.data, work.lead)}
      </a>
      {currentRun && isLastRound && (
        <button
          className="danger"
          title={activeRun ? "先取消，再删除这一轮" : "删除这一轮、它的任务和工作区"}
          onClick={() => setDeleting(true)}
        >
          {activeRun ? "取消并删除" : "删除"}
        </button>
      )}
    </div>
  );

  const top = currentRun ? (
    <div className="run-head">
      <div className="work-round-head">
        <div className="work-round-line">
          <span className="work-round-n">第 {n} 轮</span>
          <StateWord state={runState(currentRun.status)} />
          <RunModeTag run={currentRun} compact />
          <span className="muted small">
            {roundFacts(currentRun).landed} · {roundFacts(currentRun).checks}
          </span>
          <span className="spacer" />
          <span className="muted small" title={time(currentRun.created_at)}>
            {ago(currentRun.created_at)}
          </span>
        </div>
        <p className="work-round-request">{currentRun.request}</p>
        {currentRun.note && <p className="work-round-note">{currentRun.note}</p>}
      </div>
      <RelayBanner run={currentRun} tasks={tasks} agents={agents.data} />
      <InlineNeeds items={asks} agents={agents.data ?? []} />
      {currentRun.status === "paused" && (
        <OneLine tone="warn" icon={<span aria-hidden="true">❚❚</span>} title="已暂停" summary={firstSentence(currentRun.note ?? "等你决定")}>
          {`${currentRun.note ?? "等你决定"}。在「问你」里选择加预算继续，或取消这个 run。`}
        </OneLine>
      )}
      <ErrorBox error={runDetail.error} />
    </div>
  ) : null;

  const bottom =
    currentRun && (currentRun.status === "completed" || (currentRun.status === "running" && allEnded)) ? (
      <Ending run={currentRun} closing={currentRun.status === "running"} onDone={reloadAll} />
    ) : undefined;

  const side = (
    <>
      <RoundsPane
        workId={workId}
        marks={marks}
        runs={rounds}
        selected={selected?.id}
        costs={costs.data ?? {}}
        deleted={deleted}
      />
      {currentRun && <RunRail run={currentRun} tasks={tasks} agents={agents.data} onPolicyChange={reloadAll} />}
    </>
  );

  const page = (
    <SessionView
      key={work.lead}
      name={work.lead}
      header={header}
      top={top}
      bottom={bottom}
      side={side}
      sideLabel="各轮与任务"
      rounds={marks}
    />
  );

  if (!currentRun) return page;
  return (
    <FileScopeProvider value={{ kind: "run", run: currentRun.id }}>
      {page}
      {deleting && runDetail.data && (
        <DeleteRunDialog
          detail={runDetail.data}
          onClose={() => {
            setDeleting(false);
            reloadAll();
          }}
        />
      )}
    </FileScopeProvider>
  );
}
