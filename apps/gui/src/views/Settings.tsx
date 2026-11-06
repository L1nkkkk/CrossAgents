// Settings, laid out like Codex's: sections on the left, one page of cards on the right.
// 外观 is kept on this device (docs/tasks/V6-GUI.md §4.4); 模型 is kept by the hub, so every
// window and the hub's own picks (e.g. a worker's model) see the same list.
// 关于 shows both builds and the GUI's own update (electron/updates.cts).

import { useEffect, useState } from "react";
import type { HubInfo } from "@bindings/HubInfo";
import type { PluginInfo } from "@bindings/PluginInfo";
import { api, errorText } from "../api";
import { navigate } from "../App";
import { ErrorBox, FamilyMark, Icon, VendorMark, type IconName } from "../components";
import { useConnectionStatus, useHubData } from "../hub";
import { buildTag } from "../lib/build";
import { useUpdateState } from "../lib/updates";
import { groupModels, onlyModels, prettyModel, switchModels, vendorLabel } from "../lib/models";
import { cursorFamilies } from "../lib/reasoning";
import { MODES, PAPERS, resolve, setTheme, useTheme } from "../lib/theme";
import { AgentsPage } from "./Agents";
import { AvatarGallery } from "./AvatarGallery";

const PAGES: { key: string; label: string; icon: IconName }[] = [
  { key: "appearance", label: "外观", icon: "settings" },
  { key: "models", label: "模型", icon: "brain" },
  { key: "agents", label: "智能体", icon: "team" },
  { key: "about", label: "关于", icon: "info" },
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
            {current.key === "models" ? <ModelsPage /> : current.key === "agents" ? <AgentsPage /> : current.key === "about" ? <AboutPage /> : <AppearancePage />}
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
      <AvatarGallery />
    </section>
  );
}

/** What this window and the hub were built from, and the GUI's own update. */
function AboutPage() {
  const status = useConnectionStatus();
  const [hub, setHub] = useState<HubInfo>();
  useEffect(() => {
    if (status === "connected") api.info().then(setHub, () => {});
  }, [status]);
  const page = __BUILD__;
  return (
    <section className="settings-section">
      <div className="setting-card">
        <div className="setting-line">
          <div>
            <div className="setting-label">界面</div>
            <div className="setting-hint">这个窗口</div>
          </div>
          <span className="mono">
            {buildTag(page)}
            {page.dev && <span className="chip small">dev</span>}
          </span>
        </div>
        <div className="setting-line">
          <div>
            <div className="setting-label">hub</div>
            <div className="setting-hint mono">{hub ? hub.home : status === "connected" ? "…" : "未连接"}</div>
          </div>
          {hub && (
            <span className="mono">
              {buildTag(hub)}
              {hub.debug && <span className="chip small">dev</span>}
            </span>
          )}
        </div>
      </div>
      <UpdateCard />
    </section>
  );
}

/** Checks for, downloads and installs a newer GUI; the next launch then upgrades the local hub. */
function UpdateCard() {
  const bridge = window.crossagents?.updates;
  const [state, setState] = useUpdateState();
  if (state === undefined) return null;
  const at = (iso: string) => new Date(iso).toLocaleString("zh-CN", { hour12: false });
  let text: string;
  let action: { label: string; run: () => void } | undefined;
  const check = { label: "检查更新", run: () => void bridge?.check().then(setState, () => {}) };
  switch (state?.state) {
    case undefined:
      text = "开发版和浏览器里打开的页面不会自动更新。";
      break;
    case "idle":
      text = "还没有检查过。";
      action = check;
      break;
    case "checking":
      text = "正在检查…";
      break;
    case "latest":
      text = `已是最新版本（${at(state.checked_at)} 检查）。`;
      action = check;
      break;
    case "downloading":
      text = `发现 ${state.version}，正在后台下载（${state.percent}%）…`;
      break;
    case "ready":
      text = `${state.version} 已下载，退出时自动安装。也可以现在重启安装：后台的 hub 和 agent 不会中断，重新打开后自动换成新版本。`;
      action = { label: "重启并安装", run: () => bridge?.install() };
      break;
    case "error":
      text = `检查更新失败（${at(state.checked_at)}）：${state.message}`;
      action = { label: "重试", run: check.run };
      break;
  }
  return (
    <div className="setting-card">
      <div className="setting-line">
        <div>
          <div className="setting-label">更新</div>
          <div className="setting-hint">{text}</div>
          {state && <div className="setting-hint">每 4 小时自动检查一次；托盘菜单里也可以检查。</div>}
        </div>
        {action && (
          <button className={state?.state === "ready" ? "primary small" : "ghost small"} onClick={action.run}>
            {action.label}
          </button>
        )}
      </div>
    </div>
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

/** Above this many models a vendor's list is grouped by family. */
const GROUP_FROM = 12;
/** Above this many, families with nothing on start folded. */
const FOLD_FROM = 20;

/** One line of the list: a model, or (Cursor) every variant of one base model. */
type ModelRows = { key: string; label: string; id: string; models: string[] };

function VendorModels({ plugin, query, onSaved }: { plugin: PluginInfo; query: string; onSaved: () => void }) {
  const [saved, setSaved] = useState<PluginInfo>();
  const [error, setError] = useState<string>();
  // Families the user opened or folded by hand; the rest follow the default.
  const [folds, setFolds] = useState<Record<string, boolean>>({});
  const p = saved && saved.name === plugin.name ? saved : plugin;
  const all = p.all_models;
  const on = new Set(p.models);
  const allOn = !p.shown;
  // Cursor offers every mode of a model as its own ID; list them as one line per base model,
  // turned on and off together (the hub still needs the exact variant IDs).
  const cursor = p.name === "cursor";
  const rows: ModelRows[] = cursor
    ? cursorFamilies(all).map((f) => ({ key: f.base, label: prettyModel(f.base), id: f.base, models: f.variants }))
    : all.map((m) => ({ key: m, label: prettyModel(m), id: m, models: [m] }));
  const byKey = new Map(rows.map((r) => [r.key, r]));
  const matches = (m: string) => !query || m.toLowerCase().includes(query) || prettyModel(m).toLowerCase().includes(query);
  // A Cursor line matches through its base or any variant; the others through themselves.
  const matchesRow = (r: ModelRows) => matches(r.key) || r.models.some(matches);
  const checked = (r: ModelRows) => r.models.every((m) => on.has(m));
  const shown = rows.filter(matchesRow);
  const onRows = rows.filter(checked);

  /** `undefined` is a list that would leave nothing on, which is refused. */
  async function save(next: string[] | null | undefined) {
    setError(undefined);
    if (next === undefined) {
      setError("至少要留一个模型。");
      return;
    }
    try {
      setSaved(await api.setPluginModels(p.name, next));
      onSaved();
    } catch (err) {
      setError(errorText(err));
    }
  }

  const toggle = (r: ModelRows) => void save(switchModels(all, on, r.models, !checked(r)));

  if (query && !shown.length) return null;

  const offAmongShown = shown.filter((r) => !checked(r)).length;
  const onlyShown = shown.length === onRows.length && shown.every(checked);
  const families = (rows.length > GROUP_FROM ? groupModels(rows.map((r) => r.key)) : []).map((f) => ({ ...f, rows: f.models.map((k) => byKey.get(k)!) }));
  const grouped = families.length > 1;
  const row = (r: ModelRows) => (
    <ModelRow key={r.key} label={r.label} model={r.id} checked={checked(r)} last={checked(r) && onRows.length === 1} isDefault={r.models.includes(p.models[0])} onToggle={() => toggle(r)} />
  );

  return (
    <section className="settings-section">
      <div className="settings-head">
        <VendorMark plugin={p.name} size={16} />
        <h2>{vendorLabel(p.name)}</h2>
        <span className="chip">{cursor ? `${onRows.length} / ${rows.length}` : `${on.size} / ${all.length}`}</span>
        <span className="spacer" />
        {query ? (
          <>
            {offAmongShown > 0 && (
              <button className="ghost small" onClick={() => void save(switchModels(all, on, shown.flatMap((r) => r.models), true))} title="把搜到的模型都打开，其余不动">
                打开这 {shown.length} 个
              </button>
            )}
            {!onlyShown && (
              <button className="ghost small" onClick={() => void save(onlyModels(all, shown.flatMap((r) => r.models)))} title="只留搜到的模型，其余全部关闭">
                只留这 {shown.length} 个
              </button>
            )}
          </>
        ) : (
          <>
            {!allOn && (
              <button className="ghost small" onClick={() => void save(null)}>
                全部打开
              </button>
            )}
            {(cursor ? onRows.length > 1 : on.size > 1) && (
              <button className="ghost small" onClick={() => void save(cursor ? onlyModels(all, rows[0].models) : [p.models[0]])} title="只留第一个，再逐个打开想要的">
                只留第一个
              </button>
            )}
          </>
        )}
      </div>
      {p.models_pinned && <div className="setting-hint">这个服务的模型列表写在 config.toml 的 [plugins.{p.name}] models 里，这里只能在其中挑选。</div>}
      <ErrorBox error={error} />
      {grouped ? (
        <div className="setting-card model-families">
          {families.map((f) => {
            const visible = f.rows.filter(matchesRow);
            if (!visible.length) return null;
            const count = f.rows.filter(checked).length;
            const full = count === f.rows.length;
            const folded = query ? false : (folds[f.key] ?? (rows.length > FOLD_FROM && count === 0));
            const flip = () => setFolds((s) => ({ ...s, [f.key]: !folded }));
            return (
              <div key={f.key} className="model-family">
                <div className="model-family-head">
                  <button className="model-family-name" aria-expanded={!folded} onClick={flip} disabled={!!query}>
                    <Icon name={folded ? "chevronRight" : "chevron"} size={14} />
                    <FamilyMark familyKey={f.key} size={14} />
                    <span className="setting-label">{f.label}</span>
                    <span className={`chip small ${count ? "" : "muted"}`}>
                      {count} / {f.rows.length}
                    </span>
                  </button>
                  <button
                    role="switch"
                    aria-checked={full ? true : count ? "mixed" : false}
                    aria-label={`${full ? "关闭" : "打开"} ${f.label} 全部 ${f.rows.length} 个模型`}
                    className={`toggle ${full ? "on" : count ? "mixed" : ""}`}
                    title={full ? "整组关闭" : "整组打开"}
                    onClick={() => void save(switchModels(all, on, f.rows.flatMap((r) => r.models), !full))}
                  >
                    <span />
                  </button>
                </div>
                {!folded && <div className="model-grid">{visible.map(row)}</div>}
              </div>
            );
          })}
        </div>
      ) : (
        <div className="setting-card model-grid">{shown.map(row)}</div>
      )}
    </section>
  );
}

function ModelRow({ label, model, checked, last, isDefault, onToggle }: { label: string; model: string; checked: boolean; last: boolean; isDefault: boolean; onToggle: () => void }) {
  return (
    <div className="setting-line">
      <div className="model-name">
        <div className="setting-label">
          {label}
          {isDefault && <span className="chip small">默认</span>}
        </div>
        <div className="setting-hint mono">{model}</div>
      </div>
      <button
        role="switch"
        aria-checked={checked}
        aria-label={`${checked ? "关闭" : "打开"} ${model}`}
        className={`toggle ${checked ? "on" : ""}`}
        disabled={last}
        title={last ? "至少留一个" : undefined}
        onClick={onToggle}
      >
        <span />
      </button>
    </div>
  );
}
