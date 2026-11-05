// Reviewing work (docs/tasks/DOGFOOD.md §2): a task's whole diff with comments on lines and
// files that go to its worker as its next turn (or send the task back), and a run's total
// diff before it lands.

import { useMemo, useState } from "react";
import type { ReviewComment } from "@bindings/ReviewComment";
import type { Run } from "@bindings/Run";
import type { Task } from "@bindings/Task";
import { api, errorText } from "./api";
import { ErrorBox } from "./components";
import { ChangeStat, DiffView, type DiffReview } from "./diffview";
import { useHubData } from "./hub";
import { parseDiff, type DiffFile, type DiffLine } from "./lib/diff";
import {
  confidencePercent,
  findingPlace,
  severityTone,
  sortFindings,
  verdictLabel,
  verdictTone,
} from "./lib/reviewFindings";

/** Where a comment goes: a file, and a line of it (on the old side for a removed line). */
type Spot = { path: string; line?: number; side?: "new" | "old" };
type Draft = Spot & { text: string };

const same = (a: Spot, b: Spot) => a.path === b.path && a.line === b.line && (a.side ?? "new") === (b.side ?? "new");
const spotOf = (path: string, line: DiffLine): Spot =>
  line.new !== undefined ? { path, line: line.new } : { path, line: line.old, side: "old" };

/** Statuses in which the worker is still on the task, so comments reach it. */
const OPEN = ["running", "waiting_answer", "passed", "stalled", "reviewing"];
/** Statuses a comment can send back to running. */
const REOPENS = ["passed", "stalled", "reviewing"];

/** The files of a diff as a list; a click scrolls to one. */
function FileList({ files, onPick }: { files: DiffFile[]; onPick: (path: string) => void }) {
  return (
    <div className="review-files">
      {files.map((f) => (
        <button key={f.path} className="review-file" onClick={() => onPick(f.path)} title={f.path}>
          <span className="mono">{f.path}</span>
          <ChangeStat files={[f]} />
        </button>
      ))}
    </div>
  );
}

/** A task's changes against its base, with the reviewer's findings and the person's comments. */
export function TaskReview({ run, task }: { run: Run; task: Task }) {
  const changes = useHubData(
    () => api.taskDiff(run.id, task.id),
    [run.id, task.id],
    (e) => (e.type === "task.status_changed" && e.data.id === task.id) || (e.type === "agent.turn_completed" && e.data.id === task.assignee),
  );
  const files = useMemo(() => (changes.data ? parseDiff(changes.data.diff) : []), [changes.data]);
  const findings = useMemo(() => sortFindings(task.report.findings ?? []), [task.report.findings]);
  const [focus, setFocus] = useState<string>();
  const [drafts, setDrafts] = useState<Draft[]>([]);
  const [editing, setEditing] = useState<Spot>();
  const [text, setText] = useState("");
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [sent, setSent] = useState<string>();

  if (run.in_place || !task.worktree) return null;
  const open = OPEN.includes(task.status);

  function startAt(spot: Spot) {
    const existing = drafts.find((d) => same(d, spot));
    setEditing(spot);
    setText(existing?.text ?? "");
  }
  function keep() {
    if (!editing) return;
    const rest = drafts.filter((d) => !same(d, editing));
    setDrafts(text.trim() ? [...rest, { ...editing, text: text.trim() }] : rest);
    setEditing(undefined);
    setText("");
  }
  async function send(rework: boolean) {
    setBusy(true);
    setError(undefined);
    const comments: ReviewComment[] = drafts.map((d) => ({ path: d.path, line: d.line, side: d.side, text: d.text }));
    try {
      await api.reviewTask(run.id, task.id, { comments, note: note.trim() || undefined, rework });
      setDrafts([]);
      setNote("");
      setSent(rework ? "已打回，意见作为 worker 的下一轮送达" : "意见已送到 worker 的收件箱");
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  const review: DiffReview | undefined = open
    ? {
        onLine: (path, line) => startAt(spotOf(path, line)),
        onFile: (path) => startAt({ path }),
        below: (path, line) => {
          const spot = line ? (line.new ?? line.old) !== undefined ? spotOf(path, line) : undefined : { path };
          if (!spot) return null;
          if (editing && same(editing, spot)) {
            return (
              <div className="review-edit">
                <textarea rows={2} autoFocus value={text} placeholder={spot.line ? `第 ${spot.line} 行的意见` : "对整个文件的意见"} onChange={(e) => setText(e.target.value)} />
                <div className="row" style={{ gap: 6 }}>
                  <button className="primary small" onClick={keep} disabled={!text.trim()}>
                    加入意见
                  </button>
                  <button className="ghost small" onClick={() => setEditing(undefined)}>
                    取消
                  </button>
                </div>
              </div>
            );
          }
          const draft = drafts.find((d) => same(d, spot));
          if (!draft) return null;
          return (
            <div className="review-draft">
              <span className="small">{draft.text}</span>
              <button className="link tiny" onClick={() => startAt(spot)}>
                改
              </button>
              <button className="link tiny" onClick={() => setDrafts(drafts.filter((d) => d !== draft))}>
                删
              </button>
            </div>
          );
        },
      }
    : undefined;

  return (
    <div className="review">
      {(findings.length > 0 || task.report.verdict) && (
        <div className="review-findings">
          <div className="row" style={{ gap: 6, marginBottom: 4 }}>
            <span className={`chip ${verdictTone(task.report.verdict)}`}>结论：{verdictLabel(task.report.verdict)}</span>
            {findings.length > 0 && <span className="small muted">{findings.length} 条评审意见，P0/P1 挡合并、排在最前</span>}
          </div>
          {findings.map((f, i) => (
            <div key={i} className="review-finding" style={{ marginTop: 4 }}>
              <div className="row" style={{ gap: 6 }}>
                <span className={`chip ${severityTone(f.severity)}`}>{f.severity}</span>
                <span className="small">{f.title}</span>
                <span className="small muted">置信度 {confidencePercent(f.confidence)}%</span>
                {f.file && <code className="small muted">{findingPlace(f)}</code>}
              </div>
              {f.detail && (
                <div className="pre small muted" style={{ marginLeft: 44 }}>
                  {f.detail}
                </div>
              )}
            </div>
          ))}
        </div>
      )}
      {changes.error && <ErrorBox error={changes.error} />}
      {!changes.data && !changes.error && <p className="muted small">加载改动…</p>}
      {changes.data && (
        <>
          <div className="small muted">
            对 <code>{changes.data.base}</code> 改了 {files.length} 个文件 <ChangeStat files={files} />
            {changes.data.truncated && <span className="chip warn" style={{ marginLeft: 6 }}>diff 太长，已截断</span>}
          </div>
          {files.length === 0 ? (
            <p className="muted small">还没有改动。</p>
          ) : (
            <>
              <FileList files={files} onPick={setFocus} />
              <DiffView files={files} focus={focus} review={review} />
            </>
          )}
        </>
      )}
      {open && (
        <div className="review-send">
          {drafts.length > 0 && <div className="small muted">{drafts.length} 条行内 / 文件意见待发送</div>}
          <textarea rows={2} value={note} placeholder="总体意见（可选）。点行号对某一行提意见。" onChange={(e) => setNote(e.target.value)} />
          <div className="row" style={{ gap: 6 }}>
            <button className="primary small" disabled={busy || (!drafts.length && !note.trim())} onClick={() => send(false)} title="作为 worker 的下一轮送达，任务状态不变">
              发给 worker
            </button>
            {REOPENS.includes(task.status) && (
              <button className="small" disabled={busy} onClick={() => send(true)} title="任务回到运行中，意见作为 worker 的下一轮送达">
                打回重做
              </button>
            )}
            {sent && <span className="small muted">{sent}</span>}
          </div>
          <ErrorBox error={error} />
        </div>
      )}
    </div>
  );
}

/** The run branch against its base: what landing brings in. Loaded when opened. */
export function RunChanges({ run }: { run: Run }) {
  const [open, setOpen] = useState(false);
  return (
    <details className="run-changes" onToggle={(e) => setOpen((e.target as HTMLDetailsElement).open)}>
      <summary className="small">
        查看全部改动（<code>{run.branch}</code> 对 <code>{run.base_branch}</code>）
      </summary>
      {open && <RunDiff run={run} />}
    </details>
  );
}

function RunDiff({ run }: { run: Run }) {
  const changes = useHubData(() => api.runDiff(run.id), [run.id], (e) => e.type === "task.status_changed" && e.data.run_id === run.id);
  const files = useMemo(() => (changes.data ? parseDiff(changes.data.diff) : []), [changes.data]);
  const [focus, setFocus] = useState<string>();
  if (changes.error) return <ErrorBox error={changes.error} />;
  if (!changes.data) return <p className="muted small">加载改动…</p>;
  return (
    <div className="review">
      <div className="small muted">
        {files.length} 个文件 <ChangeStat files={files} />
        {changes.data.truncated && <span className="chip warn" style={{ marginLeft: 6 }}>diff 太长，已截断</span>}
      </div>
      {files.length === 0 ? (
        <p className="muted small">run 分支上还没有合并进任何改动。</p>
      ) : (
        <>
          <FileList files={files} onPick={setFocus} />
          <DiffView files={files} focus={focus} />
        </>
      )}
    </div>
  );
}
