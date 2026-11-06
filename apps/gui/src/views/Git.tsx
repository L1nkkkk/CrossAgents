// The Git page (`#/git/<project dir>`): one project's repository, read-only. Its branches on
// the left, the whole commit graph in the middle, the picked commit's details and diff on the
// right, and above them how many changes are not committed yet. The projects are the
// directories the hub knows: agents' working directories and runs' repositories.

import { useEffect, useMemo, useState } from "react";
import type { Agent } from "@bindings/Agent";
import type { GitBranch } from "@bindings/GitBranch";
import type { GitStatusEntry } from "@bindings/GitStatusEntry";
import { api, type FileScope } from "../api";
import { navigate } from "../App";
import { ErrorBox, ago, time } from "../components";
import { ChangeStat, DiffView } from "../diffview";
import { openFile } from "../FileViewer";
import { GitGraphView } from "../gitgraph";
import { useHubData, type HubEvent } from "../hub";
import { parseDiff } from "../lib/diff";
import "../git.css";

const same = (a: string, b: string) => a.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase() === b.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();

/** Work in the repository may have moved it on: a turn ended, a task changed state. */
const relevant = (e: HubEvent) => e.type === "agent.turn_completed" || e.type === "task.status_changed" || e.type === "run.status_changed";

type Projects = { paths: string[]; agents: Agent[] };

async function loadProjects(): Promise<Projects> {
  const [agents, runs] = await Promise.all([api.listAgents(), api.listRuns()]);
  const paths: string[] = [];
  for (const p of [...agents.agents.map((a) => a.cwd), ...runs.runs.map((r) => r.repo)]) {
    if (p && !paths.some((q) => same(p, q))) paths.push(p);
  }
  return { paths, agents: agents.agents };
}

export function GitView({ path }: { path?: string }) {
  const projects = useHubData(loadProjects, [], (e) => e.type === "agent.created" || e.type === "run.created");
  const current = path || projects.data?.paths[0];

  return (
    <div className="page git-page">
      <div className="git-top">
        <h2 className="git-title">Git</h2>
        {projects.data && projects.data.paths.length > 0 && (
          <select aria-label="项目" className="git-project" value={current ?? ""} onChange={(e) => navigate("git", e.target.value)}>
            {current && !projects.data.paths.some((p) => same(p, current)) && <option value={current}>{current}</option>}
            {projects.data.paths.map((p) => <option key={p} value={p}>{p}</option>)}
          </select>
        )}
        {current && <StatusBar path={current} />}
      </div>
      {projects.error && !projects.data && <ErrorBox error={projects.error} />}
      {!current && projects.data && <p className="muted git-empty">还没有项目：会话的工作目录和 run 的仓库会出现在这里。</p>}
      {!current && !projects.data && !projects.error && <p className="muted small git-empty">加载项目…</p>}
      {current && <Repository key={current} path={current} agents={projects.data?.agents ?? []} />}
    </div>
  );
}

const STATE_WORD: Record<string, string> = { modified: "修改", added: "新增", deleted: "删除", renamed: "重命名", copied: "复制", type_changed: "类型变更", unmerged: "冲突", unknown: "未知" };

function entryWord(e: GitStatusEntry): string {
  if (e.conflicted) return "冲突";
  if (e.untracked) return "未跟踪";
  const side = e.index !== "unchanged" ? e.index : e.worktree;
  return STATE_WORD[side] ?? side;
}

/** Branch, upstream, ahead/behind, and how many changes are not committed. */
function StatusBar({ path }: { path: string }) {
  const status = useHubData(() => api.gitStatus(path), [path], relevant);
  const [open, setOpen] = useState(false);
  useEffect(() => setOpen(false), [path]);
  if (status.error && !status.data) return <span className="chip warn small" title={status.error}>不是可读的 git 仓库</span>;
  if (!status.data) return <span className="muted small">读取状态…</span>;
  const s = status.data;
  const n = s.entries.length;
  return (
    <div className="git-status small">
      <span className="git-status-branch">{s.branch ? <><span className="muted">分支</span> <b>{s.branch}</b></> : <span className="chip warn">HEAD 游离</span>}</span>
      {s.upstream && <span className="muted">跟踪 {s.upstream}</span>}
      {s.ahead > 0 && <span className="git-ab" title={`比上游多 ${s.ahead} 个提交`}>↑{s.ahead}</span>}
      {s.behind > 0 && <span className="git-ab" title={`比上游少 ${s.behind} 个提交`}>↓{s.behind}</span>}
      <span className="git-dirty-wrap">
        <button className={`git-dirty ${n ? "dirty" : "clean"}`} aria-expanded={open} disabled={!n} onClick={() => setOpen(!open)}>
          {n ? `${n} 处未提交改动` : "工作区干净"}
        </button>
        {open && n > 0 && (
          <div className="git-dirty-list" role="list">
            {s.entries.map((e) => (
              <div key={e.path} className="git-dirty-item" role="listitem">
                <span className={`git-state ${e.untracked ? "untracked" : e.conflicted ? "conflicted" : e.index !== "unchanged" ? e.index : e.worktree}`}>{entryWord(e)}</span>
                <code title={e.orig_path ? `${e.orig_path} → ${e.path}` : e.path}>{e.orig_path ? `${e.orig_path} → ${e.path}` : e.path}</code>
                {e.index !== "unchanged" && <span className="muted">已暂存</span>}
              </div>
            ))}
          </div>
        )}
      </span>
      <button className="ghost small" onClick={status.reload} title="重新读取">刷新</button>
    </div>
  );
}

function Repository({ path, agents }: { path: string; agents: Agent[] }) {
  const branches = useHubData(() => api.gitBranches(path), [path], relevant);
  const graph = useHubData(() => api.gitGraph(path, 300), [path], relevant);
  const [picked, setPicked] = useState<string>();
  // Until something is picked, the commit HEAD is at.
  const shown = picked ?? branches.data?.head_sha ?? graph.data?.commits[0]?.sha;
  const owner = agents.find((a) => same(a.cwd, path));
  const fileScope: FileScope | undefined = owner ? { kind: "agent", agent: owner.id } : undefined;
  const known = useMemo(() => new Set(graph.data?.commits.map((c) => c.sha) ?? []), [graph.data]);

  const error = graph.error && !graph.data ? graph.error : undefined;
  if (error) return <div className="git-error"><ErrorBox error={error} /></div>;
  return (
    <div className="git-body">
      <aside className="git-col git-branches" aria-label="分支">
        {branches.error && !branches.data && <ErrorBox error={branches.error} />}
        {!branches.data && !branches.error && <p className="muted small">加载分支…</p>}
        {branches.data && (
          <>
            <BranchList title="本地分支" list={branches.data.branches.filter((b) => !b.remote)} picked={shown} onPick={setPicked} />
            <BranchList title="远程分支" list={branches.data.branches.filter((b) => b.remote)} picked={shown} onPick={setPicked} />
          </>
        )}
      </aside>
      <section className="git-col git-graph-col" aria-label="提交图">
        {!graph.data ? <p className="muted small">加载提交图…</p> : <GitGraphView graph={graph.data} picked={shown} onPick={setPicked} />}
      </section>
      <section className="git-col git-detail" aria-label="提交详情">
        {shown ? <CommitDetail path={path} sha={shown} known={known} onPick={setPicked} fileScope={fileScope} /> : <p className="muted small">选一个提交看它的详情。</p>}
      </section>
    </div>
  );
}

function BranchList({ title, list, picked, onPick }: { title: string; list: GitBranch[]; picked?: string; onPick: (sha: string) => void }) {
  return (
    <div className="git-branch-group">
      <h3 className="git-col-title">{title} <span className="muted">{list.length}</span></h3>
      {!list.length && <p className="muted small">没有</p>}
      {list.map((b) => (
        <button key={b.name} className={`git-branch ${b.current ? "current" : ""} ${picked === b.sha ? "active" : ""}`} onClick={() => onPick(b.sha)} title={`${b.name}${b.upstream ? ` → ${b.upstream}` : ""}\n${b.sha}`}>
          <span className="git-branch-name">{b.name}</span>
          {b.current && <span className="git-head-chip">HEAD</span>}
          {b.ahead > 0 && <span className="git-ab" title={`比 ${b.upstream} 多 ${b.ahead} 个提交`}>↑{b.ahead}</span>}
          {b.behind > 0 && <span className="git-ab" title={`比 ${b.upstream} 少 ${b.behind} 个提交`}>↓{b.behind}</span>}
          {!b.remote && !b.upstream && <span className="muted small" title="没有上游分支">仅本地</span>}
        </button>
      ))}
    </div>
  );
}

function CommitDetail({ path, sha, known, onPick, fileScope }: { path: string; sha: string; known: Set<string>; onPick: (sha: string) => void; fileScope?: FileScope }) {
  const commit = useHubData(() => api.gitCommit(path, sha), [path, sha], () => false);
  const changes = useHubData(() => api.gitCommitDiff(path, sha), [path, sha], () => false);
  const files = useMemo(() => (changes.data ? parseDiff(changes.data.diff) : []), [changes.data]);
  const c = commit.data;
  const body = c ? c.message.slice(c.subject.length).trim() : "";
  return (
    <div className="git-commit">
      {commit.error && <ErrorBox error={commit.error} />}
      {!c && !commit.error && <p className="muted small">加载提交…</p>}
      {c && (
        <div className="git-commit-head">
          <div className="git-commit-subject">{c.subject || "（无标题）"}</div>
          {body && <pre className="git-commit-body">{body}</pre>}
          <dl className="git-commit-meta small">
            <dt>作者</dt><dd>{c.author} <span className="muted">&lt;{c.author_email}&gt;</span></dd>
            <dt>时间</dt><dd title={time(c.time)}>{time(c.time)} <span className="muted">（{ago(c.time)}）</span></dd>
            <dt>提交</dt><dd><code>{c.sha}</code></dd>
            <dt>父提交</dt>
            <dd className="git-parents">
              {!c.parents.length && <span className="muted">无（第一个提交）</span>}
              {c.parents.map((p) => (known.has(p) ? <button key={p} className="link" onClick={() => onPick(p)}><code>{p.slice(0, 7)}</code></button> : <code key={p}>{p.slice(0, 7)}</code>))}
              {c.parents.length > 1 && <span className="muted">（合并，改动对比第一个父提交）</span>}
            </dd>
          </dl>
        </div>
      )}
      <div className="git-diff-head small">
        {changes.data && <span className="muted">{files.length} 个文件 <ChangeStat files={files} /></span>}
        {changes.data?.truncated && <span className="chip warn">diff 太长，已截断</span>}
      </div>
      {changes.error && <ErrorBox error={changes.error} />}
      {!changes.data && !changes.error && <p className="muted small">加载改动…</p>}
      {changes.data && (files.length
        ? <DiffView files={files} onOpenFile={fileScope ? (p) => openFile({ scope: fileScope, path: p }) : undefined} />
        : <p className="muted small">这个提交没有改文件。</p>)}
    </div>
  );
}
