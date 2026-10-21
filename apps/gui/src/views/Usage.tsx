import { useState } from "react";
import type { UsageGroup } from "@bindings/UsageGroup";
import { api } from "../api";
import { ErrorBox } from "../components";
import { useHubData } from "../hub";
import { QuotaSection } from "./Quota";

const GROUPS: { by: UsageGroup; label: string }[] = [
  { by: "role", label: "按角色" },
  { by: "model", label: "按模型" },
  { by: "run", label: "按 run" },
  { by: "agent", label: "按 agent" },
];

const usd = (n: number) => `$${n.toFixed(4)}`;

export function UsageView() {
  const [by, setBy] = useState<UsageGroup>("role");
  const usage = useHubData(() => api.usage(by), [by], (e) => e.type === "agent.usage");
  const rows = usage.data?.by === by ? usage.data.rows : undefined;

  return (
    <>
      <div className="topbar">
        <span className="title">用量</span>
      </div>
      <div className="page">
        <div className="page-inner settings">
          <QuotaSection />
          <section className="settings-section">
            <div className="settings-head">
              <h2>CrossAgents 里的花费</h2>
              {usage.data && <span className="chip accent">{usd(usage.data.total_cost_usd)}</span>}
              <div className="spacer" />
              <div className="segmented small">
                {GROUPS.map((g) => (
                  <button key={g.by} className={g.by === by ? "active" : ""} onClick={() => setBy(g.by)}>
                    {g.label}
                  </button>
                ))}
              </div>
            </div>
            <ErrorBox error={usage.error} />
            {rows && !rows.length && <div className="empty">还没有用量记录。</div>}
            {rows && rows.length > 0 && (
              <table className="table">
                <thead>
                  <tr>
                    <th>{by === "run" ? "run" : by}</th>
                    <th className="num">agents</th>
                    <th className="num">会话</th>
                    <th className="num">成本</th>
                    <th className="num">峰值上下文</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((row) => (
                    <tr key={row.key}>
                      <td>{by === "run" ? <a href={`#/runs/${row.key}`}>{row.key}</a> : by === "agent" ? <a href={`#/s/${encodeURIComponent(row.key)}`}>{row.key}</a> : row.key}</td>
                      <td className="num">{row.agents}</td>
                      <td className="num">{row.sessions}</td>
                      <td className="num">{row.cost_reported ? usd(row.cost_usd) : <span className="muted">未上报</span>}</td>
                      <td className="num">{row.peak_context.toLocaleString()}</td>
                    </tr>
                  ))}
                </tbody>
                <tfoot>
                  <tr>
                    <td>合计</td>
                    <td />
                    <td />
                    <td className="num">{usd(usage.data!.total_cost_usd)}</td>
                    <td />
                  </tr>
                </tfoot>
              </table>
            )}
          </section>
        </div>
      </div>
    </>
  );
}
