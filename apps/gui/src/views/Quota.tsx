// Account quota at a glance, with management and usage history on demand.

import { useEffect, useRef, useState, type FormEvent } from "react";
import type { AccountQuota } from "@bindings/AccountQuota";
import type { AccountUsageGroup } from "@bindings/AccountUsageGroup";
import type { ImportableAccount } from "@bindings/ImportableAccount";
import type { LoginStarted } from "@bindings/LoginStarted";
import type { ModelInfo } from "@bindings/ModelInfo";
import type { RateWindow } from "@bindings/RateWindow";
import type { ResetCredit } from "@bindings/ResetCredit";
import type { TokenTotals } from "@bindings/TokenTotals";
import { api, errorText } from "../api";
import { DayColumns, RankBars, compact, dailySeries } from "../charts";
import { ErrorBox, Field, Icon, VendorMark, ago, vendorName } from "../components";
import { useHubData } from "../hub";
import { creditText, egressOf, level, planLabel, resetText, windowLabel, type Level } from "../lib/quota";
import { prettyModel } from "../lib/models";
import { cursorMode, usesVariantReasoning } from "../lib/reasoning";
import { EgressPicker } from "../egressPicks";
import { useConnectionState } from "../lib/useConnection";
import { useArmed } from "../parts";

/** Live figures are cached by the hub for a minute; re-read about as often. */
const REFRESH_MS = 60_000;

function quotaWindowActive() {
  return document.visibilityState === "visible" && document.hasFocus();
}

export function useQuota() {
  // Even the sidebar's quota summary can start vendor CLIs through the hub.
  const quota = useHubData(() => api.quota(), [], (e) => e.type.startsWith("account."), quotaWindowActive);
  useEffect(() => {
    const reload = quota.reload;
    const timer = setInterval(reload, REFRESH_MS);
    window.addEventListener("focus", reload);
    document.addEventListener("visibilitychange", reload);
    return () => {
      clearInterval(timer);
      window.removeEventListener("focus", reload);
      document.removeEventListener("visibilitychange", reload);
    };
  }, [quota.reload]);
  return quota;
}

type ViewLayout = "deck" | "list";
type MeterMode = "remain" | "used";
type AccountHealth = Level | "unknown";

const HEALTH_LABEL: Record<AccountHealth, string> = {
  ok: "充足",
  warn: "偏紧",
  danger: "需关注",
  unknown: "暂无额度数据",
};

function accountHealth({ status, at_risk, egress_block }: AccountQuota): AccountHealth {
  if (!status.logged_in || at_risk || egress_block || status.limit_reached) return "danger";
  const windows = [...status.rate_limits, ...status.limits.slice(1).flatMap((g) => g.windows)];
  return windows.length ? level(Math.max(...windows.map((w) => w.used_percent))) : "unknown";
}

export function QuotaSection() {
  const quota = useQuota();
  const [adding, setAdding] = useState(false);
  const [open, setOpen] = useState<string>();
  const [layout, setLayout] = useState<ViewLayout>("deck");
  const [meterMode, setMeterMode] = useState<MeterMode>("remain");
  const [statusFilter, setStatusFilter] = useState<AccountHealth | "all">("all");
  const [vendorFilter, setVendorFilter] = useState("all");
  const accounts = quota.data?.accounts ?? [];
  const vendors = [...new Set(accounts.map(({ account }) => account.provider))];
  const vendor = vendors.includes(vendorFilter) ? vendorFilter : "all";
  const filtered = accounts.filter((a) => vendor === "all" || a.account.provider === vendor);
  const counts = { ok: 0, warn: 0, danger: 0, unknown: 0 };
  for (const a of filtered) counts[accountHealth(a)]++;
  const visible = filtered.filter((a) => statusFilter === "all" || accountHealth(a) === statusFilter);

  return (
    <section className="settings-section quota-section">
      <div className="settings-head quota-header">
        <h2>订阅与额度</h2>
        {accounts.length > 0 && <span className="muted small">{accounts.length} 个账号</span>}
        <div className="spacer" />
        {accounts.length > 0 && (
          <div className="segmented small" role="group" aria-label="账号视图">
            <button className={layout === "deck" ? "active" : ""} aria-pressed={layout === "deck"} onClick={() => setLayout("deck")}>卡片</button>
            <button className={layout === "list" ? "active" : ""} aria-pressed={layout === "list"} onClick={() => setLayout("list")}>列表</button>
          </div>
        )}
        {!adding && <button className="primary" onClick={() => setAdding(true)}>添加账号</button>}
      </div>
      <ErrorBox error={quota.error} />
      {adding && <AddAccount providers={quota.data?.providers ?? []} onDone={() => (setAdding(false), quota.reload())} />}
      {quota.data && !accounts.length && !adding && (
        <p className="muted">还没有账号。登录过 Claude Code、Codex 或 CodeBuddy 的话，hub 启动时会自动把本机登录加进来；也可以手动添加。</p>
      )}
      {accounts.length > 0 && (
        <>
          <div className="quota-filters">
            {vendors.length > 1 && (
              <select aria-label="按服务筛选" value={vendor} onChange={(e) => setVendorFilter(e.target.value)}>
                <option value="all">全部服务</option>
                {vendors.map((v) => <option key={v} value={v}>{vendorName(v)}</option>)}
              </select>
            )}
            <select aria-label="按额度状态筛选" value={statusFilter} onChange={(e) => setStatusFilter(e.target.value as AccountHealth | "all")}>
              <option value="all">全部状态（{filtered.length}）</option>
              {(Object.keys(counts) as AccountHealth[]).map((key) => (
                <option key={key} value={key}>{HEALTH_LABEL[key]}（{counts[key]}）</option>
              ))}
            </select>
            <div className="spacer" />
            <div className="segmented small" role="group" aria-label="额度显示方式">
              <button className={meterMode === "remain" ? "active" : ""} aria-pressed={meterMode === "remain"} onClick={() => setMeterMode("remain")}>剩余</button>
              <button className={meterMode === "used" ? "active" : ""} aria-pressed={meterMode === "used"} onClick={() => setMeterMode("used")}>已用</button>
            </div>
          </div>

          {visible.length > 0 ? (
            <div className={`accounts accounts-${layout}`}>
              {visible.map((a) => (
                <AccountRow
                  key={a.account.id}
                  quota={a}
                  color={seriesColor(accounts.indexOf(a))}
                  meterMode={meterMode}
                  open={open === a.account.id}
                  onToggle={() => setOpen(open === a.account.id ? undefined : a.account.id)}
                  onChange={quota.reload}
                />
              ))}
            </div>
          ) : <p className="muted quota-no-match">没有匹配的账号。</p>}
          <AllUsage accounts={accounts} />
        </>
      )}
    </section>
  );
}

/** Account i's colour in every chart: categorical slot i, in list order. */
const seriesColor = (i: number) => `var(--series-${(i % 8) + 1})`;


/** Cursor lists every mode of a model as its own ID; keep the first of each base model. */
function byBase(models: readonly ModelInfo[]): ModelInfo[] {
  const seen = new Set<string>();
  return models.filter((m) => {
    const base = cursorMode(m.id).base;
    if (seen.has(base)) return false;
    seen.add(base);
    return true;
  });
}

function QuotaMeter({ window: w, reached, mode, group }: { window: RateWindow; reached: boolean; mode: MeterMode; group?: string }) {
  const tone = level(w.used_percent, reached && w.used_percent >= 100);
  const used = Math.max(0, Math.min(100, Math.round(w.used_percent)));
  const value = mode === "used" ? used : 100 - used;
  const label = windowLabel(w).replace(/限额$/, "") || windowLabel(w);
  const name = group && group !== label ? `${group} · ${label}` : label;
  const reset = resetText(w.resets_at);
  const pool = w.total != null && w.remaining != null
    ? `${creditNumber(mode === "used" ? w.total - w.remaining : w.remaining)} / ${creditNumber(w.total)}`
    : undefined;
  const amount = `${mode === "used" ? "已用" : "剩余"} ${pool ?? `${value}%`}`;

  return (
    <div className="account-meter">
      <div className="account-meter-heading">
        <span className="account-meter-name">{name}</span>
        <span className={`account-meter-value ${tone}`}>{amount}</span>
      </div>
      <div className={`quota-bar account-meter-bar ${tone}`} role="progressbar" aria-label={name} aria-valuenow={value} aria-valuetext={amount}>
        <span style={{ width: `${value}%` }} />
      </div>
      {reset && <span className="account-meter-reset" title={w.resets_at ?? undefined}>{reset}</span>}
    </div>
  );
}
function AccountRow({
  quota,
  color,
  meterMode,
  open,
  onToggle,
  onChange,
}: {
  quota: AccountQuota;
  color: string;
  meterMode: MeterMode;
  open: boolean;
  onToggle: () => void;
  onChange: () => void;
}) {
  const { account, status, at_risk: atRisk, egress_block: blocked } = quota;
  const [login, setLogin] = useState<LoginStarted>();
  const [error, setError] = useState<string>();
  const remove = useArmed();
  const name = account.label || account.id;
  const plan = planLabel(status.plan);
  const credits = status.credits;
  const balance = credits && (credits.has_credits || credits.unlimited)
    ? credits.unlimited ? "不限" : creditText(credits.balance) ?? "—"
    : null;
  const windows = status.rate_limits;
  const extraGroups = status.limits.slice(1).filter((g) => g.windows.length > 0);
  const hasWindows = windows.length > 0 || extraGroups.length > 0;
  const cursor = usesVariantReasoning(account.provider);
  const models = cursor ? byBase(quota.models) : quota.models;
  const egress = egressOf(quota.egress);
  const health = accountHealth(quota);
  const stateLabel = atRisk ? "风控暂停" : blocked ? "出口待确认" : !status.logged_in ? "未登录"
    : status.limit_reached ? "已达限额" : health === "danger" ? "快用完" : health === "warn" ? "偏紧" : undefined;
  const detailId = `quota-detail-${account.id}`;

  const wasLoggedIn = useRef(status.logged_in);
  useEffect(() => {
    if (status.logged_in && !wasLoggedIn.current) setLogin(undefined);
    wasLoggedIn.current = status.logged_in;
  }, [status.logged_in]);

  async function startLogin() {
    setError(undefined);
    try {
      setLogin(await api.login(account.id));
    } catch (err) {
      setError(errorText(err));
    }
  }

  return (
    <article className={`account-row ${open ? "open" : ""} ${atRisk ? "is-risk" : blocked ? "is-blocked" : ""}`}>
      <div className="account-summary">
        <header className="account-heading">
          <span className="account-vendor" style={{ borderColor: color }}>
            <VendorMark plugin={account.provider} size={19} />
          </span>
          <div className="account-identity">
            <h3 className="account-name" title={name}>{name}</h3>
            <p className="account-meta">
              {vendorName(account.provider)}
              {account.codebuddy_site && ` · ${account.codebuddy_site === "china" ? "国内站" : "国际站"}`}
              {plan && ` · ${plan}`}
            </p>
          </div>
          {stateLabel && <span className={`health ${blocked && !atRisk ? "warn" : health}`}>{stateLabel}</span>}
        </header>

        {status.logged_in && hasWindows && (
          <div className="account-meters">
            {windows.map((w) => <QuotaMeter key={w.name} window={w} reached={status.limit_reached} mode={meterMode} />)}
            {extraGroups.map((g) => g.windows.map((w) => (
              <QuotaMeter key={`${g.id}-${w.name}`} window={w} reached={g.reached} mode={meterMode} group={g.name ?? g.model ?? g.id} />
            )))}
          </div>
        )}
        {status.logged_in && !hasWindows && (
          <div className="account-missing">
            <span>{status.note ? "额度暂不可用" : "暂无额度数据"}</span>
            {models.length > 0 && <span className="muted small">{models.length} 个可用模型</span>}
          </div>
        )}
        {status.logged_in && balance != null && (
          <div className="account-balance" title="限额用完后从额外额度扣费">
            <span>额外额度</span><strong>{balance}</strong>
          </div>
        )}

        <footer className="account-summary-footer">
          <span className="muted small">{quota.agents.length} 个智能体</span>
          <button
            className="ghost small account-who"
            type="button"
            aria-expanded={open}
            aria-controls={open ? detailId : undefined}
            onClick={onToggle}
          >
            {open ? "收起详情" : "账号详情"}
            <span className={`disclose ${open ? "open" : ""}`}><Icon name="chevron" size={13} /></span>
          </button>
        </footer>
      </div>

      {open && (
        <div className="account-detail" id={detailId}>
          <div className="account-detail-heading">
            <h4>账号管理</h4>
            <button
              className="ghost small"
              onClick={startLogin}
              disabled={!!atRisk}
              title={atRisk ? "账号因风控暂停，请先完成厂商验证，再手动解除暂停" : undefined}
            >
              {status.logged_in ? "重新登录" : "登录"}
            </button>
            <button
              className={`ghost small danger ${remove.armed ? "armed" : ""}`}
              onClick={() => remove.click(() => api.deleteAccount(account.id).then(onChange, (err) => setError(errorText(err))))}
              title="从 CrossAgents 移除账号，保留账号目录里的文件"
            >
              {remove.armed ? "再点一次移除" : "移除"}
            </button>
          </div>
          {atRisk && (
            <div className="account-alert danger">
              <span className="account-alert-title">
                <span aria-hidden="true">✕</span> 风控暂停
              </span>
              <span className="account-alert-text">{atRisk}</span>
              <span className="account-alert-hint">
                CrossAgents 不会自动重试这个账号——重试只会让厂家更怀疑。要恢复：先在厂家那边完成身份验证或人机校验，再由人手动解除暂停；界面目前还没有解除按钮。
                {quota.agents.length > 0 && ` 绑在它上面的 ${quota.agents.length} 个智能体已经停下。`}
              </span>
            </div>
          )}
          {blocked && (
            <div className="account-alert warn">
              <span className="account-alert-title">
                <span aria-hidden="true">!</span> 出口变了 · 等你确认
              </span>
              <span className="account-alert-text">{blocked}</span>
              <span className="account-alert-hint">确认前不会用这个出口启动会话。在通知里选「允许」，再重新启动就能解开。</span>
            </div>
          )}

          {login && (
            <div className="notice">
              {login.manual ? (
                <>
                  这个账号要在终端里登录：
                  {login.manual.map((step, i) => (
                    <span key={step}>
                      {i === 0 ? "运行 " : "，再输入 "}
                      <code>{step}</code>
                    </span>
                  ))}
                  。登录好后这里会在一分钟内变成已登录。
                </>
              ) : login.url ? (
                <>
                  在浏览器里完成登录：
                  <a href={login.url} target="_blank" rel="noreferrer">
                    打开登录页
                  </a>
                  ，登录好后这里会自动变成已登录。
                </>
              ) : (
                <span title={login.command}>
                  已在你的浏览器里打开授权页，完成授权后这里会自动变成已登录。
                </span>
              )}
            </div>
          )}
          <ErrorBox error={error} />
          {status.email && <p className="account-detail-note">{status.email}</p>}
          <div className={`account-egress ${egress.route}`}>
            <span className="account-egress-label" title="最后一次确认到的出口；采集不到时不写新记录，所以它可能来自上一次启动，而不是此刻正在用的">出口</span>
            {egress.known ? (
              <>
                <code className="egress-value" title="出口 IP">{egress.ip}</code>
                <span className="egress-value" title="国家">{egress.country}</span>
                <span className="egress-value muted egress-asn" title="ASN">{egress.asn}</span>
                <span className={`egress-route ${egress.route}`} title={egress.route === "unknown" ? "代理出口无法确认：allow_direct 可能回退直连，NO_PROXY 也可能绕过代理" : undefined}>
                  {egress.routeLabel}
                </span>
                <span className="muted egress-time" title={egress.changed ? `上次变化 ${egress.changed}` : "上次变化时间未知"}>
                  {egress.changed ?? "时间未知"}
                </span>
              </>
            ) : (
              <span className="muted" title="还没有记录到出口；下一次经代理启动会话时会采一次。">未记录（下次启动采集）</span>
            )}
            <EgressPicker
              value={account.proxy ?? null}
              defaultLabel="默认代理"
              onChange={(next) => {
                setError(undefined);
                api
                  .setAccountEgress(account.id, { proxy: next })
                  .then(onChange, (err) => setError(errorText(err)));
              }}
            />
          </div>
          {quota.agents.length > 0 && (
            <div className="account-members">
              <h4>绑定的智能体</h4>
              <div className="models">
                {quota.agents.map((agent) => <span key={agent} className="chip">@{agent}</span>)}
              </div>
            </div>
          )}
          <ResetCredits credits={status.reset_credits} />
          {models.length > 0 && (
            <details className="account-models">
              <summary>{models.length} 个可用模型</summary>
              <div className="models">
                {models.map((m) => (
                  <span key={m.id} className="chip" title={m.description ?? undefined}>
                    {cursor ? prettyModel(cursorMode(m.id).base) : m.name || m.id}
                    {m.context_window ? ` · ${Math.round(m.context_window / 1000)}k` : ""}
                    {m.credit_multiplier != null && <span className="muted"> · ×{m.credit_multiplier}</span>}
                  </span>
                ))}
              </div>
            </details>
          )}
          <div className="account-foot">
            <span>
              {status.source === "live" ? "实时" : status.source === "snapshot" ? "来自本地会话记录" : "暂无额度数据"}
              {status.as_of ? ` · ${ago(status.as_of)}更新` : ""}
            </span>
            {status.note && <p className="account-detail-note">{status.note}</p>}
          </div>
          <TokenUsage id={account.id} />
        </div>
      )}
    </article>
  );
}

/** Every account's tokens per day in one chart, stacked by account in the accounts' colours. */
function AllUsage({ accounts }: { accounts: AccountQuota[] }) {
  const [metric, setMetric] = useState<Metric>("output");
  const ids = accounts.map((a) => a.account.id);
  const usage = useHubData(() => Promise.all(ids.map((id) => api.accountUsage(id, "day"))), [ids.join(",")], (e) => e.type === "agent.usage");
  const metrics = metricsFor(usage.data?.map((u) => u.total) ?? []);
  const m = metrics.find((x) => x.key === metric) ?? metrics[0];
  const series = accounts.map((a, i) => ({ key: a.account.id, label: `${vendorName(a.account.provider)} · ${a.account.label || a.account.id}`, color: seriesColor(i) }));
  const data = usage.data ? dailySeries(usage.data.map((u) => u.rows.map((r) => ({ day: r.key, value: m.of(r.totals) })))) : [];
  const total = usage.data?.reduce((s, u) => s + m.of(u.total), 0) ?? 0;
  return (
    <div className="all-usage">
      <div className="token-usage-bar">
        <h3>全部账号 · Token 用量</h3>
        {usage.data && <span className="chip">{compact(total)}</span>}
        <div className="spacer" />
        <div className="segmented small" aria-label="图上显示">
          {metrics.map((x) => (
            <button key={x.key} className={x.key === m.key ? "active" : ""} onClick={() => setMetric(x.key)}>
              {x.label}
            </button>
          ))}
        </div>
      </div>
      <ErrorBox error={usage.error} />
      {usage.data && !data.length && <p className="muted small">没有会话记录。</p>}
      {data.length > 0 && <DayColumns data={data} series={series} format={(n) => n.toLocaleString()} label={`每天的${m.label}`} />}
    </div>
  );
}

/** "额度重置券 · 3 张可用 · 最早 11月21日 过期", the titles on hover. */
function ResetCredits({ credits }: { credits: ResetCredit[] }) {
  const available = credits.filter((c) => c.status === "available");
  if (!available.length) return null;
  const soonest = available
    .map((c) => c.expires_at)
    .filter((d): d is string => !!d)
    .sort()[0];
  const date = soonest ? new Date(soonest) : undefined;
  return (
    <div className="account-balance" title={available.map((c) => `${c.title}${c.description ? ` — ${c.description}` : ""}`).join("\n")}>
      <div>
        额度重置券
        <div className="quota-sub">用一张可以把限额清零，在 Codex 里使用</div>
      </div>
      <strong>
        {available.length} 张可用{date ? ` · 最早 ${date.getMonth() + 1}月${date.getDate()}日过期` : ""}
      </strong>
    </div>
  );
}


const USAGE_GROUPS: { by: AccountUsageGroup; label: string }[] = [
  { by: "day", label: "按天" },
  { by: "model", label: "按模型" },
  { by: "agent", label: "按 agent" },
  { by: "origin", label: "按来源" },
];

type Metric = "input" | "output" | "reasoning" | "sessions" | "credits";
const METRICS: { key: Metric; label: string; of: (t: TokenTotals) => number }[] = [
  { key: "output", label: "输出", of: (t) => t.output_tokens },
  { key: "input", label: "输入", of: (t) => t.input_tokens },
  { key: "reasoning", label: "推理", of: (t) => t.reasoning_output_tokens },
  { key: "sessions", label: "会话", of: (t) => t.sessions },
  { key: "credits", label: "积分", of: (t) => t.credits ?? 0 },
];

/** The metrics worth offering: 积分 only where some vendor billed in credits (CodeBuddy). */
const metricsFor = (totals: TokenTotals[]) => (totals.some((t) => (t.credits ?? 0) > 0) ? METRICS : METRICS.filter((m) => m.key !== "credits"));

/** "466", "1,234.5": credits as people read them. */
const creditNumber = (x: number) => x.toLocaleString(undefined, { maximumFractionDigits: 2 });

/** Tokens this login spent, from the vendor's own session logs (also outside CrossAgents). */
function TokenUsage({ id }: { id: string }) {
  const [by, setBy] = useState<AccountUsageGroup>("day");
  const [metric, setMetric] = useState<Metric>("output");
  const [table, setTable] = useState(false);
  const usage = useHubData(() => api.accountUsage(id, by), [id, by], (e) => e.type === "agent.usage");
  const rows = usage.data?.by === by ? usage.data.rows : undefined;
  const n = (x: number) => x.toLocaleString();
  const metrics = metricsFor(rows?.map((r) => r.totals) ?? []);
  const credits = metrics.length === METRICS.length;
  const m = metrics.find((x) => x.key === metric) ?? metrics[0];
  return (
    <div className="token-usage">
      <div className="token-usage-bar">
        <div className="segmented small">
          {USAGE_GROUPS.map((g) => (
            <button key={g.by} className={g.by === by ? "active" : ""} onClick={() => setBy(g.by)}>
              {g.label}
            </button>
          ))}
        </div>
        <div className="spacer" />
        <div className="segmented small" aria-label="图上显示">
          {metrics.map((x) => (
            <button key={x.key} className={x.key === m.key ? "active" : ""} onClick={() => setMetric(x.key)}>
              {x.label}
            </button>
          ))}
        </div>
      </div>
      <ErrorBox error={usage.error} />
      {rows && !rows.length && <p className="muted small">没有会话记录。</p>}
      {rows && rows.length > 0 &&
        (by === "day" ? (
          <DayColumns
            data={dailySeries([rows.map((r) => ({ day: r.key, value: m.of(r.totals) }))])}
            series={[{ key: id, label: m.label, color: "var(--accent)" }]}
            format={n}
            label={`每天的${m.label}`}
          />
        ) : (
          <RankBars rows={rows.map((r) => ({ key: r.key, label: r.key, value: m.of(r.totals) }))} format={compact} />
        ))}
      {rows && rows.length > 0 && (
        <button className="ghost small table-toggle" onClick={() => setTable(!table)} aria-expanded={table}>
          {table ? "收起明细" : "查看明细"}
        </button>
      )}
      {rows && rows.length > 0 && table && (
        <table className="table compact">
          <thead>
            <tr>
              <th />
              <th className="num">会话</th>
              <th className="num">输入</th>
              <th className="num">缓存命中</th>
              <th className="num">输出</th>
              <th className="num">推理</th>
              {credits && <th className="num">积分</th>}
            </tr>
          </thead>
          <tbody>
            {rows.slice(0, 30).map((row) => (
              <tr key={row.key}>
                <td>{row.key}</td>
                <td className="num">{row.totals.sessions}</td>
                <td className="num">{n(row.totals.input_tokens)}</td>
                <td className="num">{Math.round(row.cache_hit_rate * 100)}%</td>
                <td className="num">{n(row.totals.output_tokens)}</td>
                <td className="num">{n(row.totals.reasoning_output_tokens)}</td>
                {credits && <td className="num">{creditNumber(row.totals.credits ?? 0)}</td>}
              </tr>
            ))}
          </tbody>
          <tfoot>
            <tr>
              <td>合计</td>
              <td className="num">{usage.data!.total.sessions}</td>
              <td className="num">{n(usage.data!.total.input_tokens)}</td>
              <td className="num">{Math.round(usage.data!.total_cache_hit_rate * 100)}%</td>
              <td className="num">{n(usage.data!.total.output_tokens)}</td>
              <td className="num">{n(usage.data!.total.reasoning_output_tokens)}</td>
              {credits && <td className="num">{creditNumber(usage.data!.total.credits ?? 0)}</td>}
            </tr>
          </tfoot>
        </table>
      )}
    </div>
  );
}

function AddAccount({ providers, onDone }: { providers: string[]; onDone: () => void }) {
  const connection = useConnectionState();
  const isRemote = connection?.mode === "remote";
  const [provider, setProvider] = useState("");
  const [id, setId] = useState("");
  const [label, setLabel] = useState("");
  const [selectedSource, setSource] = useState<"local" | "new" | "copy">("new");
  const [site, setSite] = useState<"china" | "international">("china");
  const [candidates, setCandidates] = useState<ImportableAccount[]>();
  const [confirmed, setConfirmed] = useState(false);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const chosen = provider || providers[0] || "";
  const sharedLogin = chosen === "antigravity";
  const source = sharedLogin ? "local" : selectedSource;
  const candidate = candidates?.find((c) => c.provider === chosen);

  // The logins this machine has are only needed once copying is picked.
  useEffect(() => {
    if (source !== "copy" || candidates) return;
    let live = true;
    api.importCandidates().then(
      (found) => live && setCandidates(found.candidates),
      (err) => live && setError(errorText(err)),
    );
    return () => {
      live = false;
    };
  }, [source, candidates]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      // A copy gets its own home; the original login file is left as it is.
      if (source === "copy") {
        await api.importAccount({ provider: chosen, id: id.trim(), label: label.trim() || null, confirm: confirmed });
        onDone();
        return;
      }
      const account = await api.createAccount({
        codebuddy_site: chosen === "codebuddy" && source === "new" ? site : null,
        id: id.trim(),
        provider: chosen,
        label: label.trim() || null,
        home: null,
        import_default: source === "local",
      });
      if (chosen === "codebuddy" && source === "new" && account.codebuddy_site !== site) {
        await api.deleteAccount(account.id);
        throw new Error("当前 hub 不支持独立站点登录；请同时更新 hub 和 GUI。此次未启动登录，刚登记的账号已撤回。");
      }
      // A fresh login home starts logged out: open the login right away.
      if (source === "new") {
        const started = await api.login(account.id);
        if (started.url) window.open(started.url, "_blank");
      }
      onDone();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="card form" onSubmit={submit}>
      {isRemote && (
        <div className="notice">
          当前处于远程模式：这里管理的是<strong>中心电脑</strong>上的账号（需要 admin 权限）。要使用当前这台电脑本地已登录的 Codex / Claude，连上中心后会自动作为执行节点接入，不需要在这里添加账号。
        </div>
      )}
      <div className="row" style={{ alignItems: "flex-start" }}>
        <Field label="服务">
          <select
            value={chosen}
            onChange={(e) => {
              setProvider(e.target.value);
              setConfirmed(false);
            }}
          >
            {providers.map((p) => (
              <option key={p}>{p}</option>
            ))}
          </select>
        </Field>
        <Field label="账号 id（小写）">
          <input value={id} onChange={(e) => setId(e.target.value)} required placeholder={`${chosen}-${source === "copy" ? "import" : "work"}`} />
        </Field>
        <Field label="显示名">
          <input value={label} onChange={(e) => setLabel(e.target.value)} placeholder="工作账号" />
        </Field>
      </div>
      <div className="choice">
        {!sharedLogin && <label>
          <input type="radio" checked={source === "new"} onChange={() => setSource("new")} /> 新的独立登录（会打开浏览器登录）
        </label>}
        <label>
          <input type="radio" checked={source === "local"} onChange={() => setSource("local")} /> 使用本机已有的登录（与本机共用）
        </label>
        {!sharedLogin && <label>
          <input type="radio" checked={source === "copy"} onChange={() => setSource("copy")} /> 从本机登录复制一份到独立目录
        </label>}
      </div>
      {sharedLogin && <p className="muted small">Antigravity 的登录保存在系统钥匙串中，与本机 CLI 共用。添加后可点击「登录」或「重新登录」；重新登录会同时切换本机 CLI 使用的账号。</p>}
      {source === "copy" && (
        <div className="choice">
          {!candidates ? (
            <p className="muted small">正在查找本机的 Codex 与 Claude 登录…</p>
          ) : !candidate ? (
            <p className="muted small">
              本机没有找到 {chosen} 的登录。可以导入 Codex（~/.codex/auth.json）与 Claude（~/.claude/.credentials.json）的登录。
            </p>
          ) : (
            <>
              <p className="small">
                找到 <code>{candidate.source}</code>，将复制到 CrossAgents 的账号目录。
              </p>
              <div className="notice">{candidate.warning}</div>
              <label className="check">
                <input type="checkbox" checked={confirmed} onChange={(e) => setConfirmed(e.target.checked)} /> 我知道了，仍然导入
              </label>
            </>
          )}
        </div>
      )}
      {chosen === "codebuddy" && source === "new" && (
        <Field label="CodeBuddy 站点">
          <select value={site} onChange={(e) => setSite(e.target.value as "china" | "international")}>
            <option value="china">国内站</option>
            <option value="international">国际站</option>
          </select>
          <span className="muted">两个站点请分别添加账号。登录页中选择相应站点完成登录，之后可分别绑定给队友。</span>
        </Field>
      )}
      <ErrorBox error={error} />
      <div className="row">
        <button type="submit" className="primary" disabled={busy || !id.trim() || !chosen || (source === "copy" && (!candidate || !confirmed))}>
          添加
        </button>
        <button type="button" className="ghost" onClick={onDone}>
          取消
        </button>
      </div>
    </form>
  );
}
