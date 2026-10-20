import { useState } from "react";
import type { UsageGroup } from "@bindings/UsageGroup";
import { api } from "../api";
import { ErrorBox } from "../components";
import { useHubData } from "../hub";

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
    <div>
      <header className="page-header">
        <h1>用量</h1>
        <div className="segmented">
          {GROUPS.map((g) => (
            <button key={g.by} className={g.by === by ? "active" : ""} onClick={() => setBy(g.by)}>
              {g.label}
            </button>
          ))}
        </div>
      </header>
      <ErrorBox error={usage.error} />
      {rows && !rows.length && <p className="muted">还没有用量记录。</p>}
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
                <td>{by === "run" ? <a href={`#/runs/${row.key}`}>{row.key}</a> : row.key}</td>
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
    </div>
  );
}
