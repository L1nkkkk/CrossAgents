// Subscriptions and their quota, laid out like Claude Code's usage settings: one block per
// account, a bar per rate-limit window with "N% 已用" and when it resets. Vendor-neutral: every
// provider's AccountStatus renders the same way.

import { useEffect, useRef, useState, type FormEvent } from "react";
import type { AccountQuota } from "@bindings/AccountQuota";
import type { AccountUsageGroup } from "@bindings/AccountUsageGroup";
import type { LoginStarted } from "@bindings/LoginStarted";
import type { ModelInfo } from "@bindings/ModelInfo";
import type { RateWindow } from "@bindings/RateWindow";
import type { ResetCredit } from "@bindings/ResetCredit";
import type { TokenTotals } from "@bindings/TokenTotals";
import { api, errorText } from "../api";
import { DayColumns, RankBars, compact, dailySeries } from "../charts";
import { ErrorBox, Field, Icon, VendorMark, ago, vendorName } from "../components";
import { useHubData } from "../hub";
import { creditText, level, planLabel, resetText, windowLabel, type Level } from "../lib/quota";
import { prettyModel } from "../lib/models";
import { cursorMode } from "../lib/reasoning";
import { useArmed } from "../parts";

/** Live figures are cached by the hub for a minute; re-read about as often. */
const REFRESH_MS = 60_000;

export function useQuota() {
  const quota = useHubData(() => api.quota(), [], (e) => e.type.startsWith("account."));
  useEffect(() => {
    const timer = setInterval(quota.reload, REFRESH_MS);
    return () => clearInterval(timer);
  }, [quota.reload]);
  return quota;
}

export function QuotaSection() {
  const quota = useQuota();
  const [adding, setAdding] = useState(false);
  const [open, setOpen] = useState<string>();
  const accounts = quota.data?.accounts ?? [];

  return (
    <section className="settings-section">
      <div className="settings-head">
        <h2>订阅与额度</h2>
        {accounts.length > 0 && <span className="chip">{accounts.length} 个账号</span>}
        <div className="spacer" />
        {!adding && (
          <button className="ghost" onClick={() => setAdding(true)}>
            添加账号
          </button>
        )}
      </div>
      <ErrorBox error={quota.error} />
      {adding && <AddAccount providers={quota.data?.providers ?? []} onDone={() => (setAdding(false), quota.reload())} />}
      {quota.data && !accounts.length && !adding && (
        <p className="muted">还没有账号。登录过 Claude Code、Codex 或 CodeBuddy 的话，hub 启动时会自动把本机登录加进来；也可以手动添加。</p>
      )}
      {accounts.length > 0 && (
        <div className="accounts">
          {accounts.map((a, i) => (
            <AccountRow
              key={a.account.id}
              quota={a}
              color={seriesColor(i)}
              open={open === a.account.id}
              onToggle={() => setOpen(open === a.account.id ? undefined : a.account.id)}
              onChange={quota.reload}
            />
          ))}
        </div>
      )}
      {accounts.length > 0 && <AllUsage accounts={accounts} />}
    </section>
  );
}

/** Account i's colour in every chart: categorical slot i, in list order. */
const seriesColor = (i: number) => `var(--series-${(i % 8) + 1})`;

const shortWindow = (w: RateWindow) => windowLabel(w).replace(/限额$/, "");

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

/**
 * One account on one line: who it is, a short meter per rate-limit window, its worst health,
 * and its actions. Clicking the line opens the details (other limits, credits, token usage).
 */
function AccountRow({ quota, color, open, onToggle, onChange }: { quota: AccountQuota; color: string; open: boolean; onToggle: () => void; onChange: () => void }) {
  const { account, status } = quota;
  const [login, setLogin] = useState<LoginStarted>();
  const [error, setError] = useState<string>();
  const remove = useArmed();
  const plan = planLabel(status.plan);
  const credits = status.credits;
  const windows = status.rate_limits;
  // Cursor's list has one ID per mode; the chips show each base model once.
  const cursor = account.provider === "cursor";
  const models = cursor ? byBase(quota.models) : quota.models;
  const levels = windows.map((w) => level(w.used_percent, status.limit_reached && w.used_percent >= 100));
  const worst: Level | undefined = levels.includes("danger") ? "danger" : levels.includes("warn") ? "warn" : levels.length ? "ok" : undefined;

  // The login notice goes once the account turns logged in.
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
    <div className={`account-row ${open ? "open" : ""}`}>
      <div className="account-line">
        <button className="account-who" onClick={onToggle} aria-expanded={open} title={status.email ?? undefined}>
          <span className="series-key" style={{ background: color }} />
          <VendorMark plugin={account.provider} size={14} />
          <span className="account-name">{account.label || account.id}</span>
          {account.codebuddy_site && <span className="chip">{account.codebuddy_site === "china" ? "国内站" : "国际站"}</span>}
          {plan && <span className="chip">{plan}</span>}
        </button>
        <div className="account-meters">
          {!status.logged_in ? (
            <span className="chip warn">未登录</span>
          ) : windows.length ? (
            windows.map((w, i) => {
              const pct = Math.max(0, Math.min(100, Math.round(w.used_percent)));
              return (
                <span key={w.name} className="meter" title={`${windowLabel(w)} · ${poolText(w) ?? `${pct}% 已用`}${w.resets_at ? ` · ${resetText(w.resets_at)}` : ""}`}>
                  <span className="meter-label">{shortWindow(w)}</span>
                  <span className={`quota-bar ${levels[i]}`}>
                    <span style={{ width: `${pct}%` }} />
                  </span>
                  <span className="meter-pct">{pct}%</span>
                </span>
              );
            })
          ) : (
            <span className="muted small">暂无额度数据</span>
          )}
        </div>
        {/* Always a cell, so the actions keep their column when there is no quota data. */}
        <span className="account-health">
          {worst && (
            <span className={`health ${worst}`}>
              <span aria-hidden="true">{HEALTH[worst].glyph}</span>
              {HEALTH[worst].label}
            </span>
          )}
        </span>
        <div className="account-actions">
          <button className="ghost small" onClick={startLogin}>
            {status.logged_in ? "重新登录" : "登录"}
          </button>
          <button
            className={`ghost small danger ${remove.armed ? "armed" : ""}`}
            onClick={() => remove.click(() => api.deleteAccount(account.id).then(onChange, (err) => setError(errorText(err))))}
            title="只是不再管理它；账号目录里的文件不会删除"
          >
            {remove.armed ? "再点一次移除" : "移除"}
          </button>
          <button className="icon-btn" onClick={onToggle} aria-label={open ? "收起详情" : "展开详情"} title={open ? "收起详情" : "详情"}>
            <span className={`disclose ${open ? "open" : ""}`}>
              <Icon name="chevron" size={14} />
            </span>
          </button>
        </div>
      </div>

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
            <>登录工具已启动（{login.command}），按它的提示完成。</>
          )}
        </div>
      )}
      <ErrorBox error={error} />

      {open && (
        <div className="account-detail">
          {windows.length > 0 && (
            <div className="quota">
              <div className="quota-title">
                {windows.some((w) => w.total != null) ? `积分 · 剩 ${creditNumber(windows.reduce((s, w) => s + (w.remaining ?? 0), 0))}` : "用量限额"}
                {status.email ? ` · ${status.email}` : ""}
              </div>
              {windows.map((w) => (
                <WindowRow key={w.name} window={w} reached={status.limit_reached} />
              ))}
            </div>
          )}
          {/* Separate limits beyond the main one, e.g. one per model. */}
          {status.limits.slice(1).map((group) => (
            <div key={group.id} className="quota">
              <div className="quota-title">
                {group.name ?? group.model ?? group.id} 的限额
                {group.model && group.name && <span className="muted"> · {group.model}</span>}
              </div>
              {group.windows.map((w) => (
                <WindowRow key={w.name} window={w} reached={group.reached} />
              ))}
            </div>
          ))}
          <ResetCredits credits={status.reset_credits} />
          {credits && (credits.has_credits || credits.unlimited) && (
            <div className="quota-row plain">
              <div className="quota-label">
                额外额度
                <div className="quota-sub">限额用完后从这里扣</div>
              </div>
              <div className="quota-value">{credits.unlimited ? "不限" : `余额 ${creditText(credits.balance) ?? "—"}`}</div>
            </div>
          )}
          <TokenUsage id={account.id} />
          {models.length > 0 && (
            <div className="models">
              可用模型：
              {models.map((m) => (
                <span key={m.id} className="chip" title={m.description ?? undefined}>
                  {cursor ? prettyModel(cursorMode(m.id).base) : m.name || m.id}
                  {m.context_window ? ` · ${Math.round(m.context_window / 1000)}k` : ""}
                  {m.credit_multiplier != null && <span className="muted"> · ×{m.credit_multiplier}</span>}
                </span>
              ))}
            </div>
          )}
          <div className="account-foot">
            <span>
              {status.source === "live" ? "实时" : status.source === "snapshot" ? "来自本地会话记录" : "暂无额度数据"}
              {status.as_of ? ` · ${ago(status.as_of)}更新` : ""}
            </span>
            {status.note && <span title={status.note}>· {status.note}</span>}
            {quota.agents.length > 0 && <span>· 使用中：{quota.agents.map((n) => `@${n}`).join(" ")}</span>}
          </div>
        </div>
      )}
    </div>
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
    <div className="quota-row plain" title={available.map((c) => `${c.title}${c.description ? ` — ${c.description}` : ""}`).join("\n")}>
      <div className="quota-label">
        额度重置券
        <div className="quota-sub">用一张可以把限额清零，在 Codex 里使用</div>
      </div>
      <div className="quota-value">
        {available.length} 张可用{date ? ` · 最早 ${date.getMonth() + 1}月${date.getDate()}日过期` : ""}
      </div>
    </div>
  );
}

/** Quota health in words and a glyph, never colour alone. */
const HEALTH = {
  ok: { label: "充足", glyph: "✓" },
  warn: { label: "偏紧", glyph: "!" },
  danger: { label: "快用完", glyph: "✕" },
} as const;

/** "剩 466 / 500" for a pool of credits (CodeBuddy); undefined for a time window. */
function poolText(w: RateWindow): string | undefined {
  return w.total != null ? `剩 ${creditNumber(w.remaining ?? 0)} / ${creditNumber(w.total)}` : undefined;
}

function WindowRow({ window: w, reached }: { window: RateWindow; reached: boolean }) {
  const pct = Math.max(0, Math.min(100, Math.round(w.used_percent)));
  const tone = level(w.used_percent, reached && pct >= 100);
  return (
    <div className="quota-row">
      <div className="quota-label">
        {windowLabel(w)}
        {w.resets_at && <div className="quota-sub">{resetText(w.resets_at)}</div>}
      </div>
      <div className={`quota-bar ${tone}`} role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
        <span style={{ width: `${pct}%` }} />
      </div>
      <div className="quota-value">
        <span className={`health ${tone}`}>
          <span aria-hidden="true">{HEALTH[tone].glyph}</span>
          {pct >= 100 ? "已用完" : HEALTH[tone].label}
        </span>
        {poolText(w) ?? `${pct}% 已用`}
      </div>
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
  const [provider, setProvider] = useState("");
  const [id, setId] = useState("");
  const [label, setLabel] = useState("");
  const [source, setSource] = useState<"import" | "new">("new");
  const [site, setSite] = useState<"china" | "international">("china");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const chosen = provider || providers[0] || "";

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      const account = await api.createAccount({
        codebuddy_site: chosen === "codebuddy" && source === "new" ? site : null,
        id: id.trim(),
        provider: chosen,
        label: label.trim() || null,
        home: null,
        import_default: source === "import",
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
      <div className="row" style={{ alignItems: "flex-start" }}>
        <Field label="服务">
          <select value={chosen} onChange={(e) => setProvider(e.target.value)}>
            {providers.map((p) => (
              <option key={p}>{p}</option>
            ))}
          </select>
        </Field>
        <Field label="账号 id（小写）">
          <input value={id} onChange={(e) => setId(e.target.value)} required placeholder={`${chosen}-work`} />
        </Field>
        <Field label="显示名">
          <input value={label} onChange={(e) => setLabel(e.target.value)} placeholder="工作账号" />
        </Field>
      </div>
      <div className="choice">
        <label>
          <input type="radio" checked={source === "new"} onChange={() => setSource("new")} /> 新的独立登录（会打开浏览器登录）
        </label>
        <label>
          <input type="radio" checked={source === "import"} onChange={() => setSource("import")} /> 使用本机已有的登录
        </label>
      </div>
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
        <button type="submit" className="primary" disabled={busy || !id.trim() || !chosen}>
          添加
        </button>
        <button type="button" className="ghost" onClick={onDone}>
          取消
        </button>
      </div>
    </form>
  );
}
