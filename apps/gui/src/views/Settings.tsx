// Settings kept on this device: appearance for now (docs/tasks/V6-GUI.md §4.4).

import { MODES, PAPERS, resolve, setTheme, useTheme } from "../lib/theme";

export function SettingsView() {
  const theme = useTheme();
  const dark = resolve(theme, matchMedia("(prefers-color-scheme: dark)").matches) === "dark";
  return (
    <>
      <div className="topbar">
        <span className="title">设置</span>
      </div>
      <div className="page">
        <div className="page-inner settings">
          <section className="settings-section">
            <div className="settings-head">
              <h2>外观</h2>
            </div>
            <div className="setting-row">
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
            <div className="setting-row">
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
          </section>
        </div>
      </div>
    </>
  );
}
