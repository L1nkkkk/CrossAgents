import { useState } from "react";
import type { UsageGroup } from "@bindings/UsageGroup";
import type { UsageSeriesGroup } from "@bindings/UsageSeriesGroup";
import type { UsageSeriesRange } from "@bindings/UsageSeriesRange";
import type { UsageSeriesDay } from "@bindings/UsageSeriesDay";
import { api } from "../api";
import { RankBars } from "../charts";
import { ErrorBox } from "../components";
import { useHubData } from "../hub";
import { compactUsage, comparison, smoothPath } from "../lib/usage";
import { QuotaSection } from "./Quota";
import "../usage.css";

const GROUPS: { by: UsageGroup; label: string }[] = [{by:"role",label:"按角色"},{by:"model",label:"按模型"},{by:"run",label:"按 run"},{by:"agent",label:"按 agent"}];
const groups: {value: UsageSeriesGroup; label: string}[] = [{value:"total",label:"总计"},{value:"model",label:"模型"},{value:"origin",label:"来源"},{value:"agent",label:"agent"},{value:"run",label:"run"}];
const ranges: {value: UsageSeriesRange; label: string}[] = [{value:"7d",label:"7 天"},{value:"30d",label:"30 天"},{value:"90d",label:"90 天"},{value:"all",label:"全部"}];
const usd = (n: number) => `$${n.toFixed(4)}`;
const money = (n: number | null) => n === null ? "—" : `$${n.toFixed(2)}`;
function Info({children}: {children: string}) { return <span className="usage-info"><button type="button" aria-label={children}>ℹ</button><span role="tooltip">{children}</span></span>; }
function AreaChart({days, stacked, cost}: {days: UsageSeriesDay[]; stacked: boolean; cost: boolean}) {
  const [hover, setHover] = useState<number>();
  const width=900, height=260, left=55, right=12, top=12, bottom=32, plot=height-top-bottom;
  const values=days.map(d => cost ? [d.cost_usd ?? 0] : [d.cached_input,d.new_input,d.output]);
  const max=Math.max(1,...values.map(v => stacked ? v.reduce((a,b)=>a+b,0) : Math.max(...v)));
  const x=(i:number)=>left+(width-left-right)*(days.length===1 ? .5 : i/(days.length-1));
  const y=(v:number)=>top+plot*(1-v/max);
  const paths=(cost ? [0] : [0,1,2]).map(k => {
    const upper=values.map((v,i)=>({x:x(i),y:y(stacked ? v.slice(0,k+1).reduce((a,b)=>a+b,0) : v[k])}));
    const lower=values.map((v,i)=>({x:x(i),y:y(stacked ? v.slice(0,k).reduce((a,b)=>a+b,0) : 0)})).reverse();
    return `${smoothPath(upper)} L${lower[0]?.x ?? left},${lower[0]?.y ?? y(0)} ${smoothPath(lower).replace(/^M/,"L")} Z`;
  });
  const every=Math.max(1,Math.ceil(days.length/7));
  return <div className="usage-chart-wrap"><svg className="usage-svg" viewBox={`0 0 ${width} ${height}`} role="group" aria-label="按天用量图">
    {[0,.25,.5,.75,1].map(f=><g key={f}><line className="chart-grid" x1={left} x2={width-right} y1={y(max*f)} y2={y(max*f)}/><text className="chart-tick" x={left-8} y={y(max*f)} textAnchor="end" dy=".32em">{cost ? `$${(max*f).toFixed(1)}` : compactUsage(max*f)}</text></g>)}
    {paths.map((d,k)=><path key={k} d={d} className={`usage-area usage-area-${k}`} />)}
    {days.map((d,i)=><g key={d.date}>{i%every===0 && <text className="chart-tick" x={x(i)} y={height-7} textAnchor="middle">{d.date.slice(5)}</text>}<rect x={x(i)-(width-left-right)/Math.max(2,days.length)/2} y={top} width={(width-left-right)/Math.max(1,days.length)} height={plot} fill="transparent" tabIndex={0} role="button" aria-label={`${d.date}，缓存读入 ${compactUsage(d.cached_input)}，新输入 ${compactUsage(d.new_input)}，输出 ${compactUsage(d.output)}，API 等价 ${money(d.cost_usd)}`} onFocus={()=>setHover(i)} onBlur={()=>setHover(undefined)} onMouseEnter={()=>setHover(i)} onMouseLeave={()=>setHover(undefined)}/></g>)}
  </svg>{hover!==undefined && days[hover] && <div className="usage-tip"><b>{days[hover].date}</b> · 缓存读入 {compactUsage(days[hover].cached_input)} · 新输入 {compactUsage(days[hover].new_input)} · 输出 {compactUsage(days[hover].output)} · API 等价 {money(days[hover].cost_usd)}</div>}</div>;
}
export function UsageView() {
  const [by,setBy]=useState<UsageGroup>("role");
  const [range,setRange]=useState<UsageSeriesRange>("7d");
  const [group,setGroup]=useState<UsageSeriesGroup>("total");
  const [source,setSource]=useState("all");
  const [stacked,setStacked]=useState(true);
  const [cost,setCost]=useState(false);
  const usage=useHubData(()=>api.usage(by),[by],e=>e.type==="agent.usage");
  const series=useHubData(()=>api.usageSeries(range,group,source),[range,group,source],e=>e.type==="agent.usage");
  const rows=usage.data?.by===by ? usage.data.rows : undefined;
  const data=series.data, total=data?.total, tokens=total ? total.cached_input+total.new_input+total.output : 0;
  const delta=data ? comparison(tokens,data.previous_total) : null;
  const errors=data?.sources.filter(s=>s.error) ?? [];
  return <><div className="topbar"><span className="title">用量</span></div><div className="page"><div className="page-inner settings usage-page">
    <div className="usage-controls"><select aria-label="来源" value={source} onChange={e=>setSource(e.target.value)}><option value="all">全部来源</option>{data?.sources.map(s=><option key={s.id} value={s.id}>{s.error ? "⚠ " : ""}{s.label}</option>)}</select><div className="segmented small">{groups.map(g=><button key={g.value} className={group===g.value?"active":""} onClick={()=>setGroup(g.value)}>{g.label}</button>)}</div><div className="segmented small">{ranges.map(r=><button key={r.value} className={range===r.value?"active":""} onClick={()=>setRange(r.value)}>{r.label}</button>)}</div></div>
    {errors.length>0 && <details className="usage-errors"><summary>部分来源读取失败（{errors.length}）</summary>{errors.map(s=><div key={s.id}>{s.label}：{s.error}</div>)}</details>}
    <ErrorBox error={series.error}/>
    {series.loading && <div className="usage-skeleton" aria-label="正在加载用量"/>}
    {data && total && <><div className="usage-metrics">
      <div><span>Token</span><strong>{compactUsage(tokens)}</strong>{delta && <em>{delta} 较前 {range === "7d" ? "7 天" : range === "30d" ? "30 天" : "90 天"}</em>}<small>输入 {compactUsage(total.cached_input+total.new_input)} · 输出 {compactUsage(total.output)}</small></div>
      <div><span>缓存命中</span><strong>{total.cached_input+total.new_input ? Math.round(total.cached_input/(total.cached_input+total.new_input)*100) : 0}%</strong><small>{compactUsage(total.cached_input)} 来自缓存</small></div>
      <div><span>{total.turns===null ? "会话" : "轮数"}</span><strong>{compactUsage(total.turns ?? total.sessions)}</strong><small>日均 {((total.turns ?? total.sessions)/Math.max(1,data.days.length)).toFixed(1)}</small></div>
      <div><span>最大一次 <Info>{total.max_request===null ? data.notes.join("；") : "单次请求的输入与输出 Token 之和"}</Info></span><strong>{total.max_request===null ? "—" : compactUsage(total.max_request)}</strong></div>
      <div><span>API 等价 <Info>{`按公开 API 价折算的估算${data.unpriced_models.length ? `；未计价模型：${data.unpriced_models.join("、")}` : ""}`}</Info></span><strong>{money(total.cost_usd)}</strong></div>
    </div>{!data.first_day ? <div className="usage-empty">暂无用量数据。数据来自各厂商的本地会话日志。</div> : <section className="usage-card"><div className="usage-chart-head"><div className="usage-legend">{(cost ? ["API 等价"] : ["缓存读入","新输入","输出"]).map((label,i)=><span key={label}><i className={`usage-dot usage-dot-${i}`}/>{label}</span>)}</div><div className="segmented small"><button className={!stacked?"active":""} onClick={()=>setStacked(false)}>曲线</button><button className={stacked?"active":""} onClick={()=>setStacked(true)}>堆叠</button></div><div className="segmented small"><button className={!cost?"active":""} onClick={()=>setCost(false)}>Token</button><button className={cost?"active":""} onClick={()=>setCost(true)}>API 等价</button></div></div><AreaChart days={data.days} stacked={stacked} cost={cost}/>{group!=="total" && <RankBars rows={data.groups.map(g=>({key:g.key,label:g.label,value:cost ? g.cost_usd ?? 0 : g.cached_input+g.new_input+g.output}))} format={cost ? n=>money(n) : compactUsage}/>}</section>}{data.notes.map((note,i)=><p className="usage-note" key={i}>{note}</p>)}</>}
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
            {rows && rows.some((r) => r.cost_reported && r.cost_usd > 0) && (
              <RankBars rows={rows.filter((r) => r.cost_reported).map((r) => ({ key: r.key, label: r.key, value: r.cost_usd }))} format={(n) => `$${n.toFixed(n >= 10 ? 1 : 2)}`} />
            )}
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
        </div></div></>;
}
