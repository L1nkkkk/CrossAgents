// Subscriptions and their quota, laid out like Claude Code's usage settings: one block per
// account, a bar per rate-limit window with "N% 已用" and when it resets. Vendor-neutral: every
// provider's AccountStatus renders the same way.

import { useEffect, useState, type FormEvent } from "react";
import type { AccountQuota } from "@bindings/AccountQuota";
import type { AccountUsageGroup } from "@bindings/AccountUsageGroup";
import type { RateWindow } from "@bindings/RateWindow";
import { api, errorText } from "../api";
import { ErrorBox, Field, ago } from "../components";
import { useHubData } from "../hub";
import { creditText, level, planLabel, resetText, windowLabel } from "../lib/quota";
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
  const accounts = quota.data?.accounts ?? [];

  return (
    <section className="settings-section">
      <div className="settings-head">
        <h2>订阅与额度</h2>
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
        <p className="muted">还没有账号。登录过 Codex 的话，hub 启动时会自动把本机登录加进来；也可以手动添加。</p>
      )}
      {accounts.map((a) => (
        <AccountBlock key={a.account.id} quota={a} onChange={quota.reload} />
      ))}
    </section>
  );
}

function AccountBlock({ quota, onChange }: { quota: AccountQuota; onChange: () => void }) {
  const { account, status } = quota;
  const [login, setLogin] = useState<{ url: string | null; command: string }>();
  const [error, setError] = useState<string>();
  const [showUsage, setShowUsage] = useState(false);
  const remove = useArmed();
  const plan = planLabel(status.plan);
  const credits = status.credits;

  async function startLogin() {
    setError(undefined);
    try {
      setLogin(await api.login(account.id));
    } catch (err) {
      setError(errorText(err));
    }
  }

  return (
    <div className="account">
      <div className="account-head">
        <span className="provider">{account.provider}</span>
        <span className="account-name">{account.label || account.id}</span>
        {plan && <span className="chip">{plan}</span>}
        {status.email && <span className="muted small">{status.email}</span>}
        <div className="spacer" />
        {!status.logged_in && <span className="chip warn">未登录</span>}
        <button className="ghost small" onClick={startLogin}>
          {status.logged_in ? "重新登录" : "登录"}
        </button>
        <button
          className={`ghost small ${remove.armed ? "danger armed" : ""}`}
          onClick={() => remove.click(() => api.deleteAccount(account.id).then(onChange, (err) => setError(errorText(err))))}
          title="只是不再管理它；账号目录里的文件不会删除"
        >
          {remove.armed ? "再点一次移除" : "移除"}
        </button>
      </div>

      {login && (
        <div className="notice">
          {login.url ? (
            <>
              在浏览器里完成登录：
              <a href={login.url} target="_blank" rel="noreferrer">
                打开登录页
              </a>
            </>
          ) : (
            <>登录工具已启动（{login.command}），按它的提示完成。</>
          )}
        </div>
      )}
      <ErrorBox error={error} />

      {status.rate_limits.length > 0 && (
        <div className="quota">
          <div className="quota-title">用量限额</div>
          {status.rate_limits.map((w) => (
            <WindowRow key={w.name} window={w} reached={status.limit_reached} />
          ))}
        </div>
      )}
      {credits && (credits.has_credits || credits.unlimited) && (
        <div className="quota-row plain">
          <div className="quota-label">
            额外额度
            <div className="quota-sub">限额用完后从这里扣</div>
          </div>
          <div className="quota-value">{credits.unlimited ? "不限" : `余额 ${creditText(credits.balance) ?? "—"}`}</div>
        </div>
      )}

      <div className="account-foot">
        <span>
          {status.source === "live" ? "实时" : status.source === "snapshot" ? "来自本地会话记录" : "暂无额度数据"}
          {status.as_of ? ` · ${ago(status.as_of)}更新` : ""}
        </span>
        {status.note && <span title={status.note}>· {status.note}</span>}
        {quota.agents.length > 0 && <span>· 使用中：{quota.agents.map((n) => `@${n}`).join(" ")}</span>}
        <div className="spacer" />
        <button className="ghost small" onClick={() => setShowUsage(!showUsage)} aria-expanded={showUsage}>
          Token 用量 {showUsage ? "▾" : "▸"}
        </button>
      </div>
      {showUsage && <TokenUsage id={account.id} />}
      {quota.models.length > 0 && showUsage && (
        <div className="models">
          可用模型：
          {quota.models.map((m) => (
            <span key={m.id} className="chip" title={m.description ?? undefined}>
              {m.name || m.id}
              {m.context_window ? ` · ${Math.round(m.context_window / 1000)}k` : ""}
            </span>
          ))}
        </div>
      )}
    </div>
  );
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
      <div className="quota-value">{pct}% 已用</div>
    </div>
  );
}

const USAGE_GROUPS: { by: AccountUsageGroup; label: string }[] = [
  { by: "day", label: "按天" },
  { by: "model", label: "按模型" },
  { by: "agent", label: "按 agent" },
  { by: "origin", label: "按来源" },
];

/** Tokens this login spent, from the vendor's own session logs (also outside CrossAgents). */
function TokenUsage({ id }: { id: string }) {
  const [by, setBy] = useState<AccountUsageGroup>("day");
  const usage = useHubData(() => api.accountUsage(id, by), [id, by], (e) => e.type === "agent.usage");
  const rows = usage.data?.by === by ? usage.data.rows : undefined;
  const n = (x: number) => x.toLocaleString();
  return (
    <div className="token-usage">
      <div className="segmented small">
        {USAGE_GROUPS.map((g) => (
          <button key={g.by} className={g.by === by ? "active" : ""} onClick={() => setBy(g.by)}>
            {g.label}
          </button>
        ))}
      </div>
      <ErrorBox error={usage.error} />
      {rows && !rows.length && <p className="muted small">没有会话记录。</p>}
      {rows && rows.length > 0 && (
        <table className="table compact">
          <thead>
            <tr>
              <th />
              <th className="num">会话</th>
              <th className="num">输入</th>
              <th className="num">缓存命中</th>
              <th className="num">输出</th>
              <th className="num">推理</th>
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
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const chosen = provider || providers[0] || "";

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      const account = await api.createAccount({
        id: id.trim(),
        provider: chosen,
        label: label.trim() || null,
        home: null,
        import_default: source === "import",
      });
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
