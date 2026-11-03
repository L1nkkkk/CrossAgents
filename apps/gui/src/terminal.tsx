// The person's terminals (crates/hub/src/terminal.rs), each a tab in a dock (panes.ts). They live
// in the hub; here they are kept outside React so a tab can move between docks, or a conversation
// or project be switched, without losing the screen: an xterm stays alive with its element, and
// `TerminalBody` only puts that element where it is showing. Tabs are grouped by project: a
// project's conversations share its terminals, and another project's run on out of sight.

import { useEffect, useRef, useSyncExternalStore } from "react";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal as XTerm } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import type { TerminalFrame } from "@bindings/TerminalFrame";
import type { TerminalInfo } from "@bindings/TerminalInfo";
import { api, errorText, getBridge } from "./api";
import { Icon, isAgent, useAgents } from "./components";
import { useHubData } from "./hub";
import { fromBase64, takeBatch, termLabels } from "./lib/terminals";
import { closeBottom, isTermTab, peekTab, placeTab, shutTab, termTab, useLayout, type DockTarget, type TabId, type TermTab } from "./panes";
import { CHATS, homeOf, isChat, wsKey } from "./workspace";

/** What a terminal's connection is doing, for the line above its screen. */
type Status =
  | { kind: "connecting" | "live" | "watching" | "reconnecting" | "gone" | "unsent" }
  | { kind: "exited"; code: number | null };

/** Only the screen, the connection and what is waiting to be typed: the rest is `info`. */
type Term = {
  info: TerminalInfo;
  xterm: XTerm;
  fit: FitAddon;
  host: HTMLDivElement;
  opened: boolean;
  connection: string;
  seq: number;
  queue: string[];
  sending: boolean;
  status: Status;
  focusOnShow: boolean;
};

/** A relay's own word on the stream (electron/main.cts), besides the hub's frames. */
type RelayFrame = TerminalFrame | { type: "reconnecting" } | { type: "gone" };

const terms = new Map<string, Term>();
const listeners = new Set<() => void>();
let snapshot: Term[] = [];
/** Where a new terminal opens: the focused conversation's, or the run's, directory and project. */
let scope: { project: string; cwd: string } | null = null;
let problem: string | null = null;
let view: { terms: Term[]; scope: { project: string; cwd: string } | null; problem: string | null } = { terms: snapshot, scope, problem };

function changed() {
  snapshot = [...terms.values()].sort((a, b) => a.info.created_at.localeCompare(b.info.created_at));
  view = { terms: snapshot, scope, problem };
  listeners.forEach((l) => l());
}

const useTerms = () =>
  useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => view,
  );

const setStatus = (term: Term, status: Status) => {
  term.status = status;
  changed();
};

/** The page's colours, for the screen to sit in it. */
function theme() {
  const css = getComputedStyle(document.documentElement);
  const pick = (name: string) => css.getPropertyValue(name).trim();
  return {
    background: pick("--surface"),
    foreground: pick("--text"),
    cursor: pick("--text"),
    cursorAccent: pick("--surface"),
    selectionBackground: pick("--accent-bg"),
  };
}

let unsubscribe: (() => void) | undefined;
let themeWatch: MutationObserver | undefined;

/** Starts listening once, the first time there is a terminal to listen for. */
function listen() {
  if (!unsubscribe) {
    unsubscribe = getBridge()?.onTerminal?.((id, data) => {
      const term = terms.get(id);
      if (term) onFrame(term, JSON.parse(data) as RelayFrame);
    });
  }
  if (!themeWatch) {
    themeWatch = new MutationObserver(() => {
      for (const term of terms.values()) term.xterm.options.theme = theme();
    });
    themeWatch.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  }
}

function onFrame(term: Term, frame: RelayFrame) {
  switch (frame.type) {
    case "snapshot":
      term.xterm.reset();
      if (term.xterm.cols !== frame.cols || term.xterm.rows !== frame.rows) term.xterm.resize(frame.cols, frame.rows);
      term.xterm.write(fromBase64(frame.data), () => fitTerm(term));
      if (term.status.kind === "connecting" || term.status.kind === "reconnecting") setStatus(term, { kind: "live" });
      break;
    case "output":
      term.xterm.write(fromBase64(frame.data));
      break;
    case "writer":
      setStatus(term, { kind: frame.connection === term.connection ? "live" : "watching" });
      break;
    case "exit":
      term.queue = [];
      term.info = { ...term.info, exited: true, exit_code: frame.code };
      setStatus(term, { kind: "exited", code: frame.code });
      break;
    case "reconnecting":
      setStatus(term, { kind: "reconnecting" });
      break;
    case "gone":
      setStatus(term, { kind: "gone" });
      break;
  }
}

/** Keeps a terminal here, reading its output, from now on. */
function adopt(info: TerminalInfo): Term {
  const known = terms.get(info.id);
  if (known) return known;
  listen();
  const css = getComputedStyle(document.documentElement);
  const xterm = new XTerm({
    cols: info.cols,
    rows: info.rows,
    fontFamily: css.getPropertyValue("--font-mono").trim() || "monospace",
    fontSize: 13,
    scrollback: 5000,
    cursorBlink: true,
    minimumContrastRatio: 4.5,
    theme: theme(),
  });
  const fit = new FitAddon();
  xterm.loadAddon(fit);
  const host = document.createElement("div");
  host.className = "term-host";
  const term: Term = {
    info,
    xterm,
    fit,
    host,
    opened: false,
    connection: crypto.randomUUID(),
    seq: 0,
    queue: [],
    sending: false,
    status: { kind: "connecting" },
    focusOnShow: false,
  };
  xterm.attachCustomKeyEventHandler((e) => {
    if (e.type !== "keydown") return true;
    // Ctrl+`: the app's. Ctrl+C with a selection copies; Ctrl+V and these leave to the browser,
    // whose copy and paste events xterm handles.
    if (e.ctrlKey && !e.altKey && e.key === "`") return false;
    if (e.ctrlKey && !e.altKey && !e.metaKey) {
      const key = e.key.toLowerCase();
      if (key === "v") return false;
      if (key === "c" && (e.shiftKey || xterm.hasSelection())) return false;
    }
    return true;
  });
  xterm.onData((data) => type(term, data));
  xterm.onBinary((data) => type(term, data));
  let sized: ReturnType<typeof setTimeout> | undefined;
  xterm.onResize(({ cols, rows }) => {
    clearTimeout(sized);
    sized = setTimeout(() => {
      if (term.status.kind === "exited" || term.status.kind === "gone" || term.status.kind === "watching") return;
      void api.resizeTerminal(info.id, { connection: term.connection, cols, rows }).catch(() => {
        // The next connection sets it again; a screen a little off until then does no harm.
      });
    }, 120);
  });
  terms.set(info.id, term);
  changed();
  getBridge()?.terminalStart?.(info.id, term.connection);
  if (info.exited) term.status = { kind: "exited", code: info.exit_code };
  return term;
}

/** Types in order, one batch at a time. A batch that fails is dropped, and so is what waits behind it. */
function type(term: Term, data: string) {
  if (term.status.kind === "watching" || term.status.kind === "exited" || term.status.kind === "gone") return;
  term.queue.push(data);
  void sendNext(term);
}

async function sendNext(term: Term) {
  if (term.sending) return;
  term.sending = true;
  while (term.queue.length) {
    const data = takeBatch(term.queue);
    try {
      await api.terminalInput(term.info.id, { connection: term.connection, seq: ++term.seq, data });
      if (term.status.kind === "unsent") setStatus(term, { kind: "live" });
    } catch {
      term.queue = [];
      if (term.status.kind === "live") setStatus(term, { kind: "unsent" });
    }
  }
  term.sending = false;
}

function fitTerm(term: Term) {
  if (!term.opened || !term.host.isConnected || term.host.clientWidth === 0 || term.host.clientHeight === 0) return;
  term.fit.fit();
}

const fail = (err: unknown) => {
  problem = `终端：${errorText(err)}`;
  changed();
};

export const dismissTerminalProblem = () => {
  problem = null;
  changed();
};

/** Whether this build can open terminals at all (a plain browser page has no relay). */
export const terminalsAvailable = () => !!getBridge()?.terminalStart;

/** A new terminal where `dock` says, in the focused conversation's directory. */
export async function newTerminal(dock: DockTarget = "bottom") {
  if (!scope || !terminalsAvailable()) return;
  try {
    const info = await api.createTerminal({ cwd: scope.cwd, project: scope.project, cols: 100, rows: 30, node: null });
    const term = adopt(info);
    term.focusOnShow = true;
    placeTab(termTab(info.id), dock);
  } catch (err) {
    fail(err);
  }
}

/** Ends a terminal: the × on its tab. */
export function closeTerminal(tab: TermTab) {
  const id = tab.slice("term:".length);
  const term = terms.get(id);
  shutTab(tab);
  if (!term) return;
  terms.delete(id);
  getBridge()?.terminalStop?.(id);
  term.xterm.dispose();
  term.host.remove();
  changed();
  if (term.status.kind !== "gone") void api.closeTerminal(id).catch(fail);
}

const belongs = (term: Term, project: string | null) => term.info.project === project;

/** Ctrl+`: to this project's terminal, away from it, or a first one. */
export function toggleTerminal(active: TabId | null, bottomOpen: boolean) {
  if (!scope) return;
  if (bottomOpen && active && isTermTab(active)) return closeBottom();
  const project = scope.project;
  const own = snapshot.filter((t) => belongs(t, project));
  const last = own[own.length - 1];
  if (!last) return void newTerminal();
  last.focusOnShow = true;
  peekTab(termTab(last.info.id));
}

/** The project and directory of this page; null where it has neither. */
export const useTerminalScope = () => useTerms().scope;

/** Whether a tab shows on this page: terminals only in their own project. */
export function useTabShows(): (tab: TabId) => boolean {
  const { terms: all, scope: here } = useTerms();
  return (tab) => {
    if (!isTermTab(tab)) return true;
    const term = all.find((t) => termTab(t.info.id) === tab);
    return !!term && belongs(term, here?.project ?? null);
  };
}

/** A terminal tab's name: its number in its project. */
export function useTabLabel(): (tab: TermTab) => string {
  const { terms: all } = useTerms();
  const labels = termLabels(all.map((t) => ({ id: t.info.id, project: t.info.project ?? "" })));
  return (tab) => labels.get(tab.slice("term:".length)) ?? "终端";
}

/**
 * Nothing to see: says which project this page is in, and picks up the terminals the hub already
 * has when the page loads. One a saved layout names that the hub no longer has goes with its tab.
 */
export function TerminalScope({ follow, run }: { follow?: string; run?: string }) {
  const agents = useAgents();
  const agent = follow ? agents.data?.find((a) => isAgent(a, follow)) : undefined;
  const detail = useHubData(() => (run ? api.getRun(run) : Promise.resolve(null)), [run], () => false);
  const repo = run && detail.data?.run.id === run ? detail.data.run.repo : undefined;
  const project = agent ? (isChat(agent) ? CHATS : wsKey(homeOf(agent))) : repo ? wsKey(repo) : null;
  const cwd = agent?.cwd ?? repo ?? null;
  useEffect(() => {
    scope = project && cwd ? { project, cwd } : null;
    changed();
  }, [project, cwd]);
  return null;
}

/** Picks up the hub's terminals as the page loads, and forgets tabs of ones long gone. */
export function TerminalHydrate() {
  const { state } = useLayout();
  const listed = useHubData(() => (terminalsAvailable() ? api.listTerminals() : Promise.resolve(null)), [], () => false);
  const once = useRef(false);
  useEffect(() => {
    if (!listed.data || once.current) return;
    once.current = true;
    const alive = new Set(listed.data.terminals.map((t) => t.id));
    for (const dock of [state.docks.right, state.docks.bottom]) {
      for (const tab of dock.tabs) if (isTermTab(tab) && !alive.has(tab.slice("term:".length))) shutTab(tab);
    }
    for (const info of listed.data.terminals) {
      adopt(info);
      const tab = termTab(info.id);
      if (!state.docks.right.tabs.includes(tab) && !state.docks.bottom.tabs.includes(tab)) placeTab(tab, "bottom", false);
    }
  }, [listed.data]);
  return null;
}

/** What could not be done, until it is read. */
export function TerminalProblem() {
  const { problem: text } = useTerms();
  if (!text) return null;
  return (
    <div className="term-problem" role="alert">
      <span>{text}</span>
      <button className="ghost icon" onClick={dismissTerminalProblem} aria-label="关闭">
        <Icon name="close" size={14} />
      </button>
    </div>
  );
}

/** Where a terminal's element sits while its tab shows. */
export function TerminalBody({ tab, dock }: { tab: TermTab; dock: DockTarget }) {
  const { terms: all } = useTerms();
  const term = all.find((t) => termTab(t.info.id) === tab);
  const box = useRef<HTMLDivElement>(null);
  const id = term?.info.id;

  useEffect(() => {
    const el = box.current;
    const live = id ? terms.get(id) : undefined;
    if (!el || !live) return;
    el.appendChild(live.host);
    if (!live.opened) {
      live.xterm.open(live.host);
      live.opened = true;
    }
    let frame = 0;
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => fitTerm(live));
    });
    observer.observe(el);
    fitTerm(live);
    const focus = requestAnimationFrame(() => {
      if (!live.focusOnShow) return;
      live.focusOnShow = false;
      live.xterm.focus();
    });
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
      cancelAnimationFrame(focus);
      // Kept: the next place it shows takes it over.
      live.host.remove();
    };
  }, [id]);

  if (!term) return <p className="muted small tool-blank">这个终端已经结束。</p>;
  return (
    <div className="term-pane">
      <TermStatus term={term} dock={dock} />
      <div className="term-box" ref={box} onClick={() => term.xterm.focus()} />
    </div>
  );
}

function TermStatus({ term, dock }: { term: Term; dock: DockTarget }) {
  const again = (
    <button className="ghost small" onClick={() => void newTerminal(dock)}>
      新建终端
    </button>
  );
  switch (term.status.kind) {
    case "connecting":
    case "live":
      return null;
    case "watching":
      return (
        <div className="term-status">
          <span>另一个窗口正在输入，这里只能看。</span>
          <button
            className="ghost small"
            onClick={() => {
              void api.takeoverTerminal(term.info.id, { connection: term.connection }).catch(fail);
            }}
          >
            接管输入
          </button>
        </div>
      );
    case "reconnecting":
      return (
        <div className="term-status">
          <span>和 hub 断开了，正在重连。终端还在运行。</span>
        </div>
      );
    case "unsent":
      return (
        <div className="term-status warn">
          <span>刚才的输入没有送达，也不会重发。请看一下屏幕再继续。</span>
        </div>
      );
    case "gone":
      return (
        <div className="term-status warn">
          <span>hub 重启后这个终端已经结束，这里是它结束前的内容。</span>
          {again}
        </div>
      );
    case "exited":
      return (
        <div className="term-status">
          <span>Shell 已退出{term.status.code === null ? "" : `，退出码 ${term.status.code}`}。</span>
          {again}
        </div>
      );
  }
}
