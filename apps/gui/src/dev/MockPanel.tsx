// The scenario switcher, bottom right, only in `preview:mock`.

import { useState } from "react";
import { PAPERS, setTheme, useTheme, type Mode, type Paper } from "../lib/theme";
import { SCENARIOS } from "./scenarios";

export function MockPanel({ current, narrow, go }: { current: string; narrow: boolean; go: (scenario: string, narrow: boolean) => void }) {
  const [open, setOpen] = useState(false);
  const theme = useTheme();
  const scenario = SCENARIOS.find((s) => s.key === current);
  const looks: { label: string; mode: Mode; paper?: Paper }[] = [
    ...PAPERS.map((p) => ({ label: p.label, mode: "light" as Mode, paper: p.key })),
    { label: "深色", mode: "dark", paper: theme.paper === "ink" ? "plain" : undefined },
    { label: "深色·墨", mode: "dark", paper: "ink" },
  ];
  const on = (l: (typeof looks)[number]) =>
    theme.mode === l.mode && (l.mode === "dark" ? (theme.paper === "ink") === (l.paper === "ink") : theme.paper === l.paper);

  if (!open) {
    return (
      <button className="mock-pill" onClick={() => setOpen(true)} title="mock 预览：切换场景">
        <span className="mock-dot" /> Mock · {scenario?.label ?? current}
        {narrow ? " · 900px" : ""}
      </button>
    );
  }
  return (
    <div className="mock-panel" role="dialog" aria-label="mock 场景">
      <header>
        <strong>Mock 预览</strong>
        <span className="mock-sub">没有 hub，数据是假的</span>
        <button className="mock-x" onClick={() => setOpen(false)} aria-label="收起">
          ×
        </button>
      </header>
      <div className="mock-list">
        {SCENARIOS.map((s) => (
          <button key={s.key} className={`mock-item ${s.key === current ? "on" : ""}`} onClick={() => go(s.key, narrow)} title={s.hint}>
            <span className="mock-name">{s.label}</span>
            <span className="mock-hint">{s.hint}</span>
          </button>
        ))}
      </div>
      <label className="mock-check">
        <input type="checkbox" checked={narrow} onChange={(e) => go(current, e.target.checked)} />
        窄窗口（900px）
      </label>
      <div className="mock-looks">
        {looks.map((l) => (
          <button key={l.label} className={on(l) ? "on" : ""} onClick={() => setTheme(l.paper ? { mode: l.mode, paper: l.paper } : { mode: l.mode })}>
            {l.label}
          </button>
        ))}
      </div>
      <button className="mock-replay" onClick={() => go(current, narrow)}>
        重放（重置状态，重新播放事件）
      </button>
    </div>
  );
}
