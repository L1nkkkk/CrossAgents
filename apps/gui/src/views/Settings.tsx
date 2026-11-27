// Settings, laid out like Codex's: sections on the left, one page of cards on the right.
// 个性化 is kept on this device (docs/tasks/V6-GUI.md §4.4); 模型 is kept by the hub, so every
// window and the hub's own picks (e.g. a worker's model) see the same list.
// 关于 shows both builds and the GUI's own update (electron/updates.cts).

import { useEffect, useRef, useState, type ChangeEvent } from "react";
import type { HubInfo } from "@bindings/HubInfo";
import type { PluginInfo } from "@bindings/PluginInfo";
import { api, errorText } from "../api";
import { navigate } from "../App";
import { avatarDataUrlFromFile } from "../avatarImages";
import { ChatAvatar } from "../ChatAvatar";
import { ErrorBox, FamilyMark, Icon, VendorMark, type IconName } from "../components";
import { useConnectionStatus, usePlugins } from "../hub";
import { useFeatureSupport } from "../lib/features";
import { buildTag } from "../lib/build";
import { setChatProfile, useChatProfile, type ChatProfile } from "../lib/chatProfile";
import { useUpdateState } from "../lib/updates";
import { groupModels, onlyModels, prettyModel, switchModels, vendorLabel } from "../lib/models";
import { cursorFamilies, usesVariantReasoning } from "../lib/reasoning";
import { exportPreferences, importPreferences, PREFERENCES_MAX_BYTES } from "../lib/preferencesTransfer";
import { FONTS, MODES, PAPERS, resolve, setTheme, useTheme } from "../lib/theme";
import { AgentsPage } from "./Agents";
import { AvatarGallery } from "./AvatarGallery";
import { ConnectionPage } from "./Connection";
import { DevicesPage } from "./Devices";

const PAGES: { key: string; label: string; icon: IconName }[] = [
  { key: "appearance", label: "个性化", icon: "settings" },
  { key: "models", label: "模型", icon: "brain" },
  { key: "agents", label: "智能体", icon: "team" },
  { key: "connection", label: "连接", icon: "connection" },
  { key: "devices", label: "设备", icon: "device" },
  { key: "about", label: "关于", icon: "info" },
];

export function SettingsView({ page }: { page?: string }) {
  // 设备 came after 0.2.6 (docs/API-AUTH.md §4), so it is never inferred from a version: a hub
  // that does not list it hides the entry, and one that answered its request 403 keeps the
  // entry greyed with the hub's message.
  const devices = useFeatureSupport("devices");
  // Kept while the hub has not answered, so opening 设置 → 设备 directly does not flash another
  // page; hidden as soon as the hub says it has no such entry.
  const pages = PAGES.filter((p) => p.key !== "devices" || !devices.known || devices.show);
  const current = pages.find((p) => p.key === page) ?? pages[0];
  return (
    <>
      <div className="topbar">
        <span className="title">设置</span>
      </div>
      <div className="page">
        <div className="page-inner settings-layout">
          <nav className="settings-nav" aria-label="设置分类">
            {pages.map((p) => {
              const off = p.key === "devices" && devices.disabled;
              return (
                <button
                  key={p.key}
                  className={p.key === current.key ? "on" : ""}
                  aria-current={p.key === current.key ? "page" : undefined}
                  aria-disabled={off || undefined}
                  title={off ? devices.reason : undefined}
                  disabled={off}
                  onClick={() => navigate("settings", p.key)}
                >
                  <Icon name={p.icon} size={15} />
                  {p.label}
                </button>
              );
            })}
          </nav>
          <div className="settings">
            <h1 className="settings-title">{current.label}</h1>
            {current.key === "models" ? <ModelsPage /> : current.key === "agents" ? <AgentsPage /> : current.key === "connection" ? <ConnectionPage /> : current.key === "devices" ? <DevicesPage /> : current.key === "about" ? <AboutPage /> : <AppearancePage />}
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
      <ChatProfileSettings />
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
            <div className="setting-hint">{dark ? (theme.paper === "ink" ? "现在是深色；墨色的主操作在深色下也生效。" : "现在是深色；切回浅色时用这一套。") : "页面的底色，卡片和输入区浮在上面。"}</div>
          </div>
          <div className="papers" role="radiogroup" aria-label="浅色底色">
            {PAPERS.map((p) => (
              <button key={p.key} role="radio" aria-checked={theme.paper === p.key} className={`paper ${theme.paper === p.key ? "on" : ""}`} onClick={() => setTheme({ paper: p.key })} title={p.hint}>
                <span className="paper-swatch" style={{ background: p.swatch }}>
                  <span className="paper-card" style={p.card ? { background: p.card, borderColor: p.card } : undefined} />
                </span>
                {p.label}
              </button>
            ))}
          </div>
        </div>
        <div className="setting-line">
          <div>
            <div className="setting-label">字体风格</div>
            <div className="setting-hint">「柔和」优先苹方 / MiSans / 思源黑体并开启平滑抗锯齿；「衬线 · Claude」正文用 Source Serif + 思源宋体；代码保持等宽。</div>
          </div>
          <div className="segmented" role="radiogroup" aria-label="对话字体">
            {FONTS.map((f) => (
              <button key={f.key} role="radio" aria-checked={theme.font === f.key} className={theme.font === f.key ? "active" : ""} onClick={() => setTheme({ font: f.key })}>
                {f.label}
              </button>
            ))}
          </div>
        </div>
      </div>
      <AvatarGallery />
      <PreferencesTransfer />
    </section>
  );
}

function PreferencesTransfer() {
  const picker = useRef<HTMLInputElement>(null);
  const [error, setError] = useState<string>();
  function download() {
    try {
      const url = URL.createObjectURL(new Blob([exportPreferences(localStorage)], { type: "application/json" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = "CrossAgents-个性化设置.json";
      link.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      setError(undefined);
    } catch {
      setError("无法导出这台设备的设置。");
    }
  }

  async function upload(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    try {
      if (file.size > PREFERENCES_MAX_BYTES) throw new Error("设置文件超过 2 MiB");
      importPreferences(await file.text(), localStorage);
      location.reload();
    } catch (err) {
      setError(`导入失败。${errorText(err)}`);
    }
  }

  return (
    <div className="setting-card">
      <div className="setting-line">
        <div>
          <div className="setting-label">迁移个性化设置</div>
          <div className="setting-hint">在旧客户端导出，再到新客户端导入。导入会覆盖本机的主题、聊天资料和布局。</div>
        </div>
        <div className="row">
          <button type="button" onClick={download}>导出设置</button>
          <button type="button" onClick={() => picker.current?.click()}>导入设置</button>
          <input ref={picker} type="file" accept=".json,application/json" hidden onChange={upload} />
        </div>
      </div>
      <ErrorBox error={error} />
    </div>
  );
}

function ChatProfileSettings() {
  const profile = useChatProfile();
  const fileInput = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  function save(patch: Partial<ChatProfile>) {
    setError(undefined);
    void setChatProfile(patch).catch((err) => {
      setError(`头像/昵称已保存在本机，但同步到中心失败：${errorText(err)}`);
    });
  }

  async function chooseImage(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    setBusy(true);
    setError(undefined);
    try {
      const dataUrl = await avatarDataUrlFromFile(file);
      await setChatProfile({ avatar: dataUrl });
    } catch (err) {
      setError(`无法保存头像：${errorText(err)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="setting-card chat-profile-card">
      <div className="setting-line">
        <div className="chat-profile-identity">
          <ChatAvatar image={profile.avatar} size={44} />
          <div>
            <div className="setting-label">我的聊天资料</div>
            <div className="setting-hint">昵称和头像会自动同步到已连接的中心和其他设备。</div>
          </div>
        </div>
        <div className="row chat-profile-actions">
          <button type="button" disabled={busy} onClick={() => fileInput.current?.click()}>{busy ? "正在读取…" : "选择头像"}</button>
          {profile.avatar && <button type="button" className="ghost" disabled={busy} onClick={() => save({ avatar: null })}>恢复默认</button>}
          <input ref={fileInput} type="file" accept="image/png,image/jpeg,image/webp" hidden onChange={chooseImage} />
        </div>
      </div>
      <div className="setting-line">
        <label htmlFor="chat-profile-name" className="setting-label">聊天昵称</label>
        <input id="chat-profile-name" value={profile.name} onChange={(event) => save({ name: event.target.value })} maxLength={32} placeholder="我" />
      </div>
      <div className="setting-line">
        <div className="setting-label">聊天显示</div>
        <div className="segmented" role="group" aria-label="聊天显示">
          <button type="button" aria-pressed={profile.showAvatars} className={profile.showAvatars ? "active" : ""} onClick={() => save({ showAvatars: true })}>显示头像</button>
          <button type="button" aria-pressed={!profile.showAvatars} className={!profile.showAvatars ? "active" : ""} onClick={() => save({ showAvatars: false })}>只看消息</button>
        </div>
      </div>
      <ErrorBox error={error} />
    </div>
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
    case "manual":
      text = state.reason;
      break;
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
          {state && state.state !== "manual" && <div className="setting-hint">每 4 小时自动检查一次；托盘菜单里也可以检查。</div>}
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
  const plugins = usePlugins();
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
  // Cursor and Antigravity offer every mode of a model as its own ID; list them as one line
  // per base model, turned on and off together (the hub still needs the exact variant IDs).
  const cursor = usesVariantReasoning(p.name);
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
