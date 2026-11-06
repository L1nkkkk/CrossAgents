import "./apply.css";
import { useEffect, useState } from "react";
import type { Run } from "@bindings/Run";
import { api, errorText, type ApplyReport, type IntegrationStatus } from "./api";

export function ApplyButton({ run }: { run: Run }) {
  const [status, setStatus] = useState<IntegrationStatus>();
  const [report, setReport] = useState<ApplyReport>();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const eligible = run.mode?.lead === "in_place" && run.mode?.vcs === "git";
  useEffect(() => {
    if (!eligible) return;
    let live = true;
    api.getIntegration(run.id).then(s => { if (live) setStatus(s); }).catch(e => { if (live) setError(errorText(e)); });
    return () => { live = false; };
  }, [eligible, run.id, run.updated_at]);
  if (!eligible) return null;
  const pending = status?.outcome === "ok" && status.integration !== status.applied && status.integration !== status.snapshot;
  if (status && !pending && !report && !error) return null;
  const reason = error || (!status ? "正在读取集成状态" : !pending ? "没有待应用的结果" : "");
  async function apply() {
    if (!window.confirm(`将 Worker 的结果应用到 ${run.repo}？`)) return;
    setBusy(true); setError("");
    try {
      const result = await api.applyRun(run.id);
      setReport(result);
      setStatus(await api.getIntegration(run.id));
    } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  }
  return <div className="apply-control">
    <button className="ghost small" disabled={busy || !pending} title={reason || "将 Worker 结果应用到原目录"} onClick={apply}>应用到原目录</button>
    {reason && !report && <span className="muted">{reason}</span>}
    {(report || error) && <div role="status" className="apply-report">
      {error && <p>{error}</p>}
      {report && <><strong>应用结果：{report.outcome}</strong>{report.reason && <p>{report.reason}</p>}{report.detail && <pre>{report.detail}</pre>}{!!report.files?.length && <p>文件：{report.files.join("、")}</p>}{report.note && <p>{report.note}</p>}{report.rerun && <><p>原目录复跑检查：</p>{report.rerun.error && <p>{report.rerun.error}</p>}{report.rerun.results.length === 0 && !report.rerun.error && <p>没有匹配的检查</p>}{report.rerun.results.map((r, i) => <div key={i}><strong>{r.passed ? "通过" : "失败"}：{r.command}</strong><pre>{r.output}</pre></div>)}</>}</>}
    </div>}
  </div>;
}
