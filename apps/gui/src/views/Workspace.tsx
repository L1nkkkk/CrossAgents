import type { RunDetail } from "@bindings/RunDetail";
import { ACTIVE_RUN } from "../App";
import { Avatar, CopyCommand, Icon, Mark, StatusChip, Vendor, agentTitle, agentTone, ago, displayName, runTone, sessionHref, taskTone } from "../components";
import { selectWorkspace, useWorkspaces, type Workspace } from "../workspace";

export function WorkspaceView({ wsKey }: { wsKey: string }) {
  const { list, loaded } = useWorkspaces();
  const ws = list.find((w) => w.key === wsKey);

  if (!ws) {
    return <div className="page-inner">{loaded ? <div className="empty">没有这个工作区：{wsKey}</div> : <p className="muted">加载中…</p>}</div>;
  }
  return <Overview ws={ws} />;
}

function Overview({ ws }: { ws: Workspace }) {
  const live = ws.runs.filter((d) => ACTIVE_RUN.includes(d.run.status));
  const awaiting = live.filter((d) => d.run.status === "awaiting_approval");
  // Completed runs leave their result on the integration branch for a human to merge.
  const toMerge = ws.runs.filter((d) => d.run.status === "completed" && !d.run.in_place);
  const ended = ws.runs.filter((d) => !ACTIVE_RUN.includes(d.run.status) && d.run.status !== "completed");
  const working = ws.agents.filter((a) => a.status === "running").length;

  return (
    <>
      <div className="topbar">
        <Icon name="folder" size={16} />
        <span className="title">{ws.name}</span>
        <span className="chip mono" title={ws.path}>
          {ws.path}
        </span>
        <div className="spacer" />
        <a
          className="button primary"
          href="#/"
          onClick={() => selectWorkspace(ws.key)}
          title="在这个工作区里发起 run"
        >
          <Icon name="plus" size={14} />
          新建 run
        </a>
      </div>
      <div className="page">
        <div className="page-inner ws-page">
          <div className="stats">
            <Stat label="进行中" value={live.length} tone={live.length ? "accent" : undefined} />
            <Stat label="待审批" value={awaiting.length} tone={awaiting.length ? "warn" : undefined} />
            <Stat label="待你合并" value={toMerge.length} tone={toMerge.length ? "ok" : undefined} />
            <Stat label="Agents" value={ws.agents.length} hint={working ? `${working} 个在工作` : undefined} />
          </div>

          <section>
            <h2>进行中</h2>
            {live.length ? (
              <div className="board">
                {live.map((d) => (
                  <RunLine key={d.run.id} detail={d} />
                ))}
              </div>
            ) : (
              <p className="muted small">没有进行中的 run。</p>
            )}
          </section>

          {toMerge.length > 0 && (
            <section>
              <h2>待你合并</h2>
              <p className="muted small" style={{ marginTop: -4 }}>
                这些 run 已完成，结果在各自的集成分支上。确认无误后合并进你的分支（hub 不会替你合并）。
              </p>
              <div className="board">
                {toMerge.map((d) => (
                  <MergeLine key={d.run.id} detail={d} />
                ))}
              </div>
            </section>
          )}

          <section>
            <h2>Agents</h2>
            {ws.agents.length ? (
              <div className="board">
                {ws.agents.map((a) => (
                  <a key={a.id} className="board-row" href={sessionHref(a)}>
                    <Avatar agent={a} size={22} />
                    <div style={{ minWidth: 0 }}>
                      <div className="t">
                        {displayName(a)} <span className="muted small">{agentTitle(a)}</span>
                      </div>
                      <div className="sub">
                        @{a.name} · {a.role} · <Vendor plugin={a.plugin} model={a.model} />
                      </div>
                    </div>
                    <StatusChip status={a.status} tone={agentTone(a.status)} />
                  </a>
                ))}
              </div>
            ) : (
              <p className="muted small">
还没有 agent 在这个工作区。到 <a href="#/">首页</a> 选这个工作区发起 Chat 或 Work，或在 <a href="#/settings/agents">设置 → 智能体</a> 接入外部 agent 时把工作目录填成 <code>{ws.path}</code>。
              </p>
            )}
          </section>

          {ended.length > 0 && (
            <section>
              <h2>最近</h2>
              <div className="board">
                {ended.map((d) => (
                  <RunLine key={d.run.id} detail={d} />
                ))}
              </div>
            </section>
          )}
        </div>
      </div>
    </>
  );
}

function Stat({ label, value, tone, hint }: { label: string; value: number; tone?: "accent" | "warn" | "ok"; hint?: string }) {
  return (
    <div className={`stat ${tone ?? ""}`}>
      <span className="stat-label">{label}</span>
      <span className="stat-value">{value}</span>
      {hint && <span className="stat-hint">{hint}</span>}
    </div>
  );
}

function RunLine({ detail }: { detail: RunDetail }) {
  const { run, tasks } = detail;
  const merged = tasks.filter((t) => t.status === "merged").length;
  return (
    <a className="board-row" href={`#/runs/${run.id}`}>
      <Mark shape="run" tone={runTone(run.status)} size={12} />
      <div style={{ minWidth: 0 }}>
        <div className="t">{run.title}</div>
        <div className="sub row" style={{ gap: 8, flexWrap: "nowrap" }}>
          {tasks.length > 0 && (
            <span className="segments" style={{ width: 60, flexShrink: 0 }}>
              {tasks.map((t) => (
                <span key={t.id} className={`seg ${taskTone(t.status).color} ${taskTone(t.status).live ? "live" : ""}`} />
              ))}
            </span>
          )}
          <span>
            {tasks.length ? `${merged}/${tasks.length} merged · ` : ""}
            {ago(run.updated_at)}
          </span>
        </div>
      </div>
      <StatusChip status={run.status} tone={runTone(run.status)} />
    </a>
  );
}

function MergeLine({ detail }: { detail: RunDetail }) {
  const { run } = detail;
  return (
    <div className="board-row merge-row">
      <Mark shape="run" tone={{ color: "ok" }} size={12} />
      <div style={{ minWidth: 0 }}>
        <a className="t" href={`#/runs/${run.id}`}>
          {run.title}
        </a>
        <div className="sub">
          <code>{run.branch}</code> → <code>{run.base_branch}</code> · {ago(run.updated_at)}
        </div>
      </div>
      <CopyCommand command={`git merge ${run.branch}`} />
    </div>
  );
}
