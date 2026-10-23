// 文件 / 编辑 / 视图 / 帮助 in the title bar, like Codex's: the app's commands and their
// shortcuts in one findable place, drawn by the page (the window has no native menu). Once a
// menu is open, hovering another title opens that one, as in a native menu bar. In a small
// window the four fold into one ≡ button, as in Claude's desktop app.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { api, getBridge } from "./api";
import type { CrossAgentsBridge } from "./bridge";
import { Icon } from "./components";
import { useMedia } from "./lib/media";
import { MODES, setTheme, useTheme } from "./lib/theme";
import { toggleRail, useRail } from "./rail";
import { COMPACT, toggleSidebar, useSidebarHidden } from "./titlebar";

type Command = Parameters<NonNullable<CrossAgentsBridge["command"]>>[0];

type Entry = { label: string; keys?: string; on?: boolean; disabled?: boolean; run: () => void } | "-" | { heading: string };

const REPO = "https://github.com/L1nkkkk/CrossAgents";

/** Starts something new on the start page in `mode` (the page listens when already open). */
export function startNew(mode: "chat" | "run") {
  try {
    localStorage.setItem("crossagents.mode", JSON.stringify(mode));
  } catch {
    // The start page then keeps its mode.
  }
  window.dispatchEvent(new CustomEvent("crossagents:mode", { detail: mode }));
  window.location.hash = "/";
}

const command = (name: Command) => getBridge()?.command?.(name);

/** Runs an editing command on whatever had focus before the menu opened. */
const edit = (name: "undo" | "redo" | "cut" | "copy" | "paste" | "selectAll") => () => {
  // execCommand is deprecated but is still what reaches the focused field in Chromium.
  document.execCommand(name);
};

export function MenuBar() {
  const [open, setOpen] = useState<number>();
  const [dialog, setDialog] = useState<"keys" | "about">();
  const ref = useRef<HTMLDivElement>(null);
  const theme = useTheme();
  const sidebarHidden = useSidebarHidden();
  const rail = useRail();
  const compact = useMedia(COMPACT);
  const dev = typeof location !== "undefined" && location.protocol.startsWith("http");

  useEffect(() => {
    if (open === undefined) return;
    const close = (e: MouseEvent | KeyboardEvent) => {
      if (e instanceof KeyboardEvent ? e.key === "Escape" : !ref.current?.contains(e.target as Node)) setOpen(undefined);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", close);
    window.addEventListener("blur", () => setOpen(undefined), { once: true });
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", close);
    };
  }, [open]);

  const menus: { title: string; entries: Entry[] }[] = [
    {
      title: "文件",
      entries: [
        { label: "新建 Chat", keys: "Ctrl+N", run: () => startNew("chat") },
        { label: "新建 Work", run: () => startNew("run") },
        "-",
        { label: "设置", keys: "Ctrl+,", run: () => (window.location.hash = "/settings") },
        "-",
        { label: "退出", keys: "Alt+F4", run: () => command("quit") },
      ],
    },
    {
      title: "编辑",
      entries: [
        { label: "撤销", keys: "Ctrl+Z", run: edit("undo") },
        { label: "重做", keys: "Ctrl+Y", run: edit("redo") },
        "-",
        { label: "剪切", keys: "Ctrl+X", run: edit("cut") },
        { label: "复制", keys: "Ctrl+C", run: edit("copy") },
        { label: "粘贴", keys: "Ctrl+V", run: edit("paste") },
        { label: "全选", keys: "Ctrl+A", run: edit("selectAll") },
        "-",
        { label: "搜索会话", keys: "Ctrl+K", run: focusSearch },
      ],
    },
    {
      title: "视图",
      entries: [
        { label: "显示侧栏", keys: "Ctrl+B", on: !sidebarHidden, run: toggleSidebar },
        { label: "显示信息栏", keys: "Ctrl+I", on: !rail.hidden, run: toggleRail },
        "-",
        ...MODES.map((m): Entry => ({ label: `主题：${m.label}`, on: theme.mode === m.key, run: () => setTheme({ mode: m.key }) })),
        "-",
        { label: "放大", keys: "Ctrl+=", run: () => command("zoomIn") },
        { label: "缩小", keys: "Ctrl+-", run: () => command("zoomOut") },
        { label: "实际大小", keys: "Ctrl+0", run: () => command("zoomReset") },
        { label: "全屏", keys: "F11", run: () => command("fullscreen") },
        ...(dev
          ? (["-", { label: "重新加载", keys: "Ctrl+R", run: () => command("reload") }, { label: "开发者工具", keys: "F12", run: () => command("devtools") }] as Entry[])
          : []),
      ],
    },
    {
      title: "帮助",
      entries: [
        { label: "键盘快捷键", keys: "Ctrl+/", run: () => setDialog("keys") },
        "-",
        { label: "项目主页", run: () => window.open(REPO, "_blank") },
        { label: "报告问题", run: () => window.open(`${REPO}/issues`, "_blank") },
        "-",
        { label: "关于 CrossAgents", run: () => setDialog("about") },
      ],
    },
  ];

  // Ctrl+/ opens the shortcut list from anywhere.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "/") {
        e.preventDefault();
        setDialog("keys");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Folded: one menu holding the four, each under its name.
  const shown = compact
    ? [{ title: "菜单", entries: menus.flatMap((m, i): Entry[] => [...(i ? ["-" as const] : []), { heading: m.title }, ...m.entries]) }]
    : menus;

  return (
    <div className="menubar" ref={ref} role="menubar">
      {shown.map((menu, i) => (
        <div key={menu.title} className="menubar-item">
          <button
            className={`menubar-title ${compact ? "folded" : ""} ${open === i ? "open" : ""}`}
            // Keep focus where it was, so 编辑 commands reach the field being edited.
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => setOpen(open === i ? undefined : i)}
            onMouseEnter={() => open !== undefined && setOpen(i)}
            aria-haspopup="menu"
            aria-expanded={open === i}
            aria-label={menu.title}
            title={compact ? "菜单" : undefined}
          >
            {compact ? <Icon name="menu" size={16} /> : menu.title}
          </button>
          {open === i && (
            <div className="menu menubar-menu" role="menu">
              {menu.entries.map((entry, j) =>
                entry === "-" ? (
                  <div key={j} className="menu-sep" role="separator" />
                ) : "heading" in entry ? (
                  <div key={j} className="menu-heading">
                    {entry.heading}
                  </div>
                ) : (
                  <button
                    key={j}
                    className="menu-item"
                    role="menuitem"
                    disabled={entry.disabled}
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={() => {
                      setOpen(undefined);
                      entry.run();
                    }}
                  >
                    <span className="menu-check">{entry.on && <Icon name="check" size={13} />}</span>
                    <span className="menu-text">
                      <span className="menu-label">{entry.label}</span>
                    </span>
                    {entry.keys && <kbd className="menu-keys">{entry.keys}</kbd>}
                  </button>
                ),
              )}
            </div>
          )}
        </div>
      ))}
      {dialog && <Dialog kind={dialog} onClose={() => setDialog(undefined)} />}
    </div>
  );
}

function focusSearch() {
  const input = document.querySelector<HTMLInputElement>(".sidebar .search input");
  if (input) {
    input.focus();
    input.select();
  }
}

const SHORTCUTS: [string, string][] = [
  ["Ctrl+N", "新建对话"],
  ["Ctrl+K", "搜索会话"],
  ["Ctrl+B", "显示 / 隐藏侧栏"],
  ["Ctrl+I", "显示 / 隐藏对话右侧的信息栏"],
  ["Alt+← / Alt+→", "后退 / 前进"],
  ["Ctrl+,", "设置"],
  ["Enter", "发送（对话里）"],
  ["Shift+Enter", "换行"],
  ["Ctrl+Enter", "发送（首页）"],
  ["Ctrl+Shift+Enter", "打断当前一轮并发送"],
  ["Esc", "中断当前一轮"],
  ["Ctrl+= / Ctrl+- / Ctrl+0", "放大 / 缩小 / 实际大小"],
  ["F11", "全屏"],
  ["Ctrl+/", "这份列表"],
];

function Dialog({ kind, onClose }: { kind: "keys" | "about"; onClose: () => void }) {
  const [info, setInfo] = useState<{ home: string; version: string }>();
  useEffect(() => {
    if (kind === "about") api.info().then(setInfo, () => {});
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [kind, onClose]);

  let body: ReactNode;
  if (kind === "keys") {
    body = (
      <>
        <h2>键盘快捷键</h2>
        <table className="keys-table">
          <tbody>
            {SHORTCUTS.map(([keys, what]) => (
              <tr key={keys}>
                <td>
                  <kbd>{keys}</kbd>
                </td>
                <td>{what}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </>
    );
  } else {
    body = (
      <>
        <h2>CrossAgents</h2>
        <p className="muted">跨厂商的多 agent 协作：Claude、Codex 等在同一个 hub 里对话、分工、合并。</p>
        <p className="small">
          hub {info ? `v${info.version}` : "未连接"}
          {info && (
            <>
              <br />
              <span className="mono muted">{info.home}</span>
            </>
          )}
        </p>
      </>
    );
  }
  return (
    <div className="dialog-backdrop" onMouseDown={onClose}>
      <div className="dialog" role="dialog" aria-modal="true" onMouseDown={(e) => e.stopPropagation()}>
        {body}
        <div className="row" style={{ justifyContent: "flex-end" }}>
          <button className="ghost" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
