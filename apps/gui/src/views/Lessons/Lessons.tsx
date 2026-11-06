import { useState } from "react";
import { api } from "../../api";
import { ErrorBox, Icon, ago, time } from "../../components";
import type { HubEvent } from "../../hub";
import { useHubData } from "../../hub";
import { projectForWorkspace } from "../../lib/expertise";
import {
  OUTCOME_LABEL,
  STATUS_LABEL,
  outcomeTone,
  shortId,
  type Lesson,
  type LessonStatus,
} from "../../lessons/lessons";
import { useSelectedWorkspace, wsName } from "../../workspace";
import "../../expertise.css";
import "../../lessons/lessons.css";

const relevant = (e: HubEvent) => e.type === "lesson.created" || e.type === "lesson.updated" || e.type === "issue.triaged";
const STATUSES: (LessonStatus | "")[] = ["", "active", "review", "superseded"];

/** One record: the headline row, and when open its symptom, resolution, evidence and sources. */
function LessonRow({ l }: { l: Lesson }) {
  const [open, setOpen] = useState(false);
  return (
    <li className={`exp-entry lesson ${l.status === "superseded" ? "superseded" : ""}`}>
      <div className="exp-entry-head lesson-head" onClick={() => setOpen(!open)} title={open ? "点击收起" : "点击展开"}>
        <Icon name={open ? "chevron" : "chevronRight"} size={14} />
        <code className="muted exp-id" title={l.id}>
          {shortId(l.id)}
        </code>
        <b>{l.title}</b>
        <span className={`exp-tag ${outcomeTone(l.outcome)}`}>{OUTCOME_LABEL[l.outcome]}</span>
        {l.status !== "active" && <span className={`exp-tag ${l.status === "review" ? "yellow" : ""}`}>{STATUS_LABEL[l.status]}</span>}
        <span className="spacer" />
        <span className="muted exp-stats" title="试过的次数（含写下那次）">
          试过 {l.evidence.length} 次 · {ago(l.updated_at)}
        </span>
      </div>
      {l.keywords.length > 0 && (
        <div className="lesson-keywords">
          {l.keywords.map((k) => (
            <code key={k}>{k}</code>
          ))}
        </div>
      )}
      {open && (
        <div className="lesson-body">
          <dl className="exp-example">
            <dt>症状</dt>
            <dd className="lesson-pre">{l.symptom}</dd>
            {l.conditions && (
              <>
                <dt>条件</dt>
                <dd>{l.conditions}</dd>
              </>
            )}
            <dt>处理</dt>
            <dd className="lesson-pre">{l.resolution}</dd>
            {l.superseded_by && (
              <>
                <dt>替换为</dt>
                <dd>
                  <code>{shortId(l.superseded_by)}</code>
                </dd>
              </>
            )}
          </dl>
          <h4>试用记录</h4>
          <ul className="lesson-evidence">
            {[...l.evidence].reverse().map((e, i) => (
              <li key={i}>
                <span className={`exp-tag ${outcomeTone(e.outcome)}`}>{OUTCOME_LABEL[e.outcome]}</span>
                <span title={time(e.at)}>{ago(e.at)}</span>
                <span className="muted">
                  {e.agent} @ {e.node}
                  {e.commit && ` · ${e.commit.slice(0, 7)}${e.dirty ? "（有未提交改动）" : ""}`}
                  {e.run_id && ` · run ${shortId(e.run_id)}`}
                </span>
                {e.note && <span className="lesson-note">{e.note}</span>}
              </li>
            ))}
          </ul>
          {l.sources.length > 0 && (
            <>
              <h4>来源</h4>
              <ul className="lesson-sources">
                {l.sources.map((s) => (
                  <li key={`${s.run_id}:${s.key}`}>
                    <a href={`#/runs/${encodeURIComponent(s.run_id)}`}>run {shortId(s.run_id)}</a> <code>{s.key}</code>
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      )}
    </li>
  );
}

/** The project's records as the hub exports them. */
function Export({ project }: { project: string }) {
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const exported = useHubData(() => api.lessonsExport(project), [project], relevant);
  const text = exported.data ?? "";
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard may be unavailable; the preview can be selected by hand.
    }
  };
  return (
    <section className="settings-section">
      <div className="settings-head">
        <h2>导出 Markdown</h2>
        <div className="spacer" />
        <button onClick={() => setOpen(!open)} disabled={!text.trim()}>
          {open ? "收起预览" : "预览"}
        </button>
        <button onClick={copy} disabled={!text.trim()}>
          <Icon name={copied ? "check" : "copy"} size={14} /> {copied ? "已复制" : "复制"}
        </button>
      </div>
      <ErrorBox error={exported.error} />
      {open && text && <pre className="exp-md">{text}</pre>}
    </section>
  );
}

/** A project's troubleshooting records: search, filter by status, open one for its evidence. */
export function LessonsView() {
  const workspace = useSelectedWorkspace();
  const [q, setQ] = useState("");
  const [status, setStatus] = useState<LessonStatus | "">("");
  const projects = useHubData(() => api.projects(), [], (e) => e.type === "run.created");
  const project = projectForWorkspace(projects.data, workspace);
  const query = q.trim();
  const lessons = useHubData(
    () => (project ? api.lessons(project.id, query, status || undefined) : Promise.resolve([])),
    [project?.id, query, status],
    relevant,
  );
  const list = lessons.data ?? [];
  return (
    <>
      <div className="topbar">
        <span className="title">排障记录</span>
      </div>
      <div className="page">
        <div className="page-inner settings exp-page lessons-page">
          <div className="exp-owner">
            <span className="muted">
              {workspace ? `项目 ${project?.name ?? wsName(workspace)}` : ""} · 早先的 agent 在这里遇到过的问题和当时的处理。记录是参考，不是规则。
            </span>
          </div>
          {!workspace ? (
            <div className="empty">先在侧边栏选一个项目，再看它的排障记录。</div>
          ) : projects.data && !project ? (
            <div className="empty">这个目录还没有 run 过，没有排障记录。</div>
          ) : (
            <>
              <ErrorBox error={projects.error ?? lessons.error} />
              <div className="lessons-filter">
                <input
                  type="search"
                  placeholder="搜索：错误文本、错误码、工具名…"
                  value={q}
                  onChange={(e) => setQ(e.target.value)}
                  aria-label="搜索排障记录"
                />
                <div className="segmented small" role="radiogroup" aria-label="状态">
                  {STATUSES.map((s) => (
                    <button key={s || "all"} role="radio" aria-checked={status === s} className={status === s ? "active" : ""} onClick={() => setStatus(s)}>
                      {s ? STATUS_LABEL[s] : "全部"}
                    </button>
                  ))}
                </div>
              </div>
              <section className="settings-section">
                <div className="settings-head">
                  <h2>记录</h2>
                  {lessons.data && <span className="muted">{list.length} 条</span>}
                </div>
                {lessons.data &&
                  (list.length ? (
                    <ul className="exp-entries">
                      {list.map((l) => (
                        <LessonRow key={l.id} l={l} />
                      ))}
                    </ul>
                  ) : (
                    <div className="empty">
                      {query || status ? "没有匹配的记录。" : "还没有记录。run 结束时，lead 会把 agent 记下的问题整理成排障记录。"}
                    </div>
                  ))}
              </section>
              {project && <Export project={project.id} />}
            </>
          )}
        </div>
      </div>
    </>
  );
}
