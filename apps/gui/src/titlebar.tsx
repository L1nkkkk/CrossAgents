// The window's title bar, drawn by the page like Codex's desktop app: back and forward, the
// sidebar toggle and the 文件 / 编辑 / 视图 / 帮助 menus; the strip drags the window. Page and
// tool-column headers stay the first row of their own panels, as in Codex.
// The window buttons are the system's (macOS traffic lights inset on the left, custom controls on
// the right elsewhere), so the strip leaves room for them.

import { useEffect, useState, useSyncExternalStore } from "react";
import { Icon } from "./components";
import { useMedia } from "./lib/media";
import { isMac, shortcut, shortcutPlatform } from "./lib/shortcuts";
import { MenuBar } from "./menubar";
import { setWindowMode } from "./windowMode";

/** Only the desktop app has a window to draw a title bar for. */
export const inTauri = typeof window !== "undefined" && "__TAURI__" in window;
export const inDesktop =
  typeof window !== "undefined" &&
  (inTauri || (__MOCK__ && new URLSearchParams(location.search).has("titlebar")));

// ---- The sidebar: docked unless hidden (remembered per browser). In a small window it has no
// column: it floats over the page while the pointer is on its button or on it (like Claude's
// desktop app), or until something is picked once opened by a click or Ctrl+B. ------------------

const SIDEBAR_KEY = "crossagents.sidebarHidden";
/** Narrower than this, the sidebar floats over the page instead of taking a column. */
export const COMPACT = "(max-width: 759px)";
const listeners = new Set<() => void>();
let sidebar = {
  hidden: (() => {
    try {
      return localStorage.getItem(SIDEBAR_KEY) === "1";
    } catch {
      return false;
    }
  })(),
  /** Floating over the page (small windows only): for as long as it is hovered, or until put
   *  away; not remembered. */
  peek: false as false | "hover" | "pinned",
};

function setSidebar(next: typeof sidebar) {
  sidebar = next;
  try {
    localStorage.setItem(SIDEBAR_KEY, sidebar.hidden ? "1" : "0");
  } catch {
    // Not remembered; fine.
  }
  listeners.forEach((l) => l());
}

export function toggleSidebar() {
  if (matchMedia(COMPACT).matches) setSidebar({ ...sidebar, peek: sidebar.peek === "pinned" ? false : "pinned" });
  else setSidebar({ ...sidebar, hidden: !sidebar.hidden });
}

let leaving: ReturnType<typeof setTimeout> | undefined;

/** The pointer reached the sidebar button or the floating sidebar. */
export function hoverSidebar() {
  clearTimeout(leaving);
  if (!sidebar.peek && matchMedia(COMPACT).matches) setSidebar({ ...sidebar, peek: "hover" });
}

/** The pointer left them: a hovered sidebar goes away, after a moment to cross the gap. */
export function unhoverSidebar() {
  clearTimeout(leaving);
  leaving = setTimeout(() => sidebar.peek === "hover" && setSidebar({ ...sidebar, peek: false }), 250);
}

export function closeSidebarPeek() {
  if (sidebar.peek) setSidebar({ ...sidebar, peek: false });
}

/** Where the sidebar is: `docked` in its column, `floating` over the page, or neither. */
export function useSidebar() {
  const state = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => sidebar,
  );
  const compact = useMedia(COMPACT);
  return { docked: !compact && !state.hidden, floating: compact && !!state.peek, pinned: compact && state.peek === "pinned" };
}

/** Whether the sidebar is out of sight. */
export const useSidebarHidden = () => {
  const { docked, floating } = useSidebar();
  return !docked && !floating;
};

/** Whether the page can go back / forward (Chromium's Navigation API; enabled without it). */
function useHistoryState() {
  const nav = (window as { navigation?: { canGoBack: boolean; canGoForward: boolean; addEventListener: (t: string, f: () => void) => void; removeEventListener: (t: string, f: () => void) => void } }).navigation;
  const read = () => ({ back: nav ? nav.canGoBack : true, forward: nav ? nav.canGoForward : true });
  const [state, setState] = useState(read);
  useEffect(() => {
    const update = () => setState(read());
    window.addEventListener("hashchange", update);
    nav?.addEventListener("currententrychange", update);
    return () => {
      window.removeEventListener("hashchange", update);
      nav?.removeEventListener("currententrychange", update);
    };
  });
  return state;
}

export function Titlebar({ title }: { title?: string }) {
  const sidebarHidden = useSidebarHidden();
  const { back, forward } = useHistoryState();
  const [focused, setFocused] = useState(() => document.hasFocus());
  useEffect(() => {
    const on = () => setFocused(true);
    const off = () => setFocused(false);
    window.addEventListener("focus", on);
    window.addEventListener("blur", off);
    return () => {
      window.removeEventListener("focus", on);
      window.removeEventListener("blur", off);
    };
  }, []);

  const onBarMouseDown = (event: React.MouseEvent<HTMLElement>) => {
    if (!inTauri || event.button !== 0) return;
    if ((event.target as Element).closest("button, a, input, [role='menu'], [role='menuitem']")) return;
    window.crossagents?.command?.("drag");
  };
  const onBarDoubleClick = (event: React.MouseEvent<HTMLElement>) => {
    if (!inTauri) return;
    if ((event.target as Element).closest("button, a, input, [role='menu'], [role='menuitem']")) return;
    window.crossagents?.command?.("maximize");
  };

  return (
    <header
      className={`titlebar ${isMac ? "mac" : ""} ${focused ? "" : "blurred"}`}
      onMouseDown={onBarMouseDown}
      onDoubleClick={onBarDoubleClick}
    >
      <div className="titlebar-nav">
        <button className="tb-btn" onClick={() => history.back()} disabled={!back} title="后退（Alt+←）" aria-label="后退">
          <Icon name="back" size={16} />
        </button>
        <button className="tb-btn" onClick={() => history.forward()} disabled={!forward} title="前进（Alt+→）" aria-label="前进">
          <Icon name="forward" size={16} />
        </button>
        <button className={`tb-btn ${sidebarHidden ? "" : "on"}`} onClick={toggleSidebar} onMouseEnter={hoverSidebar} onMouseLeave={unhoverSidebar} title={`${sidebarHidden ? "显示" : "隐藏"}侧栏（${shortcut(shortcutPlatform, "Ctrl+B")}）`} aria-label="切换侧栏" aria-pressed={!sidebarHidden}>
          <Icon name="sidebar" size={16} />
        </button>
      </div>
      <MenuBar />
      {title && <div className="titlebar-title">{title}</div>}
      <div className="titlebar-right">
        <div className="titlebar-mode-actions">
          <button
            type="button"
            className="tb-btn tb-mode-btn"
            onClick={() => void setWindowMode("mini")}
            title="迷你悬浮窗（置顶 / 透明度，打游戏时看进度 · Alt+Shift+3）"
            aria-label="切换到迷你窗"
          >
            <svg width="15" height="15" viewBox="0 0 16 16" fill="none" aria-hidden="true">
              <rect x="2" y="3" width="12" height="10" rx="2.2" stroke="currentColor" strokeWidth="1.35" />
              <rect x="7.6" y="7.4" width="4.4" height="3.6" rx="0.9" fill="currentColor" />
            </svg>
          </button>
          <button
            type="button"
            className="tb-btn tb-mode-btn"
            onClick={() => void setWindowMode("classic")}
            title="经典模式（QQ 风格侧边窄面板 · 贴边自动收起 · Alt+Shift+2）"
            aria-label="切换到经典模式"
          >
            <svg width="15" height="15" viewBox="0 0 16 16" fill="none" aria-hidden="true">
              <rect x="3.6" y="1.8" width="8.8" height="12.4" rx="2" stroke="currentColor" strokeWidth="1.35" />
              <path d="M6.2 5.2h3.6M6.2 8h3.6M6.2 10.8h2.4" stroke="currentColor" strokeWidth="1.25" strokeLinecap="round" />
            </svg>
          </button>
        </div>
        {inTauri && !isMac && (
          <>
            <span className="titlebar-sep" aria-hidden="true" />
            <div className="titlebar-controls">
              <button type="button" className="titlebar-win-btn" onClick={() => window.crossagents?.command?.("minimize")} title="最小化" aria-label="最小化">
                <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M1 5h8" stroke="currentColor" strokeWidth="1.2" /></svg>
              </button>
              <button type="button" className="titlebar-win-btn" onClick={() => window.crossagents?.command?.("maximize")} title="最大化" aria-label="最大化">
                <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><rect x="1.5" y="1.5" width="7" height="7" fill="none" stroke="currentColor" strokeWidth="1.1" /></svg>
              </button>
              <button type="button" className="titlebar-win-btn close" onClick={() => window.crossagents?.command?.("close")} title="关闭" aria-label="关闭">
                <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M1.5 1.5l7 7M8.5 1.5l-7 7" stroke="currentColor" strokeWidth="1.2" /></svg>
              </button>
            </div>
          </>
        )}
      </div>
    </header>
  );
}
