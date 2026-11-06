// A work group (docs/tasks/G1.md §3.5): one continuous conversation with its lead, and the
// rounds — runs — it has had so far. The conversation stays put while the rounds change; the
// separator where a round began marks it, and the 各轮 pane on the right picks one and brings
// its separator into view. Everything that belongs to a single round (tasks, checks, landing,
// the closing summary) stays on that run's page, one click away.

import { useEffect, useMemo } from "react";
import type { Run } from "@bindings/Run";
import { api } from "../api";
import { ErrorBox, Icon, agentName, ago, sessionHref, time, useAgents } from "../components";
import { useHubData } from "../hub";
import { RailCard } from "../rail";
import { navigate } from "../App";
import { RunModeTag, leadsInPlace } from "../runmode";
import { runState } from "../lib/taskState";
import { StateWord } from "../parts";
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

  const header = (
    <div className="topbar work-topbar">
      <span className="title">{work.title}</span>
      {selected && (
        <span className="chip state-chip">
          <StateWord state={runState(selected.status)} />
        </span>
      )}
      {selected && <RunModeTag run={selected} />}
      {rounds.length > 0 && (
        <span className="chip" title={`这个工作组一共 ${rounds.length} 轮`}>
          第 {n} / {rounds.length} 轮
        </span>
      )}
      <div className="spacer" />
      <RoundsToggle n={rounds.length} />
      {selected && (
        <a className="chip" href={`#/runs/${selected.id}`} title="这一轮的任务、检查、落地和收尾">
          这一轮的 run
        </a>
      )}
      <a className="chip" href={sessionHref(work.lead)} title="主导的会话">
        主导 · {agentName(agents.data, work.lead)}
      </a>
    </div>
  );

  const top = selected ? (
    <div className="work-round-head">
      <div className="work-round-line">
        <span className="work-round-n">第 {n} 轮</span>
        <StateWord state={runState(selected.status)} />
        <RunModeTag run={selected} compact />
        <span className="muted small">
          {roundFacts(selected).landed} · {roundFacts(selected).checks}
        </span>
        <span className="spacer" />
        <span className="muted small" title={time(selected.created_at)}>
          {ago(selected.created_at)}
        </span>
      </div>
      <p className="work-round-request">{selected.request}</p>
      {selected.note && <p className="work-round-note">{selected.note}</p>}
    </div>
  ) : null;

  const side = (
    <RoundsPane
      workId={workId}
      marks={marks}
      runs={rounds}
      selected={selected?.id}
      costs={costs.data ?? {}}
      deleted={deleted}
    />
  );

  return (
    <SessionView
      key={work.lead}
      name={work.lead}
      header={header}
      top={top}
      side={side}
      sideLabel="各轮"
      rounds={marks}
    />
  );
}
