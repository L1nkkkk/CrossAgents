// Deleting a run (docs/tasks/R1.md §3): a hard delete, so the person sees what goes before it
// does. A run that has not ended is cancelled first. A checkout with work that never landed
// answers 409 with the files; the dialog lists them and offers to go ahead anyway.

import { useState } from "react";
import type { RunDetail } from "@bindings/RunDetail";
import { ACTIVE_RUN, navigate } from "./App";
import { ApiError, api, errorText } from "./api";
import { ErrorBox, agentName, useAgents } from "./components";

export function DeleteRunDialog({ detail, onClose }: { detail: RunDetail; onClose: () => void }) {
  const { run } = detail;
  const agents = useAgents();
  const lead = agentName(agents.data, run.planner);
  const live = ACTIVE_RUN.includes(run.status);
  const landed = !!run.landed;
  // A landed run's branches carry nothing the base branch lacks; an unlanded one's carry
  // everything, so they stay unless the person says so.
  const [branches, setBranches] = useState(landed);
  const [cancelled, setCancelled] = useState(false);
  const [files, setFiles] = useState<string[]>([]);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  async function remove(force: boolean) {
    setBusy(true);
    setError(undefined);
    try {
      if (live && !cancelled) {
        await api.cancelRun(run.id);
        setCancelled(true);
      }
      await api.deleteRun(run.id, { branches, force });
      const here = window.location.hash.startsWith(`#/runs/${run.id}`);
      onClose();
      if (here) navigate("");
    } catch (err) {
      setFiles(err instanceof ApiError ? err.files : []);
      setError(errorText(err));
      setBusy(false);
    }
  }

  return (
    <div className="dialog-backdrop" onMouseDown={onClose}>
      <div className="dialog" role="dialog" aria-modal="true" aria-label="删除 run" onMouseDown={(e) => e.stopPropagation()}>
        <h2>{live ? "取消并删除这个 run？" : "删除这个 run？"}</h2>
        <p className="setting-hint">
          删除是彻底的：run 的记录、任务、它起的 agent 和它的工作区都会消失。事件日志保留，可以按 run id 查。
        </p>
        <dl className="kv small">
          <dt>主导</dt>
          <dd>
            {lead}
            {!run.in_place && <span className="muted"> · 和 {lead} 的对话会一起删除</span>}
          </dd>
          <dt>工作区</dt>
          <dd className="mono" style={{ overflowWrap: "anywhere" }}>
            {run.repo}
          </dd>
          <dt>分支</dt>
          <dd>
            {run.in_place ? (
              <span className="muted">没有版本控制，直接改在工作目录里</span>
            ) : (
              <>
                <code>{run.branch}</code>
                <span className="muted"> · 基于 </span>
                <code>{run.base_branch}</code>
              </>
            )}
          </dd>
          <dt>落地</dt>
          <dd>
            {landed ? (
              <>
                已落地到 <code>{run.base_branch}</code>
                <span className="muted">（{run.landed!.reference}）</span>
              </>
            ) : run.in_place ? (
              <span className="muted">改动已直接写在工作目录里</span>
            ) : (
              <span className="chip warn" title="改动只在分支上，还没有进 base 分支">
                未落地
              </span>
            )}
          </dd>
        </dl>
        {!run.in_place && (
          <label className="row small" style={{ gap: 8 }}>
            <input type="checkbox" checked={branches} disabled={busy} onChange={(e) => setBranches(e.target.checked)} />
            <span>
              同时删除分支 <code>{run.branch}</code>
              {!landed && <span className="muted"> · 未落地的改动会丢失</span>}
            </span>
          </label>
        )}
        {files.length > 0 && (
          <div className="item-warn">
            <div>工作区里还有没提交的改动，删掉就没了：</div>
            <ul className="small" style={{ margin: "4px 0 0", paddingLeft: 18 }}>
              {files.map((file) => (
                <li key={file}>
                  <code>{file}</code>
                </li>
              ))}
            </ul>
          </div>
        )}
        {/* The files say it better than the hub's own sentence; anything else shows as it is. */}
        <ErrorBox error={files.length ? undefined : error} />
        <div className="row" style={{ justifyContent: "flex-end" }}>
          <button className="ghost" disabled={busy} onClick={onClose}>
            取消
          </button>
          {files.length > 0 ? (
            <button className="danger" disabled={busy} onClick={() => remove(true)}>
              仍然删除
            </button>
          ) : (
            <button className="danger" disabled={busy} onClick={() => remove(false)}>
              {live ? "取消并删除" : "删除"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
