// The window's title bar, drawn by the page like Codex's desktop app: back and forward, the
// sidebar toggle and the 文件 / 编辑 / 视图 / 帮助 menus; the strip drags the window. Page and
// tool-column headers stay the first row of their own panels, as in Codex.
// The window buttons are the system's (macOS traffic lights inset on the left, an overlay on
// the right elsewhere; electron/main.cts), so the strip leaves room for them.

import { useEffect, useState, useSyncExternalStore } from "react";
import { Icon } from "./components";
import { useMedia } from "./lib/media";
import { isMac, shortcut, shortcutPlatform } from "./lib/shortcuts";
import { MenuBar } from "./menubar";

/** Only the desktop app has a window to draw a title bar for. */
export const inDesktop =
  typeof navigator !== "undefined" &&
  (/Electron/i.test(navigator.userAgent) || (__MOCK__ && new URLSearchParams(location.search).has("titlebar")));

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

  return (
    <header className={`titlebar ${isMac ? "mac" : ""} ${focused ? "" : "blurred"}`}>
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
    </header>
  );
}
