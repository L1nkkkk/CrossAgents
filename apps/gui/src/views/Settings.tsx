// Settings, laid out like Codex's: sections on the left, one page of cards on the right.
// 外观 is kept on this device (docs/tasks/V6-GUI.md §4.4); 模型 is kept by the hub, so every
// window and the hub's own picks (e.g. a worker's model) see the same list.

import { useState } from "react";
import type { PluginInfo } from "@bindings/PluginInfo";
import { api, errorText } from "../api";
import { navigate } from "../App";
import { ErrorBox, Icon, VendorMark, type IconName } from "../components";
import { useHubData } from "../hub";
import { prettyModel, vendorLabel } from "../lib/models";
import { MODES, PAPERS, resolve, setTheme, useTheme } from "../lib/theme";
import { AgentsPage } from "./Agents";

const PAGES: { key: string; label: string; icon: IconName }[] = [
  { key: "appearance", label: "外观", icon: "settings" },
  { key: "models", label: "模型", icon: "brain" },
  { key: "agents", label: "智能体", icon: "team" },
];

export function SettingsView({ page }: { page?: string }) {
  const current = PAGES.find((p) => p.key === page) ?? PAGES[0];
  return (
    <>
      <div className="topbar">
        <span className="title">设置</span>
      </div>
      <div className="page">
        <div className="page-inner settings-layout">
          <nav className="settings-nav" aria-label="设置分类">
            {PAGES.map((p) => (
              <button key={p.key} className={p.key === current.key ? "on" : ""} aria-current={p.key === current.key ? "page" : undefined} onClick={() => navigate("settings", p.key)}>
                <Icon name={p.icon} size={15} />
                {p.label}
              </button>
            ))}
          </nav>
          <div className="settings">
            <h1 className="settings-title">{current.label}</h1>
            {current.key === "models" ? <ModelsPage /> : current.key === "agents" ? <AgentsPage /> : <AppearancePage />}
          </div>
        </div>
      </div>
    </>
  );
}

function AppearancePage() {
  const theme = useTheme();
  const dark = resolve(theme, matchMedia("(prefers-color-scheme: dark)").matches) === "dark";
  return (
    <section className="settings-section">
      <div className="setting-card">
        <div className="setting-line">
          <div>
            <div className="setting-label">主题</div>
            <div className="setting-hint">跟随系统时，系统切到深色这里也跟着切。</div>
          </div>
          <div className="segmented" role="radiogroup" aria-label="主题">
            {MODES.map((m) => (
              <button key={m.key} role="radio" aria-checked={theme.mode === m.key} className={theme.mode === m.key ? "active" : ""} onClick={() => setTheme({ mode: m.key })}>
                {m.label}
              </button>
            ))}
          </div>
        </div>
        <div className="setting-line">
          <div>
            <div className="setting-label">浅色底色</div>
            <div className="setting-hint">{dark ? "现在是深色；切回浅色时用这一套。" : "页面的底色，卡片和输入区浮在上面。"}</div>
          </div>
          <div className="papers" role="radiogroup" aria-label="浅色底色">
            {PAPERS.map((p) => (
              <button key={p.key} role="radio" aria-checked={theme.paper === p.key} className={`paper ${theme.paper === p.key ? "on" : ""}`} onClick={() => setTheme({ paper: p.key })} title={p.hint}>
                <span className="paper-swatch" style={{ background: p.swatch }}>
                  <span className="paper-card" />
                </span>
                {p.label}
              </button>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}

/** Per vendor, every model its logins can use, each with a switch for whether pickers list it. */
function ModelsPage() {
  const plugins = useHubData(() => api.listPlugins().then((r) => r.plugins), [], (e) => e.type.startsWith("account."));
  const [query, setQuery] = useState("");
  const list = (plugins.data ?? []).filter((p) => p.all_models.length > 0);
  return (
    <>
      <p className="settings-lead">新建 Chat、Work 和切换模型时，只列这里打开的模型；agent 找帮手、主导 agent 招人也只在这里面选。列表来自各账号登录后报告的模型，新登录的账号会自动补进来。</p>
      <label className="settings-search">
        <Icon name="search" size={14} />
        <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="搜索模型" aria-label="搜索模型" />
      </label>
      {plugins.error && <ErrorBox error={plugins.error} />}
      {list.map((p) => (
        <VendorModels key={p.name} plugin={p} query={query.trim().toLowerCase()} onSaved={plugins.reload} />
      ))}
      {plugins.data && !list.length && <div className="muted">还没有可用的模型：先在「订阅与额度」里登录一个账号。</div>}
    </>
  );
}

function VendorModels({ plugin, query, onSaved }: { plugin: PluginInfo; query: string; onSaved: () => void }) {
  const [saved, setSaved] = useState<PluginInfo>();
  const [error, setError] = useState<string>();
  const p = saved && saved.name === plugin.name ? saved : plugin;
  const all = p.all_models;
  const on = new Set(p.models);
  const allOn = !p.shown;
  const shownModels = all.filter((m) => !query || m.toLowerCase().includes(query) || prettyModel(m).toLowerCase().includes(query));

  async function save(next: string[] | null) {
    setError(undefined);
    try {
      setSaved(await api.setPluginModels(p.name, next));
      onSaved();
    } catch (err) {
      setError(errorText(err));
    }
  }

  const toggle = (model: string) => {
    const next = on.has(model) ? all.filter((m) => on.has(m) && m !== model) : all.filter((m) => on.has(m) || m === model);
    void save(next.length === all.length ? null : next);
  };

  if (query && !shownModels.length) return null;
  return (
    <section className="settings-section">
      <div className="settings-head">
        <VendorMark plugin={p.name} size={16} />
        <h2>{vendorLabel(p.name)}</h2>
        <span className="chip">
          {on.size} / {all.length}
        </span>
        <span className="spacer" />
        {!allOn && (
          <button className="ghost small" onClick={() => void save(null)}>
            全部打开
          </button>
        )}
        {on.size > 1 && (
          <button className="ghost small" onClick={() => void save([p.models[0]])} title="只留第一个，再逐个打开想要的">
            只留第一个
          </button>
        )}
      </div>
      {p.models_pinned && <div className="setting-hint">这个服务的模型列表写在 config.toml 的 [plugins.{p.name}] models 里，这里只能在其中挑选。</div>}
      <ErrorBox error={error} />
      <div className="setting-card model-grid">
        {shownModels.map((m) => {
          const checked = on.has(m);
          const last = checked && on.size === 1;
          return (
            <div key={m} className="setting-line">
              <div className="model-name">
                <div className="setting-label">
                  {prettyModel(m)}
                  {m === p.models[0] && <span className="chip small">默认</span>}
                </div>
                <div className="setting-hint mono">{m}</div>
              </div>
              <button
                role="switch"
                aria-checked={checked}
                aria-label={`${checked ? "关闭" : "打开"} ${m}`}
                className={`toggle ${checked ? "on" : ""}`}
                disabled={last}
                title={last ? "至少留一个" : undefined}
                onClick={() => toggle(m)}
              >
                <span />
              </button>
            </div>
          );
        })}
      </div>
    </section>
  );
}
